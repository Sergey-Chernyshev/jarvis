//! Codex agent-host: `codex exec --json` как ограниченный агент с инструментами
//! ТОЛЬКО `mcp__jarvis__*`. У Codex `thread.started` НЕ несёт список инструментов
//! (как Claude `init.tools[]`), поэтому INV-TOOLS воспроизвести на init нельзя —
//! заменяем на (а) чистый throwaway `CODEX_HOME` (без чужих MCP/скиллов),
//! (б) `-s read-only`, (в) **обязательный per-item kill**: любой встроенный
//! инструмент (shell/exec) или чужой MCP-вызов → процесс немедленно убивается.

use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::agent::{AgentEvent, StreamLifecycle};

/// Итог разбора одной строки `codex exec --json`.
#[derive(Debug, PartialEq)]
pub enum CodexLine {
    /// Нормальные события (0+).
    Events(Vec<AgentEvent>),
    /// Нарушение изоляции — поток нужно прервать, процесс убить.
    Kill(String),
}

/// Разобрать одну newline-JSON строку потока `codex exec --json`.
/// Маппинг: thread.started→Init, item.completed{agent_message}→Delta,
/// mcp_tool_call(jarvis)→ToolUse, turn.completed→Done. **Allowlist (fail-closed):**
/// разрешены только item.type ∈ {agent_message, reasoning, todo_list, mcp_tool_call}
/// — ВСЁ остальное (command_execution, file_change, web_search, новые типы) → Kill.
/// Это и есть Codex-замена INV-TOOLS (init без tools[], поэтому по item). Битый
/// JSON → пустые события (не паникуем).
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
        "item.started" | "item.updated" | "item.completed" => {
            let Some(item) = v.get("item") else { return CodexLine::Events(vec![]) };
            let it = item.get("type").and_then(Value::as_str).unwrap_or("");
            match it {
                // текст ассистента — только на completed (чтобы не дублировать started)
                "agent_message" => {
                    if typ == "item.completed" {
                        let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                        if !text.is_empty() {
                            return CodexLine::Events(vec![AgentEvent::Delta { text: text.to_string() }]);
                        }
                    }
                    CodexLine::Events(vec![])
                }
                // безвредные не-инструментальные item'ы
                "reasoning" | "todo_list" => CodexLine::Events(vec![]),
                // наш MCP — разрешён; чужой — kill. ToolUse только на completed (без дублей)
                "mcp_tool_call" => {
                    let server = item.get("server").and_then(Value::as_str).unwrap_or("");
                    if server != "jarvis" {
                        return CodexLine::Kill(format!(
                            "codex вызвал чужой MCP-сервер '{server}' — agent-host убит"
                        ));
                    }
                    if typ == "item.completed" {
                        let name = item.get("tool").and_then(Value::as_str).unwrap_or("").to_string();
                        let input = item.get("arguments").cloned().unwrap_or(Value::Object(Default::default()));
                        return CodexLine::Events(vec![AgentEvent::ToolUse { name, input }]);
                    }
                    CodexLine::Events(vec![])
                }
                // ВСЁ остальное — встроенный инструмент (command_execution, file_change,
                // web_search, …) или неизвестный тип → kill (fail-closed allowlist).
                other => CodexLine::Kill(format!(
                    "codex использовал встроенный инструмент '{other}' — agent-host убит (разрешён только mcp__jarvis__*)"
                )),
            }
        }
        "turn.completed" => CodexLine::Events(vec![AgentEvent::Done { result: String::new(), session_id: String::new() }]),
        "turn.failed" => CodexLine::Events(vec![AgentEvent::Failed {
            message: v.pointer("/error/message").and_then(Value::as_str)
                .or_else(|| v.get("message").and_then(Value::as_str))
                .unwrap_or("Codex завершил ход с ошибкой").to_string(),
            session_id: String::new(),
        }]),
        _ => CodexLine::Events(vec![]),
    }
}

/// Чистый throwaway `CODEX_HOME` для gated agent-host: только auth (симлинк на
/// живой OAuth) + минимальный config, БЕЗ skills/ и чужих MCP. Это и есть
/// превентивная замена INV-TOOLS (там, где per-item kill — defense-in-depth).
fn ensure_codex_agent_home() -> std::io::Result<PathBuf> {
    let registry = crate::agent_instances::load_registry(&crate::util::jarvis_dir())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let instance = registry.resolve(None)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let home = crate::util::jarvis_dir().join("codex-agent-homes").join(&instance.id);
    prepare_codex_agent_home(&home, &instance.canonical_home.join("auth.json"))?;
    Ok(home)
}

