//! Git-слой связки: worktree на руку, ребейз, очередь в main.
//!
//! Всё аргументами, без прохода через шелл: в путях и сообщениях бывают
//! пробелы и кавычки, и один непроцитированный путь стоил бы ветки. Каждая
//! функция — одна git-операция с честным ответом; политика (когда ребейзить,
//! кого вливать) живёт этажом выше.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// `git -C dir <args>` → (код, вывод). stderr сливается в stdout: диагностика
/// git почти вся там, а звать нас будут ради неё.
pub async fn git(dir: &Path, args: &[&str]) -> (i32, String) {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0") // спросить некого: никаких промтов
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(120), cmd.output()).await else {
        return (-1, "git не уложился в две минуты".into());
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(err.trim_end());
    }
    (out.status.code().unwrap_or(-1), text)
}

async fn ok(dir: &Path, args: &[&str]) -> Result<String, String> {
    let (code, out) = git(dir, args).await;
    if code == 0 {
        Ok(out)
    } else {
        Err(crate::util::ellipsize(&crate::util::one_line(&out), 300))
    }
}

pub async fn is_repo(dir: &Path) -> bool {
    git(dir, &["rev-parse", "--git-dir"]).await.0 == 0
}

/// Базовая ветка репозитория: main, master — что есть.
pub async fn base_branch(repo: &Path) -> Result<String, String> {
    for cand in ["main", "master"] {
        if git(repo, &["show-ref", "--verify", &format!("refs/heads/{cand}")]).await.0 == 0 {
            return Ok(cand.into());
        }
    }
    Err("не нашёл ни main, ни master — укажи базовую ветку".into())
}

/// Поднять worktree руки на своей ветке от базовой.
pub async fn add_worktree(repo: &Path, dir: &Path, branch: &str, base: &str) -> Result<(), String> {
    let d = dir.to_string_lossy();
    ok(repo, &["worktree", "add", "-b", branch, &d, base]).await.map(|_| ())
}

pub async fn head_sha(dir: &Path) -> Result<String, String> {
    ok(dir, &["rev-parse", "HEAD"]).await.map(|s| s.trim().to_string())
}

/// В дереве есть незакоммиченное (включая неучтённые файлы).
pub async fn dirty(dir: &Path) -> bool {
    match ok(dir, &["status", "--porcelain", "--untracked-files=all"]).await {
        Ok(out) => !out.trim().is_empty(),
        Err(_) => true, // не смогли спросить — считаем грязным: осторожность дешевле
    }
}

/// Насколько ветка впереди базы.
pub async fn ahead(repo: &Path, base: &str, branch: &str) -> u32 {
    ok(repo, &["rev-list", "--count", &format!("{base}..{branch}")])
        .await
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Ветка уже стоит на актуальной базе (база — предок ветки).
pub async fn rebased(repo: &Path, base: &str, branch: &str) -> bool {
    git(repo, &["merge-base", "--is-ancestor", base, branch]).await.0 == 0
}

/// Файлы, которых рука коснулась относительно базы.
pub async fn changed_files(repo: &Path, base: &str, branch: &str) -> Vec<String> {
    ok(repo, &["diff", "--name-only", &format!("{base}...{branch}")])
        .await
        .map(|out| out.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).take(200).collect())
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
/// Конфликт НЕ решается здесь: мы откатываем ребейз и отдаём список файлов
/// наверх, а наверху конфликт отдают агенту руки — он знает контекст своих
/// правок, мы нет. «Чинит сам» из дизайна — это ровно этот путь.
pub async fn try_rebase(worktree: &Path, base: &str) -> Result<Rebase, String> {
    let (code, out) = git(worktree, &["rebase", base]).await;
    if code == 0 {
        return Ok(Rebase::Clean);
    }
    let files = ok(worktree, &["diff", "--name-only", "--diff-filter=U"])
        .await
        .map(|o| o.lines().map(str::to_string).filter(|l| !l.is_empty()).collect::<Vec<_>>())
        .unwrap_or_default();
    let (abort_code, abort_out) = git(worktree, &["rebase", "--abort"]).await;
    if abort_code != 0 {
        // Полуразобранный ребейз хуже конфликта: дерево руки осталось в
        // промежуточном состоянии, и дальше его трогать нельзя.
        return Err(format!("ребейз не откатился: {abort_out}"));
    }
    if files.is_empty() {
        // Упал не на конфликте — например, грязное дерево. Причина в выводе.
        return Err(crate::util::ellipsize(&crate::util::one_line(&out), 300));
    }
    Ok(Rebase::Conflict(files))
}

/// Где база сейчас выписана: (путь worktree, чистое ли дерево).
async fn base_checkout(repo: &Path, base: &str) -> Option<(PathBuf, bool)> {
    let out = ok(repo, &["worktree", "list", "--porcelain"]).await.ok()?;
    let mut path: Option<PathBuf> = None;
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p.trim()));
        } else if let Some(b) = line.strip_prefix("branch ") {
            if b.trim() == format!("refs/heads/{base}") {
                let p = path.clone()?;
                let clean = !dirty(&p).await;
                return Some((p, clean));
            }
        }
    }
    None
}

