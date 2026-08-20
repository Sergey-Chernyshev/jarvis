//! Поиск по проекту задачи.
//!
//! Ревью упирается в «а где это ещё» быстрее, чем в «что тут изменилось»:
//! увидел правку — хочешь посмотреть, кто ещё зовёт эту функцию. До сих пор за
//! этим приходилось уходить в редактор или в терминал.
//!
//! Ищет `git grep` — он знает про .gitignore и не полезет в `target` и
//! `node_modules`, где ответ утонет. Если каталог не репозиторий, спускаемся к
//! обычному `grep -r`: искать всё равно надо.
//!
//! Как и всё в задаче, поиск идёт ТАМ, где живёт сессия: у задачи на узле
//! файлы на нём, и грепать у себя значило бы искать в другом дереве.

use crate::bundle::host::Host;
use serde::Serialize;

/// Одно совпадение.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hit {
    pub path: String,
    pub line: u32,
    pub text: String,
}

/// Потолок выдачи. Больше двухсот строк никто не читает — это уже не поиск, а
/// повод сузить запрос, и честнее сказать об этом прямо.
pub const MAX_HITS: usize = 200;

/// Разбор вывода `grep -n`: `путь:строка:текст`.
///
/// Двоеточие бывает и в пути, и в самом коде, поэтому режем не по первому
/// разделителю, а по первому, ЗА КОТОРЫМ идёт число со следующим двоеточием:
/// «src/a:b.rs:7:код» — это файл «src/a:b.rs», а не файл «src/a». Наивный
/// разбор молча терял такое совпадение, и человек не узнал бы, что оно было.
pub fn parse_hits(out: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    for raw in out.lines() {
        if raw.trim().is_empty() {
            continue;
        }
        let Some(hit) = parse_line(raw) else { continue };
        hits.push(hit);
        if hits.len() >= MAX_HITS {
            break;
        }
    }
    hits
}

fn parse_line(raw: &str) -> Option<Hit> {
    let mut from = 0usize;
    while let Some(rel) = raw[from..].find(':') {
        let at = from + rel;
        let rest = &raw[at + 1..];
        if let Some((num, text)) = rest.split_once(':') {
            if let Ok(line) = num.trim().parse::<u32>() {
                return Some(Hit {
                    path: raw[..at].to_string(),
                    line,
                    // Отступ в списке съедает ширину, а строка и так вырвана
                    // из контекста: показываем её содержимое, не колонку.
                    text: crate::util::ellipsize(text.trim(), 300),
                });
            }
        }
        from = at + 1;
    }
    // «Binary file … matches» и прочее без номера строки — не находки.
    None
}

/// Команда поиска. Отдельной функцией, потому что уезжает на чужую машину
/// строкой: там нет ни нашего окружения, ни нашей кавычки.
pub fn grep_cmd(query: &str, in_repo: bool) -> String {
    let q = crate::util::shell_quote(query);
    if in_repo {
        // -I пропускает двоичные, --untracked ищет и в новых файлах агента:
        // именно их чаще всего и разглядывают.
        format!("git grep -n -I --untracked -e {q} | head -n {MAX_HITS}")
    } else {
        format!("grep -rnI -e {q} . | head -n {MAX_HITS}")
    }
}

/// Найти в проекте задачи.
pub async fn search(host: &Host, cwd: &str, query: &str) -> Result<Vec<Hit>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("что искать?".into());
    }
    let in_repo = crate::bundle::git::is_repo(host, cwd).await;
    // stdout — находки, stderr — чужое ворчание (удалённый шелл про локаль,
    // grep про недоступные каталоги). Код возврата не смотрим: у grep «ничего
    // не нашлось» — это 1, и пустой список тут честнее ошибки.
    //
    // А вот отказ САМОГО вызова — отвалившийся ssh, таймаут в 30 с — это не
    // «ничего не найдено»: показать пустоту значит соврать, что искали. Разница
    // видна прямо здесь: у «нет совпадений» диагностики нет, у транспортного
    // отказа она есть.
    match host
        .sh_data(cwd, &grep_cmd(query, in_repo), std::time::Duration::from_secs(30))
        .await
    {
        Ok(out) => Ok(parse_hits(&out)),
        Err(why) => Err(search_error(&why)),
    }
}

