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

/// Кусок сообщения с позиции `from` (в СИМВОЛАХ) длиной не больше `max_chars`
/// (0 — без потолка) → (кусок, всего символов, смещение продолжения).
///
/// Символы, а не байты: смещение уезжает наружу курсором, и на кириллице
/// байтовое пришлось бы объяснять.
pub fn slice_chars(text: &str, from: usize, max_chars: usize) -> (String, usize, Option<usize>) {
    let chars: Vec<char> = text.chars().collect();
    let total = chars.len();
    let from = from.min(total);
    let end = if max_chars == 0 { total } else { from.saturating_add(max_chars).min(total) };
    let piece: String = chars[from..end].iter().collect();
    (piece, total, (end < total).then_some(end))
}

/// Усечение с ЯВНОЙ пометкой. Молчаливый обрыв хуже пустого ответа: пустой
/// виден сразу, а обрезанный выглядит целым — читатель делает вывод по половине
/// отчёта и докладывает с полной уверенностью.
pub fn clip_marked(s: &str, max_chars: usize) -> String {
    let (head, total, next) = slice_chars(s, 0, max_chars);
    match next {
        None => head,
        Some(n) => format!("{head}\n\n[…обрезано {} симв. из {total}; целиком — chats.read]", total - n),
    }
}

