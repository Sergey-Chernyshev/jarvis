//! Агент-хост: запуск `claude` CLI как ограниченного агента (фаза 5).
//!
//! Единственные инструменты агента — наши MCP-капабилити (`mcp__jarvis__*`).
//! INV-TOOLS: если при инициализации хотя бы один инструмент не начинается с
//! `mcp__jarvis__` (например, `Bash`, `Read`, `Write`), хост немедленно убивает
//! процесс — агент вышел за пределы гейта.
//!
//! Ключевые помощники вынесены в чистые функции, тестируемые без живого процесса.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod assistant;

// ── Структуры событий ──────────────────────────────────────────────────────

/// Событие потока `--output-format stream-json` от `claude`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Первое событие: список инструментов и модель.
    Init { tools: Vec<String>, model: String, session_id: String },
    /// Текстовый дельта от ассистента.
    Delta { text: String },
    /// Вызов инструмента агентом.
    ToolUse { name: String, input: Value },
    /// Финальный результат сессии.
    Done { result: String, session_id: String },
    /// Агент не ответил: `--resume` в никуда, обрыв процесса, нарушенный инвариант.
    /// `lost_session` — прошлого разговора больше нет, сохранённый id пора забыть.
    Failed { message: String, lost_session: bool },
    /// Неизвестный / неинтересный тип события — игнорируется.
    Other,
}

// ── Парсинг одной строки stream-json ──────────────────────────────────────

/// Разобрать одну newline-delimited JSON строку потока `claude --output-format stream-json`.
///
/// Возвращает `Vec<AgentEvent>` (обычно 1 элемент, но `assistant` может содержать
/// несколько контент-блоков). Плохой/пустой JSON → пустой вектор, никогда не паникует.
pub fn parse_stream_line(line: &str) -> Vec<AgentEvent> {
    let line = line.trim();
    if line.is_empty() {
        return vec![];
    }
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let typ = v.get("type").and_then(Value::as_str).unwrap_or("");

    match typ {
        "system" => {
            // subtype == "init"
            let subtype = v.get("subtype").and_then(Value::as_str).unwrap_or("");
            if subtype != "init" {
                return vec![];
            }
            let tools: Vec<String> = v
                .get("tools")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|t| t.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let model = v
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let session_id = v
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            vec![AgentEvent::Init { tools, model, session_id }]
        }

        "assistant" => {
            // Один assistant-event может содержать несколько content-блоков
            let blocks = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_array);
            let Some(blocks) = blocks else { return vec![] };

            let mut events = Vec::new();
            for block in blocks {
                let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
                match kind {
                    "text" => {
                        if let Some(text) = block.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                events.push(AgentEvent::Delta { text: text.to_string() });
                            }
                        }
                    }
                    "tool_use" => {
                        let name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let input = block
                            .get("input")
                            .cloned()
                            .unwrap_or(Value::Object(Default::default()));
                        events.push(AgentEvent::ToolUse { name, input });
                    }
                    _ => {}
                }
            }
            events
        }

        "result" => {
            // is_error — отказ CLI (например, `--resume` на пропавший транскрипт).
            // Раньше он приезжал как Done с пустым result: окно снимало «думает…»
            // и молча делало вид, что агент ответил пустотой.
            if v.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
                return vec![AgentEvent::Failed {
                    message: result_error_message(&v),
                    lost_session: false, // знает только хост: он один в курсе про --resume
                }];
            }
            let result = v
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let session_id = v
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            vec![AgentEvent::Done { result, session_id }]
        }

        _ => vec![],
    }
}

// ── Построение аргументов для claude CLI ──────────────────────────────────

