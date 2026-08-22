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

/// Отвечает ли на порту живой сайдкар (`GET <path>` → любой 2xx).
///
/// Синхронно и без клиента: зовётся из супервизоров, которые крутятся вне
/// tokio, и тащить туда рантайм ради одной пробы незачем.
///
/// Нужна она вот зачем. Сайдкар, осиротевший от прошлого запуска приложения,
/// продолжает слушать свой порт. Новый супервизор видел только «мой процесс
/// мёртв», поднимал ещё один, тот падал на `address already in use`, и так по
/// кругу — на этой машине двое суток, каждые пять секунд, с записью «сайдкар
/// запущен» в журнал. Спросить порт дешевле, чем плодить обречённые процессы,
/// а ответивший сайдкар — рабочий: демон и так ходит к нему по порту.
pub fn port_serves(port: u16, path: &str, timeout: std::time::Duration) -> bool {
    use std::io::{Read, Write};
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(timeout));
    let _ = s.set_write_timeout(Some(timeout));
    let req = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    // Хватит первой строки ответа: нам нужен код, а не тело.
    let mut buf = [0u8; 64];
    let n = s.read(&mut buf).unwrap_or(0);
    String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 2")
        || String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.0 2")
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

    /// Проба порта. Отвечает — берём чужой сайдкар; молчит или отвечает не тем —
    /// поднимаем свой. Ошибка в любую сторону дорогая: ложное «отвечает» оставит
    /// голос без сайдкара навсегда, ложное «не отвечает» вернёт тот самый цикл
    /// обречённых запусков, ради которого проба и заведена.
    #[test]
    fn port_probe_tells_a_live_sidecar_from_silence_and_from_a_stranger() {
        use std::io::{Read, Write};
        let quick = std::time::Duration::from_millis(400);

        // Свободный порт: слушателя нет — connect не удастся.
        let free = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let free_port = free.local_addr().unwrap().port();
        drop(free);
        assert!(!port_serves(free_port, "/health", quick), "на пустом порту померещился сайдкар");

        // Отвечает как сайдкар.
        let ok = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let ok_port = ok.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut c, _)) = ok.accept() {
                let mut b = [0u8; 128];
                let _ = c.read(&mut b);
                let _ = c.write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok");
            }
        });
        assert!(port_serves(ok_port, "/health", quick), "живой сайдкар не опознан");

        // Порт занят кем-то другим: соединение есть, ответ не наш. Поднимать свой
        // всё равно бесполезно (порт занят), но и молча считать это сайдкаром
        // нельзя — иначе голос будет «работать» через чужую программу.
        let alien = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let alien_port = alien.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut c, _)) = alien.accept() {
                let mut b = [0u8; 128];
                let _ = c.read(&mut b);
                let _ = c.write_all(b"HTTP/1.0 404 Not Found\r\n\r\n");
            }
        });
        assert!(!port_serves(alien_port, "/health", quick), "чужая программа сошла за сайдкар");
    }

    #[test]
    fn one_line_collapses_whitespace() {
        assert_eq!(one_line("  a\n\tb   c "), "a b c");
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
