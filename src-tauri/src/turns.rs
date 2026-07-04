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
    // Факты — по сырым записям, в ход, которому принадлежит первый item записи.
    // Инвариант: каждая запись, из которой collect_facts достаёт факты
    // (assistant с tool_use у Claude; function_call/custom_tool_call у Codex),
    // ОБЯЗАНА давать хотя бы один item в to_chat_items — иначе idx укажет на
    // первый item СЛЕДУЮЩЕЙ записи и факты уедут в чужой ход. Сегодня это так:
    // tool_use → чип "tool"; менять to_chat_items — только вместе с этим циклом.
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

/// Версия промпта/схемы — растёт при любом изменении PROMPT_HEAD/бюджетов,
/// инвалидирует кэш сводок (turnsum.rs).
pub const PROMPT_VERSION: u32 = 1;

/// Шапка: правила + схема + few-shot (по ресёрчу: пример держит язык и форму
/// JSON лучше инструкций; префилл `{` через CLI недоступен — компенсируем).
const PROMPT_HEAD: &str = r#"Ты суммаризируешь один ход кодинг-агента для ленты чата. Отвечай СТРОГО одним JSON-объектом, без markdown и текста вокруг.
Правила:
- Пиши по-русски. Пути файлов, команды, имена функций/тестов, флаги — оставляй как есть на английском, НЕ переводи и НЕ транслитерируй.
- Используй ТОЛЬКО факты из блока FACTS и текста хода. Не выдумывай файлы, команды или результаты, которых там нет.
- files: ровно те пути, что даны в FACTS.files (копируй посимвольно); note — одна фраза до 60 символов, что изменилось.
- summary: 2–5 предложений, что сделано и итог.
- docs_digest: если агент выдал доку/отчёт/длинные выводы — сжатый пересказ в 3–6 пунктов, числа/имена/пути дословно; иначе пустая строка.
- commands: итог команд/тестов одной строкой; не было — пустая строка.
Схема: {"summary": string, "files": [{"path": string, "note": string}], "docs_digest": string, "commands": string}

Пример.
FACTS:
files:
  ui/settings2.js (edited)
commands:
  npm test
---
ХОД:
Пользователь: почини сохранение хоткеев и прогони тесты
Агент: Исправил сериализацию биндингов в settings2.js — раньше терялся сентинел "none". Тесты зелёные: 281 passed.

Ответ:
{"summary": "Починено сохранение хоткеев: при сериализации биндингов терялось состояние «не назначен» (сентинел none). Тесты прогнаны, все зелёные.", "files": [{"path": "ui/settings2.js", "note": "исправлена сериализация биндингов"}], "docs_digest": "", "commands": "npm test — 281 passed"}

Теперь реальный ход.
"#;

/// «Голова+хвост»: длинный текст режем с серединой-заглушкой (середина наименее
/// информативна — lost in the middle). Лимиты в символах (chars, не bytes).
/// Precondition: head <= max и tail <= max (иначе выход длиннее max и возможен
/// выход за границы) — проверяется debug_assert.
pub fn head_tail(s: &str, max: usize, head: usize, tail: usize) -> String {
    debug_assert!(head <= max && tail <= max, "head_tail: head/tail не должны превышать max");
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let h: String = chars[..head].iter().collect();
    let t: String = chars[chars.len() - tail..].iter().collect();
    format!("{h}\n[…]\n{t}")
}

