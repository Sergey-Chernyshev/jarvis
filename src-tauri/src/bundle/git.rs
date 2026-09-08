//! Git-слой связки: worktree на руку, ребейз, очередь в base.
//!
//! Исполняется через [`Host`] — одинаково на этой машине и на узле по ssh.
//! Каждая функция — одна git-операция с честным ответом; политика (когда
//! ребейзить, кого вливать) живёт этажом выше.

use super::host::Host;
use std::time::Duration;

/// Одна git-операция: stdout — данные, stderr — объяснение неудачи.
///
/// Потоки врозь принципиально. У ssh-хоста удалённый `bash -lc` ворчит на
/// старте про локаль, и в слитом потоке это ворчание становилось
/// «незакоммиченной правкой» в чистом дереве (`dirty`), лишним конфликтным
/// файлом при ребейзе и мусором в sha — то есть связка отказывалась вливать
/// по причине, которой нет.
async fn ok(host: &Host, dir: &str, args: &[&str]) -> Result<String, String> {
    let (code, out, err) = host.git_split(dir, args).await;
    if code == 0 {
        Ok(out)
    } else {
        let why = format!("{} {}", out.trim(), err.trim());
        Err(crate::util::ellipsize(
            &crate::util::one_line(why.trim()),
            300,
        ))
    }
}

pub async fn is_repo(host: &Host, dir: &str) -> bool {
    host.git(dir, &["rev-parse", "--git-dir"]).await.0 == 0
}

/// Базовая ветка: main, master — что есть.
pub async fn base_branch(host: &Host, repo: &str) -> Result<String, String> {
    for cand in ["main", "master"] {
        if host
            .git(
                repo,
                &["show-ref", "--verify", &format!("refs/heads/{cand}")],
            )
            .await
            .0
            == 0
        {
            return Ok(cand.into());
        }
    }
    Err("не нашёл ни main, ни master — укажи базовую ветку".into())
}

/// Привести каталог к пригодному для связки виду. Возвращает базовую ветку.
///
/// Как в «Проектах»: человек называет директорию, остальное — наша забота.
/// Нет каталога — создаём. Нет git — инициализируем. Нет ни одного коммита —
/// коммитим то, что лежит: worktree и ветки существуют только от коммита,
/// и без этого связке не от чего отпочковать руки.
pub async fn ensure_repo(host: &Host, dir: &str) -> Result<String, String> {
    let (code, out) = host
        .sh(
            if dir.starts_with('/') { "/" } else { "." },
            &format!("mkdir -p {}", crate::util::shell_quote(dir)),
            Duration::from_secs(20),
        )
        .await;
    if code != 0 {
        return Err(format!(
            "не создал каталог: {}",
            crate::util::one_line(&out)
        ));
    }
    if !is_repo(host, dir).await {
        ok(host, dir, &["init", "-q"]).await?;
    }
    if host.git(dir, &["rev-parse", "HEAD"]).await.0 != 0 {
        // Личность — флагами на один коммит: у свежей машины (и у CI) может не
        // быть git config, а падать из-за этого на первом шаге нечестно.
        ok(host, dir, &["add", "-A"]).await?;
        ok(
            host,
            dir,
            &[
                "-c",
                "user.email=jarvis@local",
                "-c",
                "user.name=jarvis",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "начало связки",
            ],
        )
        .await?;
    }
    base_branch(host, dir).await
}

/// Поднять worktree руки на своей ветке от базовой.
pub async fn add_worktree(
    host: &Host,
    repo: &str,
    dir: &str,
    branch: &str,
    base: &str,
) -> Result<(), String> {
    ok(host, repo, &["worktree", "add", "-b", branch, dir, base])
        .await
        .map(|_| ())
}

pub async fn head_sha(host: &Host, dir: &str) -> Result<String, String> {
    ok(host, dir, &["rev-parse", "HEAD"])
        .await
        .map(|s| s.trim().to_string())
}