/// Собрать argv для `claude`.
///
/// `tools` — список `mcp__jarvis__<id>` (доступность), `resume` — session_id
/// для продолжения диалога.
pub fn build_args(
    config_path: &str,
    system_prompt: &str,
    tools: &[String],
    message: &str,
    resume: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        message.to_string(),
        "--strict-mcp-config".to_string(),
        "--mcp-config".to_string(),
        config_path.to_string(),
        "--append-system-prompt".to_string(),
        system_prompt.to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        // Авто-одобряем mcp__jarvis__* на слое Claude — реальное подтверждение
        // делает наш гейт через PanelConfirmer.
        "--allowedTools".to_string(),
        "mcp__jarvis__*".to_string(),
    ];

    // Список доступных инструментов (только наши MCP-капабилити)
    if !tools.is_empty() {
        args.push("--tools".to_string());
        for t in tools {
            args.push(t.clone());
        }
    }

    args.extend([
        "--permission-mode".to_string(),
        "default".to_string(),
        "--setting-sources".to_string(),
        "project,local".to_string(),
        "--disable-slash-commands".to_string(),
    ]);

    if let Some(id) = resume {
        args.push("--resume".to_string());
        args.push(id.to_string());
    }

    args
}

// ── INV-TOOLS: инвариант безопасности ─────────────────────────────────────

/// Проверить, что ВСЕ инструменты в списке начинаются с `mcp__jarvis__`.
///
/// Нарушение — нежелательный встроенный инструмент (`Bash`, `Read`, `Write`, …)
/// просочился через конфиг. Агент немедленно убивается.
pub fn inv_tools_ok(init_tools: &[String]) -> Result<(), String> {
    for tool in init_tools {
        if !tool.starts_with("mcp__jarvis__") {
            return Err(format!(
                "INV-TOOLS: инструмент '{}' не является mcp__jarvis__*; агент убит",
                tool
            ));
        }
    }
    Ok(())
}

// ── Сохранённый чат: id переживает закрытие окна ──────────────────────────

/// Блок настроек и ключ, где лежит id текущего разговора с агентом.
pub const CHAT_BLOCK: &str = "agentChat";
pub const CHAT_KEY: &str = "sessionId";