/// Промпт сводки хода. Бюджеты из спеки: FACTS ~1К, юзер 0.5К (уже порезан в
/// segment), финальный ответ 4К (голова+хвост), хроника тулов 1.5К, дока 2К.
pub fn build_prompt(user_prompt: &str, facts: &TurnFacts) -> String {
    let mut p = String::from(PROMPT_HEAD);
    p.push_str("FACTS:\nfiles:");
    if facts.files.is_empty() {
        p.push_str(" (нет)");
    }
    p.push('\n');
    for f in facts.files.iter().take(20) {
        p.push_str(&format!("  {} ({})\n", f.path, f.kind));
    }
    p.push_str("commands:");
    if facts.commands.is_empty() {
        p.push_str(" (нет)");
    }
    p.push('\n');
    let mut cmd_budget = 600usize;
    for c in &facts.commands {
        let line = format!("  {c}\n");
        if line.chars().count() > cmd_budget {
            break;
        }
        cmd_budget -= line.chars().count();
        p.push_str(&line);
    }
    p.push_str("---\nХОД:\n");
    p.push_str(&format!("Пользователь: {user_prompt}\n"));
    p.push_str(&format!("Агент: {}\n", head_tail(&facts.final_reply, 4000, 2600, 1200)));
    if !facts.tool_log.is_empty() {
        p.push_str(&format!(
            "Инструменты: {}\n",
            ellipsize(&facts.tool_log.join("; "), 1500)
        ));
    }
    let mut md_budget = 2000usize;
    for m in &facts.md_heads {
        if md_budget < 200 {
            break;
        }
        let head = ellipsize(&m.head, md_budget);
        md_budget = md_budget.saturating_sub(head.chars().count());
        p.push_str(&format!("Записанная дока {}:\n{}\n", m.path, head));
    }
    p.push_str("\nНапоминание: ответ — ОДИН JSON-объект по схеме, проза по-русски, идентификаторы as-is.");
    p
}

/// Карточка сводки хода — то, что кэшируется и уходит в UI (поля как в схеме
/// промпта, snake_case: JS читает card.docs_digest).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TurnCard {
    pub summary: String,
    pub files: Vec<CardFile>,
    pub docs_digest: String,
    pub commands: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CardFile {
    pub path: String,
    pub note: String,
}

