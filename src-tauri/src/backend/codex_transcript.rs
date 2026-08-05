//! Парсер rollout-транскрипта Codex → общий `ChatItem`.
//!
//! Формат: `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`, каждая строка
//! `{timestamp, type, payload}`. type ∈ session_meta | turn_context | response_item
//! | event_msg. Каноничная переписка — в `response_item` (event_msg дублирует её
//! как телеметрию, его пропускаем). Defensive: неизвестное → пропуск, не паникуем.

use serde_json::Value;

use crate::transcript::{parse_ts, ChatItem};
use crate::util::{ellipsize, now_ms, one_line};

#[derive(Debug, PartialEq)]
enum NormalizedItem {
    UserText(String),
    AssistantFinal(String),
    Activity(String),
    Ignored,
}

/// Одна строка rollout → элементы чата (обычно 0–1). Пропускаем developer/system
/// (системный промпт), assistant commentary/progress, reasoning,
/// function_call_output, session_meta, turn_context, event_msg (дубль).
pub fn to_chat_items(entry: &Value) -> Vec<ChatItem> {
    let ts = entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_ts)
        .unwrap_or_else(now_ms);

    match normalize(entry) {
        NormalizedItem::UserText(text) => vec![ChatItem {
            role: "user",
            kind: "text",
            text,
            ts,
        }],
        NormalizedItem::AssistantFinal(text) => vec![ChatItem {
            role: "assistant",
            kind: "text",
            text,
            ts,
        }],
        NormalizedItem::Activity(text) => vec![ChatItem {
            role: "assistant",
            kind: "tool",
            text,
            ts,
        }],
        NormalizedItem::Ignored => vec![],
    }
}

fn normalize(entry: &Value) -> NormalizedItem {
    if entry.get("type").and_then(Value::as_str) != Some("response_item") {
        return NormalizedItem::Ignored;
    }
    let Some(payload) = entry.get("payload") else {
        return NormalizedItem::Ignored;
    };

    match payload.get("type").and_then(Value::as_str) {
        Some("message") => normalize_message(payload),
        Some("function_call") => {
            let Some(name) = payload
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
            else {
                return NormalizedItem::Ignored;
            };
            let args = payload
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|s| serde_json::from_str::<Value>(s).ok());
            NormalizedItem::Activity(tool_label(name, args.as_ref()))
        }
        Some("custom_tool_call") => {
            // apply_patch и т.п.
            let Some(name) = payload
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
            else {
                return NormalizedItem::Ignored;
            };
            let input = payload.get("input").and_then(Value::as_str);
            NormalizedItem::Activity(custom_tool_label(name, input))
        }
        _ => NormalizedItem::Ignored, // reasoning, tool outputs, unknown runtime events
    }
}

fn normalize_message(payload: &Value) -> NormalizedItem {
    let Some(text) = message_text(payload) else {
        return NormalizedItem::Ignored;
    };

    match payload.get("role").and_then(Value::as_str) {
        Some("user") => NormalizedItem::UserText(text),
        Some("assistant") => match payload.get("phase") {
            None => NormalizedItem::AssistantFinal(text), // legacy rollout format
            Some(Value::String(phase)) if phase == "final_answer" => {
                NormalizedItem::AssistantFinal(text)
            }
            Some(_) => NormalizedItem::Ignored,
        },
        _ => NormalizedItem::Ignored,
    }
}

fn message_text(payload: &Value) -> Option<String> {
    let blocks = payload.get("content").and_then(Value::as_array)?;
    let texts = blocks
        .iter()
        .filter_map(|block| {
            let block_type = block.get("type").and_then(Value::as_str)?;
            if !matches!(block_type, "input_text" | "output_text" | "text") {
                return None;
            }
            let text = block.get("text").and_then(Value::as_str)?;
            // Служебные инъекции: <...> (как Claude) + впрыск AGENTS.md, который
            // Codex кладёт первым user-блоком как «# AGENTS.md instructions …».
            if text.is_empty()
                || text.starts_with('<')
                || text.starts_with("# AGENTS.md instructions")
            {
                return None;
            }
            Some(text)
        })
        .collect::<Vec<_>>();

    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n\n"))
    }
}