/// Влить готовую руку: докрутить базу до её головы (fast-forward).
///
/// Рука к этому моменту перебазирована, так что вливание — это ровно сдвиг
/// ссылки вперёд, без merge-коммита. Осторожность одна, но принципиальная:
/// база может быть выписана в рабочем дереве человека. Чистое дерево двигаем
/// честным `merge --ff-only` (ссылка и файлы едут вместе); грязное не трогаем
/// вовсе — «закоммить или спрячь» лучше, чем молча испортить правку.
pub async fn ff_advance(repo: &Path, base: &str, branch: &str) -> Result<(), String> {
    if !rebased(repo, base, branch).await {
        return Err("ветка не на актуальной базе — сначала ребейз".into());
    }
    match base_checkout(repo, base).await {
        Some((path, true)) => {
            ok(&path, &["merge", "--ff-only", branch]).await.map(|_| ())
        }
        Some((path, false)) => Err(format!(
            "{base} выписана в {} с незакоммиченной правкой — закоммить или спрячь, тогда волью",
            path.display()
        )),
        None => {
            let sha = head_sha_of(repo, branch).await?;
            ok(repo, &["update-ref", &format!("refs/heads/{base}"), &sha]).await.map(|_| ())
        }
    }
}

async fn head_sha_of(repo: &Path, branch: &str) -> Result<String, String> {
    ok(repo, &["rev-parse", branch]).await.map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Настоящий репозиторий во временном каталоге — никакой имитации git.
    async fn repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jarvis-bundle-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(git(&dir, &["init", "-q", "-b", "main"]).await.0, 0);
        // Личность — локально в репозиторий: у CI глобальной нет.
        git(&dir, &["config", "user.email", "t@t"]).await;
        git(&dir, &["config", "user.name", "t"]).await;
        commit(&dir, "a.txt", "один\n", "начало").await;
        dir
    }

    async fn commit(dir: &Path, file: &str, text: &str, msg: &str) {
        std::fs::write(dir.join(file), text).unwrap();
        assert_eq!(git(dir, &["add", "."]).await.0, 0);
        assert_eq!(git(dir, &["commit", "-q", "-m", msg]).await.0, 0, "{msg}");
    }

    #[tokio::test]
    async fn worktree_branch_and_merge_round_trip() {
        let r = repo("round").await;
        let wt = r.parent().unwrap().join(format!("wt-round-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&wt);

        add_worktree(&r, &wt, "team/auth", "main").await.unwrap();
        assert!(is_repo(&wt).await);
        commit(&wt, "auth.txt", "логин\n", "экран логина").await;

        assert_eq!(ahead(&r, "main", "team/auth").await, 1);
        assert!(rebased(&r, "main", "team/auth").await);
        assert_eq!(changed_files(&r, "main", "team/auth").await, vec!["auth.txt".to_string()]);

        // База никем не выписана в грязном виде (главное дерево чистое) — вливаем.
        ff_advance(&r, "main", "team/auth").await.unwrap();
        assert_eq!(ahead(&r, "main", "team/auth").await, 0, "после вливания рука не впереди");

        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&r);
    }

    #[tokio::test]
    async fn conflict_is_reported_and_rolled_back() {
        let r = repo("conflict").await;
        let wt = r.parent().unwrap().join(format!("wt-conf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&wt);
        add_worktree(&r, &wt, "team/billing", "main").await.unwrap();

        // Обе стороны правят один файл по-разному.
        commit(&wt, "a.txt", "платёжка\n", "поле в типах").await;
        commit(&r, "a.txt", "auth\n", "переименование в типах").await;

        assert!(!rebased(&r, "main", "team/billing").await, "main уехал вперёд");
        let sha_before = head_sha(&wt).await.unwrap();
        match try_rebase(&wt, "main").await.unwrap() {
            Rebase::Conflict(files) => assert_eq!(files, vec!["a.txt".to_string()]),
            other => panic!("ожидал конфликт, а вышло {other:?}"),
        }
        // Дерево цело: ребейз откатан, голова та же, ничего не повисло.
        assert_eq!(head_sha(&wt).await.unwrap(), sha_before);
        assert!(!dirty(&wt).await, "после отката не должно остаться конфликтных меток");

        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&r);
    }

    #[tokio::test]
    async fn clean_rebase_then_merge() {
        let r = repo("clean").await;
        let wt = r.parent().unwrap().join(format!("wt-clean-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&wt);
        add_worktree(&r, &wt, "team/docs", "main").await.unwrap();

        commit(&wt, "docs.txt", "доки\n", "README").await;
        commit(&r, "other.txt", "мимо\n", "не пересекается").await;

        assert!(!rebased(&r, "main", "team/docs").await);
        assert_eq!(try_rebase(&wt, "main").await.unwrap(), Rebase::Clean);
        assert!(rebased(&r, "main", "team/docs").await, "после ребейза рука на свежей базе");

        ff_advance(&r, "main", "team/docs").await.unwrap();
        assert_eq!(ahead(&r, "main", "team/docs").await, 0);

        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&r);
    }

    #[tokio::test]
    async fn dirty_base_checkout_refuses_to_merge() {
        let r = repo("dirty").await;
        let wt = r.parent().unwrap().join(format!("wt-dirty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&wt);
        add_worktree(&r, &wt, "team/x", "main").await.unwrap();
        commit(&wt, "x.txt", "x\n", "правка").await;

        // main выписан в главном дереве и там незакоммиченная правка.
        std::fs::write(r.join("a.txt"), "недописанное\n").unwrap();
        let err = ff_advance(&r, "main", "team/x").await.unwrap_err();
        assert!(err.contains("незакоммиченной"), "{err}");
        // Правка человека цела.
        assert_eq!(std::fs::read_to_string(r.join("a.txt")).unwrap(), "недописанное\n");

        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&r);
    }

    #[tokio::test]
    async fn base_branch_is_detected() {
        let r = repo("base").await;
        assert_eq!(base_branch(&r).await.unwrap(), "main");
        let empty = std::env::temp_dir().join(format!("jarvis-nogit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&empty);
        std::fs::create_dir_all(&empty).unwrap();
        assert!(!is_repo(&empty).await);
        let _ = std::fs::remove_dir_all(&empty);
        let _ = std::fs::remove_dir_all(&r);
    }
}
