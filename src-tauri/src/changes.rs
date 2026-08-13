//! Изменения задачи: что агент наделал в рабочем каталоге — и что с этим
//! делать.
//!
//! До сих пор панель показывала дифф ОДНОГО файла и только того, что всплыл в
//! ходах сессии. Для «посмотреть, что наработал агент» этого мало: правки
//! лежат в рабочем дереве целиком, и человеку нужен их свод — список файлов со
//! счётчиками, дифф по клику и два решения: принять (закоммитить) или откатить.
//!
//! Работает одинаково здесь и на узле: git запускается через `Host` — ту же
//! абстракцию, на которой стоит «Связка». Свои правки на сервере человек
//! ревьюит так же, как местные.
//!
//! Границы намеренные:
//!
//! * новый (неотслеживаемый) файл откатить нельзя — `git checkout` про него не
//!   знает, а удалять файлы молча панель не станет. Так и говорим;
//! * пути для коммита и отката сверяются со списком изменений, а не берутся на
//!   веру: путь приходит из webview, и `git checkout -- <что угодно>` в чужом
//!   репозитории — не то, что должно быть достижимо подделкой запроса.

use crate::bundle::host::Host;
use serde::Serialize;
use serde_json::{json, Value};

/// Один изменённый файл.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Change {
    pub path: String,
    /// Человеческое состояние: «изменён», «новый», «удалён», «переименован».
    pub state: String,
    pub added: u32,
    pub removed: u32,
    /// Файл не под контролем git: откатывать нечем.
    pub untracked: bool,
}

/// Разбор `git status --porcelain`.
///
/// Читаем только то, что нужно списку: путь и состояние. Переименование git
/// отдаёт как «старое -> новое» — показываем новое, потому что смотреть и
/// откатывать человек будет его.
pub fn parse_status(out: &str) -> Vec<Change> {
    let mut list = Vec::new();
    for line in out.lines() {
        if line.len() < 4 {
            continue;
        }
        let (x, y) = (line.as_bytes()[0] as char, line.as_bytes()[1] as char);
        let rest = line[3..].trim();
        if rest.is_empty() {
            continue;
        }
        let path = match rest.split_once(" -> ") {
            Some((_, new)) => new.trim(),
            None => rest,
        }
        .trim_matches('"')
        .to_string();
        let untracked = x == '?' && y == '?';
        let state = if untracked {
            "новый"
        } else if x == 'A' {
            "новый"
        } else if x == 'D' || y == 'D' {
            "удалён"
        } else if x == 'R' {
            "переименован"
        } else {
            "изменён"
        };
        list.push(Change {
            path,
            state: state.to_string(),
            added: 0,
            removed: 0,
            untracked,
        });
    }
    list
}

/// Досыпать счётчики строк из `git diff --numstat`.
///
/// Двоичный файл git помечает прочерками — счётчиков у него нет, и выдумывать
/// их незачем: в списке он останется просто «изменён».
pub fn apply_numstat(list: &mut [Change], numstat: &str) {
    for line in numstat.lines() {
        let mut parts = line.split('\t');
        let (Some(a), Some(r), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let path = match path.split_once(" => ") {
            // «dir/{old => new}.rs» встречается при обнаружении переименований;
            // нам нужен путь, под которым файл лежит сейчас.
            Some(_) => path.replace(['{', '}'], "").replace(" => ", ""),
            None => path.to_string(),
        };
        let path = path.trim_matches('"');
        if let Some(c) = list.iter_mut().find(|c| c.path == path) {
            c.added = a.parse().unwrap_or(0);
            c.removed = r.parse().unwrap_or(0);
        }
    }
}

/// Свод изменений рабочего каталога.
pub async fn collect(host: &Host, cwd: &str) -> Result<Value, String> {
    let (code, out) = host
        .git(
            cwd,
            &[
                // Кириллица в путях иначе приезжает в escape-последовательностях
                // и не совпадает ни с одним настоящим файлом.
                "-c",
                "core.quotePath=false",
                "status",
                "--porcelain",
                "--untracked-files=all",
            ],
        )
        .await;
    if code != 0 {
        return Err(if out.trim().is_empty() {
            "не репозиторий git".to_string()
        } else {
            crate::util::one_line(out.trim())
        });
    }
    let mut list = parse_status(&out);
    let (_, numstat) = host
        .git(cwd, &["-c", "core.quotePath=false", "diff", "--numstat", "HEAD"])
        .await;
    apply_numstat(&mut list, &numstat);

    let (_, branch) = host.git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).await;
    Ok(json!({
        "ok": true,
        "branch": branch.trim(),
        "files": list,
    }))
}