/// В дереве есть незакоммиченное (включая неучтённые файлы).
pub async fn dirty(host: &Host, dir: &str) -> bool {
    match ok(
        host,
        dir,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .await
    {
        Ok(out) => !out.trim().is_empty(),
        Err(_) => true, // не смогли спросить — считаем грязным: осторожность дешевле
    }
}

/// Насколько ветка впереди базы.
pub async fn ahead(host: &Host, repo: &str, base: &str, branch: &str) -> u32 {
    ok(
        host,
        repo,
        &["rev-list", "--count", &format!("{base}..{branch}")],
    )
    .await
    .ok()
    .and_then(|s| s.trim().parse().ok())
    .unwrap_or(0)
}

/// Ветка уже стоит на актуальной базе (база — предок ветки).
pub async fn rebased(host: &Host, repo: &str, base: &str, branch: &str) -> bool {
    host.git(repo, &["merge-base", "--is-ancestor", base, branch])
        .await
        .0
        == 0
}

/// Файлы, которых рука коснулась относительно базы.
pub async fn changed_files(host: &Host, repo: &str, base: &str, branch: &str) -> Vec<String> {
    ok(
        host,
        repo,
        &["diff", "--name-only", &format!("{base}...{branch}")],
    )
    .await
    .map(|out| {
        out.lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .take(200)
            .collect()
    })
    .unwrap_or_default()
}

/// Итог попытки авторебейза.
#[derive(Debug, PartialEq)]
pub enum Rebase {
    Clean,
    /// Конфликт: файлы, на которых он случился. Ребейз откатан — дерево цело.
    Conflict(Vec<String>),
}

/// Попробовать перебазировать руку на базу — в её собственном worktree.
///
/// Конфликт НЕ решается здесь: откатываем ребейз и отдаём список файлов
/// наверх, а наверху его отдают агенту руки — он знает контекст своих правок,
/// мы нет. «Чинит сам» из дизайна — ровно этот путь.
pub async fn try_rebase(host: &Host, worktree: &str, base: &str) -> Result<Rebase, String> {
    let (code, out) = host.git(worktree, &["rebase", base]).await;
    if code == 0 {
        return Ok(Rebase::Clean);
    }
    let files = ok(host, worktree, &["diff", "--name-only", "--diff-filter=U"])
        .await
        .map(|o| {
            o.lines()
                .map(str::to_string)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let (abort_code, abort_out) = host.git(worktree, &["rebase", "--abort"]).await;
    if abort_code != 0 {
        // Полуразобранный ребейз хуже конфликта: дерево руки в промежуточном
        // состоянии, и дальше его трогать нельзя.
        return Err(format!("ребейз не откатился: {abort_out}"));
    }
    if files.is_empty() {
        return Err(crate::util::ellipsize(&crate::util::one_line(&out), 300));
    }
    Ok(Rebase::Conflict(files))
}

/// Где база сейчас выписана: (каталог, чистое ли дерево).
async fn base_checkout(host: &Host, repo: &str, base: &str) -> Option<(String, bool)> {
    let out = ok(host, repo, &["worktree", "list", "--porcelain"])
        .await
        .ok()?;
    let mut path: Option<String> = None;
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(p.trim().to_string());
        } else if let Some(b) = line.strip_prefix("branch ") {
            if b.trim() == format!("refs/heads/{base}") {
                let p = path.clone()?;
                let clean = !dirty(host, &p).await;
                return Some((p, clean));
            }
        }
    }
    None
}

/// Влить готовую руку: докрутить базу до её головы (fast-forward).
///
/// Рука перебазирована, так что вливание — сдвиг ссылки вперёд. Осторожность
/// одна, но принципиальная: база может быть выписана в рабочем дереве
/// человека. Чистое дерево двигаем честным `merge --ff-only` (ссылка и файлы
/// едут вместе); грязное не трогаем вовсе.
pub async fn ff_advance(host: &Host, repo: &str, base: &str, branch: &str) -> Result<(), String> {
    if !rebased(host, repo, base, branch).await {
        return Err("ветка не на актуальной базе — сначала ребейз".into());
    }
    match base_checkout(host, repo, base).await {
        Some((path, true)) => ok(host, &path, &["merge", "--ff-only", branch])
            .await
            .map(|_| ()),
        Some((path, false)) => Err(format!(
            "{base} выписана в {path} с незакоммиченной правкой — закоммить или спрячь, тогда волью"
        )),
        None => {
            let sha = ok(host, repo, &["rev-parse", branch])
                .await?
                .trim()
                .to_string();
            ok(
                host,
                repo,
                &["update-ref", &format!("refs/heads/{base}"), &sha],
            )
            .await
            .map(|_| ())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: &Host = &Host::Local;

    /// Настоящий репозиторий во временном каталоге — никакой имитации git.
    async fn repo(tag: &str) -> String {
        let dir = std::env::temp_dir().join(format!("jarvis-bundle-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_string_lossy().into_owned();
        assert_eq!(H.git(&d, &["init", "-q", "-b", "main"]).await.0, 0);
        H.git(&d, &["config", "user.email", "t@t"]).await;
        H.git(&d, &["config", "user.name", "t"]).await;
        commit(&d, "a.txt", "один\n", "начало").await;
        d
    }

    async fn commit(dir: &str, file: &str, text: &str, msg: &str) {
        std::fs::write(std::path::Path::new(dir).join(file), text).unwrap();
        assert_eq!(H.git(dir, &["add", "."]).await.0, 0);
        assert_eq!(H.git(dir, &["commit", "-q", "-m", msg]).await.0, 0, "{msg}");
    }

    fn sibling(dir: &str, name: &str) -> String {
        format!(
            "{}/{}-{}",
            super::super::host::parent_of(dir),
            name,
            std::process::id()
        )
    }

    #[tokio::test]
    async fn nested_new_directory_and_dirty_worktree_cleanup_preserve_data() {
        let root =
            std::env::temp_dir().join(format!("jarvis-bundle-nested-{}", std::process::id()));
        let dir = root.join("new/deep/repo");
        let base = ensure_repo(H, &dir.to_string_lossy()).await.unwrap();
        let wt = root.join("wt");
        add_worktree(
            H,
            &dir.to_string_lossy(),
            &wt.to_string_lossy(),
            "team/keep",
            &base,
        )
        .await
        .unwrap();
        std::fs::write(wt.join("unsaved.txt"), "human draft").unwrap();
        let (code, _) = H
            .git(
                &dir.to_string_lossy(),
                &["worktree", "remove", &wt.to_string_lossy()],
            )
            .await;
        assert_ne!(code, 0);
        assert_eq!(
            std::fs::read_to_string(wt.join("unsaved.txt")).unwrap(),
            "human draft"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn ensure_repo_builds_a_usable_base_from_nothing() {
        let dir = std::env::temp_dir()
            .join(format!("jarvis-fresh-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let _ = std::fs::remove_dir_all(&dir);
        // Каталога нет вовсе: связка создаёт его, инициализирует git и делает
        // первый коммит — иначе рукам не от чего отпочковаться.
        let base = ensure_repo(H, &dir).await.unwrap();
        assert!(["main", "master"].contains(&base.as_str()), "{base}");
        let wt = sibling(&dir, "wt-fresh");
        let _ = std::fs::remove_dir_all(&wt);
        add_worktree(H, &dir, &wt, "team/fresh", &base)
            .await
            .expect("worktree от свежеинициализированной базы");
        // Повторный вызов ничего не ломает.
        assert_eq!(ensure_repo(H, &dir).await.unwrap(), base);
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ensure_repo_commits_existing_files() {
        let dir = std::env::temp_dir()
            .join(format!("jarvis-files-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::Path::new(&dir).join("старое.txt"),
            "лежало тут\n",
        )
        .unwrap();
        ensure_repo(H, &dir).await.unwrap();
        // Файлы, что лежали в каталоге, вошли в первый коммит, дерево чистое.
        assert!(
            !dirty(H, &dir).await,
            "существующие файлы должны быть закоммичены"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn worktree_branch_and_merge_round_trip() {
        let r = repo("round").await;
        let wt = sibling(&r, "wt-round");
        let _ = std::fs::remove_dir_all(&wt);

        add_worktree(H, &r, &wt, "team/auth", "main").await.unwrap();
        assert!(is_repo(H, &wt).await);
        commit(&wt, "auth.txt", "логин\n", "экран логина").await;

        assert_eq!(ahead(H, &r, "main", "team/auth").await, 1);
        assert!(rebased(H, &r, "main", "team/auth").await);
        assert_eq!(
            changed_files(H, &r, "main", "team/auth").await,
            vec!["auth.txt".to_string()]
        );

        ff_advance(H, &r, "main", "team/auth").await.unwrap();
        assert_eq!(
            ahead(H, &r, "main", "team/auth").await,
            0,
            "после вливания рука не впереди"
        );

        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&r);
    }

    #[tokio::test]
    async fn conflict_is_reported_and_rolled_back() {
        let r = repo("conflict").await;
        let wt = sibling(&r, "wt-conf");
        let _ = std::fs::remove_dir_all(&wt);
        add_worktree(H, &r, &wt, "team/billing", "main")
            .await
            .unwrap();

        commit(&wt, "a.txt", "платёжка\n", "поле в типах").await;
        commit(&r, "a.txt", "auth\n", "переименование в типах").await;

        assert!(
            !rebased(H, &r, "main", "team/billing").await,
            "main уехал вперёд"
        );
        let sha_before = head_sha(H, &wt).await.unwrap();
        match try_rebase(H, &wt, "main").await.unwrap() {
            Rebase::Conflict(files) => assert_eq!(files, vec!["a.txt".to_string()]),
            other => panic!("ожидал конфликт, а вышло {other:?}"),
        }
        assert_eq!(head_sha(H, &wt).await.unwrap(), sha_before);
        assert!(
            !dirty(H, &wt).await,
            "после отката не должно остаться конфликтных меток"
        );

        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&r);
    }

    #[tokio::test]
    async fn clean_rebase_then_merge() {
        let r = repo("clean").await;
        let wt = sibling(&r, "wt-clean");
        let _ = std::fs::remove_dir_all(&wt);
        add_worktree(H, &r, &wt, "team/docs", "main").await.unwrap();

        commit(&wt, "docs.txt", "доки\n", "README").await;
        commit(&r, "other.txt", "мимо\n", "не пересекается").await;

        assert!(!rebased(H, &r, "main", "team/docs").await);
        assert_eq!(try_rebase(H, &wt, "main").await.unwrap(), Rebase::Clean);
        assert!(
            rebased(H, &r, "main", "team/docs").await,
            "после ребейза рука на свежей базе"
        );

        ff_advance(H, &r, "main", "team/docs").await.unwrap();
        assert_eq!(ahead(H, &r, "main", "team/docs").await, 0);

        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&r);
    }

    #[tokio::test]
    async fn dirty_base_checkout_refuses_to_merge() {
        let r = repo("dirty").await;
        let wt = sibling(&r, "wt-dirty");
        let _ = std::fs::remove_dir_all(&wt);
        add_worktree(H, &r, &wt, "team/x", "main").await.unwrap();
        commit(&wt, "x.txt", "x\n", "правка").await;

        std::fs::write(std::path::Path::new(&r).join("a.txt"), "недописанное\n").unwrap();
        let err = ff_advance(H, &r, "main", "team/x").await.unwrap_err();
        assert!(err.contains("незакоммиченной"), "{err}");
        assert_eq!(
            std::fs::read_to_string(std::path::Path::new(&r).join("a.txt")).unwrap(),
            "недописанное\n"
        );

        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&r);
    }

    #[tokio::test]
    async fn base_branch_is_detected() {
        let r = repo("base").await;
        assert_eq!(base_branch(H, &r).await.unwrap(), "main");
        let empty = std::env::temp_dir()
            .join(format!("jarvis-nogit-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let _ = std::fs::remove_dir_all(&empty);
        std::fs::create_dir_all(&empty).unwrap();
        assert!(!is_repo(H, &empty).await);
        let _ = std::fs::remove_dir_all(&empty);
        let _ = std::fs::remove_dir_all(&r);
    }
}
