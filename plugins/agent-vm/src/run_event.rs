use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const MAX_EVENT_TEXT_CHARS: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Claude,
    Codex,
}

impl Backend {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            _ => Err("agent должен быть claude или codex".into()),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum BackendEvent {
    Session {
        id: String,
        model: Option<String>,
    },
    AssistantDelta {
        text: String,
    },
    AssistantMessage {
        text: String,
    },
    ToolStarted {
        id: String,
        name: String,
        detail: Option<String>,
    },
    ToolCompleted {
        id: String,
        is_error: bool,
        detail: Option<String>,
    },
    FileChanged {
        guest_path: String,
        change: String,
    },
    Question {
        id: String,
        payload: Value,
    },
    Usage {
        payload: Value,
    },
    Result {
        text: String,
        is_error: bool,
        session_id: Option<String>,
    },
    TurnCompleted,
    Failure {
        message: String,
    },
    Unmapped {
        upstream_type: String,
        keys: Vec<String>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunEvent {
    pub run_id: String,
    pub turn_id: String,
    pub seq: u64,
    pub at: i64,
    #[serde(rename = "type")]
    pub event_type: String,
    pub payload: Value,
    pub backend: Backend,
    pub vm: String,
}

pub fn parse_backend_line(backend: Backend, line: &str) -> Vec<BackendEvent> {
    let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
        return Vec::new();
    };
    match backend {
        Backend::Claude => parse_claude(&value),
        Backend::Codex => parse_codex(&value),
    }
}

fn parse_claude(value: &Value) -> Vec<BackendEvent> {
    let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    match event_type {
        "system" if value.get("subtype").and_then(Value::as_str) == Some("init") => {
            let id = text_field(value, "session_id");
            if id.is_empty() {
                Vec::new()
            } else {
                vec![BackendEvent::Session {
                    id,
                    model: optional_text_field(value, "model"),
                }]
            }
        }
        "stream_event" => parse_claude_stream_event(value.get("event").unwrap_or(&Value::Null)),
        "assistant" => parse_claude_assistant(value),
        "user" => parse_claude_tool_results(value),
        "result" => {
            let mut events = Vec::new();
            if let Some(usage) = value.get("usage").and_then(Value::as_object) {
                events.push(BackendEvent::Usage {
                    payload: normalize_usage(usage),
                });
            }
            events.push(BackendEvent::Result {
                text: bounded_text(value.get("result").and_then(Value::as_str).unwrap_or("")),
                is_error: value
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    || !matches!(
                        value.get("subtype").and_then(Value::as_str),
                        None | Some("success")
                    ),
                session_id: optional_text_field(value, "session_id"),
            });
            events
        }
        "" => Vec::new(),
        other => vec![unmapped(other, value)],
    }
}

fn parse_claude_stream_event(value: &Value) -> Vec<BackendEvent> {
    match value.get("type").and_then(Value::as_str).unwrap_or("") {
        "content_block_delta" => {
            let delta = value.get("delta").unwrap_or(&Value::Null);
            match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                "text_delta" => optional_text_field(delta, "text")
                    .map(|text| BackendEvent::AssistantDelta { text })
                    .into_iter()
                    .collect(),
                _ => Vec::new(),
            }
        }
        "message_delta" => value
            .get("usage")
            .and_then(Value::as_object)
            .map(|usage| {
                vec![BackendEvent::Usage {
                    payload: normalize_usage(usage),
                }]
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn parse_claude_assistant(value: &Value) -> Vec<BackendEvent> {
    let Some(blocks) = value
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut events = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => {
                if let Some(text) = optional_text_field(block, "text") {
                    events.push(BackendEvent::AssistantMessage { text });
                }
            }
            "tool_use" => {
                let id = text_field(block, "id");
                let name = text_field(block, "name");
                let input = block.get("input").unwrap_or(&Value::Null);
                if name == "AskUserQuestion" {
                    events.push(BackendEvent::Question {
                        id,
                        payload: bounded_json(input),
                    });
                    continue;
                }
                events.push(BackendEvent::ToolStarted {
                    id,
                    name: name.clone(),
                    detail: tool_detail(&name, input),
                });
                if let Some(path) = tool_file_path(&name, input) {
                    events.push(BackendEvent::FileChanged {
                        guest_path: path,
                        change: if name == "Write" {
                            "created".into()
                        } else {
                            "modified".into()
                        },
                    });
                }
            }
            _ => {}
        }
    }
    events
}

fn parse_claude_tool_results(value: &Value) -> Vec<BackendEvent> {
    let Some(blocks) = value
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .map(|block| BackendEvent::ToolCompleted {
            id: text_field(block, "tool_use_id"),
            is_error: block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            detail: content_preview(block.get("content")),
        })
        .collect()
}

fn parse_codex(value: &Value) -> Vec<BackendEvent> {
    let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    match event_type {
        "thread.started" => {
            let id = text_field(value, "thread_id");
            if id.is_empty() {
                Vec::new()
            } else {
                vec![BackendEvent::Session { id, model: None }]
            }
        }
        "turn.started" => Vec::new(),
        "item.started" | "item.updated" | "item.completed" => {
            parse_codex_item(event_type, value.get("item").unwrap_or(&Value::Null))
        }
        "turn.completed" => {
            let mut events = Vec::new();
            if let Some(usage) = value.get("usage").and_then(Value::as_object) {
                events.push(BackendEvent::Usage {
                    payload: normalize_usage(usage),
                });
            }
            events.push(BackendEvent::TurnCompleted);
            events
        }
        "turn.failed" | "error" => vec![BackendEvent::Failure {
            message: error_preview(value),
        }],
        "" => Vec::new(),
        other => vec![unmapped(other, value)],
    }
}

fn parse_codex_item(event_type: &str, item: &Value) -> Vec<BackendEvent> {
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
    let id = text_field(item, "id");
    match item_type {
        "agent_message" if event_type == "item.completed" => optional_text_field(item, "text")
            .map(|text| vec![BackendEvent::AssistantMessage { text }])
            .unwrap_or_default(),
        "command_execution" => {
            if event_type == "item.started" {
                vec![BackendEvent::ToolStarted {
                    id,
                    name: "command".into(),
                    detail: optional_text_field(item, "command"),
                }]
            } else if event_type == "item.completed" {
                vec![BackendEvent::ToolCompleted {
                    id,
                    is_error: item.get("exit_code").and_then(Value::as_i64).unwrap_or(0) != 0
                        || item.get("status").and_then(Value::as_str) == Some("failed"),
                    detail: content_preview(item.get("aggregated_output")),
                }]
            } else {
                Vec::new()
            }
        }
        "file_change" if event_type == "item.completed" => item
            .get("changes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|change| {
                let guest_path = change.get("path").and_then(Value::as_str)?;
                let change = match change.get("kind").and_then(Value::as_str).unwrap_or("") {
                    "add" | "create" => "created",
                    "delete" | "remove" => "deleted",
                    _ => "modified",
                };
                Some(BackendEvent::FileChanged {
                    guest_path: bounded_text(guest_path),
                    change: change.into(),
                })
            })
            .collect(),
        "mcp_tool_call" | "web_search" => {
            let name = item
                .get("tool")
                .or_else(|| item.get("query"))
                .and_then(Value::as_str)
                .unwrap_or(item_type);
            if event_type == "item.started" {
                vec![BackendEvent::ToolStarted {
                    id,
                    name: bounded_text(name),
                    detail: None,
                }]
            } else if event_type == "item.completed" {
                vec![BackendEvent::ToolCompleted {
                    id,
                    is_error: item.get("status").and_then(Value::as_str) == Some("failed"),
                    detail: None,
                }]
            } else {
                Vec::new()
            }
        }
        "request_user_input" if event_type == "item.completed" => {
            vec![BackendEvent::Question {
                id,
                payload: bounded_json(item),
            }]
        }
        "reasoning" | "todo_list" => Vec::new(),
        "" => Vec::new(),
        other => vec![BackendEvent::Unmapped {
            upstream_type: format!("item.{other}"),
            keys: object_keys(item),
        }],
    }
}

fn tool_file_path(name: &str, input: &Value) -> Option<String> {
    if !matches!(name, "Edit" | "Write" | "NotebookEdit" | "MultiEdit") {
        return None;
    }
    ["file_path", "path", "notebook_path"]
        .iter()
        .find_map(|key| input.get(key).and_then(Value::as_str))
        .map(bounded_text)
}

fn tool_detail(name: &str, input: &Value) -> Option<String> {
    match name {
        "Bash" => input
            .get("command")
            .and_then(Value::as_str)
            .map(bounded_text),
        "Edit" | "Write" | "NotebookEdit" | "MultiEdit" | "Read" => {
            ["file_path", "path", "notebook_path"]
                .iter()
                .find_map(|key| input.get(key).and_then(Value::as_str))
                .map(bounded_text)
        }
        _ => None,
    }
}

fn normalize_usage(usage: &serde_json::Map<String, Value>) -> Value {
    let number = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| usage.get(*name).and_then(Value::as_u64))
            .unwrap_or(0)
    };
    json!({
        "inputTokens": number(&["input_tokens", "inputTokens"]),
        "cachedInputTokens": number(&["cache_read_input_tokens", "cached_input_tokens", "cachedInputTokens"]),
        "outputTokens": number(&["output_tokens", "outputTokens"])
    })
}

