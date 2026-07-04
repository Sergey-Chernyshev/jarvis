//! Сегментация транскрипта на «ходы» (юзер-текст → всё до следующего юзер-текста)
//! и детерминированные факты хода (файлы, команды) для карточек сводки.
//! Принцип extract-then-abstract: пути/команды достаёт ЭТОТ код, LLM только
//! аннотирует — см. docs/superpowers/specs/2026-07-03-chat-turn-summaries-design.md.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::backend::{Agent, Backend};
use crate::transcript::ChatItem;
use crate::util::{ellipsize, one_line};

/// Диапазон одного хода в плоском списке элементов чата.
/// `key` — ts юзер-реплики (мс, строкой); "pre" — частичный головной ход,
/// у которого юзер-реплика обрезана окном чтения (такой не суммаризируем).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSpan {
    pub key: String,
    pub start: usize,
    pub end: usize, // эксклюзивно
    pub complete: bool,
}

/// Границы ходов по плоскому списку элементов: граница — юзер-текст
/// (tool_result-записи Claude в ленту не попадают — см. to_chat_items).
pub fn spans(items: &[ChatItem]) -> Vec<TurnSpan> {
    let mut out: Vec<TurnSpan> = Vec::new();
    for (i, it) in items.iter().enumerate() {
        if it.role == "user" && it.kind == "text" {
            if let Some(last) = out.last_mut() {
                last.end = i;
            }
            out.push(TurnSpan { key: it.ts.to_string(), start: i, end: i, complete: true });
        } else if out.is_empty() {
            out.push(TurnSpan { key: "pre".into(), start: 0, end: 0, complete: false });
        }
    }
    if let Some(last) = out.last_mut() {
        last.end = items.len();
    }
    out
}

/// Файл, тронутый агентом за ход. kind: "created" | "edited".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileTouch {
    pub path: String,
    pub kind: String,
}

/// Голова записанного .md — вход для дайджеста доки.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MdHead {
    pub path: String,
    pub head: String,
}

/// Детерминированные факты хода — единственный источник путей/команд для LLM.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnFacts {
    pub files: Vec<FileTouch>,
    pub commands: Vec<String>,
    pub final_reply: String,
    pub md_heads: Vec<MdHead>,
    pub tool_log: Vec<String>,
}

/// Ход целиком: диапазон + промпт юзера + факты.
#[derive(Debug, Clone)]
pub struct Turn {
    pub span: TurnSpan,
    pub user_prompt: String,
    pub facts: TurnFacts,
}