fn prepare_codex_agent_home(home: &Path, real_auth: &Path) -> std::io::Result<()> {
    if !real_auth.is_file() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "выбранный инстанс не содержит файловую авторизацию Codex"));
    }
    std::fs::create_dir_all(home)?;
    let auth_link = home.join("auth.json");
    // Only replace our generated symlink; never rewrite the user's auth file.
    // A custom CODEX_HOME is the same account source used by CLI/history.
    match std::fs::symlink_metadata(&auth_link) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if std::fs::read_link(&auth_link)? != real_auth { std::fs::remove_file(&auth_link)?; }
        }
        Ok(_) => return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "сгенерированный auth.json не является ссылкой; файл сохранён")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(error),
    }
    if std::fs::symlink_metadata(&auth_link).is_err() {
        if let Err(error) = std::os::unix::fs::symlink(real_auth, &auth_link) {
            if error.kind() != std::io::ErrorKind::AlreadyExists || std::fs::read_link(&auth_link).ok().as_deref() != Some(real_auth) { return Err(error); }
        }
    }
    let cfg = home.join("config.toml");
    if !cfg.exists() {
        // MCP инжектим через -c (токен не пишем в файл); skills/ намеренно нет.
        std::fs::write(&cfg, "model = \"gpt-5.5\"\napproval_policy = \"never\"\n")?;
    }
    Ok(())
}

