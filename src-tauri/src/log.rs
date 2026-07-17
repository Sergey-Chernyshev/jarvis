//! Файловый лог демона: ~/.jarvis/jarvis.log.
//!
//! Нужен, чтобы постфактум разбирать поведение без подключения к stdout:
//! поток событий хуков, статусы доставки/уведомлений, тайминги пайплайна.
//! НЕ пишем конфиденциальное: текст промптов/ответов агента, тело уведомлений,
//! содержимое транскриптов — только метки событий, типы и усечённые id сессий.
//! Best-effort — ошибки записи глотаем, демон от лога не зависит.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use crate::util::jarvis_dir;
use regex::Regex;

const MAX_BYTES: u64 = 4 * 1024 * 1024; // при разрастании — ротация в .old
static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

fn log_path() -> std::path::PathBuf {
    jarvis_dir().join("jarvis.log")
}

/// Локальная метка времени ЧЧ:ММ:СС.мс (chrono уже в зависимостях).
fn stamp() -> String {
    chrono::Local::now().format("%H:%M:%S%.3f").to_string()
}

fn proxy_userinfo_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b((?:https?|socks(?:4|5h?))://)[^/\s:@]+:[^@\s/]+@")
            .expect("valid proxy credential regex")
    })
}

fn bearer_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(Bearer\s+)[A-Za-z0-9._~+/=-]+").expect("valid bearer credential regex")
    })
}

fn named_secret_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(([A-Z0-9_]*(?:API[_-]?KEY|TOKEN|PASSWORD|SECRET))\s*[:=]\s*)(?:"[^"]*"|'[^']*'|[^\s,;]+)"#,
        )
        .expect("valid named secret regex")
    })
}

fn standalone_secret_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?:sk-ant-[A-Za-z0-9_-]{8,}|sk-proj-[A-Za-z0-9_-]{8,}|glpat-[A-Za-z0-9_-]{8,}|gh[pousr]_[A-Za-z0-9_]{8,})\b",
        )
        .expect("valid standalone secret regex")
    })
}

/// Последняя страховочная сетка перед stdout/файлом: скрыть распространённые
/// формы прокси-учёток и токенов, которые могли попасть в полезный текст ошибки.
fn sanitize(msg: &str) -> String {
    let safe = proxy_userinfo_re()
        .replace_all(msg, "${1}[REDACTED]@")
        .into_owned();
    let safe = bearer_re()
        .replace_all(&safe, "${1}[REDACTED]")
        .into_owned();
    let safe = named_secret_re()
        .replace_all(&safe, "${1}[REDACTED]")
        .into_owned();
    standalone_secret_re()
        .replace_all(&safe, "[REDACTED]")
        .into_owned()
}

fn open_secure_append(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

/// Дописать строку в лог (и продублировать в stdout — его ловит nohup).
pub fn line(msg: &str) {
    if cfg!(test) || !ENABLED.load(Ordering::Relaxed) {
        return; // юнит-тесты не должны писать в боевой ~/.jarvis/jarvis.log
    }
    let msg = sanitize(msg);
    println!("{msg}"); // stdout → daemon.log при запуске под nohup
    let path = log_path();
    let _ = std::fs::create_dir_all(jarvis_dir());
    if std::fs::metadata(&path)
        .map(|m| m.len() > MAX_BYTES)
        .unwrap_or(false)
    {
        let old_path = path.with_extension("log.old");
        let _ = std::fs::rename(&path, &old_path);
        let _ = std::fs::set_permissions(&old_path, std::fs::Permissions::from_mode(0o600));
    }
    if let Ok(mut f) = open_secure_append(&path) {
        let _ = writeln!(f, "{} {}", stamp(), msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_redacts_proxy_userinfo() {
        let message = "connect HTTP_PROXY=http://alice:very-secret@proxy.example:8080/path";

        let safe = sanitize(message);

        assert_eq!(
            safe,
            "connect HTTP_PROXY=http://[REDACTED]@proxy.example:8080/path"
        );
        assert!(!safe.contains("alice"));
        assert!(!safe.contains("very-secret"));
    }

    #[test]
    fn sanitize_redacts_tokens_and_bearer_credentials() {
        let message = "ANTHROPIC_API_KEY=sk-ant-api-secret Authorization: Bearer abc.def-123";

        let safe = sanitize(message);

        assert!(!safe.contains("sk-ant-api-secret"));
        assert!(!safe.contains("abc.def-123"));
        assert!(safe.contains("ANTHROPIC_API_KEY=[REDACTED]"));
        assert!(safe.contains("Bearer [REDACTED]"));
    }

    #[test]
    fn secure_append_restricts_an_existing_file_to_owner_only() {
        let path = std::env::temp_dir().join(format!(
            "jarvis-log-permissions-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::write(&path, b"legacy\n").expect("create fixture");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("set insecure fixture mode");

        let file = open_secure_append(&path).expect("open log");
        drop(file);

        let mode = std::fs::metadata(&path)
            .expect("read fixture metadata")
            .permissions()
            .mode()
            & 0o777;
        let _ = std::fs::remove_file(&path);
        assert_eq!(mode, 0o600);
    }
}
