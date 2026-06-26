//! Codex agent-host: `codex exec --json` как ограниченный агент с инструментами
//! ТОЛЬКО `mcp__jarvis__*`. У Codex `thread.started` НЕ несёт список инструментов
//! (как Claude `init.tools[]`), поэтому INV-TOOLS воспроизвести на init нельзя —
//! заменяем на (а) чистый throwaway `CODEX_HOME` (без чужих MCP/скиллов),
//! (б) `-s read-only`, (в) **обязательный per-item kill**: любой встроенный
//! инструмент (shell/exec) или чужой MCP-вызов → процесс немедленно убивается.

use serde_json::Value;
use std::path::PathBuf;

use crate::agent::AgentEvent;

/// Итог разбора одной строки `codex exec --json`.
#[derive(Debug, PartialEq)]
pub enum CodexLine {
    /// Нормальные события (0+).
    Events(Vec<AgentEvent>),
    /// Нарушение изоляции — поток нужно прервать, процесс убить.
    Kill(String),
}

/// Встроенные инструменты Codex (выполнение команд/патчей) — для agent-host
/// запрещены: единственный разрешённый инструмент — наш MCP.
fn is_builtin_tool(item_type: &str) -> bool {
    matches!(
        item_type,
        "command_execution" | "local_shell" | "exec" | "shell" | "apply_patch" | "custom_tool_call"
    )
}

/// Разобрать одну newline-JSON строку потока `codex exec --json`.
/// Маппинг: thread.started→Init, item.completed{agent_message}→Delta,
/// mcp_tool_call(jarvis)→ToolUse, turn.completed→Done. Любой встроенный
/// инструмент или чужой MCP → Kill. Битый JSON → пустые события (не паникуем).
pub fn classify_codex_line(line: &str) -> CodexLine {
    let line = line.trim();
    if line.is_empty() {
        return CodexLine::Events(vec![]);
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return CodexLine::Events(vec![]);
    };
    let typ = v.get("type").and_then(Value::as_str).unwrap_or("");

    match typ {
        "thread.started" => {
            let id = v.get("thread_id").and_then(Value::as_str).unwrap_or("").to_string();
            CodexLine::Events(vec![AgentEvent::Init { tools: vec![], model: String::new(), session_id: id }])
        }
        "item.started" | "item.completed" => {
            let Some(item) = v.get("item") else { return CodexLine::Events(vec![]) };
            let it = item.get("type").and_then(Value::as_str).unwrap_or("");
            if is_builtin_tool(it) {
                return CodexLine::Kill(format!(
                    "codex использовал встроенный инструмент '{it}' — agent-host убит (разрешён только mcp__jarvis__*)"
                ));
            }
            if it == "mcp_tool_call" {
                let server = item.get("server").and_then(Value::as_str).unwrap_or("");
                if server != "jarvis" {
                    return CodexLine::Kill(format!(
                        "codex вызвал чужой MCP-сервер '{server}' — agent-host убит"
                    ));
                }
                let name = item.get("tool").and_then(Value::as_str).unwrap_or("").to_string();
                let input = item.get("arguments").cloned().unwrap_or(Value::Object(Default::default()));
                return CodexLine::Events(vec![AgentEvent::ToolUse { name, input }]);
            }
            // текст ассистента — только на completed (чтобы не дублировать started)
            if it == "agent_message" && typ == "item.completed" {
                let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                if !text.is_empty() {
                    return CodexLine::Events(vec![AgentEvent::Delta { text: text.to_string() }]);
                }
            }
            CodexLine::Events(vec![]) // reasoning, error, ...
        }
        "turn.completed" => CodexLine::Events(vec![AgentEvent::Done { result: String::new(), session_id: String::new() }]),
        _ => CodexLine::Events(vec![]),
    }
}

/// Чистый throwaway `CODEX_HOME` для gated agent-host: только auth (симлинк на
/// живой OAuth) + минимальный config, БЕЗ skills/ и чужих MCP. Это и есть
/// превентивная замена INV-TOOLS (там, где per-item kill — defense-in-depth).
fn ensure_codex_agent_home() -> std::io::Result<PathBuf> {
    let home = crate::util::jarvis_dir().join("codex-agent-home");
    std::fs::create_dir_all(&home)?;
    let auth_link = home.join("auth.json");
    let real_auth = crate::util::home_dir().join(".codex/auth.json");
    if real_auth.exists() && !auth_link.exists() {
        let _ = std::os::unix::fs::symlink(&real_auth, &auth_link); // живой токен, не копия
    }
    let cfg = home.join("config.toml");
    if !cfg.exists() {
        // MCP инжектим через -c (токен не пишем в файл); skills/ намеренно нет.
        let _ = std::fs::write(&cfg, "model = \"gpt-5.5\"\napproval_policy = \"never\"\n");
    }
    Ok(home)
}