fn build_codex_args(mcp_bin: &str, token: &str, sock: &str, message: &str, resume: Option<&str>) -> Vec<String> {
    let mut args = vec!["exec".to_string()];
    if let Some(id) = resume {
        args.extend(["resume".to_string(), id.to_string()]);
    }
    args.extend([
        "--json".to_string(),
        // This host intentionally works in a temporary runtime directory.
        // This flag does not change the read-only sandbox or tool allowlist.
        "--skip-git-repo-check".to_string(),
        "-c".to_string(),
        "sandbox_mode=\"read-only\"".to_string(),
        "-c".to_string(),
        format!("mcp_servers.jarvis.command={}", serde_json::to_string(mcp_bin).unwrap()),
        "-c".to_string(),
        format!("mcp_servers.jarvis.env.JARVIS_TOKEN={}", serde_json::to_string(token).unwrap()),
        "-c".to_string(),
        format!("mcp_servers.jarvis.env.JARVIS_SOCK={}", serde_json::to_string(sock).unwrap()),
        // Only our gated bridge is pre-approved at the CLI layer, matching
        // Claude --allowedTools mcp__jarvis__*. The daemon's grant and
        // PanelConfirmer still authorize every actual capability invocation.
        "-c".to_string(),
        "mcp_servers.jarvis.default_tools_approval_mode=\"approve\"".to_string(),
        "--".to_string(),
        message.to_string(),
    ]);
    args
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

        let mut state = StreamLifecycle::new(resume);
        let Some(bin) = crate::backend::codex::resolve_codex_bin() else {
            if let Some(event) = state.fail("Codex CLI не найден") { emit_event(&self.app, &event); }
            return;
        };
        let Ok(home) = ensure_codex_agent_home() else {
            if let Some(event) = state.fail("Не удалось подготовить окружение Codex") { emit_event(&self.app, &event); }
            return;
        };

        let args = build_codex_args(&self.mcp_bin, &self.token,
            &crate::util::sock_path().to_string_lossy(), message, resume);

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
                if let Some(event) = state.fail(format!("Не удалось запустить Codex: {e}")) { emit_event(&self.app, &event); }
                return;
            }
        };
        let Some(stdout) = child.stdout.take() else {
            if let Some(event) = state.fail("Codex не открыл поток ответа") { emit_event(&self.app, &event); }
            return;
        };
        let mut reader = BufReader::new(stdout).lines();
        let mut last_error = None;
        loop {
            let line = match reader.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => { last_error = Some(format!("Поток Codex оборвался: {error}")); break; }
            };
            // error может сопровождать reconnect; только turn.failed/EOF
            // завершает ход. Успешный turn.completed отменяет эту причину.
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if value.get("type").and_then(Value::as_str) == Some("error") {
                    last_error = value.get("message").and_then(Value::as_str).map(String::from);
                }
            }
            match classify_codex_line(&line) {
                CodexLine::Kill(msg) => {
                    crate::log::line(&format!("[codex-agent] {msg}"));
                    let _ = child.kill().await;
                    if let Some(event) = state.fail(msg) { emit_event(&self.app, &event); }
                    return;
                }
                CodexLine::Events(evs) => {
                    for ev in evs {
                        if let Some(event) = state.accept(ev) { emit_event(&self.app, &event); }
                    }
                }
            }
        }
        crate::agent::finish_cli_stream(&self.app, &mut child, &mut state, "Codex", last_error).await;
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
    fn runtime_arguments_support_non_git_cwd_resume_and_literal_values() {
        let path = "/tmp/Jarvis \"qa\"/jarvis-mcp";
        for resume in [None, Some("qa-session")] {
            let args = build_codex_args(path, "qa-token", "/tmp/qa.sock", "--help", resume);
            assert!(args.iter().any(|arg| arg == "--skip-git-repo-check"));
            assert!(args.iter().any(|arg| arg == "sandbox_mode=\"read-only\""));
            assert_eq!(&args[args.len() - 2..], ["--", "--help"]);
            let command = args.iter().find_map(|arg| arg.strip_prefix("mcp_servers.jarvis.command=")).unwrap();
            assert_eq!(serde_json::from_str::<String>(command).unwrap(), path);
            assert!(args.iter().any(|arg| arg == "mcp_servers.jarvis.env.JARVIS_SOCK=\"/tmp/qa.sock\""));
            assert!(args.iter().any(|arg| arg == "mcp_servers.jarvis.default_tools_approval_mode=\"approve\""));
            if resume.is_some() { assert_eq!(&args[..3], ["exec", "resume", "qa-session"]); }
        }
    }

    #[test]
    fn custom_account_source_replaces_only_generated_auth_symlink() {
        let root = std::env::temp_dir().join(format!("jarvis-codex-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let first = root.join("first-auth.json");
        let second = root.join("custom-auth.json");
        std::fs::write(&first, "synthetic-first").unwrap();
        std::fs::write(&second, "synthetic-second").unwrap();
        let home = root.join("agent");
        prepare_codex_agent_home(&home, &first).unwrap();
        prepare_codex_agent_home(&home, &second).unwrap();
        assert_eq!(std::fs::read_link(home.join("auth.json")).unwrap(), second);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "synthetic-first");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "synthetic-second");
        let link = home.join("auth.json");
        assert!(prepare_codex_agent_home(&home, &root.join("missing.json")).is_err());
        std::fs::remove_file(&link).unwrap();
        std::fs::write(&link, "keep-foreign-file").unwrap();
        assert!(prepare_codex_agent_home(&home, &first).is_err());
        assert_eq!(std::fs::read_to_string(link).unwrap(), "keep-foreign-file");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Real provider + real jarvis-mcp transport, synthetic daemon capability.
    /// Explicit opt-in: consumes two small provider requests; auth is copied
    /// into a private temporary home so refresh never edits the source file.
    #[tokio::test]
    #[ignore = "requires JARVIS_QA_CODEX_BIN, JARVIS_QA_MCP_BIN and existing Codex authorization"]
    async fn real_codex_agent_runtime_and_resume_with_synthetic_capability() {
        use axum::{extract::State, routing::{get, post}, Json, Router};
        use std::os::unix::fs::PermissionsExt;
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
        use std::time::Duration;
        let bin = std::env::var("JARVIS_QA_CODEX_BIN").expect("explicit real CLI path required");
        let mcp = std::env::var("JARVIS_QA_MCP_BIN").expect("explicit real jarvis-mcp path required");
        let root = PathBuf::from("/tmp").join(format!("jarvis-agent-qa-{}-{}", std::process::id(), crate::util::now_ms()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
        }
        let _cleanup = Cleanup(root.clone());
        let auth = root.join("qa-auth.json");
        std::fs::copy(crate::util::codex_dir().join("auth.json"), &auth).expect("existing auth required");
        std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o600)).unwrap();
        let home = root.join("agent");
        prepare_codex_agent_home(&home, &auth).unwrap();
        let cwd = root.join("empty-runtime");
        std::fs::create_dir(&cwd).unwrap();
        let sock = root.join("fixture.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/capabilities", get(|| async { Json(serde_json::json!([
                {"name":"qa_echo", "description":"Return a fixed QA marker; no side effects.",
                 "inputSchema":{"type":"object","properties":{},"additionalProperties":false}}
            ])) }))
            .route("/capability", post(|State(calls): State<Arc<AtomicUsize>>, Json(body): Json<Value>| async move {
                assert_eq!(body["id"], "qa_echo");
                calls.fetch_add(1, Ordering::SeqCst);
                Json(serde_json::json!({"ok":true,"value":"JARVIS_CODEX_MCP_QA_OK","provenance":"trusted"}))
            }))
            .with_state(calls.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let mut resume = None;
        for (number, prompt) in [
            "Call the jarvis qa_echo tool exactly once, then reply with its exact marker. Do not use other tools or read files.",
            "Reply exactly JARVIS_CODEX_RESUME_QA_OK. Do not use tools or read files.",
        ].into_iter().enumerate() {
            let args = build_codex_args(&mcp, "qa-fixture-token", &sock.to_string_lossy(), prompt, resume.as_deref());
            let child = tokio::process::Command::new(&bin).args(args)
                .current_dir(&cwd).env("CODEX_HOME", &home).env("JARVIS_SOCK", &sock)
                .env("JARVIS_IGNORE", "1")
                .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null()).kill_on_drop(true).spawn().unwrap();
            let output = tokio::time::timeout(Duration::from_secs(90), child.wait_with_output())
                .await.expect("real provider exceeded 90 seconds").unwrap();
            assert!(output.status.success(), "real Codex CLI exited unsuccessfully");
            let mut lifecycle = StreamLifecycle::new(resume.as_deref());
            let mut events = Vec::new();
            for line in String::from_utf8(output.stdout).unwrap().lines() {
                match classify_codex_line(line) {
                    CodexLine::Events(parsed) => events.extend(parsed.into_iter().filter_map(|e| lifecycle.accept(e))),
                    CodexLine::Kill(message) => panic!("real provider left the tool allowlist: {message}"),
                }
            }
            let done: Vec<_> = events.iter().filter_map(|event| match event {
                AgentEvent::Done { session_id, .. } => Some(session_id), _ => None,
            }).collect();
            assert_eq!(done.len(), 1);
            assert!(!done[0].is_empty());
            if let Some(previous) = &resume { assert_eq!(previous, done[0]); }
            resume = Some(done[0].clone());
            let marker = if number == 0 { "JARVIS_CODEX_MCP_QA_OK" } else { "JARVIS_CODEX_RESUME_QA_OK" };
            assert!(events.iter().any(|e| matches!(e, AgentEvent::Delta { text } if text.contains(marker))),
                "expected synthetic marker missing; bridge calls={}, response={:?}", calls.load(Ordering::SeqCst),
                events.iter().filter_map(|e| match e { AgentEvent::Delta { text } => Some(text), _ => None }).collect::<Vec<_>>());
            eprintln!("real Codex QA turn {number}: one terminal Done, session preserved, expected marker received");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.abort();
    }

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
    fn allowlist_kills_builtins_and_unknown() {
        // реальные exec --json типы инструментов + неизвестный → kill (fail-closed)
        for it in ["command_execution", "file_change", "web_search", "some_future_tool"] {
            let line = format!(r#"{{"type":"item.completed","item":{{"type":"{it}","id":"i1"}}}}"#);
            match classify_codex_line(&line) {
                CodexLine::Kill(msg) => assert!(msg.contains(it), "kill называет инструмент: {msg}"),
                _ => panic!("'{it}' должен убивать (allowlist fail-closed)"),
            }
        }
        // разрешённые не-инструменты — НЕ убивают
        for it in ["reasoning", "todo_list"] {
            let line = format!(r#"{{"type":"item.completed","item":{{"type":"{it}"}}}}"#);
            assert_eq!(classify_codex_line(&line), CodexLine::Events(vec![]), "'{it}' разрешён");
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

    #[test]
    fn failed_turn_surfaces_provider_message_and_reconnect_error_is_not_terminal() {
        assert_eq!(classify_codex_line(r#"{"type":"turn.failed","error":{"message":"Usage limit exceeded"}}"#),
            CodexLine::Events(vec![AgentEvent::Failed { message: "Usage limit exceeded".into(), session_id: String::new() }]));
        assert_eq!(classify_codex_line(r#"{"type":"error","message":"Reconnecting 1/5"}"#), CodexLine::Events(vec![]));
    }

    #[test]
    fn updated_items_cannot_bypass_tool_allowlist() {
        assert!(matches!(classify_codex_line(r#"{"type":"item.updated","item":{"type":"command_execution"}}"#), CodexLine::Kill(_)));
        assert!(matches!(classify_codex_line(r#"{"type":"item.updated","item":{"type":"mcp_tool_call","server":"foreign"}}"#), CodexLine::Kill(_)));
        assert_eq!(classify_codex_line(r#"{"type":"item.updated","item":{"type":"agent_message","text":"partial"}}"#), CodexLine::Events(vec![]));
    }
}
