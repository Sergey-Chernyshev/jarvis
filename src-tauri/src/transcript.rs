//! Чтение транскриптов Claude Code (JSONL-логи сессий).
//!
//! Формат внутренний и дрейфует — парсим defensive: неизвестное поле → дефолт,
//! битая строка → скип, никогда не падаем. Старое не тянем: файлы бывают на
//! мегабайты, читаем только хвост.

use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::util::{basename, ellipsize, friendly_model, home_dir, now_ms, one_line};

/// Элемент ленты чата для панели: текст юзера/ассистента или чип тула.
#[derive(Debug, Clone, Serialize)]
pub struct ChatItem {
    pub role: &'static str, // 'user' | 'assistant'
    pub kind: &'static str, // 'text' | 'tool'
    pub text: String,
    pub ts: i64,
}

/// Хвост файла → массив распарсенных JSONL-строк.
pub fn read_recent_entries(file: &Path, max_bytes: u64) -> Vec<Value> {
    let mut out = Vec::new();
    let Ok(meta) = fs::metadata(file) else { return out };
    let size = meta.len();
    let start = size.saturating_sub(max_bytes);
    let Ok(mut f) = fs::File::open(file) else { return out };
    if f.seek(SeekFrom::Start(start)).is_err() {
        return out;
    }
    let mut buf = Vec::with_capacity((size - start) as usize);
    if f.read_to_end(&mut buf).is_err() {
        return out;
    }
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        // первая строка могла обрезаться посередине
        text = match text.find('\n') {
            Some(i) => text[i + 1..].to_string(),
            None => return out,
        };
    }
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            out.push(v);
        }
    }
    out
}

/// Лог — дерево (resume/форки): идём от последней user/assistant-записи вверх
/// по parentUuid и возвращаем живую ветку в хронологическом порядке.
pub fn chain_from_entries(entries: Vec<Value>) -> Vec<Value> {
    let mut by_uuid: HashMap<String, usize> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        if let Some(u) = e.get("uuid").and_then(Value::as_str) {
            by_uuid.insert(u.to_string(), i);
        }
    }
    let mut last: Option<usize> = None;
    for (i, e) in entries.iter().enumerate().rev() {
        let typ = e.get("type").and_then(Value::as_str).unwrap_or("");
        if e.get("uuid").and_then(Value::as_str).is_some() && (typ == "user" || typ == "assistant") {
            last = Some(i);
            break;
        }
    }
    let Some(mut cur) = last else { return Vec::new() };
    let mut chain_idx = Vec::new();
    let mut seen = HashSet::new();
    loop {
        let e = &entries[cur];
        let Some(uuid) = e.get("uuid").and_then(Value::as_str) else { break };
        if !seen.insert(uuid.to_string()) {
            break;
        }
        chain_idx.push(cur);
        match e
            .get("parentUuid")
            .and_then(Value::as_str)
            .and_then(|p| by_uuid.get(p))
        {
            Some(&next) => cur = next,
            None => break,
        }
    }
    chain_idx.reverse();
    let mut taken: Vec<Option<Value>> = entries.into_iter().map(Some).collect();
    chain_idx
        .into_iter()
        .filter_map(|i| taken[i].take())
        .collect()
}

/// Короткая подпись тул-вызова для чипа: `Bash · npm test`.
pub fn short_tool_label(name: &str, input: Option<&Value>) -> String {
    let mut detail = String::new();
    if let Some(Value::Object(input)) = input {
        for key in ["command", "file_path", "pattern", "url", "description"] {
            if let Some(v) = input.get(key).and_then(Value::as_str) {
                detail = if key == "file_path" { basename(v) } else { v.to_string() };
                break;
            }
        }
    }
    let detail = ellipsize(&one_line(&detail), 64);
    // mcp__plugin_playwright_playwright__browser_click → browser_click
    let name = if name.is_empty() { "tool" } else { name };
    let short = match name.strip_prefix("mcp__").and_then(|rest| rest.rfind("__").map(|i| &rest[i + 2..])) {
        Some(s) if !s.is_empty() => s,
        _ => name,
    };
    if detail.is_empty() {
        short.to_string()
    } else {
        format!("{short} · {detail}")
    }
}