fn error_preview(value: &Value) -> String {
    value
        .get("error")
        .and_then(|error| {
            error
                .as_str()
                .or_else(|| error.get("message").and_then(Value::as_str))
        })
        .or_else(|| value.get("message").and_then(Value::as_str))
        .map(bounded_text)
        .unwrap_or_else(|| "backend turn failed".into())
}

fn content_preview(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(bounded_text(text)),
        Value::Array(items) => items
            .iter()
            .find_map(|item| item.get("text").and_then(Value::as_str).map(bounded_text)),
        _ => None,
    }
}

fn bounded_json(value: &Value) -> Value {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        Value::String(text) => Value::String(bounded_text(text)),
        Value::Array(items) => Value::Array(items.iter().take(32).map(bounded_json).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .take(64)
                .filter(|(key, _)| !sensitive_key(key))
                .map(|(key, value)| (key.clone(), bounded_json(value)))
                .collect(),
        ),
    }
}

fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    ["authorization", "credential", "password", "secret", "token"]
        .iter()
        .any(|marker| key.contains(marker))
}

fn bounded_text(value: &str) -> String {
    value
        .replace('\0', "")
        .chars()
        .take(MAX_EVENT_TEXT_CHARS)
        .collect()
}

fn text_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(bounded_text)
        .unwrap_or_default()
}