/// Причина отказа поиска человеческим текстом. Таймаут называем таймаутом и
/// говорим, что делать: «ничего не найдено» на нём — самый вредный ответ,
/// человек начнёт искать ошибку в запросе.
pub fn search_error(why: &str) -> String {
    let why = crate::util::one_line(why);
    let why = why.trim();
    if why.contains("не уложилось") {
        return "Поиск не уложился в 30 с — сузь запрос или ищи в подкаталоге".into();
    }
    if why.is_empty() {
        return "Поиск не выполнился — проверь связь с машиной задачи".into();
    }
    format!("Поиск не выполнился: {}", crate::util::ellipsize(why, 200))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hits_are_read_as_path_line_text() {
        let out = "src/main.rs:12:    let x = 1;\nui/app.js:3:const a = 2;\n";
        let hits = parse_hits(out);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path, "src/main.rs");
        assert_eq!(hits[0].line, 12);
        assert_eq!(hits[0].text, "let x = 1;");
    }

    /// «Binary file … matches» и прочие строки без номера — не находки: в
    /// списке они выглядели бы как файл со строкой ноль.
    #[test]
    fn lines_without_a_number_are_not_hits() {
        assert!(parse_hits("Binary file src/logo.png matches\n").is_empty());
        assert!(parse_hits("мусор без двоеточий\n").is_empty());
    }

    /// Двоеточие в пути не должно съедать совпадение: «src/a:b.rs:7:код» —
    /// это файл «src/a:b.rs», а не «src/a». Наивный разбор терял такую строку
    /// молча, и человек не узнал бы, что находка была.
    #[test]
    fn a_colon_in_the_path_keeps_the_hit() {
        let hits = parse_hits("src/a:b.rs:7:код\n");
        assert_eq!(hits.len(), 1, "совпадение потерялось");
        assert_eq!(hits[0].path, "src/a:b.rs");
        assert_eq!(hits[0].line, 7);
        assert_eq!(hits[0].text, "код");
    }

    /// Двоеточие в самом коде тоже не должно ломать разбор.
    #[test]
    fn a_colon_in_the_code_stays_in_the_text() {
        let hits = parse_hits("src/main.rs:3:let m: Map = x;\n");
        assert_eq!(hits[0].path, "src/main.rs");
        assert_eq!(hits[0].line, 3);
        assert_eq!(hits[0].text, "let m: Map = x;");
    }

    #[test]
    fn the_output_is_capped() {
        let many = (1..=(MAX_HITS + 50))
            .map(|i| format!("f.rs:{i}:строка"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(parse_hits(&many).len(), MAX_HITS);
    }

    /// Запрос уезжает на чужую машину строкой — кавычки обязаны его удержать.
    #[test]
    fn the_query_is_quoted_whole() {
        let cmd = grep_cmd("rm -rf / ; echo", true);
        assert!(cmd.contains("'rm -rf / ; echo'"), "{cmd}");
        assert!(cmd.starts_with("git grep"));
        assert!(grep_cmd("x", false).starts_with("grep -rnI"));
    }

    /// Таймаут и отвалившийся ssh обязаны отличаться от «ничего не найдено»:
    /// пустой список на них — ложь про то, что искали.
    #[test]
    fn a_timeout_is_not_an_empty_result() {
        let t = search_error("не уложилось в 30 с");
        assert!(t.contains("30 с"), "{t}");
        assert!(t.contains("сузь"), "подсказан следующий шаг: {t}");
        let ssh = search_error("ssh: connect to host vps port 22: Operation timed out");
        assert!(ssh.starts_with("Поиск не выполнился"), "{ssh}");
        assert!(ssh.contains("ssh"), "причина не потеряна: {ssh}");
        assert!(!search_error("").is_empty(), "пустая диагностика — всё равно ошибка");
    }

    /// …а настоящее «ничего не нашлось» (grep вернул 1, stdout пуст, но код
    /// пайплайна нулевой) остаётся пустым списком, а не ошибкой.
    #[tokio::test]
    async fn no_matches_is_an_empty_list_not_an_error() {
        let dir = std::env::temp_dir()
            .join(format!("jarvis-search-none-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(std::path::Path::new(&dir).join("a.txt"), "ничего такого\n").unwrap();
        let hits = search(&Host::Local, &dir, "такогонетнигде")
            .await
            .expect("«не нашлось» — это не отказ поиска");
        assert!(hits.is_empty(), "{hits:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /* Живой поиск в НАСТОЯЩЕМ репозитории: обещание «ищет там, где надо, и не
     * тонет в мусоре» держится на git grep, а не на разборе строк. CI гоняет
     * это на macOS. */
    #[tokio::test]
    async fn a_real_repo_search_finds_code_and_skips_ignored() {
        let dir = std::env::temp_dir()
            .join(format!("jarvis-search-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let h = &Host::Local;
        assert_eq!(h.git(&dir, &["init", "-q", "-b", "main", "."]).await.0, 0);
        h.git(&dir, &["config", "user.email", "t@j"]).await;
        h.git(&dir, &["config", "user.name", "j"]).await;
        let p = std::path::Path::new(&dir);
        std::fs::write(p.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(p.join("main.rs"), "fn искомое() {}\n").unwrap();
        std::fs::create_dir_all(p.join("target")).unwrap();
        std::fs::write(p.join("target/junk.rs"), "fn искомое() {}\n").unwrap();
        h.git(&dir, &["add", "."]).await;
        h.git(&dir, &["commit", "-q", "-m", "первый"]).await;
        // Новый файл агента: его ещё нет в индексе, но искать в нём надо.
        std::fs::write(p.join("fresh.rs"), "// искомое рядом\n").unwrap();

        let hits = search(h, &dir, "искомое").await.unwrap();
        let paths: Vec<&str> = hits.iter().map(|x| x.path.as_str()).collect();
        assert!(paths.contains(&"main.rs"), "{paths:?}");
        assert!(paths.contains(&"fresh.rs"), "новый файл не найден: {paths:?}");
        assert!(
            !paths.iter().any(|x| x.starts_with("target/")),
            "в выдачу попал игнорируемый мусор: {paths:?}"
        );
        assert!(hits.iter().all(|x| x.line > 0));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