/// Одна строка JSONL → 0..n элементов чата (юзер-текст, ассистент-текст, тул-чипы).
pub fn to_chat_items(entry: &Value) -> Vec<ChatItem> {
    let mut items = Vec::new();
    let Some(obj) = entry.as_object() else { return items };
    if obj.get("isSidechain").and_then(Value::as_bool).unwrap_or(false)
        || obj.get("isMeta").and_then(Value::as_bool).unwrap_or(false)
    {
        return items;
    }
    let Some(msg) = obj.get("message").and_then(Value::as_object) else { return items };
    let ts = obj
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_ts)
        .unwrap_or_else(now_ms);

    let push_text = |role: &'static str, text: &str, items: &mut Vec<ChatItem>| {
        let t = text.trim();
        // служебные вставки (<system-reminder>, <command-name>…) в чат не показываем
        if !t.is_empty() && !t.starts_with('<') {
            items.push(ChatItem { role, kind: "text", text: ellipsize(t, 4000), ts });
        }
    };

    match obj.get("type").and_then(Value::as_str) {
        Some("user") => match msg.get("content") {
            Some(Value::String(s)) => push_text("user", s, &mut items),
            Some(Value::Array(blocks)) => {
                for b in blocks {
                    if b.get("type").and_then(Value::as_str) == Some("text") {
                        push_text("user", b.get("text").and_then(Value::as_str).unwrap_or(""), &mut items);
                    }
                }
            }
            _ => {}
        },
        Some("assistant") => {
            if let Some(Value::Array(blocks)) = msg.get("content") {
                for b in blocks {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            push_text("assistant", b.get("text").and_then(Value::as_str).unwrap_or(""), &mut items)
                        }
                        Some("tool_use") => items.push(ChatItem {
                            role: "assistant",
                            kind: "tool",
                            text: short_tool_label(
                                b.get("name").and_then(Value::as_str).unwrap_or(""),
                                b.get("input"),
                            ),
                            ts,
                        }),
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }
    items
}

/// ISO-таймстамп → мс эпохи.
pub fn parse_ts(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp_millis())
}

/// Маркдаун → одна плотная строка для тоста: код-блоки вон, рез по предложению.
pub fn squeeze_reply(t: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;
    static FENCE: OnceLock<Regex> = OnceLock::new();
    static CODE: OnceLock<Regex> = OnceLock::new();
    static BOLD: OnceLock<Regex> = OnceLock::new();
    static LEAD: OnceLock<Regex> = OnceLock::new();
    static DECOR: OnceLock<Regex> = OnceLock::new();
    let fence = FENCE.get_or_init(|| Regex::new(r"(?s)```.*?```").unwrap());
    let code = CODE.get_or_init(|| Regex::new(r"`([^`]*)`").unwrap());
    let bold = BOLD.get_or_init(|| Regex::new(r"\*\*([^*]*)\*\*").unwrap());
    let lead = LEAD.get_or_init(|| Regex::new(r"(?m)^[#>\-•*\s]+").unwrap());
    let decor = DECOR.get_or_init(|| Regex::new(r"[★─━]+").unwrap());

    let x = fence.replace_all(t, " ");
    let x = code.replace_all(&x, "$1");
    let x = bold.replace_all(&x, "$1");
    let x = lead.replace_all(&x, " ");
    let x = decor.replace_all(&x, " ");
    let x = one_line(&x);
    let chars: Vec<char> = x.chars().collect();
    if chars.len() <= 220 {
        return x;
    }
    let cut: String = chars[..220].iter().collect();
    // рез по концу предложения, если он не слишком рано
    let dot = [". ", "! ", "? "]
        .iter()
        .filter_map(|p| cut.rfind(p).map(|b| cut[..b].chars().count()))
        .max();
    if let Some(d) = dot {
        if d > 90 {
            let upto: String = chars[..=d].iter().collect();
            return upto;
        }
    }
    let sp = cut.rfind(' ').map(|b| cut[..b].chars().count()).unwrap_or(0);
    let end = if sp > 150 { sp } else { 220 };
    format!("{}…", chars[..end].iter().collect::<String>())
}