/// Чип инструмента в том же формате, что у Claude: «Tool · аргумент».
fn tool_label(name: &str, args: Option<&Value>) -> String {
    match name {
        "exec_command" => label_with_hint("Bash", command_hint(args)),
        "write_stdin" => label_with_hint("Bash", Some("stdin".into())),
        "wait" => "Wait".into(),
        "update_plan" => {
            let count = args
                .and_then(|a| a.get("plan"))
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            if count > 0 {
                format!("TodoWrite · {count} {}", ru_tasks(count))
            } else {
                "TodoWrite".into()
            }
        }
        "multi_tool_use.parallel" => {
            let count = args
                .and_then(|a| a.get("tool_uses"))
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            if count > 0 {
                format!("Task · {count} {}", ru_commands(count))
            } else {
                "Task".into()
            }
        }
        "web.run" => web_tool_label(args).unwrap_or_else(|| "WebSearch".into()),
        "resolve_library_id" => {
            label_with_hint("Search", named_arg_hint(args, &["libraryName", "query"]))
        }
        "query_docs" => label_with_hint("Read", named_arg_hint(args, &["libraryId", "query"])),
        _ => label_with_hint(display_tool_name(name), first_arg_hint(args)),
    }
}

fn custom_tool_label(name: &str, input: Option<&str>) -> String {
    match name {
        "apply_patch" => label_with_hint("Edit", input.and_then(patch_file_hint)),
        "exec" => "Task".into(),
        _ => label_with_hint(
            display_tool_name(name),
            input.map(|s| ellipsize(&one_line(s), 96)),
        ),
    }
}

fn label_with_hint(tool: impl Into<String>, hint: Option<String>) -> String {
    let tool = tool.into();
    match hint {
        Some(h) if !h.is_empty() => format!("{tool} · {}", ellipsize(&one_line(&h), 96)),
        _ => tool,
    }
}

fn display_tool_name(name: &str) -> String {
    match name {
        "exec_command" => "Bash".into(),
        "write_stdin" => "Bash".into(),
        "update_plan" => "TodoWrite".into(),
        "apply_patch" => "Edit".into(),
        "web.run" => "WebSearch".into(),
        _ => name
            .rsplit(['.', ':'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(name)
            .to_string(),
    }
}

fn command_hint(args: Option<&Value>) -> Option<String> {
    args.and_then(|a| {
        a.get("cmd")
            .or_else(|| a.get("command"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
    })
}

fn first_arg_hint(args: Option<&Value>) -> Option<String> {
    named_arg_hint(
        args,
        &[
            "cmd",
            "command",
            "file_path",
            "path",
            "pattern",
            "query",
            "url",
            "description",
        ],
    )
}

fn named_arg_hint(args: Option<&Value>, keys: &[&str]) -> Option<String> {
    args.and_then(|a| {
        keys.iter()
            .find_map(|k| a.get(*k).and_then(Value::as_str))
            .map(ToString::to_string)
    })
}

fn web_tool_label(args: Option<&Value>) -> Option<String> {
    let a = args?;
    if let Some(q) = a
        .get("search_query")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("q"))
        .and_then(Value::as_str)
    {
        return Some(label_with_hint("WebSearch", Some(q.to_string())));
    }
    if let Some(target) = a
        .get("open")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("ref_id"))
        .and_then(Value::as_str)
    {
        return Some(label_with_hint("WebFetch", Some(target.to_string())));
    }
    None
}

fn patch_file_hint(input: &str) -> Option<String> {
    for line in input.lines() {
        for prefix in ["*** Update File: ", "*** Add File: ", "*** Delete File: "] {
            if let Some(path) = line.strip_prefix(prefix) {
                let path = path.trim();
                if !path.is_empty() {
                    return Some(path.to_string());
                }
            }
        }
    }
    None
}

fn ru_tasks(count: usize) -> &'static str {
    ru_count_word(count, "задача", "задачи", "задач")
}

fn ru_commands(count: usize) -> &'static str {
    ru_count_word(count, "команда", "команды", "команд")
}

