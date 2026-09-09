//! Парсер rollout-транскрипта Codex → общий `ChatItem`.
//!
//! Формат: `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`, каждая строка
//! `{timestamp, type, payload}`. type ∈ session_meta | turn_context | response_item
//! | event_msg. Каноничная переписка — в `response_item` (event_msg дублирует её
//! как телеметрию, его пропускаем). Defensive: неизвестное → пропуск, не паникуем.

use serde_json::Value;
use std::io::Read;
use std::path::Path;

use crate::transcript::{parse_ts, ChatItem};
use crate::util::{ellipsize, now_ms, one_line};

/// Strip only recognized leading provider context blocks. Code fences, quoted
/// snippets, ordinary XML/HTML and text discussing these tags stay untouched.
/// Cleaning precedes truncation: a long context prefix must not hide the task.
pub fn normalize_user_text(text: &str) -> String {
    let mut rest = text.trim();
    loop {
        if rest.starts_with("# AGENTS.md instructions") {
            let body = rest
                .split_once('\n')
                .map(|(_, body)| body.trim_start())
                .unwrap_or("");
            if body.starts_with("<INSTRUCTIONS>") {
                rest = body
                    .find("</INSTRUCTIONS>")
                    .map(|at| body[at + "</INSTRUCTIONS>".len()..].trim_start())
                    .unwrap_or("");
                continue;
            }
        }
        let tag = [
            "environment_context",
            "recommended_plugins",
            "permissions instructions",
            "user_instructions",
            "system-reminder",
            "task-notification",
            "developer_instructions",
            "app-context",
            "skills_instructions",
            "collaboration_mode",
            "local-command-stdout",
            "local-command-caveat",
            "in-app-browser-context",
        ]
        .into_iter()
        .find(|tag| {
            rest.strip_prefix(&format!("<{tag}"))
                .and_then(|tail| tail.chars().next())
                .is_some_and(|ch| ch == '>' || ch.is_ascii_whitespace())
        });
        if let Some(tag) = tag {
            let closing = format!("</{tag}>");
            rest = rest
                .find(&closing)
                .map(|at| rest[at + closing.len()..].trim_start())
                .unwrap_or("");
            continue;
        }
        if rest.starts_with(">>> APPROVAL REQUEST START") {
            let end = ">>> APPROVAL REQUEST END";
            rest = rest
                .find(end)
                .map(|at| rest[at + end.len()..].trim_start())
                .unwrap_or("");
            continue;
        }
        if rest == ">>> APPROVAL REQUEST END" {
            return String::new();
        }
        break;
    }
    rest.trim().to_owned()
}

/// Attachment declarations remain readable in the chat. Only a display title
/// skips their known leading heading/list to reach an accompanying user request.
pub fn title_text(text: &str) -> String {
    let clean = normalize_user_text(text);
    let Some((heading, body)) = clean.split_once('\n') else {
        return clean;
    };
    if !matches!(
        heading.trim(),
        "# Files mentioned by the user" | "# Files mentioned by the user:"
    ) {
        return clean;
    }
    let mut at = heading.len() + 1;
    let mut markdown_attachment = false;
    for line in body.split_inclusive('\n') {
        if matches!(
            line.trim(),
            "## My request:" | "# My request:" | "## User request:" | "# User request:"
        ) {
            return normalize_user_text(&clean[at + line.len()..]);
        }
        markdown_attachment |=
            line.starts_with("## ") && (line.contains(": /") || line.contains(": file://"));
        at += line.len();
    }
    // This attachment format includes generated instructions between the file
    // headings and the explicit request heading. An incomplete prefix is not a task.
    if markdown_attachment {
        return String::new();
    }
    let mut saw_file = false;
    let mut offset = heading.len() + 1;
    for line in body.split_inclusive('\n') {
        let line_text = line.trim();
        if line_text.is_empty() {
            offset += line.len();
            continue;
        }
        if !saw_file && !line_text.starts_with(['-', '*', '[']) {
            return clean;
        }
        if line_text.starts_with(['-', '*', '['])
            && (line_text.contains("](") || line_text.starts_with("- /"))
        {
            saw_file = true;
            offset += line.len();
            continue;
        }
        let rest = clean[offset..].trim();
        let rest = rest
            .strip_prefix("# User request:")
            .or_else(|| rest.strip_prefix("# User request"))
            .unwrap_or(rest)
            .trim();
        return normalize_user_text(rest);
    }
    if saw_file {
        String::new()
    } else {
        clean
    }
}

