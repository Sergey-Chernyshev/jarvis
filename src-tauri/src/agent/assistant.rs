//! Внешний ассистент (под-проект 4, веха 4a): голосовой Джарвис отвечает на
//! ПРОИЗВОЛЬНЫЕ вопросы и ищет в интернете. Спавнит настоящий `claude` агент с
//! READ-набором инструментов (`WebSearch WebFetch Read Grep Glob`) — все они
//! авто-разрешены, сайд-эффекты (Bash/Write/Edit) НЕдоступны (их нет в
//! allowedTools, permission-mode default → запрос на них просто не исполнится).
//!
//! Рабочая папка — ИЗОЛИРОВАННЫЙ скретч (`~/.jarvis[-dev]/assistant-cwd`), не
//! репозиторий: `--setting-sources project,local` в пустой папке = ноль чужих
//! MCP/хуков. Прокси (egress) наследуется из env — как у `run_claude`.
//!
//! Сборка флагов и извлечение финального ответа — чистые функции (юнит-тесты);
//! `run` — тонкий спавн + парс потока через `agent::parse_stream_line`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use crate::agent::{parse_stream_line, AgentEvent};

/// Системный промпт ассистента: ответ ДЛЯ ОЗВУЧКИ — кратко, по-русски, без
/// markdown/ссылок/списков (всё это плохо звучит). Это голосовой ассистент.
pub const ASSISTANT_SYSTEM: &str = "Ты — голосовой ассистент Jarvis, отвечаешь вслух. \
Ответь по существу на русском языке, разговорным стилем, как живой ассистент. \
Если нужен свежий факт — используй веб-поиск. \
ВАЖНО для озвучки: без markdown, без списков с маркерами, без ссылок и URL, без кода и таблиц. \
Пиши обычным текстом, который приятно слушать. Будь информативным, но не растекайся: \
2–5 предложений для простого вопроса, больше — только если вопрос правда сложный.";

/// READ-набор авто-разрешённых инструментов (read-only, без сайд-эффектов).
const READ_TOOLS: &str = "WebSearch WebFetch Read Grep Glob";

/// Модель ассистента по умолчанию: веб/тул-оркестрация Haiku не по силам,
/// берём Sonnet (дефолт Claude Code).
const ASSISTANT_MODEL: &str = "sonnet";

/// Таймаут одного запроса к ассистенту (веб-поиск бывает медленным).
pub const ASSISTANT_TIMEOUT: Duration = Duration::from_secs(90);

/// Собрать argv для `claude` в режиме внешнего ассистента (чистая функция).
pub fn build_assistant_args(query: &str, model: &str) -> Vec<String> {
    vec![
        "-p".into(),
        query.into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        // READ-набор: авто-разрешён, сайд-эффекты вне списка → не исполнятся.
        "--allowedTools".into(),
        READ_TOOLS.into(),
        // ноль чужих MCP + пропустить user-настройки (плагины/хуки); скретч-cwd
        // не содержит project/local → ничего лишнего не грузится.
        "--strict-mcp-config".into(),
        "--disable-slash-commands".into(),
        "--setting-sources".into(),
        "project,local".into(),
        "--no-session-persistence".into(),
        "--permission-mode".into(),
        "default".into(),
        "--model".into(),
        model.into(),
        "--append-system-prompt".into(),
        ASSISTANT_SYSTEM.into(),
    ]
}