/// Транскрипт → (плоская лента, ходы с фактами). Дороже `spans` (ходит по
/// сырым записям) — зовётся только при генерации сводок, не на каждый рендер.
pub fn segment(be: &dyn Backend, entries: &[Value]) -> (Vec<ChatItem>, Vec<Turn>) {
    let mut items: Vec<ChatItem> = Vec::new();
    let mut entry_first_item = Vec::with_capacity(entries.len());
    for e in entries {
        entry_first_item.push(items.len());
        items.extend(be.to_chat_items(e));
    }
    let mut turns: Vec<Turn> = spans(&items)
        .into_iter()
        .map(|span| Turn {
            user_prompt: items
                .get(span.start)
                .filter(|it| it.role == "user" && it.kind == "text")
                .map(|it| ellipsize(&one_line(&it.text), 500))
                .unwrap_or_default(),
            span,
            facts: TurnFacts::default(),
        })
        .collect();
    // факты — по сырым записям, в ход, которому принадлежит первый item записи
    for (ei, e) in entries.iter().enumerate() {
        let idx = entry_first_item[ei];
        let Some(t) = turns.iter_mut().find(|t| t.span.start <= idx && idx < t.span.end) else {
            continue; // запись без items в самом конце — фактов не даёт
        };
        collect_facts(be.agent(), e, &mut t.facts);
    }
    for t in &mut turns {
        let slice = &items[t.span.start..t.span.end];
        t.facts.final_reply = ellipsize(
            &slice
                .iter()
                .filter(|i| i.role == "assistant" && i.kind == "text")
                .map(|i| i.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            6000,
        );
        t.facts.tool_log = slice
            .iter()
            .filter(|i| i.kind == "tool")
            .take(60)
            .map(|i| i.text.clone())
            .collect();
    }
    (items, turns)
}

fn collect_facts(agent: Agent, entry: &Value, f: &mut TurnFacts) {
    match agent {
        Agent::Claude => facts_claude(entry, f),
        Agent::Codex => facts_codex(entry, f),
    }
}

fn facts_claude(entry: &Value, f: &mut TurnFacts) {
    if entry.get("type").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let Some(Value::Array(blocks)) = entry.pointer("/message/content") else { return };
    for b in blocks {
        if b.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let name = b.get("name").and_then(Value::as_str).unwrap_or("");
        let input = b.get("input");
        match name {
            "Edit" | "MultiEdit" | "NotebookEdit" | "Write" => {
                let Some(p) = input.and_then(|i| i.get("file_path")).and_then(Value::as_str) else {
                    continue;
                };
                if name == "Write" && p.ends_with(".md") {
                    if let Some(c) = input.and_then(|i| i.get("content")).and_then(Value::as_str) {
                        f.md_heads.push(MdHead { path: p.to_string(), head: ellipsize(c, 2000) });
                    }
                }
                push_file(f, p, if name == "Write" { "created" } else { "edited" });
            }
            "Bash" => {
                if let Some(c) = input.and_then(|i| i.get("command")).and_then(Value::as_str) {
                    f.commands.push(ellipsize(&one_line(c), 200));
                }
            }
            _ => {}
        }
    }
}

fn facts_codex(entry: &Value, f: &mut TurnFacts) {
    if entry.get("type").and_then(Value::as_str) != Some("response_item") {
        return;
    }
    let Some(p) = entry.get("payload") else { return };
    match p.get("type").and_then(Value::as_str) {
        Some("function_call") => {
            if p.get("name").and_then(Value::as_str) != Some("exec_command") {
                return;
            }
            let args = p
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|s| serde_json::from_str::<Value>(s).ok());
            if let Some(c) = args
                .as_ref()
                .and_then(|a| a.get("cmd").or_else(|| a.get("command")))
                .and_then(Value::as_str)
            {
                f.commands.push(ellipsize(&one_line(c), 200));
            }
        }
        Some("custom_tool_call") if p.get("name").and_then(Value::as_str) == Some("apply_patch") => {
            let Some(input) = p.get("input").and_then(Value::as_str) else { return };
            for (path, kind) in patch_files(input) {
                if kind == "created" && path.ends_with(".md") {
                    f.md_heads.push(MdHead { path: path.clone(), head: ellipsize(input, 2000) });
                }
                push_file(f, &path, kind);
            }
        }
        _ => {}
    }
}

/// Все файлы из apply_patch (`*** Update/Add File:`). Delete пропускаем —
/// открывать нечего.
fn patch_files(patch: &str) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    for line in patch.lines() {
        for (prefix, kind) in [("*** Update File: ", "edited"), ("*** Add File: ", "created")] {
            if let Some(p) = line.strip_prefix(prefix) {
                let p = p.trim();
                if !p.is_empty() {
                    out.push((p.to_string(), kind));
                }
            }
        }
    }
    out
}