/// Полный финальный ответ агента: все текст-блоки после последнего промпта юзера.
pub fn full_final_reply(transcript: &str) -> Option<String> {
    let entries = read_recent_entries(Path::new(transcript), 256 * 1024);
    let items: Vec<ChatItem> = chain_from_entries(entries)
        .iter()
        .flat_map(to_chat_items)
        .filter(|i| i.kind == "text")
        .collect();
    let last_user = items.iter().rposition(|i| i.role == "user");
    let reply = items
        .into_iter()
        .skip(last_user.map(|i| i + 1).unwrap_or(0))
        .filter(|i| i.role == "assistant")
        .map(|i| i.text)
        .collect::<Vec<_>>()
        .join("\n");
    let reply = reply.trim();
    if reply.is_empty() {
        None
    } else {
        Some(ellipsize(reply, 6000))
    }
}

/// Claude Code кодирует cwd в имя каталога проекта, заменяя / и . на -
pub fn project_dir_for(cwd: &str) -> PathBuf {
    let encoded: String = cwd
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    home_dir().join(".claude").join("projects").join(encoded)
}

/// transcript_path из хука бывает форкнут (диалог уезжает в новый файл) —
/// читаем модель из самого свежего транскрипта в каталоге проекта.
pub fn read_model_from_project(cwd: &str) -> Option<String> {
    let dir = project_dir_for(cwd);
    let mut files: Vec<(PathBuf, std::time::SystemTime)> = fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|p| {
            let m = fs::metadata(&p).ok()?.modified().ok()?;
            Some((p, m))
        })
        .collect();
    files.sort_by(|a, b| b.1.cmp(&a.1));
    for (p, _) in files.into_iter().take(4) {
        let entries = read_recent_entries(&p, 64 * 1024);
        for e in entries.iter().rev() {
            if e.get("type").and_then(Value::as_str) == Some("assistant") {
                if let Some(m) = e.pointer("/message/model").and_then(Value::as_str) {
                    return Some(friendly_model(m));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_label_strips_mcp_prefix_and_takes_detail() {
        let input = json!({"command": "npm test"});
        assert_eq!(short_tool_label("Bash", Some(&input)), "Bash · npm test");
        assert_eq!(
            short_tool_label("mcp__plugin_playwright_playwright__browser_click", None),
            "browser_click"
        );
        let input = json!({"file_path": "/a/b/c.rs"});
        assert_eq!(short_tool_label("Edit", Some(&input)), "Edit · c.rs");
    }

    #[test]
    fn chat_items_skip_service_and_meta() {
        let user = json!({
            "type": "user", "uuid": "u1", "timestamp": "2026-06-12T10:00:00Z",
            "message": {"content": "привет"}
        });
        assert_eq!(to_chat_items(&user).len(), 1);
        let service = json!({
            "type": "user", "uuid": "u2",
            "message": {"content": "<system-reminder>x</system-reminder>"}
        });
        assert!(to_chat_items(&service).is_empty());
        let meta = json!({"type": "user", "isMeta": true, "message": {"content": "x"}});
        assert!(to_chat_items(&meta).is_empty());
    }

    #[test]
    fn chain_walks_parent_uuid() {
        let entries = vec![
            json!({"type":"user","uuid":"a","message":{"content":"1"}}),
            json!({"type":"assistant","uuid":"b","parentUuid":"a","message":{"content":[{"type":"text","text":"2"}]}}),
            // форк-ветка, не связанная с последней записью
            json!({"type":"user","uuid":"x","message":{"content":"dead"}}),
            json!({"type":"user","uuid":"c","parentUuid":"b","message":{"content":"3"}}),
        ];
        let chain = chain_from_entries(entries);
        let uuids: Vec<&str> = chain.iter().map(|e| e["uuid"].as_str().unwrap()).collect();
        assert_eq!(uuids, vec!["a", "b", "c"]);
    }

    #[test]
    fn squeeze_strips_markdown() {
        let s = squeeze_reply("Готово. **Важно**: `cargo test` прошёл.\n```rust\nfn main(){}\n```\n- пункт");
        assert!(!s.contains("**") && !s.contains("```") && !s.contains('`'), "{s}");
        assert!(s.contains("cargo test"));
    }

    #[test]
    fn project_dir_encodes_cwd() {
        let p = project_dir_for("/Users/x/my.app");
        assert!(p.to_string_lossy().ends_with("/.claude/projects/-Users-x-my-app"));
    }
}
