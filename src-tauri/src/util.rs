//! Мелкие утилиты, общие для всех модулей.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Каталог данных Jarvis: $JARVIS_DIR или ~/.jarvis.
/// Переопределение через env даёт изоляцию dev-сборки от продовой
/// (`npm start` запускается с JARVIS_DIR=~/.jarvis-dev).
pub fn jarvis_dir() -> std::path::PathBuf {
    match std::env::var("JARVIS_DIR") {
        Ok(d) if !d.is_empty() => std::path::PathBuf::from(d),
        _ => home_dir().join(".jarvis"),
    }
}

/// Каталог Claude Code: ~/.claude
pub fn claude_dir() -> std::path::PathBuf {
    home_dir().join(".claude")
}

/// Каталог Codex: $CODEX_HOME или ~/.codex.
pub fn codex_dir() -> std::path::PathBuf {
    match std::env::var("CODEX_HOME") {
        Ok(d) if !d.is_empty() => std::path::PathBuf::from(d),
        _ => home_dir().join(".codex"),
    }
}

pub fn home_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

/// Раскрыть `~` в пути, который набрал человек. Шелла на пути к `create_dir_all`
/// нет, и без этого `~/projects/app` превращается в каталог с именем `~` рядом с
/// рабочим — то есть проект заводится не там, где его просили.
///
/// Раскрывает ТОЛЬКО от домашнего каталога ЭТОЙ машины: путь на узле считает
/// сам узел, он один знает свой `$HOME`.
pub fn expand_home(path: &str) -> String {
    let path = path.trim();
    let Some(rest) = path.strip_prefix('~') else {
        return path.to_string();
    };
    // `~user` — не наш случай: чужой домашний каталог мы не ищем и подменять
    // его своим не имеем права.
    if rest.starts_with(|c: char| c.is_alphanumeric()) {
        return path.to_string();
    }
    let home = home_dir();
    let home = home.to_string_lossy();
    let home = home.trim_end_matches('/');
    let rest = rest.trim_start_matches('/');
    if rest.is_empty() {
        home.to_string()
    } else {
        format!("{home}/{rest}")
    }
}

/// Путь к unix-сокету демона (JARVIS_SOCK переопределяет — нужно тестам).
pub fn sock_path() -> std::path::PathBuf {
    std::env::var("JARVIS_SOCK")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| jarvis_dir().join("run.sock"))
}

/// Date.now() — миллисекунды эпохи, как в JS-версии (и в state.json на диске).
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Схлопнуть пробелы в один, обрезать края — аналог oneLine().
pub fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Обернуть строку в одинарные кавычки для POSIX-шелла (экранируя `'`).
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Обрезка по СИМВОЛАМ (JS slice работает по кодпоинтам; байтовый срез
/// русского текста ломал бы UTF-8 на границе).
pub fn ellipsize(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect()
    }
}

/// ~/ вместо домашнего каталога — для логов.
pub fn short_home(p: &str) -> String {
    let home = home_dir();
    p.replacen(&home.to_string_lossy().to_string(), "~", 1)
}

pub fn basename(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string())
}

/// Человекочитаемое имя модели из id (claude-opus-4-8 → Opus).
pub fn friendly_model(id: &str) -> String {
    let v = id.to_lowercase();
    for (needle, name) in [
        ("opus", "Opus"),
        ("sonnet", "Sonnet"),
        ("haiku", "Haiku"),
        ("fable", "Fable"),
        ("mythos", "Mythos"),
    ] {
        if v.contains(needle) {
            return name.to_string();
        }
    }
    id.split('-').next().unwrap_or("").to_string()
}

/// «47м» / «3ч 12м» до момента ts (мс эпохи) — подписи сброса лимита.
pub fn fmt_reset_in(ts: i64) -> String {
    let min = ((ts - now_ms()) as f64 / 60_000.0).round().max(0.0) as i64;
    if min < 60 {
        format!("{min}м")
    } else {
        format!("{}ч {}м", min / 60, min % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ellipsize_respects_char_boundaries() {
        assert_eq!(ellipsize("привет мир", 6), "привет");
        assert_eq!(ellipsize("abc", 10), "abc");
    }

    #[test]
    fn one_line_collapses_whitespace() {
        assert_eq!(one_line("  a\n\tb   c "), "a b c");
    }

    #[test]
    fn expand_home_handles_tilde_but_not_other_users() {
        let home = home_dir();
        let home = home.to_string_lossy().trim_end_matches('/').to_string();
        assert_eq!(expand_home("~/projects/app"), format!("{home}/projects/app"));
        assert_eq!(expand_home("~"), home);
        assert_eq!(expand_home("/srv/x"), "/srv/x");
        assert_eq!(expand_home("  /srv/x  "), "/srv/x");
        // чужой домашний каталог не наше дело — оставляем как есть
        assert_eq!(expand_home("~bob/x"), "~bob/x");
    }

    #[test]
    fn shell_quote_escapes_spaces_and_single_quotes() {
        assert_eq!(shell_quote("/a b/c"), "'/a b/c'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn friendly_model_known_and_unknown() {
        assert_eq!(friendly_model("claude-opus-4-8"), "Opus");
        assert_eq!(friendly_model("claude-fable-5"), "Fable");
        assert_eq!(friendly_model("gpt-x"), "gpt");
    }
}