/// Отпечаток текста для курсора «дочитать»: позиция сообщения в ленте съезжает
/// (лог дописывается, окно чтения едет), и без отпечатка курсор молча отдал бы
/// хвост ЧУЖОГО сообщения. FNV-1a: хеш здесь не криптография, а сверка.
pub fn text_fingerprint(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Курсор продолжения: `<индекс>@<смещение в символах>#<отпечаток>`.
pub fn make_cursor(idx: usize, from: usize, text: &str) -> String {
    format!("{idx}@{from}#{}", text_fingerprint(text))
}

/// Разбор курсора → (индекс, смещение, отпечаток). None — мусор.
pub fn parse_cursor(s: &str) -> Option<(usize, usize, String)> {
    let (pos, fp) = s.split_once('#')?;
    let (idx, from) = pos.split_once('@')?;
    Some((idx.parse().ok()?, from.parse().ok()?, fp.to_string()))
}

/// Хвост файла → массив распарсенных JSONL-строк.
pub fn read_recent_entries(file: &Path, max_bytes: u64) -> Vec<Value> {
    read_recent_text(file, max_bytes).map_or_else(Vec::new, |t| entries_from_text(&t))
}

/// Хвост файла целыми строками. Отдельно от разбора: тот же текст у удалённой
/// сессии приезжает по HTTP, и дальше по коду разница исчезает.
pub fn read_recent_text(file: &Path, max_bytes: u64) -> Option<String> {
    let size = fs::metadata(file).ok()?.len();
    let start = size.saturating_sub(max_bytes);
    let mut f = fs::File::open(file).ok()?;
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity((size - start) as usize);
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Some(whole_lines(&text, start > 0).to_string())
}

/// Разбор JSONL в записи. Отдельно от чтения файла: транскрипт удалённой
/// сессии приезжает по HTTP — файла на этой машине нет, а разбирать его надо
/// ровно так же.
pub fn entries_from_text(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
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

/// API failures are assistant messages too, but their synthetic model is not
/// the model selected by the user. Keep looking for the last real response.
pub fn extract_claude_model(entries: &[Value]) -> Option<String> {
    entries.iter().rev().find_map(|entry| {
        if entry["type"] != "assistant"
            || entry["isApiErrorMessage"] == true
            || entry["is_error"] == true
            || entry.pointer("/message/is_error") == Some(&Value::Bool(true))
            || entry.pointer("/message/type").and_then(Value::as_str) == Some("error")
        {
            return None;
        }
        let model = entry.pointer("/message/model")?.as_str()?;
        if model.is_empty() || model.len() > 256
            || !model.as_bytes()[0].is_ascii_alphanumeric()
            || !model.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-._:/[]".contains(&byte))
            || model.eq_ignore_ascii_case("synthetic")
        {
            return None;
        }
        let lower = model.to_ascii_lowercase();
        // Friendly labels for Claude's own IDs; custom provider/model IDs
        // retain their complete name instead of being cut at the first '-'.
        let known = ["opus", "sonnet", "haiku", "fable", "mythos"];
        if known.iter().any(|family| lower == *family || lower.starts_with(&format!("claude-{family}-"))) {
            Some(friendly_model(model))
        } else {
            Some(model.to_string())
        }
    })
}

/// Отрезать оборванную первую строку, если чтение началось не с начала файла.
pub fn whole_lines(text: &str, mid_file: bool) -> &str {
    if !mid_file {
        return text;
    }
    match text.find('\n') {
        Some(i) => &text[i + 1..],
        None => "", // кусок целиком внутри одной строки — целых записей нет
    }
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
        if e.get("uuid").and_then(Value::as_str).is_some() && (typ == "user" || typ == "assistant")
        {
            last = Some(i);
            break;
        }
    }
    let Some(mut cur) = last else {
        return Vec::new();
    };
    let mut chain_idx = Vec::new();
    let mut seen = HashSet::new();
    loop {
        let e = &entries[cur];
        let Some(uuid) = e.get("uuid").and_then(Value::as_str) else {
            break;
        };
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
                detail = if key == "file_path" {
                    basename(v)
                } else {
                    v.to_string()
                };
                break;
            }
        }
    }
    let detail = ellipsize(&one_line(&detail), 64);
    // mcp__plugin_playwright_playwright__browser_click → browser_click
    let name = if name.is_empty() { "tool" } else { name };
    let short = match name
        .strip_prefix("mcp__")
        .and_then(|rest| rest.rfind("__").map(|i| &rest[i + 2..]))
    {
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
    let Some(obj) = entry.as_object() else {
        return items;
    };
    if obj
        .get("isSidechain")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || obj.get("isMeta").and_then(Value::as_bool).unwrap_or(false)
    {
        return items;
    }
    let Some(msg) = obj.get("message").and_then(Value::as_object) else {
        return items;
    };
    let ts = obj
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_ts)
        .unwrap_or_else(now_ms);

    let push_text = |role: &'static str, text: &str, items: &mut Vec<ChatItem>| {
        let t = if role == "user" {
            crate::backend::codex_transcript::normalize_user_text(text)
        } else {
            text.trim().to_owned()
        };
        if !t.is_empty() {
            items.push(ChatItem { role, kind: "text", text: t, ts });
        }
    };

    match obj.get("type").and_then(Value::as_str) {
        Some("user") => match msg.get("content") {
            Some(Value::String(s)) => push_text("user", s, &mut items),
            Some(Value::Array(blocks)) => {
                for b in blocks {
                    if b.get("type").and_then(Value::as_str) == Some("text") {
                        push_text(
                            "user",
                            b.get("text").and_then(Value::as_str).unwrap_or(""),
                            &mut items,
                        );
                    }
                }
            }
            _ => {}
        },
        Some("assistant") => {
            if let Some(Value::Array(blocks)) = msg.get("content") {
                for b in blocks {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => push_text(
                            "assistant",
                            b.get("text").and_then(Value::as_str).unwrap_or(""),
                            &mut items,
                        ),
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
    let sp = cut
        .rfind(' ')
        .map(|b| cut[..b].chars().count())
        .unwrap_or(0);
    let end = if sp > 150 { sp } else { 220 };
    format!("{}…", chars[..end].iter().collect::<String>())
}

/// Полный финальный ответ агента: все текст-блоки после последнего промпта юзера.
pub fn full_final_reply(transcript: &str) -> Option<String> {
    let entries = read_recent_entries(Path::new(transcript), 256 * 1024);
    final_reply_from(chain_from_entries(entries))
}

/// То же над готовой цепочкой: у удалённой сессии записи приезжают по HTTP.
pub fn final_reply_from(chain: Vec<Value>) -> Option<String> {
    let items: Vec<ChatItem> = chain
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
        Some(clip_marked(reply, 6000))
    }
}

/// Claude Code кодирует cwd в имя каталога проекта, заменяя / и . на -
///
/// Кодирует он РАЗРЕШЁННЫЙ путь. Для чата с агентом это решает всё: хост
/// работает из `std::env::temp_dir()`, а это `/var/folders/…` — симлинк на
/// `/private/var/folders/…`. Без резолва мы искали каталог, которого нет, и
/// говорили «транскрипт не найден» при живом файле на диске.
pub fn project_dir_for(cwd: &str) -> PathBuf {
    let root = home_dir().join(".claude").join("projects");
    let encode = |p: &str| -> String {
        p.chars().map(|c| if c == '/' || c == '.' { '-' } else { c }).collect()
    };
    // Резолв — только подсказка: у несуществующего пути её нет, и тогда
    // кодируем как дали (поведение для обычных сессий не меняется).
    if let Some(real) = std::fs::canonicalize(cwd).ok().and_then(|p| p.to_str().map(String::from)) {
        if real != cwd {
            let dir = root.join(encode(&real));
            if dir.is_dir() {
                return dir;
            }
        }
    }
    root.join(encode(cwd))
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
        if let Some(model) = extract_claude_model(&entries) { return Some(model); }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_model_ignores_api_errors_and_keeps_the_last_real_response() {
        let real = json!({"type":"assistant","message":{"model":"claude-opus-4-6"}});
        for failed in [
            json!({"type":"assistant","isApiErrorMessage":true,"message":{"model":"<synthetic>"}}),
            json!({"type":"assistant","isApiErrorMessage":true,"message":{"model":"claude-sonnet-4-6"}}),
            json!({"type":"assistant","is_error":true,"message":{"model":"claude-sonnet-4-6"}}),
            json!({"type":"assistant","message":{"model":"claude-sonnet-4-6","is_error":true}}),
            json!({"type":"assistant","message":{"type":"error","model":"claude-sonnet-4-6"}}),
            json!({"type":"user","message":{"model":"claude-sonnet-4-6"}}),
        ] {
            assert_eq!(extract_claude_model(&[real.clone(), failed.clone()]).as_deref(), Some("Opus"));
            assert_eq!(extract_claude_model(&[failed]), None);
        }
        let newer = json!({"type":"assistant","message":{"model":"claude-sonnet-4-6"}});
        assert_eq!(extract_claude_model(&[real, newer]).as_deref(), Some("Sonnet"));
    }

    #[test]
    fn claude_model_rejects_technical_values_without_truncating_custom_ids() {
        for model in ["<synthetic>", "synthetic", "<internal>", "", " model ", "model\nother", "--model"] {
            assert_eq!(extract_claude_model(&[json!({"type":"assistant","message":{"model":model}})]), None, "{model}");
        }
        for model in ["provider/my-model-v2:fast", "custom-sonnet-compatible", "us.anthropic.claude-opus-4-6-v1"] {
            assert_eq!(extract_claude_model(&[json!({"type":"assistant","message":{"model":model}})]).as_deref(), Some(model));
        }
        assert_eq!(extract_claude_model(&[json!({"type":"assistant","message":{"model":"claude-opus-4-6[1m]"}})]).as_deref(), Some("Opus"));
    }

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
    fn claude_preserves_user_xml_and_strips_only_known_generated_prefixes() {
        for role in ["user", "assistant"] {
            let entry = json!({"type":role,"message":{"content":[{"type":"text","text":"<div>Actual user code</div>"}]}});
            assert_eq!(to_chat_items(&entry)[0].text, "<div>Actual user code</div>");
        }
        let mixed = json!({"type":"user","message":{"content":"<system-reminder>context</system-reminder>\nFix this actual task"}});
        assert_eq!(to_chat_items(&mixed)[0].text, "Fix this actual task");
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

    /// Финальный ответ длиннее потолка обязан НАЗВАТЬ обрыв: молча обрезанный
    /// выглядит целым, и половина отчёта уходит человеку как весь отчёт.
    #[test]
    fn long_reply_says_that_it_was_cut() {
        let chain = vec![
            json!({"type":"user","uuid":"a","message":{"content":"давай"}}),
            json!({"type":"assistant","uuid":"b","parentUuid":"a","message":{"content":[
                {"type":"text","text": "я".repeat(6500)}
            ]}}),
        ];
        let r = final_reply_from(chain).unwrap();
        assert!(r.contains("обрезано 500 симв. из 6500"), "{}", &r[r.len() - 80..]);
        // короткий ответ пометки не получает
        assert_eq!(clip_marked("готово", 6000), "готово");
        // курсор переживает круг: индекс, смещение и отпечаток на месте
        let c = make_cursor(7, 4000, "текст");
        assert_eq!(parse_cursor(&c), Some((7, 4000, text_fingerprint("текст"))));
        assert_eq!(parse_cursor("мусор"), None);
    }

    #[test]
    fn squeeze_strips_markdown() {
        let s = squeeze_reply(
            "Готово. **Важно**: `cargo test` прошёл.\n```rust\nfn main(){}\n```\n- пункт",
        );
        assert!(
            !s.contains("**") && !s.contains("```") && !s.contains('`'),
            "{s}"
        );
        assert!(s.contains("cargo test"));
    }

    #[test]
    fn project_dir_encodes_cwd() {
        let p = project_dir_for("/Users/x/my.app");
        assert!(p
            .to_string_lossy()
            .ends_with("/.claude/projects/-Users-x-my-app"));
    }

    #[test]
    fn project_dir_follows_symlink_when_claude_named_it_by_real_path() {
        // Ровно случай чата с агентом: хост работает из temp_dir(), а это симлинк.
        // Claude назвал каталог по разрешённому пути — искать надо там.
        let base = std::env::temp_dir().join("jarvis-projdir-test");
        let _ = std::fs::remove_dir_all(&base);
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let enc = |p: &std::path::Path| -> String {
            p.to_string_lossy().chars().map(|c| if c == '/' || c == '.' { '-' } else { c }).collect()
        };
        let projects = crate::util::home_dir().join(".claude").join("projects");
        let canon = std::fs::canonicalize(&real).unwrap();
        let named = projects.join(enc(&canon));
        let existed = named.is_dir();
        std::fs::create_dir_all(&named).unwrap();

        let got = project_dir_for(link.to_str().unwrap());
        assert_eq!(got, named, "нашли каталог по разрешённому пути, а не по симлинку");

        if !existed {
            let _ = std::fs::remove_dir(&named);
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