/// Хост Codex-агента: `codex exec --json` в изолированном CODEX_HOME, инструменты
/// только `mcp__jarvis__*` (инжектим [mcp_servers.jarvis] через -c), sandbox
/// read-only, и обязательный per-item kill из [`classify_codex_line`].
pub struct CodexCliHost {
    pub app: tauri::AppHandle,
    /// Путь к бинарю jarvis-mcp.
    pub mcp_bin: String,
    /// Агент-токен (предъявляется демону мостом).
    pub token: String,
}

impl CodexCliHost {
    pub async fn run(&self, message: &str, _tools: &[String], resume: Option<&str>) {
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::process::Command;

        let Some(bin) = crate::backend::codex::resolve_codex_bin() else {
            crate::log::line("[codex-agent] codex не найден");
            return;
        };
        let Ok(home) = ensure_codex_agent_home() else {
            crate::log::line("[codex-agent] не смог подготовить CODEX_HOME");
            return;
        };

        // codex exec [resume <id>] --json -s read-only -c mcp... "<msg>"
        let mut args: Vec<String> = vec!["exec".into()];
        if let Some(id) = resume {
            args.push("resume".into());
            args.push(id.to_string());
        }
        args.extend([
            "--json".into(),
            "-s".into(),
            "read-only".into(),
            "-c".into(),
            format!("mcp_servers.jarvis.command=\"{}\"", self.mcp_bin),
            "-c".into(),
            format!("mcp_servers.jarvis.env.JARVIS_TOKEN=\"{}\"", self.token),
            message.to_string(),
        ]);

        let mut child = match Command::new(&bin)
            .args(&args)
            .current_dir(std::env::temp_dir())
            .env("CODEX_HOME", &home)
            .env("JARVIS_IGNORE", "1")
            .env("JARVIS_SOCK", crate::util::sock_path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                crate::log::line(&format!("[codex-agent] spawn: {e}"));
                return;
            }
        };
        let Some(stdout) = child.stdout.take() else { return };
        let mut reader = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            match classify_codex_line(&line) {
                CodexLine::Kill(msg) => {
                    crate::log::line(&format!("[codex-agent] {msg}"));
                    let _ = child.kill().await;
                    return;
                }
                CodexLine::Events(evs) => {
                    for ev in evs {
                        emit_event(&self.app, &ev);
                    }
                }
            }
        }
    }
}

fn emit_event(app: &tauri::AppHandle, ev: &AgentEvent) {
    use tauri::Emitter;
    if matches!(ev, AgentEvent::Other) {
        return;
    }
    if let Err(e) = app.emit("agent:event", ev) {
        crate::log::line(&format!("[codex-agent] emit error: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_started_is_init() {
        let out = classify_codex_line(r#"{"type":"thread.started","thread_id":"019f-abc"}"#);
        match out {
            CodexLine::Events(ev) => assert!(matches!(&ev[0], AgentEvent::Init { session_id, .. } if session_id == "019f-abc")),
            _ => panic!("ожидали Init"),
        }
    }

    #[test]
    fn agent_message_completed_is_delta() {
        let out = classify_codex_line(r#"{"type":"item.completed","item":{"type":"agent_message","text":"привет"}}"#);
        assert_eq!(out, CodexLine::Events(vec![AgentEvent::Delta { text: "привет".into() }]));
        // started НЕ даёт дельту (без дублей)
        let started = classify_codex_line(r#"{"type":"item.started","item":{"type":"agent_message","text":"привет"}}"#);
        assert_eq!(started, CodexLine::Events(vec![]));
    }

    #[test]
    fn builtin_shell_triggers_kill() {
        for it in ["command_execution", "local_shell", "exec", "shell", "apply_patch"] {
            let line = format!(r#"{{"type":"item.started","item":{{"type":"{it}","id":"i1"}}}}"#);
            match classify_codex_line(&line) {
                CodexLine::Kill(msg) => assert!(msg.contains(it), "kill называет инструмент: {msg}"),
                _ => panic!("встроенный инструмент '{it}' должен убивать"),
            }
        }
    }

    #[test]
    fn jarvis_mcp_allowed_foreign_killed() {
        let ok = classify_codex_line(r#"{"type":"item.completed","item":{"type":"mcp_tool_call","server":"jarvis","tool":"sessions_reply","arguments":{"x":1}}}"#);
        assert!(matches!(ok, CodexLine::Events(ev) if matches!(&ev[0], AgentEvent::ToolUse { name, .. } if name == "sessions_reply")));
        let bad = classify_codex_line(r#"{"type":"item.completed","item":{"type":"mcp_tool_call","server":"posthog","tool":"query"}}"#);
        assert!(matches!(bad, CodexLine::Kill(msg) if msg.contains("posthog")), "чужой MCP → kill");
    }

    #[test]
    fn turn_completed_is_done_and_garbage_safe() {
        assert_eq!(
            classify_codex_line(r#"{"type":"turn.completed","usage":{}}"#),
            CodexLine::Events(vec![AgentEvent::Done { result: String::new(), session_id: String::new() }])
        );
        assert_eq!(classify_codex_line("not json"), CodexLine::Events(vec![]));
        assert_eq!(classify_codex_line(""), CodexLine::Events(vec![]));
    }
}