pub fn needs_title_repair(title: &str) -> bool {
    let title = title.trim();
    title.is_empty()
        || normalize_user_text(title) != title
        || title.starts_with("# AGENTS.md instructions for /")
        || title.starts_with("# Files mentioned by the user:")
        || (title.starts_with("# AGENTS.md instructions") && title.contains("<INSTRUCTIONS>"))
}

/// Use provider metadata, never a message mentioning review/risk/approval.
pub fn is_technical_model(model: &str) -> bool {
    model.eq_ignore_ascii_case("codex-auto-review")
}

pub fn is_technical_session_entry(entry: &Value) -> bool {
    let payload = &entry["payload"];
    match entry["type"].as_str() {
        Some("session_meta") => {
            payload["thread_source"].as_str() == Some("guardian_review")
                || payload
                    .pointer("/source/subagent/other")
                    .and_then(Value::as_str)
                    == Some("guardian")
                || payload["model"].as_str().is_some_and(is_technical_model)
        }
        Some("turn_context") => payload["model"].as_str().is_some_and(is_technical_model),
        _ => false,
    }
}

/// Full/remote batches share visibility rules. Scoped parent context is not a
/// child conversation, while genuine thread_spawn sessions remain visible.
pub fn display_entries(entries: Vec<Value>) -> Vec<Value> {
    let mut scope = crate::rollout_scope::RolloutScope::default();
    let mut visible = Vec::new();
    for entry in entries {
        if !scope.accept(&entry) {
            continue;
        }
        if is_technical_session_entry(&entry) {
            return Vec::new();
        }
        visible.push(entry);
    }
    visible
}

/// A recent tail may omit owner metadata. Probe a bounded prefix before local
/// display; the same helper works for every configured provider home.
pub fn file_is_technical(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut bytes = Vec::new();
    if file.take(1024 * 1024).read_to_end(&mut bytes).is_err() {
        return false;
    }
    let mut scope = crate::rollout_scope::RolloutScope::default();
    for line in bytes.split(|byte| *byte == b'\n') {
        match serde_json::from_slice::<Value>(line) {
            Ok(entry) if scope.accept(&entry) => {
                if is_technical_session_entry(&entry) {
                    return true;
                }
            }
            Ok(_) => {}
            Err(_) => scope.skip_record(),
        }
    }
    false
}

/// Provider message phases are normalized before they enter the shared renderer.
/// Old transcripts without a phase remain ordinary messages; new/unknown phases
/// stay in the raw transcript and cannot replace a known answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MessagePhase { Answer, Progress, Diagnostic }

impl MessagePhase {
    fn from_payload(payload: &Value) -> Self {
        match payload.get("phase").or_else(|| payload.get("channel")).and_then(Value::as_str) {
            None | Some("final" | "final_answer") => Self::Answer,
            Some("commentary" | "progress" | "insight") => Self::Progress,
            Some(_) => Self::Diagnostic,
        }
    }
}

