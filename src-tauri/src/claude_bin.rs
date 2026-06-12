//! Запуск настоящего бинаря `claude` для служебных нужд демона
//! (haiku-переводы, саммари, официальный /usage, effort-уровни).
//!
//! Все вызовы идут с JARVIS_IGNORE=1 — шим-хук видит переменную и не шлёт
//! события, иначе служебные запуски засоряли бы реестр сессий.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use crate::util::jarvis_dir;

/// Настоящий claude в PATH (плюс типовые каталоги), минуя наш tmux-шим.
pub fn resolve_claude_bin() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    for extra in [
        crate::util::home_dir().join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ] {
        if !dirs.contains(&extra) {
            dirs.push(extra);
        }
    }
    let shims = jarvis_dir().join("shims");
    for d in dirs {
        if d == shims {
            continue; // настоящий бинарь, не наш шим
        }
        let p = d.join("claude");
        if let Ok(meta) = std::fs::metadata(&p) {
            if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                return Some(p);
            }
        }
    }
    None
}

/// `claude <args>` с таймаутом; stdout при нулевом коде выхода.
/// Ошибка/таймаут → None: без сети и квоты демон живёт на локальных данных.
pub async fn run_claude(args: &[&str], timeout: Duration) -> Option<String> {
    let bin = resolve_claude_bin()?;
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args)
        .current_dir(std::env::temp_dir())
        .env("JARVIS_IGNORE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child = cmd.output();
    let out = tokio::time::timeout(timeout, child).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Headless-вызов haiku одним промптом — общий путь переводов и саммари.
pub async fn run_haiku(prompt: &str, timeout: Duration) -> Option<String> {
    run_claude(
        &["-p", "--no-session-persistence", "--model", "haiku", prompt],
        timeout,
    )
    .await
}