/// Выход LLM → карточка: срез {..}, ремонт усечённого JSON, валидация
/// (пути ⊆ facts, клампы длин). None → зовущий падает на детерминированную
/// карточку. Ремонт свой, маленький: полноценный json-repair тут оверкилл.
pub fn parse_card(out: &str, facts: &TurnFacts) -> Option<TurnCard> {
    let start = out.find('{')?;
    let cut = &out[start..];
    let mut card: Option<TurnCard> = None;
    // 1) кандидаты «до каждой } с конца» — отрезают прозу/заборы после JSON
    for (i, _) in cut.char_indices().rev().filter(|(_, c)| *c == '}').take(6) {
        if let Ok(c) = serde_json::from_str::<TurnCard>(&cut[..=i]) {
            card = Some(c);
            break;
        }
    }
    // 2) усечённый вывод: докрутить закрытие строки/объекта
    if card.is_none() {
        for fix in ["\"}", "}", "\"}]}", "]}"] {
            if let Ok(c) = serde_json::from_str::<TurnCard>(&format!("{cut}{fix}")) {
                card = Some(c);
                break;
            }
        }
    }
    let mut card = card?;
    let allowed: std::collections::HashSet<&str> =
        facts.files.iter().map(|f| f.path.as_str()).collect();
    card.files.retain(|f| allowed.contains(f.path.as_str()));
    for f in &mut card.files {
        f.note = ellipsize(&one_line(&f.note), 80);
    }
    card.summary = ellipsize(&one_line(&card.summary), 600);
    card.docs_digest = ellipsize(&card.docs_digest, 1200);
    card.commands = ellipsize(&one_line(&card.commands), 200);
    (!card.summary.is_empty()).then_some(card)
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
    fn segment_attributes_facts_to_own_turn() {
        // два хода: факты записи не должны утекать в соседний ход
        let entries = vec![
            json!({"type":"user","uuid":"u1","timestamp":"2026-07-04T10:00:00Z",
                   "message":{"content":"сделай A"}}),
            json!({"type":"assistant","uuid":"a1","parentUuid":"u1","timestamp":"2026-07-04T10:00:05Z",
                   "message":{"content":[
                       {"type":"tool_use","name":"Edit","input":{"file_path":"a.rs"}},
                       {"type":"tool_use","name":"Bash","input":{"command":"cmd A"}}]}}),
            json!({"type":"user","uuid":"u2","timestamp":"2026-07-04T10:01:00Z",
                   "message":{"content":"сделай B"}}),
            json!({"type":"assistant","uuid":"a2","parentUuid":"u2","timestamp":"2026-07-04T10:01:05Z",
                   "message":{"content":[
                       {"type":"tool_use","name":"Edit","input":{"file_path":"b.rs"}},
                       {"type":"tool_use","name":"Bash","input":{"command":"cmd B"}}]}}),
        ];
        let (_items, turns) = segment(backend(Agent::Claude), &entries);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].facts.files, vec![FileTouch { path: "a.rs".into(), kind: "edited".into() }]);
        assert_eq!(turns[0].facts.commands, vec!["cmd A".to_string()]);
        assert_eq!(turns[1].facts.files, vec![FileTouch { path: "b.rs".into(), kind: "edited".into() }]);
        assert_eq!(turns[1].facts.commands, vec!["cmd B".to_string()]);
    }

    #[test]
    fn push_file_dedups_and_upgrades_to_created() {
        let mut f = TurnFacts::default();
        push_file(&mut f, "a.rs", "edited");
        push_file(&mut f, "a.rs", "created");
        push_file(&mut f, "a.rs", "edited");
        assert_eq!(f.files, vec![FileTouch { path: "a.rs".into(), kind: "created".into() }]);
    }

    #[test]
    fn prompt_contains_facts_fewshot_and_reminder() {
        let mut facts = TurnFacts::default();
        push_file(&mut facts, "src/a.rs", "edited");
        facts.commands.push("cargo test".into());
        facts.final_reply = "Готово.".into();
        let p = build_prompt("сделай A", &facts);
        assert!(p.contains("src/a.rs (edited)"));
        assert!(p.contains("cargo test"));
        assert!(p.contains("Пользователь: сделай A"));
        assert!(p.contains("Пример."), "few-shot присутствует");
        assert!(p.trim_end().ends_with("идентификаторы as-is."), "языковой якорь в конце");
    }

    #[test]
    fn prompt_trims_long_reply_head_tail() {
        let mut facts = TurnFacts::default();
        facts.final_reply = "начало ".repeat(400) + &"конец ".repeat(400); // ~5.4К
        let p = build_prompt("x", &facts);
        assert!(p.contains("[…]"), "длинный ответ порезан головой+хвостом");
        assert!(p.contains("начало") && p.contains("конец"));
    }

    #[test]
    fn head_tail_short_passthrough() {
        assert_eq!(head_tail("абв", 10, 5, 3), "абв");
        let cut = head_tail(&"x".repeat(100), 10, 5, 3);
        assert_eq!(cut, format!("{}\n[…]\n{}", "x".repeat(5), "x".repeat(3)));
    }

    fn facts_with(paths: &[&str]) -> TurnFacts {
        let mut f = TurnFacts::default();
        for p in paths {
            push_file(&mut f, p, "edited");
        }
        f
    }

    #[test]
    fn parse_card_clean_json() {
        let out = r#"{"summary": "Сделано.", "files": [{"path": "a.rs", "note": "правка"}], "docs_digest": "", "commands": "cargo test — ok"}"#;
        let c = parse_card(out, &facts_with(&["a.rs"])).unwrap();
        assert_eq!(c.summary, "Сделано.");
        assert_eq!(c.files.len(), 1);
        assert_eq!(c.commands, "cargo test — ok");
    }

    #[test]
    fn parse_card_strips_prose_and_fences() {
        let out = "Вот JSON:\n```json\n{\"summary\": \"Готово.\", \"files\": [], \"docs_digest\": \"\", \"commands\": \"\"}\n```";
        assert_eq!(parse_card(out, &facts_with(&[])).unwrap().summary, "Готово.");
    }

    #[test]
    fn parse_card_repairs_truncated_json() {
        // модель оборвалась посреди строки — докручиваем "}
        let out = r#"{"summary": "Полдела сделано"#;
        assert_eq!(parse_card(out, &facts_with(&[])).unwrap().summary, "Полдела сделано");
    }

    #[test]
    fn parse_card_drops_foreign_paths_and_empty_summary() {
        let out = r#"{"summary": "Ок.", "files": [{"path": "a.rs", "note": "x"}, {"path": "hallucinated.rs", "note": "y"}], "docs_digest": "", "commands": ""}"#;
        let c = parse_card(out, &facts_with(&["a.rs"])).unwrap();
        assert_eq!(c.files.len(), 1, "чужой путь отброшен");
        assert_eq!(c.files[0].path, "a.rs");
        let empty = r#"{"summary": "", "files": [], "docs_digest": "", "commands": ""}"#;
        assert!(parse_card(empty, &facts_with(&[])).is_none(), "пустое summary → None");
        assert!(parse_card("совсем не json", &facts_with(&[])).is_none());
    }
}