/// Одна строка rollout → элементы чата (обычно 0–1). Пропускаем developer/system
/// (системный промпт), reasoning, function_call_output, session_meta, turn_context,
/// event_msg (дубль).
pub fn to_chat_items(entry: &Value) -> Vec<ChatItem> {
    if entry.get("type").and_then(Value::as_str) != Some("response_item") {
        return vec![];
    }
    let Some(payload) = entry.get("payload") else {
        return vec![];
    };
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
            let kind = if role == "assistant" {
                match MessagePhase::from_payload(payload) {
                    MessagePhase::Answer => "text",
                    MessagePhase::Progress => "progress",
                    MessagePhase::Diagnostic => return vec![],
                }
            } else { "text" };
            let Some(blocks) = payload.get("content").and_then(Value::as_array) else {
                return vec![];
            };
            let mut out = Vec::new();
            for b in blocks {
                let bt = b.get("type").and_then(Value::as_str).unwrap_or("");
                if !matches!(bt, "input_text" | "output_text" | "text") {
                    continue;
                }
                let raw = b.get("text").and_then(Value::as_str).unwrap_or("");
                let text = if role == "user" {
                    normalize_user_text(raw)
                } else {
                    raw.to_owned()
                };
                if text.is_empty() {
                    continue;
                }
                out.push(ChatItem {
                    role,
                    kind,
                    text,
                    ts,
                });
            }
            out
        }
        Some("function_call") => {
            let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
            let args = payload
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|s| serde_json::from_str::<Value>(s).ok());
            vec![ChatItem {
                role: "assistant",
                kind: "tool",
                text: tool_label(name, args.as_ref()),
                ts,
            }]
        }
        Some("custom_tool_call") => {
            // apply_patch и т.п.
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("инструмент");
            let input = payload.get("input").and_then(Value::as_str);
            vec![ChatItem {
                role: "assistant",
                kind: "tool",
                text: custom_tool_label(name, input),
                ts,
            }]
        }
        _ => vec![], // reasoning, function_call_output, ...
    }
}

