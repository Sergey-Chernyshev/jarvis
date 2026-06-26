//! Парсер rollout-транскрипта Codex → общий `ChatItem`.
//!
//! Формат: `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`, каждая строка
//! `{timestamp, type, payload}`. type ∈ session_meta | turn_context | response_item
//! | event_msg. Каноничная переписка — в `response_item` (event_msg дублирует её
//! как телеметрию, его пропускаем). Defensive: неизвестное → пропуск, не паникуем.

use serde_json::Value;

use crate::transcript::{parse_ts, ChatItem};
use crate::util::{ellipsize, now_ms, one_line};

/// Одна строка rollout → элементы чата (обычно 0–1). Пропускаем developer/system
/// (системный промпт), reasoning, function_call_output, session_meta, turn_context,
/// event_msg (дубль).
pub fn to_chat_items(entry: &Value) -> Vec<ChatItem> {
    if entry.get("type").and_then(Value::as_str) != Some("response_item") {
        return vec![];
    }
    let Some(payload) = entry.get("payload") else { return vec![] };
    let ts = entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_ts)
        .unwrap_or_else(now_ms);

    match payload.get("type").and_then(Value::as_str) {
        Some("message") => {
            let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
            // только реплики юзера/ассистента; developer/system (огромный системный
            // промпт и инъекции контекста) — мимо ленты.
            let role: &'static str = match role {
                "user" => "user",
                "assistant" => "assistant",
                _ => return vec![],
            };
            let Some(blocks) = payload.get("content").and_then(Value::as_array) else { return vec![] };
            let mut out = Vec::new();
            for b in blocks {
                let bt = b.get("type").and_then(Value::as_str).unwrap_or("");
                if !matches!(bt, "input_text" | "output_text" | "text") {
                    continue;
                }
                let text = b.get("text").and_then(Value::as_str).unwrap_or("");
                // служебные инъекции <...> — как в Claude-парсере
                if text.is_empty() || text.starts_with('<') {
                    continue;
                }
                out.push(ChatItem { role, kind: "text", text: text.to_string(), ts });
            }
            out
        }
        Some("function_call") => {
            let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
            let args = payload
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|s| serde_json::from_str::<Value>(s).ok());
            vec![ChatItem { role: "assistant", kind: "tool", text: tool_label(name, args.as_ref()), ts }]
        }
        Some("custom_tool_call") => {
            // apply_patch и т.п.
            let name = payload.get("name").and_then(Value::as_str).unwrap_or("инструмент");
            vec![ChatItem { role: "assistant", kind: "tool", text: name.to_string(), ts }]
        }
        _ => vec![], // reasoning, function_call_output, ...
    }
}

/// Чип инструмента: «name: <первый осмысленный аргумент>».
fn tool_label(name: &str, args: Option<&Value>) -> String {
    let hint = args.and_then(|a| {
        ["cmd", "command", "file_path", "path", "pattern", "query", "url", "description"]
            .iter()
            .find_map(|k| a.get(*k).and_then(Value::as_str))
            .map(|s| s.to_string())
    });
    match hint {
        Some(h) if !h.is_empty() => format!("{name}: {}", ellipsize(&one_line(&h), 80)),
        _ => name.to_string(),
    }
}

/// Модель сессии: последний `turn_context.model`.
pub fn extract_model(entries: &[Value]) -> Option<String> {
    entries.iter().rev().find_map(|e| {
        if e.get("type").and_then(Value::as_str) == Some("turn_context") {
            e.get("payload").and_then(|p| p.get("model")).and_then(Value::as_str).map(String::from)
        } else {
            None
        }
    })
}

/// Заголовок: первая реплика юзера (укорочена). session_index.jsonl с thread_name
/// читается отдельно демоном при необходимости.
pub fn extract_title(entries: &[Value]) -> Option<String> {
    for e in entries {
        for item in to_chat_items(e) {
            if item.role == "user" && item.kind == "text" {
                let t = ellipsize(&one_line(&item.text), 60);
                if !t.is_empty() {
                    return Some(t);
                }
            }
        }
    }
    None
}

/// Финальный ответ ассистента: последняя assistant-text реплика. (Демон
/// предпочитает `last_assistant_message` из Stop-хука — rollout может быть не
/// сфлашен; это фолбэк.)
pub fn full_final_reply(entries: &[Value]) -> Option<String> {
    let mut last: Option<String> = None;
    for e in entries {
        for item in to_chat_items(e) {
            if item.role == "assistant" && item.kind == "text" && !item.text.trim().is_empty() {
                last = Some(item.text);
            }
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(typ: &str, payload: Value) -> Value {
        json!({ "timestamp": "2026-06-26T22:06:56.000Z", "type": typ, "payload": payload })
    }

    #[test]
    fn message_user_and_assistant() {
        let u = line("response_item", json!({"type":"message","role":"user","content":[{"type":"input_text","text":"привет"}]}));
        let a = line("response_item", json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"здравствуй"}]}));
        let iu = to_chat_items(&u);
        let ia = to_chat_items(&a);
        assert_eq!(iu.len(), 1);
        assert_eq!(iu[0].role, "user");
        assert_eq!(iu[0].text, "привет");
        assert_eq!(ia[0].role, "assistant");
        assert_eq!(ia[0].text, "здравствуй");
    }

    #[test]
    fn developer_and_event_msg_skipped() {
        let dev = line("response_item", json!({"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions> огромный системный промпт"}]}));
        let em = line("event_msg", json!({"type":"agent_message","message":"дубль"}));
        let meta = line("session_meta", json!({"id":"x","cwd":"/tmp"}));
        assert!(to_chat_items(&dev).is_empty(), "developer-роль не в ленте");
        assert!(to_chat_items(&em).is_empty(), "event_msg — телеметрия, не в ленте");
        assert!(to_chat_items(&meta).is_empty());
    }

    #[test]
    fn function_call_becomes_tool_chip() {
        let fc = line("response_item", json!({
            "type":"function_call","name":"exec_command",
            "arguments":"{\"cmd\":\"sed -n '1,20p' SKILL.md\",\"workdir\":\"/x\"}","call_id":"c1"
        }));
        let items = to_chat_items(&fc);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "tool");
        assert!(items[0].text.starts_with("exec_command: sed -n"), "чип: {}", items[0].text);
    }

    #[test]
    fn extract_model_and_title_and_reply() {
        let entries = vec![
            line("session_meta", json!({"id":"x","cwd":"/x"})),
            line("response_item", json!({"type":"message","role":"user","content":[{"type":"input_text","text":"сделай рефактор парсера"}]})),
            line("turn_context", json!({"model":"gpt-5.5","turn_id":"t1"})),
            line("response_item", json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"готово"}]})),
            line("turn_context", json!({"model":"gpt-5-codex","turn_id":"t2"})),
        ];
        assert_eq!(extract_model(&entries).as_deref(), Some("gpt-5-codex"), "последний turn_context.model");
        assert_eq!(extract_title(&entries).as_deref(), Some("сделай рефактор парсера"));
        assert_eq!(full_final_reply(&entries).as_deref(), Some("готово"));
    }
}