/// Сохранённый id разговора из среза настроек. Пусто/не строка → None.
pub fn saved_chat_session(settings: &Value) -> Option<String> {
    settings
        .pointer(&format!("/{CHAT_BLOCK}/{CHAT_KEY}"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Что уйдёт в `--resume`. Явный id окна важнее сохранённого: окно знает про
/// «Новый чат» и про свежий id раньше, чем они доедут до настроек.
pub fn resume_for(explicit: Option<&str>, saved: Option<&str>) -> Option<String> {
    let pick = |s: Option<&str>| s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    pick(explicit).or_else(|| pick(saved))
}

/// Писать ли новый id на диск. Init и Done приносят один и тот же id каждый
/// ход — без этой проверки настройки переписывались бы дважды за реплику.
pub fn session_id_to_persist(saved: Option<&str>, incoming: &str) -> Option<String> {
    let incoming = incoming.trim();
    if incoming.is_empty() || saved.map(str::trim) == Some(incoming) {
        return None;
    }
    Some(incoming.to_string())
}

/// id сессии, который принесло событие (Init/Done). Пусто → None.
pub fn event_session_id(ev: &AgentEvent) -> Option<&str> {
    match ev {
        AgentEvent::Init { session_id, .. } | AgentEvent::Done { session_id, .. } => {
            Some(session_id.trim()).filter(|s| !s.is_empty())
        }
        _ => None,
    }
}

/// Причина отказа из `result`-события: errors[] → result → subtype.
pub fn result_error_message(v: &Value) -> String {
    let pick = |s: Option<&str>| s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    pick(
        v.get("errors")
            .and_then(Value::as_array)
            .and_then(|a| a.iter().find_map(Value::as_str)),
    )
    .or_else(|| pick(v.get("result").and_then(Value::as_str)))
    .or_else(|| pick(v.get("subtype").and_then(Value::as_str)))
    .unwrap_or_else(|| "агент завершился с ошибкой".to_string())
}

/// Отказ означает «прошлого разговора больше нет» (а не «сеть моргнула»)?
/// Только в первом случае честно забыть id: во втором это стоило бы человеку
/// всей нити разговора из-за одной неудачной попытки.
pub fn is_lost_session(message: &str) -> bool {
    let m = message.to_lowercase();
    m.contains("no conversation found")
        || m.contains("no such session")
        || (m.contains("session") && m.contains("not found"))
}

// ── Тестируемый драйвер потока (без живого процесса) ──────────────────────

/// Результат обработки одной строки (для `drive_stream`).
pub enum DriveResult {
    /// Нормальные события.
    Events(Vec<AgentEvent>),
    /// INV-TOOLS нарушен — поток нужно прервать.
    InvToolsViolation(String),
}

/// Прогнать итератор строк потока через парсер + INV-TOOLS проверку.
///
/// Возвращает отсортированные события ДО нарушения (включительно если нужно),
/// прерывает итерацию при `InvToolsViolation`. Используется в тестах напрямую.
pub fn drive_stream(lines: impl Iterator<Item = String>) -> (Vec<AgentEvent>, Option<String>) {
    let mut events: Vec<AgentEvent> = Vec::new();
    let mut violation: Option<String> = None;

    for line in lines {
        let parsed = parse_stream_line(&line);
        for ev in &parsed {
            // Проверяем INV-TOOLS сразу на Init
            if let AgentEvent::Init { tools, .. } = ev {
                if let Err(msg) = inv_tools_ok(tools) {
                    violation = Some(msg);
                    return (events, violation);
                }
            }
        }
        events.extend(parsed);
    }

    (events, violation)
}

// ── Хост: ClaudeCliHost ────────────────────────────────────────────────────

/// Хост агента: конфигурация для запуска `claude`.
pub struct ClaudeCliHost {
    pub app: tauri::AppHandle,
    /// Путь к ~/.jarvis/jarvis-mcp.json
    pub mcp_config: String,
}

/// Промпт, который агент получает вместе с каждым сообщением.
const AGENT_SYSTEM_PROMPT: &str =
    "Ты — ассистент Jarvis. Используй только предоставленные MCP-инструменты. \
     Не обращайся к файловой системе, командной оболочке или сети напрямую.";

impl ClaudeCliHost {
    /// Асинхронно запустить агент-сессию, получить все строки stdout и разобрать события.
    ///
    /// Этот метод тонкий: запускает `claude`, читает stdout построчно, делегирует
    /// тяжёлую логику в `parse_stream_line` / `inv_tools_ok` / `drive_stream`.
    ///
    /// На INV-TOOLS: kill процесса + emit AgentEvent::Other (ошибка уже залогирована).
    pub async fn run(
        &self,
        message: &str,
        tools: &[String],
        resume: Option<&str>,
    ) {
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::process::Command;

        let Some(bin) = crate::claude_bin::resolve_claude_bin() else {
            crate::log::line("[agent] claude не найден");
            // Молча выйти нельзя: окно осталось бы в «думает…» навсегда.
            emit_event(&self.app, &failure("claude не найден — агент не запустился", false));
            return;
        };

        let args = build_args(&self.mcp_config, AGENT_SYSTEM_PROMPT, tools, message, resume);

        let mut child = match Command::new(&bin)
            .args(&args)
            .current_dir(std::env::temp_dir())
            .env("JARVIS_IGNORE", "1")
            .env("DISABLE_NON_ESSENTIAL_MODEL_CALLS", "1")
            // jarvis-mcp (его спавнит claude) наследует сокет НАШЕГО демона —
            // иначе в dev-сборке агент бил бы в прод-сокет (JARVIS_SOCK→JARVIS_DIR).
            .env("JARVIS_SOCK", crate::util::sock_path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                crate::log::line(&format!("[agent] spawn claude: {e}"));
                emit_event(&self.app, &failure(&format!("claude не запустился: {e}"), false));
                return;
            }
        };

        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                crate::log::line("[agent] нет stdout от claude");
                emit_event(&self.app, &failure("агент не отдал вывод", false));
                return;
            }
        };

        let mut reader = BufReader::new(stdout).lines();
        let app = self.app.clone();
        // Помним последний записанный id, чтобы не писать настройки на каждое событие.
        let mut saved = saved_chat_session(&crate::daemon::Daemon::get(&app).settings.load());
        let mut finished = false; // дошло ли до Done/Failed

        while let Ok(Some(line)) = reader.next_line().await {
            let parsed = parse_stream_line(&line);
            for ev in parsed {
                // INV-TOOLS: проверяем первое Init-событие
                if let AgentEvent::Init { ref tools, .. } = ev {
                    if let Err(msg) = inv_tools_ok(tools) {
                        crate::log::line(&format!("[agent] {msg}"));
                        // Убиваем процесс (kill_on_drop = true; явный kill для надёжности)
                        let _ = child.kill().await;
                        emit_event(&app, &failure(&msg, false));
                        return;
                    }
                }
                // Отказ на --resume лечится только забыванием id: иначе следующее
                // сообщение уедет в тот же пропавший транскрипт.
                let ev = match ev {
                    AgentEvent::Failed { message, .. } => AgentEvent::Failed {
                        lost_session: resume.is_some() && is_lost_session(&message),
                        message,
                    },
                    other => other,
                };
                if matches!(ev, AgentEvent::Failed { lost_session: true, .. }) {
                    forget_chat_session(&app);
                    saved = None;
                }
                if let Some(id) = event_session_id(&ev) {
                    if let Some(fresh) = session_id_to_persist(saved.as_deref(), id) {
                        remember_chat_session(&app, &fresh);
                        saved = Some(fresh);
                    }
                }
                finished |= matches!(ev, AgentEvent::Done { .. } | AgentEvent::Failed { .. });
                emit_event(&app, &ev);
            }
        }

        // Поток кончился, а итога не было — процесс умер по дороге. Об этом надо
        // сказать: тишина здесь читается как «агент задумался навсегда».
        if !finished {
            let code = match child.wait().await {
                Ok(st) => st.code().map(|c| c.to_string()).unwrap_or_else(|| "сигнал".into()),
                Err(_) => "?".into(),
            };
            emit_event(&app, &failure(&format!("агент оборвался без ответа (код {code})"), false));
        }
    }
}