/// Дифф одного файла из свода.
///
/// Новый файл git показывает только сравнением с пустотой (`--no-index`):
/// `diff HEAD` про него не знает вовсе, и без этого «новый файл» открывался бы
/// пустым — ровно там, где смотреть интереснее всего.
pub async fn file_diff(host: &Host, cwd: &str, path: &str, untracked: bool) -> Result<Value, String> {
    Ok(json!({
        "ok": true,
        "mode": "worktree",
        "label": path,
        "hunks": file_hunks(host, cwd, path, untracked).await,
    }))
}

/// Ханки файла как данные — их же читает разбор «что тронуто».
pub async fn file_hunks(
    host: &Host,
    cwd: &str,
    path: &str,
    untracked: bool,
) -> Vec<crate::gitdiff::Hunk> {
    let args: Vec<&str> = if untracked {
        vec!["diff", "--no-index", "--", "/dev/null", path]
    } else {
        vec!["diff", "HEAD", "--", path]
    };
    let (_, out) = host.git(cwd, &args).await;
    // Код возврата не смотрим: `git diff` отдаёт 1 просто потому, что различия
    // есть — это не ошибка.
    crate::gitdiff::parse_unified(&out)
}

/// Принять правки: добавить в индекс и закоммитить.
///
/// Коммитим ровно перечисленные файлы, а не всё дерево: рядом может лежать
/// работа человека, и забирать её в коммит агента — чужое решение.
pub async fn commit(
    host: &Host,
    cwd: &str,
    message: &str,
    paths: &[String],
) -> Result<String, String> {
    let message = message.trim();
    if message.is_empty() {
        return Err("без сообщения коммита не обойтись".into());
    }
    if paths.is_empty() {
        return Err("нечего принимать: не выбран ни один файл".into());
    }
    let mut add: Vec<&str> = vec!["add", "--"];
    add.extend(paths.iter().map(String::as_str));
    let (code, out) = host.git(cwd, &add).await;
    if code != 0 {
        return Err(format!("git add: {}", crate::util::one_line(out.trim())));
    }
    let mut args: Vec<&str> = vec!["commit", "-m", message, "--"];
    args.extend(paths.iter().map(String::as_str));
    let (code, out) = host.git(cwd, &args).await;
    if code != 0 {
        // Самая частая причина — не настроен user.email: говорим как есть,
        // догадка «что-то пошло не так» стоила бы человеку получаса.
        return Err(format!("git commit: {}", crate::util::one_line(out.trim())));
    }
    let (_, sha) = host.git(cwd, &["rev-parse", "--short", "HEAD"]).await;
    Ok(sha.trim().to_string())
}

/// Откатить правку файла к состоянию последнего коммита.
pub async fn revert(host: &Host, cwd: &str, path: &str) -> Result<(), String> {
    let (code, out) = host.git(cwd, &["checkout", "HEAD", "--", path]).await;
    if code != 0 {
        return Err(format!("git checkout: {}", crate::util::one_line(out.trim())));
    }
    Ok(())
}

/* ---------- ревью изменений агентом ---------- */

