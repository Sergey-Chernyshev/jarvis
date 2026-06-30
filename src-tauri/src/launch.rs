//! Запуск новой/возобновляемой сессии прямо из Jarvis: открыть терминал из
//! настроек, по желанию выполнить прокси-команду, затем `claude`/`codex` в
//! директории проекта. Заменяет прежнее «скопировать команду» вкладки «Проекты».
//!
//! macOS-only в текущей итерации. `custom`-терминал (sh -lc по шаблону) — точка
//! расширения под Ghostty/Warp/kitty сейчас и под Windows/Linux в будущем.

use std::process::Stdio;

/// Команда агента: новая сессия или `--resume`/`resume`, с dangerous-флагами при
/// включённом «опасном режиме». Флаги сверены с `resumeCommand` в renderer.js:
/// claude → `--dangerously-skip-permissions`, codex → `--dangerously-bypass-approvals-and-sandbox`.
pub fn agent_command(agent: &str, session_id: Option<&str>, dangerous: bool) -> String {
    if agent == "codex" {
        let flag = if dangerous { " --dangerously-bypass-approvals-and-sandbox" } else { "" };
        match session_id {
            Some(id) => format!("codex resume {id}{flag}"),
            None => format!("codex{flag}"),
        }
    } else {
        let flag = if dangerous { " --dangerously-skip-permissions" } else { "" };
        match session_id {
            Some(id) => format!("claude --resume {id}{flag}"),
            None => format!("claude{flag}"),
        }
    }
}

/// Обернуть строку в одинарные кавычки для POSIX-шелла (экранируя `'`).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Полная команда для терминала: `[<proxy> && ] cd '<cwd>' && <agent_cmd>`.
pub fn inner_command(cwd: &str, proxy_cmd: &str, agent_cmd: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let proxy = proxy_cmd.trim();
    if !proxy.is_empty() {
        parts.push(proxy.to_string());
    }
    parts.push(format!("cd {}", shell_quote(cwd)));
    parts.push(agent_cmd.to_string());
    parts.join(" && ")
}

/// Экранирование под двойные кавычки AppleScript-строки: `\` и `"`.
/// Одинарные кавычки (из shell_quote) внутри неё безопасны.
fn applescript_escape(s: &str) -> String {
    s.replace('\\', r"\\").replace('"', "\\\"")
}

async fn osascript(args: &[String]) -> Result<(), String> {
    let out = tokio::process::Command::new("osascript")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("не удалось запустить osascript: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&out.stderr);
        Err(if msg.trim().is_empty() { "терминал не открылся".into() } else { msg.trim().to_string() })
    }
}

/// Открыть терминал из настроек и выполнить в нём `inner`.
pub async fn spawn(terminal: &str, custom_cmd: &str, inner: &str) -> Result<(), String> {
    match terminal {
        "iterm2" => {
            let esc = applescript_escape(inner);
            // Создаём окно с дефолт-профилем и пишем команду в его сессию.
            osascript(&[
                "-e".into(), "tell application \"iTerm2\"".into(),
                "-e".into(), "set w to (create window with default profile)".into(),
                "-e".into(), format!("tell current session of w to write text \"{esc}\""),
                "-e".into(), "activate".into(),
                "-e".into(), "end tell".into(),
            ])
            .await
        }
        "custom" => {
            let tmpl = custom_cmd.trim();
            if tmpl.is_empty() {
                return Err("шаблон команды терминала пуст (настройки → Запуск)".into());
            }
            if !tmpl.contains("{cmd}") {
                return Err("в шаблоне нет плейсхолдера {cmd}".into());
            }
            let expanded = tmpl.replace("{cmd}", &shell_quote(inner));
            let status = tokio::process::Command::new("sh")
                .arg("-lc")
                .arg(&expanded)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("не удалось запустить терминал: {e}"))?;
            // Не ждём завершения: терминал живёт своей жизнью.
            drop(status);
            Ok(())
        }
        // 'terminal-app' и любое неизвестное значение → системный Terminal.app.
        _ => {
            let esc = applescript_escape(inner);
            osascript(&[
                "-e".into(), "tell application \"Terminal\"".into(),
                "-e".into(), format!("do script \"{esc}\""),
                "-e".into(), "activate".into(),
                "-e".into(), "end tell".into(),
            ])
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_command_variants() {
        assert_eq!(agent_command("claude", None, false), "claude");
        assert_eq!(agent_command("claude", None, true), "claude --dangerously-skip-permissions");
        assert_eq!(agent_command("claude", Some("abc"), true), "claude --resume abc --dangerously-skip-permissions");
        assert_eq!(agent_command("codex", None, false), "codex");
        assert_eq!(agent_command("codex", None, true), "codex --dangerously-bypass-approvals-and-sandbox");
        assert_eq!(agent_command("codex", Some("x1"), false), "codex resume x1");
    }

    #[test]
    fn inner_command_with_and_without_proxy() {
        assert_eq!(inner_command("/tmp/p", "", "claude"), "cd '/tmp/p' && claude");
        assert_eq!(
            inner_command("/tmp/p", "export X=1", "claude"),
            "export X=1 && cd '/tmp/p' && claude"
        );
    }

    #[test]
    fn shell_quote_escapes_single_quote() {
        assert_eq!(shell_quote("/a b/c"), "'/a b/c'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn applescript_escape_quotes_and_backslash() {
        assert_eq!(applescript_escape(r#"a"b\c"#), r#"a\"b\\c"#);
    }
}
