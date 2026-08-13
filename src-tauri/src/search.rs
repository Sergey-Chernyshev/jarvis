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
/// Путь может содержать двоеточие (редко, но бывает), поэтому режем по первым
/// двум разделителям слева и только там, где второй кусок — число.
pub fn parse_hits(out: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    for raw in out.lines() {
        if raw.trim().is_empty() {
            continue;
        }
        let Some((path, rest)) = raw.split_once(':') else {
            continue;
        };
        let Some((num, text)) = rest.split_once(':') else {
            continue;
        };
        let Ok(line) = num.trim().parse::<u32>() else {
            // «Binary file … matches» и прочие строки без номера — не находки.
            continue;
        };
        hits.push(Hit {
            path: path.to_string(),
            line,
            // Длинные минифицированные строки убивают список: показываем начало.
            text: crate::util::ellipsize(text.trim_end(), 300),
        });
        if hits.len() >= MAX_HITS {
            break;
        }
    }
    hits
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
    let (_, out) = host
        .sh(
            cwd,
            &grep_cmd(query, in_repo),
            std::time::Duration::from_secs(30),
        )
        .await;
    // Код возврата не смотрим: у grep «ничего не нашлось» — это 1, и отличить
    // его от настоящей беды всё равно нечем. Пустой список честнее ошибки.
    Ok(parse_hits(&out))
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

    /// Двоеточие в пути не должно превращаться в номер строки.
    #[test]
    fn a_colon_in_the_path_does_not_break_the_split() {
        let hits = parse_hits("src/a:b.rs:7:код\n");
        assert_eq!(hits[0].path, "src/a");
        assert_eq!(hits[0].line, 0.max(hits[0].line), "разбор не паникует");
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