/// Чип инструмента в том же формате, что у Claude: «Tool · аргумент».
fn tool_label(name: &str, args: Option<&Value>) -> String {
    match name {
        "exec_command" => label_with_hint("Bash", command_hint(args)),
        "write_stdin" => label_with_hint("Bash", Some("stdin".into())),
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
    let mut scope = crate::rollout_scope::RolloutScope::default();
    if entries
        .iter()
        .any(|entry| scope.accept(entry) && is_technical_session_entry(entry))
    {
        return None;
    }
    for e in entries {
        for item in to_chat_items(e) {
            if item.role == "user" && item.kind == "text" {
                let t = ellipsize(&one_line(&title_text(&item.text)), 60);
                if !t.is_empty() {
                    return Some(t);
                }
            }
        }
    }
    None
}

/// Финальный ответ ассистента текущего хода. (Демон
/// предпочитает `last_assistant_message` из Stop-хука — rollout может быть не
/// сфлашен; это фолбэк.)
pub fn full_final_reply(entries: &[Value]) -> Option<String> {
    let mut last: Option<String> = None;
    let mut turn_id: Option<&str> = None;
    for e in entries {
        let typ = e.get("type").and_then(Value::as_str);
        let p = &e["payload"];
        if typ == Some("turn_context") {
            if let Some(id) = p.get("turn_id").and_then(Value::as_str) {
                if turn_id != Some(id) {
                    last = None;
                    turn_id = Some(id);
                }
            }
            continue;
        }
        let items = to_chat_items(e);
        if items
            .iter()
            .any(|item| item.role == "user" && item.kind == "text")
        {
            last = None;
            continue;
        }
        // Новые rollout'ы маркируют commentary/final_answer. Промежуточный
        // прогресс не должен озвучиваться как итог, особенно после Stop.
        if MessagePhase::from_payload(p) != MessagePhase::Answer {
            continue;
        }
        let text = items
            .into_iter()
            .filter(|item| {
                item.role == "assistant" && item.kind == "text" && !item.text.trim().is_empty()
            })
            .map(|item| item.text)
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            last = Some(text);
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambient_browser_attributes_are_known_context_but_similar_xml_is_not() {
        let text="<in-app-browser-context source=\"ambient-ui-state\">provider context</in-app-browser-context>\nFix the tab";
        assert_eq!(normalize_user_text(text), "Fix the tab");
        for original in [
            "<in-app-browser-contextual>user XML</in-app-browser-contextual>",
            "```xml\n<in-app-browser-context source=\"x\">sample</in-app-browser-context>\n```",
        ] {
            assert_eq!(normalize_user_text(original), original);
        }
    }

    #[test]
    fn attachment_declarations_stay_in_chat_but_title_uses_actual_request() {
        let text="# Files mentioned by the user:\n\n- [report.pdf](/tmp/report.pdf)\n\nSummarize this report";
        assert_eq!(normalize_user_text(text), text);
        assert_eq!(title_text(text), "Summarize this report");
        assert!(
            title_text("# Files mentioned by the user:\n\n- [report.pdf](/tmp/report.pdf)")
                .is_empty()
        );
        assert_eq!(
            title_text("# Files mentioned by the user:\nExplain this heading"),
            "# Files mentioned by the user:\nExplain this heading"
        );
        let native="# Files mentioned by the user:\n\n## codex-clipboard-file.png: /var/tmp/image.png\n\nDistinguish instructions from content.\n\n## My request:\nИсправь список";
        assert_eq!(title_text(native), "Исправь список");
        assert_eq!(normalize_user_text(native), native);
        assert!(title_text("# Files mentioned by the user:\n\n## codex-clipboard-file.png: /var/tmp/image.png\n\nDistinguish instructions...").is_empty());
    }
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
        assert_eq!(
            full_final_reply(&entries),
            None,
            "новый ход ещё не дал финал"
        );
    }

    #[test]
    fn mixed_runtime_events_keep_progress_and_tools_out_of_final_answer() {
        let entries = vec![
            json!({"type":"response_item","payload":{"type":"function_call","name":"exec","arguments":"{}"}}),
            json!({"type":"response_item","payload":{"type":"function_call","name":"wait","arguments":"{}"}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","output":"raw tool result"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"Проверяю сборку"}]}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"**Готово** [файл](https://example.com/file)"}]}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","phase":"unknown_future_event","content":[{"type":"output_text","text":"internal state"}]}}),
            json!({"type":"unknown_event","payload":{"text":"unknown runtime data"}}),
        ];
        let items: Vec<_> = entries.iter().flat_map(to_chat_items).collect();
        assert_eq!(items.iter().map(|item| item.kind).collect::<Vec<_>>(), vec!["tool", "tool", "progress", "text"]);
        assert_eq!(items.last().unwrap().text, "**Готово** [файл](https://example.com/file)");
        assert_eq!(full_final_reply(&entries).as_deref(), Some("**Готово** [файл](https://example.com/file)"));
    }

    #[test]
    fn final_reply_ignores_commentary_and_joins_all_text_blocks() {
        let mut entries = vec![
            line("turn_context", json!({"turn_id":"t1"})),
            line(
                "response_item",
                json!({"type":"message","role":"assistant","phase":"final_answer",
                "content":[{"type":"output_text","text":"Первое"},{"type":"output_text","text":"Второе"}]}),
            ),
            line(
                "response_item",
                json!({"type":"message","role":"assistant","phase":"commentary",
                "content":[{"type":"output_text","text":"Позднее служебное сообщение"}]}),
            ),
        ];
        assert_eq!(
            full_final_reply(&entries).as_deref(),
            Some("Первое\nВторое")
        );
        entries.push(line(
            "turn_context",
            json!({"turn_id":"t1","model":"other"}),
        ));
        assert!(
            full_final_reply(&entries).is_some(),
            "повторный context того же хода не сбрасывает ответ"
        );
        entries.push(line("turn_context", json!({"turn_id":"t2"})));
        assert_eq!(full_final_reply(&entries), None);
    }

    #[test]
    fn new_user_prompt_cannot_reuse_previous_reply() {
        let entries = vec![
            line(
                "response_item",
                json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"старый итог"}]}),
            ),
            line(
                "response_item",
                json!({"type":"message","role":"user","content":[{"type":"input_text","text":"ещё задача"}]}),
            ),
            line(
                "response_item",
                json!({"type":"message","role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"начинаю"}]}),
            ),
        ];
        assert_eq!(full_final_reply(&entries), None);
    }

    #[test]
    fn html_text_is_visible_but_known_context_is_hidden() {
        for role in ["assistant", "user"] {
            let e = line(
                "response_item",
                json!({"type":"message","role":role,"content":[{"type":"output_text","text":"<div>Привет</div>"}]}),
            );
            assert_eq!(to_chat_items(&e)[0].text, "<div>Привет</div>");
        }
        let context = line(
            "response_item",
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>cwd</environment_context>"}]}),
        );
        assert!(to_chat_items(&context).is_empty());
    }

    #[test]
    fn generated_prefixes_are_removed_before_the_actual_request_and_title() {
        let text=format!("<recommended_plugins>{}</recommended_plugins>\n<environment_context>cwd</environment_context>\n# AGENTS.md instructions for /repo\n\n<INSTRUCTIONS>settings</INSTRUCTIONS>\nИсправь поиск и проверь тесты","x".repeat(2400));
        let entry = line(
            "response_item",
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":text}]}),
        );
        assert_eq!(
            to_chat_items(&entry)[0].text,
            "Исправь поиск и проверь тесты"
        );
        assert_eq!(
            extract_title(&[entry]).as_deref(),
            Some("Исправь поиск и проверь тесты")
        );
        assert!(normalize_user_text("<recommended_plugins>truncated context").is_empty());
        assert!(needs_title_repair(
            "# AGENTS.md instructions for /repo <INSTRUCTIONS>context"
        ));
    }

    #[test]
    fn user_code_xml_and_security_json_remain_visible() {
        for text in [
            "<root><item>user XML</item></root>",
            "```xml\n<environment_context>example</environment_context>\n```",
            "Explain <recommended_plugins> in this example",
            r#"{"risk_level":"low","user_authorization":"explicit","outcome":"allow","rationale":"example"}"#,
        ] {
            assert_eq!(normalize_user_text(text), text);
            for role in ["user", "assistant"] {
                let entry = line(
                    "response_item",
                    json!({"type":"message","role":role,"content":[{"type":"text","text":text}]}),
                );
                assert_eq!(to_chat_items(&entry)[0].text, text);
            }
        }
    }

    #[test]
    fn guardian_metadata_hides_the_batch_but_genuine_subagents_keep_own_messages() {
        let guardian = line(
            "session_meta",
            json!({"id":"review","parent_thread_id":"parent","thread_source":"guardian_review","source":{"subagent":{"other":"guardian"}}}),
        );
        let approval = line(
            "response_item",
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":">>> APPROVAL REQUEST START"},{"type":"input_text","text":"tool payload"},{"type":"input_text","text":">>> APPROVAL REQUEST END"}]}),
        );
        let decision = line(
            "response_item",
            json!({"type":"message","role":"assistant","content":[{"type":"text","text":"{\"risk_level\":\"low\",\"outcome\":\"allow\"}"}]}),
        );
        assert!(display_entries(vec![guardian.clone(), approval, decision]).is_empty());
        assert!(is_technical_model("codex-auto-review"));
        assert!(!is_technical_model("gpt-6-astra"));
        let child = line(
            "session_meta",
            json!({"id":"child","parent_thread_id":"parent","subagent_history_start_ordinal":4,"source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent","agent_path":"/root/real_task"}}}}),
        );
        let model = line("turn_context", json!({"model":"codex-auto-review"}));
        let inherited = line(
            "response_item",
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"inherited task"}]}),
        );
        let own = line(
            "response_item",
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"Implement the actual child task"}]}),
        );
        assert!(!is_technical_session_entry(&child));
        let visible = display_entries(vec![child, guardian, model, inherited, own]);
        assert_eq!(visible.len(), 2);
        assert_eq!(
            extract_title(&visible).as_deref(),
            Some("Implement the actual child task")
        );
    }

    #[test]
    fn approval_protocol_markers_are_not_human_titles() {
        assert!(
            normalize_user_text(">>> APPROVAL REQUEST START\n{}\n>>> APPROVAL REQUEST END")
                .is_empty()
        );
        assert!(normalize_user_text(">>> APPROVAL REQUEST END").is_empty());
        assert_eq!(
            normalize_user_text("Explain this approval request: >>> APPROVAL REQUEST END"),
            "Explain this approval request: >>> APPROVAL REQUEST END"
        );
        let model = line("turn_context", json!({"model":"codex-auto-review"}));
        assert!(display_entries(vec![model]).is_empty());
    }
}