/// Изолированная скретч-папка ассистента (создаётся при первом обращении).
fn ensure_assistant_cwd() -> PathBuf {
    let dir = crate::util::jarvis_dir().join("assistant-cwd");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Извлечь финальный ответ из событий потока. Предпочитаем последний непустой
/// `Done.result`; иначе склейка всех `Delta`. None — если пусто. Чистая.
pub fn extract_answer(events: &[AgentEvent]) -> Option<String> {
    // последний непустой result
    let result = events.iter().rev().find_map(|e| match e {
        AgentEvent::Done { result, .. } if !result.trim().is_empty() => Some(result.trim().to_string()),
        _ => None,
    });
    if let Some(r) = result {
        return Some(r);
    }
    // фолбэк: склейка дельт
    let joined: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Delta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    let t = joined.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Внешний ассистент. Запускает `claude`, ждёт финальный текст для озвучки.
pub struct AssistantHost;

impl AssistantHost {
    /// Ответить на запрос (веб-поиск + рассуждение). None — нет claude / таймаут /
    /// пустой ответ. Прокси наследуется (egress); JARVIS_IGNORE — не засорять реестр.
    pub async fn run(query: &str, timeout: Duration) -> Option<String> {
        let bin = crate::claude_bin::resolve_claude_bin()?;
        let cwd = ensure_assistant_cwd();
        let args = build_assistant_args(query, ASSISTANT_MODEL);

        crate::log::line(&format!(
            "[assistant] → {}",
            crate::util::ellipsize(&crate::util::one_line(query), 200)
        ));

        let mut cmd = tokio::process::Command::new(bin);
        cmd.args(&args)
            .current_dir(&cwd)
            .env("JARVIS_IGNORE", "1")
            .env("DISABLE_NON_ESSENTIAL_MODEL_CALLS", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        let out = tokio::time::timeout(timeout, cmd.output()).await.ok()?.ok()?;
        if !out.status.success() {
            crate::log::line("[assistant] ← <ненулевой код выхода>");
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let events: Vec<AgentEvent> = text.lines().flat_map(parse_stream_line).collect();
        let ans = extract_answer(&events);
        crate::log::line(&format!(
            "[assistant] ← {}",
            match &ans {
                Some(s) => crate::util::ellipsize(&crate::util::one_line(s), 200),
                None => "<пусто>".into(),
            }
        ));
        ans
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_args_has_read_tools_and_no_sideeffects() {
        let args = build_assistant_args("какая погода", "sonnet");
        // запрос
        let i = args.iter().position(|a| a == "-p").unwrap();
        assert_eq!(args[i + 1], "какая погода");
        // READ-набор как единое значение
        let i = args.iter().position(|a| a == "--allowedTools").unwrap();
        assert_eq!(args[i + 1], "WebSearch WebFetch Read Grep Glob");
        // НЕТ опасных инструментов в allowedTools
        assert!(!args[i + 1].contains("Bash"));
        assert!(!args[i + 1].contains("Write"));
        assert!(!args[i + 1].contains("Edit"));
        // stream-json + изоляция
        let i = args.iter().position(|a| a == "--output-format").unwrap();
        assert_eq!(args[i + 1], "stream-json");
        assert!(args.contains(&"--strict-mcp-config".to_string()));
        assert!(args.contains(&"--no-session-persistence".to_string()));
        // модель и system-prompt
        let i = args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(args[i + 1], "sonnet");
        assert!(args.contains(&"--append-system-prompt".to_string()));
    }

    fn done(result: &str) -> AgentEvent {
        AgentEvent::Done { result: result.into(), session_id: "s".into() }
    }
    fn delta(text: &str) -> AgentEvent {
        AgentEvent::Delta { text: text.into() }
    }

    #[test]
    fn extract_prefers_done_result() {
        let evs = vec![delta("часть… "), done("Сейчас в Москве плюс двадцать.")];
        assert_eq!(extract_answer(&evs).unwrap(), "Сейчас в Москве плюс двадцать.");
    }

    #[test]
    fn extract_falls_back_to_delta_join() {
        let evs = vec![delta("Привет, "), delta("это ответ."), done("   ")];
        assert_eq!(extract_answer(&evs).unwrap(), "Привет, это ответ.");
    }

    #[test]
    fn extract_empty_is_none() {
        assert!(extract_answer(&[]).is_none());
        assert!(extract_answer(&[done(""), delta("   ")]).is_none());
    }

    #[test]
    fn extract_parses_real_stream_lines() {
        // склейка через реальный парсер потока
        let lines = [
            r#"{"type":"system","subtype":"init","session_id":"s","tools":["WebSearch"],"model":"claude-sonnet-4-5"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Думаю…"}]}}"#,
            r#"{"type":"result","subtype":"success","result":"Готовый ответ.","session_id":"s"}"#,
        ];
        let evs: Vec<AgentEvent> = lines.iter().flat_map(|l| parse_stream_line(l)).collect();
        assert_eq!(extract_answer(&evs).unwrap(), "Готовый ответ.");
        // и форма json для самопроверки фикстуры
        let _ = json!({});
    }
}