/// Сколько диффа отдавать ревьюеру. Тот же потолок, что у критика циклов:
/// дальше начинается не ревью, а пересказ.
const DIFF_FOR_REVIEW: usize = 60_000;
const REVIEW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// Промт ревьюера.
///
/// Договор про первую строку — тот же, что у критика циклов, и по той же
/// причине: гадать по тексту нельзя. «Выглядит нормально, но тесты снял» не
/// должно проходить за одобрение только потому, что в нём есть «нормально».
pub fn review_prompt(diff: &str, untracked: &[String]) -> String {
    let new_files = if untracked.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nНовые файлы (их дифф git не показывает): {}",
            untracked.join(", ")
        )
    };
    format!(
        "Ты ревьюишь правки, которые агент сделал в рабочем каталоге. Смотри по \
         существу: решена ли задача или обойдена (снятые тесты, заглушки, \
         ослабленные проверки), нет ли поломок и забытого мусора.\n\n\
         Ответь РОВНО в таком виде. Первая строка — вердикт одним словом:\n\
         OK — правки можно принимать\n\
         RETURN — есть чем заняться, перечисли чем\n\
         ASK — решение спорное, нужен человек\n\
         Со второй строки — по делу и коротко.{new_files}\n\nДифф:\n{diff}"
    )
}