/// Событие отказа — единая точка, чтобы «тихих» веток выхода не заводилось.
fn failure(message: &str, lost_session: bool) -> AgentEvent {
    AgentEvent::Failed { message: message.to_string(), lost_session }
}

/// Запомнить id разговора в настройках.
fn remember_chat_session(app: &tauri::AppHandle, id: &str) {
    let mut patch = serde_json::Map::new();
    patch.insert(CHAT_KEY.to_string(), Value::String(id.to_string()));
    crate::daemon::Daemon::get(app).settings.set_block(CHAT_BLOCK, patch);
}

/// Забыть id разговора («Новый чат» либо пропавший транскрипт). Сам транскрипт
/// остаётся на диске — мы теряем только ниточку к нему.
pub fn forget_chat_session(app: &tauri::AppHandle) {
    let mut patch = serde_json::Map::new();
    patch.insert(CHAT_KEY.to_string(), Value::String(String::new()));
    crate::daemon::Daemon::get(app).settings.set_block(CHAT_BLOCK, patch);
}

/// Отправить событие в главное окно Tauri.
fn emit_event(app: &tauri::AppHandle, ev: &AgentEvent) {
    use tauri::Emitter;
    // Игнорируем Other события
    if matches!(ev, AgentEvent::Other) {
        return;
    }
    if let Err(e) = app.emit("agent:event", ev) {
        crate::log::line(&format!("[agent] emit error: {e}"));
    }
}