fn optional_text_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(bounded_text)
}

fn object_keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|fields| fields.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

fn unmapped(upstream_type: &str, value: &Value) -> BackendEvent {
    BackendEvent::Unmapped {
        upstream_type: bounded_text(upstream_type),
        keys: object_keys(value),
    }
}

pub fn map_guest_path(
    guest_workspace: &Path,
    host_workspace: &Path,
    path: &Path,
) -> Result<PathBuf, String> {
    if !guest_workspace.is_absolute() || !host_workspace.is_absolute() {
        return Err("workspace mapping требует absolute roots".into());
    }
    let relative = if path.is_absolute() {
        path.strip_prefix(guest_workspace)
            .map_err(|_| "guest path находится вне project mount".to_string())?
    } else {
        path
    };
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err("guest path содержит unsafe components".into());
    }
    Ok(host_workspace.join(relative))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;

    #[test]
    fn claude_stream_maps_session_delta_tools_files_usage_and_result() {
        let init = parse_backend_line(
            Backend::Claude,
            r#"{"type":"system","subtype":"init","session_id":"018f0000-0000-7000-8000-000000000001","model":"synthetic","tools":["Read","Edit"]}"#,
        );
        assert_eq!(
            init,
            vec![BackendEvent::Session {
                id: "018f0000-0000-7000-8000-000000000001".into(),
                model: Some("synthetic".into()),
            }]
        );

        let delta = parse_backend_line(
            Backend::Claude,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Готовлю"}}}"#,
        );
        assert_eq!(
            delta,
            vec![BackendEvent::AssistantDelta {
                text: "Готовлю".into()
            }]
        );

        let tool = parse_backend_line(
            Backend::Claude,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tool-1","name":"Edit","input":{"file_path":"/home/dev/project/src/lib.rs"}}]}}"#,
        );
        assert_eq!(tool.len(), 2);
        assert!(matches!(
            &tool[0],
            BackendEvent::ToolStarted { id, name, .. } if id == "tool-1" && name == "Edit"
        ));
        assert_eq!(
            tool[1],
            BackendEvent::FileChanged {
                guest_path: "/home/dev/project/src/lib.rs".into(),
                change: "modified".into(),
            }
        );

        let result = parse_backend_line(
            Backend::Claude,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"Готово","session_id":"018f0000-0000-7000-8000-000000000001","total_cost_usd":0.01,"usage":{"input_tokens":10,"output_tokens":4}}"#,
        );
        assert!(matches!(
            &result[0],
            BackendEvent::Usage { payload } if payload["inputTokens"] == json!(10)
        ));
        assert!(matches!(
            &result[1],
            BackendEvent::Result { text, is_error, .. } if text == "Готово" && !is_error
        ));
    }

    #[test]
    fn codex_jsonl_maps_thread_messages_commands_files_and_turn_completion() {
        assert_eq!(
            parse_backend_line(
                Backend::Codex,
                r#"{"type":"thread.started","thread_id":"019f0000-0000-7000-8000-000000000002"}"#,
            ),
            vec![BackendEvent::Session {
                id: "019f0000-0000-7000-8000-000000000002".into(),
                model: None,
            }]
        );
        assert!(matches!(
            parse_backend_line(
                Backend::Codex,
                r#"{"type":"item.started","item":{"id":"cmd-1","type":"command_execution","command":"/usr/bin/git status","status":"in_progress"}}"#,
            )
            .as_slice(),
            [BackendEvent::ToolStarted { id, name, .. }] if id == "cmd-1" && name == "command"
        ));
        let file = parse_backend_line(
            Backend::Codex,
            r#"{"type":"item.completed","item":{"id":"patch-1","type":"file_change","changes":[{"path":"src/main.rs","kind":"update"},{"path":"README.md","kind":"add"}],"status":"completed"}}"#,
        );
        assert!(matches!(
            &file[0],
            BackendEvent::FileChanged { guest_path, change } if guest_path == "src/main.rs" && change == "modified"
        ));
        assert!(matches!(
            &file[1],
            BackendEvent::FileChanged { guest_path, change } if guest_path == "README.md" && change == "created"
        ));
        assert!(matches!(
            parse_backend_line(
                Backend::Codex,
                r#"{"type":"item.completed","item":{"id":"msg-1","type":"agent_message","text":"Сделано"}}"#,
            )
            .as_slice(),
            [BackendEvent::AssistantMessage { text }] if text == "Сделано"
        ));
        assert!(matches!(
            parse_backend_line(
                Backend::Codex,
                r#"{"type":"turn.completed","usage":{"input_tokens":12,"cached_input_tokens":3,"output_tokens":7}}"#,
            )
            .as_slice(),
            [BackendEvent::Usage { payload }, BackendEvent::TurnCompleted]
                if payload["outputTokens"] == json!(7)
        ));
    }

    #[test]
    fn unknown_backend_payload_keeps_shape_only_and_never_raw_values() {
        let events = parse_backend_line(
            Backend::Codex,
            r#"{"type":"future.private","credential":"SYNTHETIC_PRIVATE_VALUE","nested":{"token":"hidden"}}"#,
        );
        assert_eq!(
            events,
            vec![BackendEvent::Unmapped {
                upstream_type: "future.private".into(),
                keys: vec!["credential".into(), "nested".into(), "type".into()],
            }]
        );
        assert!(!format!("{events:?}").contains("SYNTHETIC_PRIVATE_VALUE"));
    }

    #[test]
    fn guest_file_mapping_is_confined_to_the_project_mount() {
        let guest = Path::new("/home/dev/project");
        let host = Path::new("/host/project");

        assert_eq!(
            map_guest_path(guest, host, Path::new("/home/dev/project/src/lib.rs")).unwrap(),
            Path::new("/host/project/src/lib.rs")
        );
        assert_eq!(
            map_guest_path(guest, host, Path::new("docs/spec.md")).unwrap(),
            Path::new("/host/project/docs/spec.md")
        );
        assert!(map_guest_path(guest, host, Path::new("../outside")).is_err());
        assert!(map_guest_path(guest, host, Path::new("/etc/passwd")).is_err());
    }
}