fn ru_count_word(
    count: usize,
    one: &'static str,
    few: &'static str,
    many: &'static str,
) -> &'static str {
    let mod100 = count % 100;
    if (11..=14).contains(&mod100) {
        return many;
    }
    match count % 10 {
        1 => one,
        2..=4 => few,
        _ => many,
    }
}

/// Модель сессии: последний `turn_context.model`.
pub fn extract_model(entries: &[Value]) -> Option<String> {
    entries.iter().rev().find_map(|e| {
        if e.get("type").and_then(Value::as_str) == Some("turn_context") {
            e.get("payload")
                .and_then(|p| p.get("model"))
                .and_then(Value::as_str)
                .map(String::from)
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
        let u = line(
            "response_item",
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"привет"}]}),
        );
        let a = line(
            "response_item",
            json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"здравствуй"}]}),
        );
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
        let dev = line(
            "response_item",
            json!({"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions> огромный системный промпт"}]}),
        );
        let em = line(
            "event_msg",
            json!({"type":"agent_message","message":"дубль"}),
        );
        let meta = line("session_meta", json!({"id":"x","cwd":"/tmp"}));
        assert!(to_chat_items(&dev).is_empty(), "developer-роль не в ленте");
        assert!(
            to_chat_items(&em).is_empty(),
            "event_msg — телеметрия, не в ленте"
        );
        assert!(to_chat_items(&meta).is_empty());
    }

    #[test]
    fn function_call_becomes_tool_chip() {
        let fc = line(
            "response_item",
            json!({
                "type":"function_call","name":"exec_command",
                "arguments":"{\"cmd\":\"sed -n '1,20p' SKILL.md\",\"workdir\":\"/x\"}","call_id":"c1"
            }),
        );
        let items = to_chat_items(&fc);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "tool");
        assert_eq!(items[0].text, "Bash · sed -n '1,20p' SKILL.md");
    }

    #[test]
    fn custom_apply_patch_becomes_edit_tool_chip() {
        let fc = line(
            "response_item",
            json!({
                "type":"custom_tool_call","name":"apply_patch",
                "input":"*** Begin Patch\n*** Update File: ui/renderer.js\n@@\n-old\n+new\n*** End Patch\n"
            }),
        );
        let items = to_chat_items(&fc);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "tool");
        assert_eq!(items[0].text, "Edit · ui/renderer.js");
    }

    #[test]
    fn update_plan_becomes_todo_tool_chip() {
        let fc = line(
            "response_item",
            json!({
                "type":"function_call","name":"update_plan",
                "arguments":"{\"plan\":[{\"step\":\"A\",\"status\":\"completed\"},{\"step\":\"B\",\"status\":\"in_progress\"}]}"
            }),
        );
        let items = to_chat_items(&fc);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "tool");
        assert_eq!(items[0].text, "TodoWrite · 2 задачи");
    }

    #[test]
    fn docs_tools_become_search_and_read_chips() {
        let resolve = line(
            "response_item",
            json!({
                "type":"function_call","name":"resolve_library_id",
                "arguments":"{\"libraryName\":\"tauri-plugin-global-shortcut\",\"query\":\"Shortcut API\"}"
            }),
        );
        let docs = line(
            "response_item",
            json!({
                "type":"function_call","name":"query_docs",
                "arguments":"{\"libraryId\":\"/tauri-apps/tauri-plugin-global-shortcut\",\"query\":\"Shortcut API\"}"
            }),
        );

        assert_eq!(
            to_chat_items(&resolve)[0].text,
            "Search · tauri-plugin-global-shortcut"
        );
        assert_eq!(
            to_chat_items(&docs)[0].text,
            "Read · /tauri-apps/tauri-plugin-global-shortcut"
        );
    }

    #[test]
    fn responses_api_fixture_keeps_one_markdown_final_and_compacts_runtime_activity() {
        let entries = vec![
            line(
                "response_item",
                json!({
                    "type":"message",
                    "role":"assistant",
                    "phase":"commentary",
                    "content":[{"type":"output_text","text":"Промежуточный прогресс"}]
                }),
            ),
            line(
                "response_item",
                json!({
                    "type":"custom_tool_call",
                    "name":"exec",
                    "input":"const secret = await tools.exec_command({\"cmd\":\"cargo test\"});"
                }),
            ),
            line(
                "response_item",
                json!({
                    "type":"custom_tool_call_output",
                    "call_id":"exec-1",
                    "output":"raw exec output must stay out of the answer"
                }),
            ),
            line(
                "response_item",
                json!({
                    "type":"function_call",
                    "name":"wait",
                    "arguments":"{\"cell_id\":\"84\",\"yield_time_ms\":10000}"
                }),
            ),
            line(
                "response_item",
                json!({
                    "type":"function_call_output",
                    "call_id":"wait-1",
                    "output":"raw wait output must stay out of the answer"
                }),
            ),
            line(
                "response_item",
                json!({
                    "type":"message",
                    "role":"assistant",
                    "phase":"final_answer",
                    "content":[
                        {"type":"output_text","text":"## Готово\n\n- [результат](https://example.com)"},
                        {"type":"output_text","text":"```text\nmarkdown сохранён\n```"}
                    ]
                }),
            ),
            line(
                "response_item",
                json!({
                    "type":"message",
                    "role":"assistant",
                    "phase":{"unexpected":"shape"},
                    "content":[{"type":"output_text","text":"malformed progress must stay out"}]
                }),
            ),
            line(
                "event_msg",
                json!({"type":"agent_message","phase":"final_answer","message":"дубль"}),
            ),
            line(
                "response_item",
                json!({"type":"future_runtime_event","text":"unknown must be ignored"}),
            ),
        ];

        let items = entries.iter().flat_map(to_chat_items).collect::<Vec<_>>();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].kind, "tool");
        assert_eq!(items[0].text, "Task");
        assert_eq!(items[1].kind, "tool");
        assert_eq!(items[1].text, "Wait");
        assert_eq!(items[2].kind, "text");
        assert_eq!(
            items[2].text,
            "## Готово\n\n- [результат](https://example.com)\n\n```text\nmarkdown сохранён\n```"
        );
        assert_eq!(
            full_final_reply(&entries).as_deref(),
            Some(
                "## Готово\n\n- [результат](https://example.com)\n\n```text\nmarkdown сохранён\n```"
            )
        );
    }

    #[test]
    fn extract_model_and_title_and_reply() {
        let entries = vec![
            line("session_meta", json!({"id":"x","cwd":"/x"})),
            line(
                "response_item",
                json!({"type":"message","role":"user","content":[{"type":"input_text","text":"сделай рефактор парсера"}]}),
            ),
            line("turn_context", json!({"model":"gpt-5.5","turn_id":"t1"})),
            line(
                "response_item",
                json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"готово"}]}),
            ),
            line(
                "turn_context",
                json!({"model":"gpt-5-codex","turn_id":"t2"}),
            ),
        ];
        assert_eq!(
            extract_model(&entries).as_deref(),
            Some("gpt-5-codex"),
            "последний turn_context.model"
        );
        assert_eq!(
            extract_title(&entries).as_deref(),
            Some("сделай рефактор парсера")
        );
        assert_eq!(full_final_reply(&entries).as_deref(), Some("готово"));
    }
}