/// Позвать агента посмотреть на правки. Возвращает (вердикт, текст).
///
/// Ревьюер работает ТАМ, где лежат правки: у задачи на узле дифф и репозиторий
/// живут на нём, и звать агента у себя значило бы ревьюить пустоту.
pub async fn review(
    host: &Host,
    cwd: &str,
    model: Option<&str>,
) -> Result<(String, String), String> {
    let (_, diff) = host.git(cwd, &["diff", "HEAD"]).await;
    let listed = collect(host, cwd).await?;
    let untracked: Vec<String> = listed
        .get("files")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|f| f.get("untracked").and_then(Value::as_bool).unwrap_or(false))
                .filter_map(|f| f.get("path").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if diff.trim().is_empty() && untracked.is_empty() {
        return Err("нечего ревьюить: рабочее дерево чистое".into());
    }
    // Обрезаем по границе символа: байтовый срез рвёт UTF-8, а в диффе
    // кириллицы хватает.
    let cut = diff
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|i| *i <= DIFF_FOR_REVIEW)
        .last()
        .unwrap_or(0);
    let short = if diff.len() > DIFF_FOR_REVIEW {
        format!("{}\n… дифф обрезан", &diff[..cut])
    } else {
        diff.clone()
    };
    let prompt = review_prompt(&short, &untracked);

    let text = match host {
        Host::Local => {
            let out = crate::loops::runner::run_agent(
                "claude",
                std::path::Path::new(cwd),
                &prompt,
                model,
                REVIEW_TIMEOUT,
            )
            .await;
            if out.failed {
                return Err(out.text);
            }
            out.text
        }
        // На узле бинарь и авторизация свои — команду собираем строкой. Stderr
        // гасим прямо в ней: ssh склеивает потоки, и чужая строка прилипла бы
        // к json агента.
        Host::Ssh { .. } => {
            let mut cmd = format!(
                "claude -p {} --output-format json --dangerously-skip-permissions",
                crate::util::shell_quote(&prompt)
            );
            if let Some(m) = model {
                cmd.push_str(&format!(" --model {}", crate::util::shell_quote(m)));
            }
            cmd.push_str(" 2>/dev/null");
            let (code, out) = host.sh(cwd, &cmd, REVIEW_TIMEOUT).await;
            if code != 0 {
                return Err(format!("агент на узле не отработал: {}", crate::util::one_line(out.trim())));
            }
            crate::loops::runner::parse_agent_json(&out).text
        }
    };

    let verdict = match crate::loops::engine::parse_critic(&text) {
        crate::loops::engine::CriticSays::Fine => "ok",
        crate::loops::engine::CriticSays::Ask(_) => "ask",
        crate::loops::engine::CriticSays::Return(_) => "return",
    };
    let body = text
        .trim()
        .lines()
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    Ok((
        verdict.to_string(),
        if body.is_empty() { text.trim().to_string() } else { body },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /* Ниже — тесты на НАСТОЯЩЕМ git: временный репозиторий, правки, коммит,
     * откат. Разбор строк можно проверить и всухую, но обещание команды —
     * «правки агента видно и их можно принять» — держится не на разборе, а на
     * том, что git отвечает так, как мы думаем. CI гоняет их на macOS, то есть
     * там же, где живёт настольная версия. */
    const H: &Host = &Host::Local;

    async fn repo(tag: &str) -> String {
        let dir = std::env::temp_dir()
            .join(format!("jarvis-chg-{tag}-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(H.git(&dir, &["init", "-q", "-b", "main", "."]).await.0, 0);
        // Личность в репозитории, а не в глобальном конфиге: у машины CI его
        // может не быть вовсе, и коммит бы не прошёл.
        H.git(&dir, &["config", "user.email", "test@jarvis"]).await;
        H.git(&dir, &["config", "user.name", "jarvis"]).await;
        std::fs::write(std::path::Path::new(&dir).join("main.rs"), "fn main() {}\n").unwrap();
        assert_eq!(H.git(&dir, &["add", "."]).await.0, 0);
        assert_eq!(H.git(&dir, &["commit", "-q", "-m", "первый"]).await.0, 0);
        dir
    }

    #[tokio::test]
    async fn a_real_repo_shows_edits_and_new_files() {
        let dir = repo("collect").await;
        std::fs::write(
            std::path::Path::new(&dir).join("main.rs"),
            "fn main() {\n    println!(\"привет\");\n}\n",
        )
        .unwrap();
        std::fs::write(std::path::Path::new(&dir).join("новый файл.txt"), "раз\nдва\n").unwrap();

        let v = collect(H, &dir).await.expect("свод изменений");
        let files = v["files"].as_array().unwrap();
        assert_eq!(files.len(), 2, "{files:?}");
        let edited = files.iter().find(|f| f["path"] == "main.rs").unwrap();
        assert_eq!(edited["state"], "изменён");
        assert!(edited["added"].as_u64().unwrap() >= 2, "счётчик правок: {edited}");
        // Путь с пробелом и кириллицей обязан дойти целым — ради этого
        // core.quotePath=false, и проверяется это только живым git.
        let fresh = files.iter().find(|f| f["path"] == "новый файл.txt").unwrap();
        assert_eq!(fresh["state"], "новый");
        assert_eq!(fresh["untracked"], true);
        assert_eq!(v["branch"], "main");

        // Дифф нового файла: git показывает его только сравнением с пустотой.
        let d = file_diff(H, &dir, "новый файл.txt", true).await.unwrap();
        assert!(!d["hunks"].as_array().unwrap().is_empty(), "дифф нового файла пуст");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn commit_takes_only_the_chosen_file() {
        let dir = repo("commit").await;
        std::fs::write(std::path::Path::new(&dir).join("main.rs"), "fn main() { }\n").unwrap();
        std::fs::write(std::path::Path::new(&dir).join("side.txt"), "чужая работа\n").unwrap();

        let sha = commit(H, &dir, "правка агента", &["main.rs".to_string()])
            .await
            .expect("коммит выбранного");
        assert!(!sha.is_empty(), "короткий sha не вернулся");

        // Рядом лежала работа человека — она обязана остаться неприкосновенной.
        let left = collect(H, &dir).await.unwrap();
        let files = left["files"].as_array().unwrap();
        assert_eq!(files.len(), 1, "в коммит уехало лишнее: {files:?}");
        assert_eq!(files[0]["path"], "side.txt");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn revert_returns_the_file_to_the_last_commit() {
        let dir = repo("revert").await;
        let file = std::path::Path::new(&dir).join("main.rs");
        std::fs::write(&file, "всё сломал\n").unwrap();
        assert_eq!(collect(H, &dir).await.unwrap()["files"].as_array().unwrap().len(), 1);

        revert(H, &dir, "main.rs").await.expect("откат");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "fn main() {}\n");
        assert!(
            collect(H, &dir).await.unwrap()["files"].as_array().unwrap().is_empty(),
            "после отката дерево обязано быть чистым"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Пустое сообщение и пустой выбор — отказы, а не «тихий коммит ни о чём».
    #[tokio::test]
    async fn commit_refuses_the_meaningless() {
        let dir = repo("refuse").await;
        std::fs::write(std::path::Path::new(&dir).join("main.rs"), "x\n").unwrap();
        assert!(commit(H, &dir, "  ", &["main.rs".to_string()]).await.is_err());
        assert!(commit(H, &dir, "есть сообщение", &[]).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Не репозиторий — понятный отказ, а не пустой список «изменений нет».
    #[tokio::test]
    async fn a_plain_directory_says_it_is_not_a_repo() {
        let dir = std::env::temp_dir()
            .join(format!("jarvis-chg-plain-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let e = collect(H, &dir).await.unwrap_err();
        assert!(!e.is_empty(), "отказ обязан объяснить причину");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_reads_states_and_paths() {
        let out = " M src/main.rs\n?? ui/new.js\n D docs/old.md\nR  a.rs -> b.rs\nA  added.rs\n";
        let list = parse_status(out);
        assert_eq!(list.len(), 5);
        assert_eq!(list[0].path, "src/main.rs");
        assert_eq!(list[0].state, "изменён");
        assert!(!list[0].untracked);
        assert_eq!(list[1].state, "новый");
        assert!(list[1].untracked, "неотслеживаемый — откатывать нечем");
        assert_eq!(list[2].state, "удалён");
        assert_eq!(list[3].path, "b.rs", "переименование показываем по новому имени");
        assert_eq!(list[4].state, "новый");
    }

    /// Путь с пробелами и кириллицей обязан дойти целым: именно на нём
    /// ломаются наивные разборы по пробелу.
    #[test]
    fn status_keeps_spaces_and_cyrillic() {
        let list = parse_status(" M src/мой файл.rs\n");
        assert_eq!(list[0].path, "src/мой файл.rs");
    }

    #[test]
    fn numstat_fills_counters_and_skips_binaries() {
        let mut list = parse_status(" M src/main.rs\n M logo.png\n");
        apply_numstat(&mut list, "12\t3\tsrc/main.rs\n-\t-\tlogo.png\n");
        assert_eq!((list[0].added, list[0].removed), (12, 3));
        assert_eq!(
            (list[1].added, list[1].removed),
            (0, 0),
            "у двоичного счётчиков нет, и выдумывать их незачем"
        );
    }

    /// Промт обязан назвать новые файлы: их дифф git не показывает, и без
    /// упоминания ревьюер не узнает о целом файле.
    #[test]
    fn review_prompt_mentions_new_files_and_the_verdict_contract() {
        let p = review_prompt("@@ -1 +1 @@\n-a\n+b", &["ui/new.js".to_string()]);
        assert!(p.contains("ui/new.js"), "новый файл не назван");
        assert!(p.contains("OK") && p.contains("RETURN") && p.contains("ASK"));
        assert!(p.contains("Первая строка"), "договор о вердикте не напечатан");
        let bare = review_prompt("diff", &[]);
        assert!(!bare.contains("Новые файлы"), "лишней строки быть не должно");
    }

    /// Пустой ответ git — это «изменений нет», а не поломка.
    #[test]
    fn empty_status_is_an_empty_list() {
        assert!(parse_status("").is_empty());
        assert!(parse_status("\n").is_empty());
    }
}