fn push_file(f: &mut TurnFacts, path: &str, kind: &str) {
    if let Some(existing) = f.files.iter_mut().find(|x| x.path == path) {
        if kind == "created" {
            existing.kind = "created".into();
        }
        return;
    }
    f.files.push(FileTouch { path: path.to_string(), kind: kind.to_string() });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{backend, Agent};
    use serde_json::json;

    fn it(role: &'static str, kind: &'static str, text: &str, ts: i64) -> ChatItem {
        ChatItem { role, kind, text: text.into(), ts }
    }

    #[test]
    fn spans_split_on_user_text() {
        let items = vec![
            it("user", "text", "сделай A", 100),
            it("assistant", "tool", "Edit · a.rs", 101),
            it("assistant", "text", "готово A", 102),
            it("user", "text", "теперь B", 200),
            it("assistant", "text", "готово B", 201),
        ];
        let s = spans(&items);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].key.as_str(), s[0].start, s[0].end, s[0].complete), ("100", 0, 3, true));
        assert_eq!((s[1].key.as_str(), s[1].start, s[1].end, s[1].complete), ("200", 3, 5, true));
    }

    #[test]
    fn spans_head_without_user_is_partial() {
        let items = vec![
            it("assistant", "tool", "Bash · ls", 50),
            it("assistant", "text", "хвост прошлого хода", 51),
            it("user", "text", "новый ход", 100),
            it("assistant", "text", "ок", 101),
        ];
        let s = spans(&items);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].key.as_str(), s[0].start, s[0].end, s[0].complete), ("pre", 0, 2, false));
        assert!(s[1].complete);
    }

    #[test]
    fn spans_empty_input() {
        assert!(spans(&[]).is_empty());
    }

    #[test]
    fn spans_consecutive_user_texts_each_get_own_span() {
        let items = vec![it("user", "text", "раз", 1), it("user", "text", "два", 2)];
        let s = spans(&items);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].start, s[0].end), (0, 1));
        assert_eq!((s[1].start, s[1].end), (1, 2));
        assert!(s[0].complete && s[1].complete);
    }

    #[test]
    fn spans_trailing_turn_without_reply_is_complete() {
        let items = vec![
            it("user", "text", "вопрос", 1),
            it("assistant", "text", "ответ", 2),
            it("user", "text", "ещё вопрос без ответа", 3),
        ];
        let s = spans(&items);
        assert_eq!(s.len(), 2);
        assert_eq!((s[1].start, s[1].end, s[1].complete), (2, 3, true));
    }

    #[test]
    fn segment_claude_extracts_files_commands_reply() {
        let entries = vec![
            json!({"type":"user","uuid":"u1","timestamp":"2026-07-04T10:00:00Z",
                   "message":{"content":"добавь ретраи и прогони тесты"}}),
            json!({"type":"assistant","uuid":"a1","parentUuid":"u1","timestamp":"2026-07-04T10:00:05Z",
                   "message":{"content":[
                       {"type":"tool_use","name":"Edit","input":{"file_path":"src/install/mod.rs"}},
                       {"type":"tool_use","name":"Write","input":{"file_path":"docs/retry.md","content":"# Ретраи\nдизайн"}},
                       {"type":"tool_use","name":"Bash","input":{"command":"cargo test"}},
                       {"type":"text","text":"Готово: ретраи добавлены."}]}}),
        ];
        let be = backend(Agent::Claude);
        let (items, turns) = segment(be, &entries);
        assert_eq!(turns.len(), 1);
        let t = &turns[0];
        assert!(t.span.complete);
        assert_eq!(t.user_prompt, "добавь ретраи и прогони тесты");
        assert_eq!(
            t.facts.files,
            vec![
                FileTouch { path: "src/install/mod.rs".into(), kind: "edited".into() },
                FileTouch { path: "docs/retry.md".into(), kind: "created".into() },
            ]
        );
        assert_eq!(t.facts.commands, vec!["cargo test".to_string()]);
        assert_eq!(t.facts.final_reply, "Готово: ретраи добавлены.");
        assert_eq!(t.facts.md_heads.len(), 1);
        assert_eq!(t.facts.md_heads[0].path, "docs/retry.md");
        // хроника тулов — из чипов ленты
        assert_eq!(t.facts.tool_log.len(), 3);
        assert!(items.len() >= 5); // юзер + 3 чипа + текст
    }

    #[test]
    fn segment_codex_extracts_patch_and_command() {
        let entries = vec![
            json!({"timestamp":"2026-07-04T10:00:00Z","type":"response_item","payload":
                {"type":"message","role":"user","content":[{"type":"input_text","text":"поправь рендерер"}]}}),
            json!({"timestamp":"2026-07-04T10:00:05Z","type":"response_item","payload":
                {"type":"custom_tool_call","name":"apply_patch",
                 "input":"*** Begin Patch\n*** Update File: ui/renderer.js\n@@\n-a\n+b\n*** Add File: docs/new.md\n+# Дока\n*** End Patch\n"}}),
            json!({"timestamp":"2026-07-04T10:00:06Z","type":"response_item","payload":
                {"type":"function_call","name":"exec_command",
                 "arguments":"{\"cmd\":\"npm test\"}","call_id":"c1"}}),
            json!({"timestamp":"2026-07-04T10:00:09Z","type":"response_item","payload":
                {"type":"message","role":"assistant","content":[{"type":"output_text","text":"сделал"}]}}),
        ];
        let be = backend(Agent::Codex);
        let (_items, turns) = segment(be, &entries);
        assert_eq!(turns.len(), 1);
        let f = &turns[0].facts;
        assert_eq!(
            f.files,
            vec![
                FileTouch { path: "ui/renderer.js".into(), kind: "edited".into() },
                FileTouch { path: "docs/new.md".into(), kind: "created".into() },
            ]
        );
        assert_eq!(f.commands, vec!["npm test".to_string()]);
        assert_eq!(f.final_reply, "сделал");
    }

    #[test]
    fn push_file_dedups_and_upgrades_to_created() {
        let mut f = TurnFacts::default();
        push_file(&mut f, "a.rs", "edited");
        push_file(&mut f, "a.rs", "created");
        push_file(&mut f, "a.rs", "edited");
        assert_eq!(f.files, vec![FileTouch { path: "a.rs".into(), kind: "created".into() }]);
    }
}