// ── Тесты ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── build_args ────────────────────────────────────────────────────────

    #[test]
    fn build_args_contains_required_flags() {
        let tools = vec!["mcp__jarvis__sessions.reply".to_string()];
        let args = build_args("/path/to/mcp.json", "system", &tools, "hello", None);

        // -p и сообщение
        assert!(args.contains(&"-p".to_string()), "нет -p");
        assert!(args.contains(&"hello".to_string()), "нет message");

        // --strict-mcp-config
        assert!(args.contains(&"--strict-mcp-config".to_string()));

        // --mcp-config <path>
        let idx = args.iter().position(|a| a == "--mcp-config").expect("нет --mcp-config");
        assert_eq!(args[idx + 1], "/path/to/mcp.json");

        // --output-format stream-json
        let idx = args.iter().position(|a| a == "--output-format").expect("нет --output-format");
        assert_eq!(args[idx + 1], "stream-json");

        // --tools
        assert!(args.contains(&"--tools".to_string()), "нет --tools");
        assert!(args.contains(&"mcp__jarvis__sessions.reply".to_string()), "нет инструмента");

        // --verbose
        assert!(args.contains(&"--verbose".to_string()), "нет --verbose");

        // --allowedTools
        assert!(args.contains(&"--allowedTools".to_string()), "нет --allowedTools");
        let idx = args.iter().position(|a| a == "--allowedTools").unwrap();
        assert_eq!(args[idx + 1], "mcp__jarvis__*");

        // --disable-slash-commands
        assert!(args.contains(&"--disable-slash-commands".to_string()));
    }

    #[test]
    fn build_args_with_resume_appends_resume_flag() {
        let args = build_args("/mcp.json", "sys", &[], "msg", Some("sess-123"));
        let idx = args.iter().position(|a| a == "--resume").expect("нет --resume");
        assert_eq!(args[idx + 1], "sess-123");
    }

    #[test]
    fn build_args_without_resume_has_no_resume_flag() {
        let args = build_args("/mcp.json", "sys", &[], "msg", None);
        assert!(!args.contains(&"--resume".to_string()));
    }

    #[test]
    fn build_args_no_tools_skips_tools_flag() {
        let args = build_args("/mcp.json", "sys", &[], "msg", None);
        assert!(!args.contains(&"--tools".to_string()), "--tools не должен быть при пустом списке");
    }

    // ── parse_stream_line ─────────────────────────────────────────────────

    #[test]
    fn parse_init_event() {
        let line = r#"{"type":"system","subtype":"init","session_id":"s1","tools":["mcp__jarvis__sessions.reply","mcp__jarvis__metrics.query"],"mcp_servers":[{"name":"jarvis","status":"connected"}],"model":"claude-sonnet-4-5"}"#;
        let events = parse_stream_line(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgentEvent::Init { tools, model, session_id } => {
                assert_eq!(tools, &["mcp__jarvis__sessions.reply", "mcp__jarvis__metrics.query"]);
                assert_eq!(model, "claude-sonnet-4-5");
                assert_eq!(session_id, "s1");
            }
            other => panic!("ожидали Init, получили {:?}", other),
        }
    }

    #[test]
    fn parse_assistant_text_event() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Привет!"}]}}"#;
        let events = parse_stream_line(line);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AgentEvent::Delta { text } if text == "Привет!"));
    }

    #[test]
    fn parse_assistant_tool_use_event() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"mcp__jarvis__sessions.reply","input":{"session_id":"s1","text":"ok"}}]}}"#;
        let events = parse_stream_line(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgentEvent::ToolUse { name, input } => {
                assert_eq!(name, "mcp__jarvis__sessions.reply");
                assert_eq!(input, &json!({"session_id":"s1","text":"ok"}));
            }
            other => panic!("ожидали ToolUse, получили {:?}", other),
        }
    }

    #[test]
    fn parse_assistant_multiple_blocks() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Делаю..."},{"type":"tool_use","name":"mcp__jarvis__metrics.query","input":{}}]}}"#;
        let events = parse_stream_line(line);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], AgentEvent::Delta { text } if text == "Делаю..."));
        assert!(matches!(&events[1], AgentEvent::ToolUse { name, .. } if name == "mcp__jarvis__metrics.query"));
    }

    #[test]
    fn parse_result_event() {
        let line = r#"{"type":"result","subtype":"success","result":"Готово","session_id":"s2","total_cost_usd":0.001}"#;
        let events = parse_stream_line(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgentEvent::Done { result, session_id } => {
                assert_eq!(result, "Готово");
                assert_eq!(session_id, "s2");
            }
            other => panic!("ожидали Done, получили {:?}", other),
        }
    }

    #[test]
    fn parse_garbage_returns_empty() {
        assert_eq!(parse_stream_line(""), vec![]);
        assert_eq!(parse_stream_line("   "), vec![]);
        assert_eq!(parse_stream_line("not json at all"), vec![]);
        assert_eq!(parse_stream_line("{incomplete"), vec![]);
    }

    #[test]
    fn parse_unknown_type_returns_empty() {
        let line = r#"{"type":"tool_result","content":"ok"}"#;
        let events = parse_stream_line(line);
        assert_eq!(events.len(), 0, "неизвестный тип → пустой вектор");
    }

    #[test]
    fn parse_system_non_init_subtype_returns_empty() {
        let line = r#"{"type":"system","subtype":"other","data":{}}"#;
        let events = parse_stream_line(line);
        assert_eq!(events.len(), 0);
    }

    // ── inv_tools_ok ─────────────────────────────────────────────────────

    #[test]
    fn inv_tools_ok_passes_all_mcp_jarvis() {
        let tools = vec!["mcp__jarvis__sessions.reply".to_string()];
        assert!(inv_tools_ok(&tools).is_ok());
    }

    #[test]
    fn inv_tools_ok_passes_empty_list() {
        assert!(inv_tools_ok(&[]).is_ok());
    }

    #[test]
    fn inv_tools_ok_fails_on_bash() {
        let tools = vec!["mcp__jarvis__sessions.reply".to_string(), "Bash".to_string()];
        let err = inv_tools_ok(&tools).unwrap_err();
        assert!(err.contains("Bash"), "ошибка должна называть нарушителя: {err}");
    }

    #[test]
    fn inv_tools_ok_fails_on_read() {
        let tools = vec!["Read".to_string()];
        let err = inv_tools_ok(&tools).unwrap_err();
        assert!(err.contains("Read"), "ошибка должна называть Read: {err}");
    }

    #[test]
    fn inv_tools_ok_fails_on_write() {
        let tools = vec!["mcp__jarvis__x".to_string(), "Write".to_string()];
        let err = inv_tools_ok(&tools).unwrap_err();
        assert!(err.contains("Write"));
    }

    // ── сохранённый чат ───────────────────────────────────────────────────

    #[test]
    fn saved_chat_session_reads_block() {
        let s = json!({ "agentChat": { "sessionId": "s-42" } });
        assert_eq!(saved_chat_session(&s).as_deref(), Some("s-42"));
    }

    #[test]
    fn saved_chat_session_empty_is_none() {
        // Сброс пишет пустую строку — она не должна читаться как живой id.
        assert_eq!(saved_chat_session(&json!({ "agentChat": { "sessionId": "" } })), None);
        assert_eq!(saved_chat_session(&json!({ "agentChat": { "sessionId": "  " } })), None);
        assert_eq!(saved_chat_session(&json!({ "agentChat": {} })), None);
        assert_eq!(saved_chat_session(&json!({})), None);
    }

    #[test]
    fn resume_for_falls_back_to_saved() {
        // Окно после перезапуска не знает id — подставляем сохранённый.
        assert_eq!(resume_for(None, Some("s-42")).as_deref(), Some("s-42"));
        assert_eq!(resume_for(Some(""), Some("s-42")).as_deref(), Some("s-42"));
    }

    #[test]
    fn resume_for_prefers_explicit() {
        assert_eq!(resume_for(Some("s-new"), Some("s-old")).as_deref(), Some("s-new"));
    }

    #[test]
    fn resume_for_after_reset_starts_new() {
        // «Новый чат» стёр сохранённый id → resume пуст, claude заводит сессию.
        assert_eq!(resume_for(None, None), None);
        assert_eq!(resume_for(None, saved_chat_session(&json!({ "agentChat": { "sessionId": "" } })).as_deref()), None);
    }

    #[test]
    fn session_id_to_persist_skips_noise() {
        assert_eq!(session_id_to_persist(None, ""), None);
        assert_eq!(session_id_to_persist(Some("s1"), "s1"), None); // Init и Done несут один id
        assert_eq!(session_id_to_persist(Some("s1"), "s2").as_deref(), Some("s2"));
        assert_eq!(session_id_to_persist(None, "s1").as_deref(), Some("s1"));
    }

    #[test]
    fn event_session_id_only_from_id_carrying_events() {
        let init = AgentEvent::Init { tools: vec![], model: String::new(), session_id: "s1".into() };
        assert_eq!(event_session_id(&init), Some("s1"));
        let done = AgentEvent::Done { result: "ok".into(), session_id: "s2".into() };
        assert_eq!(event_session_id(&done), Some("s2"));
        assert_eq!(event_session_id(&AgentEvent::Delta { text: "x".into() }), None);
        assert_eq!(
            event_session_id(&AgentEvent::Done { result: String::new(), session_id: String::new() }),
            None
        );
    }

    // ── честный фолбэк ────────────────────────────────────────────────────

    #[test]
    fn parse_failed_resume_result_is_not_done() {
        // Живой ответ claude на `--resume <несуществующий>` (exit 1).
        let line = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"session_id":"00000000-0000-0000-0000-000000000000","result":"","errors":["No conversation found with session ID: 00000000-0000-0000-0000-000000000000"]}"#;
        let events = parse_stream_line(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgentEvent::Failed { message, .. } => assert!(message.contains("No conversation found")),
            other => panic!("отказ обязан быть Failed, а не {:?}", other),
        }
    }

    #[test]
    fn result_error_message_prefers_errors_then_result_then_subtype() {
        assert_eq!(result_error_message(&json!({"errors":["boom"],"result":"r","subtype":"s"})), "boom");
        assert_eq!(result_error_message(&json!({"errors":[],"result":"r","subtype":"s"})), "r");
        assert_eq!(result_error_message(&json!({"subtype":"error_during_execution"})), "error_during_execution");
        assert_eq!(result_error_message(&json!({})), "агент завершился с ошибкой");
    }

    #[test]
    fn is_lost_session_only_for_missing_transcript() {
        assert!(is_lost_session("No conversation found with session ID: abc"));
        assert!(is_lost_session("session abc not found"));
        // Сеть моргнула — id не трогаем, иначе разговор терялся бы от одной осечки.
        assert!(!is_lost_session("API Error: 529 overloaded"));
        assert!(!is_lost_session("Credit balance is too low"));
    }

    // ── drive_stream ─────────────────────────────────────────────────────

    #[test]
    fn drive_stream_happy_path() {
        let lines = vec![
            r#"{"type":"system","subtype":"init","session_id":"s1","tools":["mcp__jarvis__sessions.reply"],"mcp_servers":[],"model":"claude-haiku-3-5"}"#.to_string(),
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Выполняю"}]}}"#.to_string(),
            r#"{"type":"result","subtype":"success","result":"Сделано","session_id":"s1","total_cost_usd":0}"#.to_string(),
        ];
        let (events, violation) = drive_stream(lines.into_iter());
        assert!(violation.is_none(), "нарушений нет");
        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0], AgentEvent::Init { .. }));
        assert!(matches!(&events[1], AgentEvent::Delta { .. }));
        assert!(matches!(&events[2], AgentEvent::Done { .. }));
    }

    #[test]
    fn drive_stream_inv_tools_violation_aborts() {
        let lines = vec![
            // Init с нарушением: Bash просочился
            r#"{"type":"system","subtype":"init","session_id":"s1","tools":["mcp__jarvis__sessions.reply","Bash"],"mcp_servers":[],"model":"claude-sonnet-4-5"}"#.to_string(),
            // Это сообщение НЕ должно войти в результат
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Я свободен!"}]}}"#.to_string(),
        ];
        let (events, violation) = drive_stream(lines.into_iter());
        assert!(violation.is_some(), "ожидаем нарушение INV-TOOLS");
        assert!(violation.unwrap().contains("Bash"));
        // События до нарушения (т.е. до Init) — нет, Init сам нарушитель
        assert_eq!(events.len(), 0, "нарушение на Init — событий нет");
    }

    #[test]
    fn drive_stream_clean_init_then_violation_later_impossible() {
        // Если Init чистый, Bash не появится через parse (он парсит по типу tool_use),
        // но проверим, что чистый init проходит
        let lines = vec![
            r#"{"type":"system","subtype":"init","session_id":"s1","tools":["mcp__jarvis__metrics.query"],"mcp_servers":[],"model":"claude-haiku-3-5"}"#.to_string(),
        ];
        let (events, violation) = drive_stream(lines.into_iter());
        assert!(violation.is_none());
        assert_eq!(events.len(), 1);
    }
}
