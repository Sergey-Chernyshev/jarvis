# Саммари ходов агента в чате — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Один ход агента (юзер-текст → всё до Stop) отображается в чате сессии одной саммари-карточкой: проза, кликабельные файлы, дайджест доки, команды; сырьё — под «развернуть».

**Architecture:** Extract-then-abstract: `turns.rs` детерминированно сегментирует транскрипт на ходы и достаёт факты (файлы/команды) из tool-вызовов обоих бэкендов; LLM (`run_service_llm`) только пишет прозу по фактам, выход валидируется (пути ⊆ факты). Кэш по ключу хода (ts юзер-реплики) в `~/.jarvis/turn-summaries/`. Триггеры: Stop при открытом чате + ленивый backfill последних 5 ходов + кнопка. UI: группировка ленты в `.turn`-блоки, карточка `chat:summary`-событием, тумблер «Сводка/Лента».

**Tech Stack:** Rust (Tauri 2, serde), vanilla JS (ui/renderer.js, DOM без innerHTML), существующий служебный LLM (`claude_bin::run_service_llm`).

**Спека:** `docs/superpowers/specs/2026-07-03-chat-turn-summaries-design.md` (коммит ac6cb41).

**Setup:** работа в отдельной ветке/worktree от `master`; спека закоммичена на `feat/hotkeys-redesign` (ac6cb41) — cherry-pick её первым коммитом: `git cherry-pick ac6cb41`. Тесты: `cargo test --manifest-path src-tauri/Cargo.toml` (алиас `npm test`).

---

### Task 1: `turns.rs` — сегментация ленты на ходы (`spans`)

**Files:**
- Create: `src-tauri/src/turns.rs`
- Modify: `src-tauri/src/main.rs` (объявить модуль рядом с `mod transcript;`)
- Test: внутри `src-tauri/src/turns.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Создать модуль с типом и падающим тестом**

Создать `src-tauri/src/turns.rs`:

```rust
//! Сегментация транскрипта на «ходы» (юзер-текст → всё до следующего юзер-текста)
//! и детерминированные факты хода (файлы, команды) для карточек сводки.
//! Принцип extract-then-abstract: пути/команды достаёт ЭТОТ код, LLM только
//! аннотирует — см. docs/superpowers/specs/2026-07-03-chat-turn-summaries-design.md.

use serde::{Deserialize, Serialize};

use crate::transcript::ChatItem;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
```

В `src-tauri/src/main.rs` рядом с `mod transcript;` добавить:

```rust
mod turns;
```

- [ ] **Step 2: Прогнать тесты**

Run: `cargo test --manifest-path src-tauri/Cargo.toml turns::`
Expected: PASS (3 теста). Логика написана вместе с тестами — важно убедиться, что компилируется и границы/partial-head верны.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/turns.rs src-tauri/src/main.rs
git commit -m "feat(chatsum): сегментация ленты чата на ходы (spans)"
```

---

### Task 2: `turns.rs` — факты хода (`TurnFacts`, `segment`) для Claude и Codex

**Files:**
- Modify: `src-tauri/src/turns.rs`
- Test: там же

- [ ] **Step 1: Написать падающие тесты на извлечение фактов**

Добавить в `mod tests`:

```rust
    use crate::backend::{backend, Agent};
    use serde_json::json;

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
```

- [ ] **Step 2: Прогнать — убедиться, что падают (нет типов/функций)**

Run: `cargo test --manifest-path src-tauri/Cargo.toml turns::`
Expected: FAIL компиляцией (нет `TurnFacts`, `segment`, `push_file`).

- [ ] **Step 3: Реализация**

Добавить в `turns.rs` (после `spans`):

```rust
use serde_json::Value;

use crate::backend::{Agent, Backend};
use crate::util::{ellipsize, one_line};

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
```

- [ ] **Step 4: Прогнать тесты**

Run: `cargo test --manifest-path src-tauri/Cargo.toml turns::`
Expected: PASS (6 тестов).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/turns.rs
git commit -m "feat(chatsum): TurnFacts — детерминированные факты хода (Claude tool_use, Codex apply_patch/exec)"
```

---

### Task 3: `turns.rs` — промпт с бюджетами и few-shot

**Files:**
- Modify: `src-tauri/src/turns.rs`
- Test: там же

- [ ] **Step 1: Падающие тесты**

```rust
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
```

- [ ] **Step 2: Прогнать — FAIL (нет `build_prompt`/`head_tail`)**

Run: `cargo test --manifest-path src-tauri/Cargo.toml turns::`
Expected: FAIL компиляцией.

- [ ] **Step 3: Реализация**

```rust
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
files: ui/settings2.js (edited)
commands: npm test
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
pub fn head_tail(s: &str, max: usize, head: usize, tail: usize) -> String {
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
```

- [ ] **Step 4: Прогнать тесты**

Run: `cargo test --manifest-path src-tauri/Cargo.toml turns::`
Expected: PASS (9 тестов).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/turns.rs
git commit -m "feat(chatsum): промпт сводки хода — few-shot, бюджеты, голова+хвост"
```

---

### Task 4: `turns.rs` — парс/ремонт JSON и валидация карточки

**Files:**
- Modify: `src-tauri/src/turns.rs`
- Test: там же

- [ ] **Step 1: Падающие тесты**

```rust
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
```

- [ ] **Step 2: Прогнать — FAIL (нет `TurnCard`/`parse_card`)**

Run: `cargo test --manifest-path src-tauri/Cargo.toml turns::`
Expected: FAIL компиляцией.

- [ ] **Step 3: Реализация**

```rust
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
```

- [ ] **Step 4: Прогнать тесты**

Run: `cargo test --manifest-path src-tauri/Cargo.toml turns::`
Expected: PASS (13 тестов).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/turns.rs
git commit -m "feat(chatsum): TurnCard — парс с ремонтом JSON и валидацией путей"
```

---

### Task 5: `turnsum.rs` — кэш сводок на диске

**Files:**
- Create: `src-tauri/src/turnsum.rs`
- Modify: `src-tauri/src/main.rs` (рядом с `mod turns;` добавить `mod turnsum;`)
- Test: внутри `src-tauri/src/turnsum.rs`

- [ ] **Step 1: Падающие тесты**

Создать `src-tauri/src/turnsum.rs`:

```rust
//! Кэш сводок ходов на диске + генерация (LLM-слой поверх turns.rs).
//! Файл на сессию: ~/.jarvis/turn-summaries/<sid>.json, версия = PROMPT_VERSION
//! (смена промпта/схемы инвалидирует кэш целиком).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::turns::{TurnCard, PROMPT_VERSION};
use crate::util::jarvis_dir;

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    v: u32,
    turns: HashMap<String, TurnCard>,
}

/// Один write-lock на все сессии: записи редкие и мелкие, гранулярность не нужна.
static WRITE: Mutex<()> = Mutex::new(());

fn sanitize(sid: &str) -> String {
    sid.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect()
}

fn dir() -> PathBuf {
    jarvis_dir().join("turn-summaries")
}

fn file_for(base: &Path, sid: &str) -> PathBuf {
    base.join(format!("{}.json", sanitize(sid)))
}

fn load_in(base: &Path, sid: &str) -> HashMap<String, TurnCard> {
    let Ok(raw) = fs::read_to_string(file_for(base, sid)) else {
        return HashMap::new();
    };
    match serde_json::from_str::<CacheFile>(&raw) {
        Ok(c) if c.v == PROMPT_VERSION => c.turns,
        _ => HashMap::new(), // битый или старая версия — пересоберётся лениво
    }
}

fn save_in(base: &Path, sid: &str, key: &str, card: &TurnCard) {
    let _g = WRITE.lock().unwrap();
    let mut c = CacheFile { v: PROMPT_VERSION, turns: load_in(base, sid) };
    c.turns.insert(key.to_string(), card.clone());
    let _ = fs::create_dir_all(base);
    if let Ok(json) = serde_json::to_string(&c) {
        let _ = fs::write(file_for(base, sid), json);
    }
}

/// Кэш сводок сессии (пустой, если файла нет или версия промпта сменилась).
pub fn load_cards(sid: &str) -> HashMap<String, TurnCard> {
    load_in(&dir(), sid)
}

pub fn save_card(sid: &str, key: &str, card: &TurnCard) {
    save_in(&dir(), sid, key, card);
}

#[cfg(test)]
mod tests {
    use super::*;

    // каталог на тест (cargo test параллелен — общий каталог дал бы гонку)
    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("jarvis-turnsum-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn save_load_roundtrip() {
        let base = tmp("roundtrip");
        let card = TurnCard { summary: "Сделано.".into(), ..Default::default() };
        save_in(&base, "sid-1", "100", &card);
        let got = load_in(&base, "sid-1");
        assert_eq!(got.get("100"), Some(&card));
        assert!(load_in(&base, "нет-такой").is_empty());
    }

    #[test]
    fn version_mismatch_resets() {
        let base = tmp("version");
        fs::create_dir_all(&base).unwrap();
        fs::write(
            file_for(&base, "sid-2"),
            r#"{"v": 0, "turns": {"1": {"summary": "старьё", "files": [], "docs_digest": "", "commands": ""}}}"#,
        )
        .unwrap();
        assert!(load_in(&base, "sid-2").is_empty(), "старая версия промпта → кэш пуст");
    }

    #[test]
    fn sid_sanitized_for_filename() {
        assert_eq!(sanitize("a1-b2"), "a1-b2");
        assert_eq!(sanitize("../evil/й"), "evil");
    }
}
```

В `main.rs` рядом с `mod turns;`:

```rust
mod turnsum;
```

- [ ] **Step 2: Прогнать тесты**

Run: `cargo test --manifest-path src-tauri/Cargo.toml turnsum::`
Expected: PASS (3 теста).

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/turnsum.rs src-tauri/src/main.rs
git commit -m "feat(chatsum): дисковый кэш сводок ходов с версией промпта"
```

---

### Task 6: генерация сводок + триггер на Stop (демон, tail)

**Files:**
- Modify: `src-tauri/src/tail.rs` (запомнить сессию активного хвоста)
- Modify: `src-tauri/src/daemon.rs` (`busy_take`/`busy_release` → `pub(crate)`; Effect + методы генерации)
- Test: `src-tauri/src/tail.rs` (минимальный), остальное — интеграция (чистые части уже покрыты)

- [ ] **Step 1: tail — активная сессия**

В `src-tauri/src/tail.rs` заменить структуру и методы:

```rust
pub struct TailHandle {
    current: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Сессия открытого чата — гейт для сводок ходов (Stop суммаризирует
    /// только открытый чат, чтобы не жечь служебный LLM на каждый Stop).
    session: Mutex<Option<String>>,
}

impl TailHandle {
    pub fn new() -> Self {
        Self { current: Mutex::new(None), session: Mutex::new(None) }
    }

    pub fn stop(&self) {
        if let Some(h) = self.current.lock().unwrap().take() {
            h.abort();
        }
        *self.session.lock().unwrap() = None;
    }

    pub fn start(&self, app: AppHandle, agent: Agent, session_id: String, file: String) {
        self.stop();
        *self.session.lock().unwrap() = Some(session_id.clone());
        let handle = tauri::async_runtime::spawn(tail_loop(app, agent, session_id, PathBuf::from(file)));
        *self.current.lock().unwrap() = Some(handle);
    }

    /// Сессия, чей чат сейчас открыт (tail активен), либо None.
    pub fn active_session(&self) -> Option<String> {
        self.session.lock().unwrap().clone()
    }
}
```

И тест в конец файла:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_session_none_by_default_and_after_stop() {
        let t = TailHandle::new();
        assert_eq!(t.active_session(), None);
        t.stop();
        assert_eq!(t.active_session(), None);
    }
}
```

- [ ] **Step 2: daemon — busy наружу, Effect, генерация**

В `src-tauri/src/daemon.rs`:

1. `fn busy_take` и `fn busy_release` (строки ~690–694) → `pub(crate) fn …` (без других изменений).

2. В `enum Effect` рядом с `DoneSummary { sid: String },` (строка ~170) добавить:

```rust
    /// Сводка последнего хода для открытого чата (панель показывает карточку).
    TurnSummary { sid: String },
```

3. В обработчике `"stop"` (строка ~956, после `effects.push(Effect::DoneSummary { sid: sid.clone() });`) добавить:

```rust
                    effects.push(Effect::TurnSummary { sid: sid.clone() });
```

4. В `run_effects` рядом с `Effect::GenSummary { sid } => d.gen_summary(sid),` добавить:

```rust
                Effect::TurnSummary { sid } => d.turn_stop_summary(sid),
```

5. Методы в `impl Daemon` (рядом с `done_summary`):

```rust
    /// Stop: сводка последнего завершённого хода — только если чат этой сессии
    /// открыт (гейт по tail), иначе сводка догонит лениво при открытии чата.
    fn turn_stop_summary(self: &std::sync::Arc<Self>, sid: String) {
        let d = self.clone();
        tauri::async_runtime::spawn(async move {
            if d.tail.active_session().as_deref() != Some(sid.as_str()) {
                return;
            }
            let Some((be, entries)) = d.turn_entries(&sid) else { return };
            let (_items, turns) = crate::turns::segment(be, &entries);
            let Some(t) = turns.iter().rev().find(|t| t.span.complete) else { return };
            if crate::turnsum::load_cards(&sid).contains_key(&t.span.key) {
                return;
            }
            d.turn_generate(&sid, t).await;
        });
    }

    /// Транскрипт сессии → (бэкенд, записи). None — сессии/файла нет.
    pub(crate) fn turn_entries(
        &self,
        sid: &str,
    ) -> Option<(&'static dyn crate::backend::Backend, Vec<Value>)> {
        let s = self.session(sid)?;
        let tr = s.transcript?;
        let be = crate::backend::backend(crate::backend::Agent::from_opt(s.agent.as_deref()));
        Some((be, be.read_entries(std::path::Path::new(&tr), 512 * 1024)))
    }

    /// Один LLM-вызов сводки хода: промпт → парс/валидация → кириллица-гейт с
    /// одним ретраем → кэш + событие chat:summary. None — фолбэк на
    /// детерминированную карточку (UI уже её показывает).
    pub(crate) async fn turn_generate(
        self: &std::sync::Arc<Self>,
        sid: &str,
        t: &crate::turns::Turn,
    ) -> Option<crate::turns::TurnCard> {
        if !claude_bin::any_service_bin() || !self.busy_take("turnsum", sid) {
            return None;
        }
        let result = async {
            let prompt = crate::turns::build_prompt(&t.user_prompt, &t.facts);
            for _attempt in 0..2 {
                let Some(out) = claude_bin::run_service_llm(&prompt, Duration::from_secs(45)).await
                else {
                    continue;
                };
                let Some(card) = crate::turns::parse_card(&out, &t.facts) else { continue };
                if !ru::has_cyrillic(&card.summary) {
                    continue; // модель съехала в английский — ретрай
                }
                crate::turnsum::save_card(sid, &t.span.key, &card);
                windows::emit_to_panel(
                    &self.app,
                    "chat:summary",
                    &serde_json::json!({ "sessionId": sid, "turnKey": t.span.key, "card": card }),
                );
                return Some(card);
            }
            None
        }
        .await;
        self.busy_release("turnsum", sid);
        result
    }

    /// Ленивый дозабор: последние `max` завершённых ходов без кэша,
    /// последовательно (не душим служебный LLM), со стопом при закрытии чата.
    pub(crate) fn turn_backfill(self: &std::sync::Arc<Self>, sid: String, max: usize) {
        let d = self.clone();
        tauri::async_runtime::spawn(async move {
            let Some((be, entries)) = d.turn_entries(&sid) else { return };
            let (_items, turns) = crate::turns::segment(be, &entries);
            let cards = crate::turnsum::load_cards(&sid);
            let todo: Vec<crate::turns::Turn> = turns
                .into_iter()
                .rev()
                .filter(|t| t.span.complete && !cards.contains_key(&t.span.key))
                .take(max)
                .collect();
            for t in todo {
                if d.tail.active_session().as_deref() != Some(sid.as_str()) {
                    break; // чат закрыли — не тратим вызовы
                }
                d.turn_generate(&sid, &t).await;
            }
        });
    }
```

(Импорты `claude_bin`, `ru`, `windows`, `Duration`, `Value` в daemon.rs уже есть — используются в `ai_toast_summary`.)

- [ ] **Step 3: Компиляция и тесты**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS, включая `tail::tests::active_session_none_by_default_and_after_stop`; всё старое зелёное.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/tail.rs src-tauri/src/daemon.rs
git commit -m "feat(chatsum): генерация сводок ходов — Stop-гейт по открытому чату, backfill, кириллица-ретрай"
```

---

### Task 7: IPC — `chat_open` с ходами, `chat_summarize`, `file_open`; мост

**Files:**
- Modify: `src-tauri/src/ipc.rs` (`chat_open` + 2 новые команды + чистый `resolve_user_file`)
- Modify: `src-tauri/src/main.rs:119` (регистрация команд рядом с `ipc::chat_open`)
- Modify: `ui/bridge.js` (3 новых метода)
- Test: `src-tauri/src/ipc.rs` (тесты `resolve_user_file`)

- [ ] **Step 1: Падающие тесты `resolve_user_file`**

В конец `src-tauri/src/ipc.rs` (если в файле нет `#[cfg(test)]` — добавить блок):

```rust
#[cfg(test)]
mod turn_ipc_tests {
    use super::*;

    #[test]
    fn resolve_user_file_relative_and_missing() {
        let dir = std::env::temp_dir().join(format!("jarvis-ipc-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.rs"), "x").unwrap();
        let cwd = dir.to_string_lossy().to_string();

        let ok = resolve_user_file(Some(&cwd), "sub/a.rs").unwrap();
        assert!(ok.ends_with("sub/a.rs"));
        let abs = resolve_user_file(None, ok.to_str().unwrap()).unwrap();
        assert_eq!(abs, ok);

        assert!(resolve_user_file(Some(&cwd), "нет/такого.rs").is_err());
        assert!(resolve_user_file(None, "relative/without/cwd.rs").is_err());
        assert!(resolve_user_file(Some(&cwd), "sub").is_err(), "каталог — не файл");
    }
}
```

- [ ] **Step 2: Прогнать — FAIL (нет функции)**

Run: `cargo test --manifest-path src-tauri/Cargo.toml turn_ipc`
Expected: FAIL компиляцией.

- [ ] **Step 3: Реализация IPC**

В `src-tauri/src/ipc.rs` заменить тело `chat_open` (строки ~766–793) на:

```rust
#[tauri::command]
pub fn chat_open(app: AppHandle, session_id: String) -> Value {
    let d = Daemon::get(&app);
    let Some(s) = d.session(&session_id) else {
        return err("Сессия не найдена");
    };
    let Some(tr) = s.transcript else {
        return err("Нет транскрипта — сессия ещё не слала событий (перезапусти claude)");
    };
    // Парсер транскрипта — по бэкенду сессии (Claude JSONL vs Codex rollout).
    let agent = crate::backend::Agent::from_opt(s.agent.as_deref());
    let be = crate::backend::backend(agent);
    let entries = be.read_entries(std::path::Path::new(&tr), 512 * 1024);
    let (all_items, turns) = crate::turns::segment(be, &entries);
    let tail_start = all_items.len().saturating_sub(80);
    let items = &all_items[tail_start..];
    // разметка ходов в координатах видимого хвоста; факты — для дет-карточек
    let spans: Vec<Value> = turns
        .iter()
        .filter(|t| t.span.end > tail_start)
        .map(|t| {
            json!({
                "key": t.span.key,
                "start": t.span.start.saturating_sub(tail_start),
                "end": t.span.end - tail_start,
                // ход, чья юзер-реплика отрезана хвостом, не суммаризируем из UI
                "complete": t.span.complete && t.span.start >= tail_start,
                "files": t.facts.files,
                "commands": t.facts.commands,
            })
        })
        .collect();
    let cards = crate::turnsum::load_cards(&session_id);
    d.tail
        .start(app.clone(), agent, session_id.clone(), tr.clone());
    let llm = claude_bin::any_service_bin();
    if llm {
        d.turn_backfill(session_id.clone(), 5);
    }
    println!(
        "[jarvis] chat:open {} items={} turns={} cards={} file={}",
        ellipsize(&session_id, 8),
        items.len(),
        spans.len(),
        cards.len(),
        short_home(&tr)
    );
    json!({ "ok": true, "items": items, "spans": spans, "cards": cards, "llm": llm, "project": s.project })
}
```

(Если `claude_bin` не в импортах ipc.rs — добавить `use crate::claude_bin;`.)

После `chat_close` добавить:

```rust
/// Сводка конкретного хода по кнопке. Fire-and-forget: карточку принесёт
/// событие chat:summary (кэш — turnsum), UI показывает спиннер сам.
#[tauri::command]
pub fn chat_summarize(app: AppHandle, session_id: String, turn_key: String) -> Value {
    let d = Daemon::get(&app);
    if d.session(&session_id).is_none() {
        return err("Сессия не найдена");
    }
    tauri::async_runtime::spawn(async move {
        let Some((be, entries)) = d.turn_entries(&session_id) else { return };
        let (_items, turns) = crate::turns::segment(be, &entries);
        if let Some(t) = turns.iter().find(|t| t.span.key == turn_key) {
            d.turn_generate(&session_id, t).await;
        }
    });
    ok()
}

/// Открыть файл из карточки сводки. path из транскрипта (запись агента),
/// не свободный ввод; резолв от cwd сессии + канонизация + только обычные файлы.
#[tauri::command]
pub fn file_open(app: AppHandle, session_id: String, path: String, reveal: bool) -> Value {
    let d = Daemon::get(&app);
    let cwd = d.session(&session_id).and_then(|s| s.cwd);
    let p = match resolve_user_file(cwd.as_deref(), &path) {
        Ok(p) => p,
        Err(e) => return err(&e),
    };
    let mut cmd = std::process::Command::new("open");
    if reveal {
        cmd.arg("-R"); // показать в Finder
    }
    match cmd.arg(&p).spawn() {
        Ok(_) => ok(),
        Err(e) => err(&format!("open: {e}")),
    }
}

/// Путь юзер-файла: абсолютный как есть, относительный — от cwd сессии;
/// канонизация отсекает несуществующее и ../-фокусы, берём только файлы.
fn resolve_user_file(cwd: Option<&str>, path: &str) -> Result<std::path::PathBuf, String> {
    let raw = if std::path::Path::new(path).is_absolute() {
        std::path::PathBuf::from(path)
    } else {
        std::path::PathBuf::from(cwd.ok_or("нет рабочего каталога сессии")?).join(path)
    };
    let p = raw
        .canonicalize()
        .map_err(|_| format!("файл не найден: {path}"))?;
    if !p.is_file() {
        return Err(format!("не файл: {path}"));
    }
    Ok(p)
}
```

В `src-tauri/src/main.rs` в `generate_handler![` после `ipc::chat_open,` (строка ~119) добавить:

```rust
            ipc::chat_summarize,
            ipc::file_open,
```

В `ui/bridge.js` после строки `closeChat: () => invoke('chat_close'),` добавить:

```js
    summarizeTurn: (sessionId, turnKey) => invoke('chat_summarize', { sessionId, turnKey }),
    openFile: (sessionId, path, reveal) => invoke('file_open', { sessionId, path, reveal: !!reveal }),
```

И рядом с `onChatAppend: (cb) => on('chat:append', cb),`:

```js
    onChatSummary: (cb) => on('chat:summary', cb),
```

- [ ] **Step 4: Прогнать тесты**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS (включая `turn_ipc_tests`); компиляция всего крейта зелёная.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/ipc.rs src-tauri/src/main.rs ui/bridge.js
git commit -m "feat(chatsum): IPC — chat_open с разметкой ходов, chat_summarize, file_open"
```

---

### Task 8: UI — карточки сводок в чате (renderer.js + index.html)

**Files:**
- Modify: `ui/renderer.js`
- Modify: `ui/index.html` (кнопка-тумблер в шапке чата + CSS)

Тестов на JS в проекте нет (проверка — вручную в Task 9); правки держим маленькими и изолированными.

- [ ] **Step 1: index.html — тумблер и стили**

Найти в `ui/index.html` элемент `id="chatModel"` (шапка чата) и сразу после него добавить:

```html
<button id="sumToggle" class="sumtoggle" title="Сводка ходов / полная лента">Сводка</button>
```

В `<style>` рядом с блоком `.msg.tools .chip` (~строка 822) добавить:

```css
  /* --- сводки ходов --- */
  .sumtoggle {
    margin-left: 6px; padding: 2px 8px; font-size: 11px; border-radius: 9px;
    border: 1px solid var(--line, rgba(128,128,128,.35)); background: none;
    color: var(--muted); cursor: pointer;
  }
  .sumtoggle.on { color: var(--fg, inherit); border-color: currentColor; }
  .turnsum {
    margin: 6px 0; padding: 8px 10px; border-radius: 10px;
    background: rgba(127,127,127,.08); border: 1px solid rgba(127,127,127,.15);
    font-size: 12.5px; line-height: 1.45;
  }
  .turnsum .tsum-files { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 6px; }
  .turnsum .fchip {
    display: inline-flex; gap: 4px; align-items: baseline; max-width: 100%;
    padding: 2px 8px; border-radius: 8px; font-size: 11.5px; cursor: pointer;
    background: rgba(127,127,127,.12);
  }
  .turnsum .fchip:hover { background: rgba(127,127,127,.22); }
  .turnsum .fchip .fnote { color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .turnsum .tsum-cmds { margin-top: 6px; font-family: ui-monospace, monospace; font-size: 11px; color: var(--muted); }
  .turnsum details { margin-top: 6px; }
  .turnsum details summary { cursor: pointer; color: var(--muted); font-size: 11.5px; }
  .turnsum .tsum-foot { margin-top: 6px; display: flex; gap: 10px; }
  .turnsum .tsum-btn { background: none; border: none; padding: 0; font-size: 11px; color: var(--muted); cursor: pointer; text-decoration: underline; }
  /* режим «Сводка»: сырьё завершённых ходов спрятано, пока не развернули */
  #chatlog.sum .turn.done .turnraw { display: none; }
  #chatlog.sum .turn.done.expanded .turnraw { display: block; }
  #chatlog:not(.sum) .turnsum { display: none; }
```

- [ ] **Step 2: renderer.js — группировка ходов**

После строки `let toolsGroup = null; // текущая группа чипов (обнуляется текстовой репликой)` (~455) добавить:

```js
/* --- сводки ходов: лента группируется в .turn-блоки по юзер-репликам --- */
let curTurn = null; // { key, wrap, raw } — текущий ход агента
const turnFacts = new Map(); // key → {files, commands} из chat_open.spans
let chatLlmOk = false; // есть ли служебный LLM (кнопка «Сводка»)
const turnTarget = () => (curTurn ? curTurn.raw : chatlogEl);

function startTurn(key, complete) {
  const wrap = document.createElement('div');
  wrap.className = 'turn';
  wrap.dataset.key = key;
  if (complete) wrap.dataset.complete = '1';
  const raw = document.createElement('div');
  raw.className = 'turnraw';
  wrap.appendChild(raw);
  chatlogEl.appendChild(wrap);
  curTurn = { key, wrap, raw };
}
```

В `addToolChip` заменить `chatlogEl.appendChild(toolsGroup);` на:

```js
    turnTarget().appendChild(toolsGroup);
```

В `appendChatItems` ветку юзера и ассистента поменять так (полный новый вид цикла):

```js
  for (const it of items) {
    if (it.kind === 'tool') {
      addToolChip(it.text);
      continue;
    }
    toolsGroup = null;
    if (it.role === 'user') {
      // реальная реплика из транскрипта пришла — снимаем оптимистичный дубль
      const pi = pendingReplies.findIndex((p) => p.text === it.text.trim());
      if (pi >= 0) { pendingReplies[pi].el.remove(); pendingReplies.splice(pi, 1); }
      const msg = document.createElement('div');
      msg.className = 'msg user';
      msg.appendChild(userBubble(it.text));
      chatlogEl.appendChild(msg);
      startTurn(String(it.ts), true); // ответ агента на эту реплику — новый ход
    } else {
      turnTarget().appendChild(assistantMsg(it));
    }
  }
```

- [ ] **Step 3: renderer.js — карточка сводки**

После функции `assistantMsg` добавить:

```js
/* Карточка сводки хода. card=null → детерминированная (факты + сжатый ответ). */
function buildCard(key, card) {
  const facts = turnFacts.get(key) || { files: [], commands: [] };
  const box = document.createElement('div');
  box.className = 'turnsum';

  const files = card ? card.files : facts.files.map((f) => ({ path: f.path, note: '' }));
  const sumText = card ? card.summary : detSummary(key);
  if (sumText) {
    const s = document.createElement('div');
    renderMarkdown(s, sumText);
    box.appendChild(s);
  }
  if (files.length) {
    const fl = document.createElement('div');
    fl.className = 'tsum-files';
    for (const f of files) {
      const chip = document.createElement('span');
      chip.className = 'fchip';
      chip.title = `${f.path} — клик: открыть, ⌥клик: показать в Finder`;
      const p = document.createElement('span');
      p.textContent = '📄 ' + f.path.split('/').pop();
      chip.appendChild(p);
      if (f.note) {
        const n = document.createElement('span');
        n.className = 'fnote';
        n.textContent = '· ' + f.note;
        chip.appendChild(n);
      }
      chip.addEventListener('click', async (ev) => {
        const res = await window.jarvis.openFile(chatSessionId, f.path, ev.altKey);
        if (res && res.error) showToast(res.error);
      });
      fl.appendChild(chip);
    }
    box.appendChild(fl);
  }
  if (card && card.docs_digest) {
    const det = document.createElement('details');
    const sm = document.createElement('summary');
    sm.textContent = 'Дока';
    det.appendChild(sm);
    const body = document.createElement('div');
    renderMarkdown(body, card.docs_digest);
    det.appendChild(body);
    box.appendChild(det);
  }
  const cmds = card ? card.commands : facts.commands.slice(0, 3).join(' · ');
  if (cmds) {
    const c = document.createElement('div');
    c.className = 'tsum-cmds';
    c.textContent = cmds;
    box.appendChild(c);
  }

  const foot = document.createElement('div');
  foot.className = 'tsum-foot';
  const exp = document.createElement('button');
  exp.className = 'tsum-btn';
  const wrapOf = () => box.closest('.turn');
  const relabel = () => { exp.textContent = wrapOf()?.classList.contains('expanded') ? 'свернуть' : 'развернуть'; };
  exp.addEventListener('click', () => { wrapOf()?.classList.toggle('expanded'); relabel(); });
  foot.appendChild(exp);
  if (!card && chatLlmOk) {
    const gen = document.createElement('button');
    gen.className = 'tsum-btn';
    gen.textContent = 'Сводка';
    gen.addEventListener('click', () => {
      gen.textContent = 'готовлю…';
      gen.disabled = true;
      window.jarvis.summarizeTurn(chatSessionId, key);
    });
    foot.appendChild(gen);
  }
  box.appendChild(foot);
  queueMicrotask(relabel);
  return box;
}

// детерминированное саммари: сжатый хвост последней реплики агента в ходе
function detSummary(key) {
  const wrap = chatlogEl.querySelector(`.turn[data-key="${CSS.escape(key)}"]`);
  const bubbles = wrap ? wrap.querySelectorAll('.msg.assistant .bubble') : [];
  const last = bubbles.length ? bubbles[bubbles.length - 1].textContent.trim() : '';
  return last.length > 220 ? last.slice(0, 220) + '…' : last;
}

/* Вставить/заменить карточку хода; card=null — детерминированная. */
function applyCard(key, card) {
  const wrap = chatlogEl.querySelector(`.turn[data-key="${CSS.escape(key)}"]`);
  if (!wrap) return;
  wrap.querySelector('.turnsum')?.remove();
  wrap.insertBefore(buildCard(key, card), wrap.firstChild);
  wrap.classList.add('done');
}
```

- [ ] **Step 4: renderer.js — openChat, событие, тумблер**

В `openChat` после `chatlogEl.textContent = '';` и `toolsGroup = null;` добавить:

```js
  curTurn = null;
  turnFacts.clear();
  chatLlmOk = !!res.llm;
  chatlogEl.classList.toggle('sum', summaryModeOn());
```

Там же, внутри ветки `if (res.items.length) { … }` после `appendChatItems(res.items);` добавить:

```js
    for (const sp of res.spans || []) {
      if (sp.key === 'pre') continue; // частичный головной ход — только сырьё
      turnFacts.set(sp.key, { files: sp.files || [], commands: sp.commands || [] });
      if (!sp.complete) continue;
      applyCard(sp.key, (res.cards || {})[sp.key] || null);
    }
```

После обработчика `window.jarvis.onChatAppend(…)` добавить:

```js
window.jarvis.onChatSummary(({ sessionId, turnKey, card }) => {
  if (view === 'chat' && sessionId === chatSessionId) applyCard(turnKey, card);
});

/* тумблер «Сводка/Лента» — запоминается локально, по умолчанию сводка */
const sumToggleEl = document.getElementById('sumToggle');
const summaryModeOn = () => localStorage.getItem('chatSummary') !== '0';
function renderSumToggle() {
  const on = summaryModeOn();
  sumToggleEl.classList.toggle('on', on);
  sumToggleEl.textContent = on ? 'Сводка' : 'Лента';
  chatlogEl.classList.toggle('sum', on);
}
sumToggleEl.addEventListener('click', () => {
  localStorage.setItem('chatSummary', summaryModeOn() ? '0' : '1');
  renderSumToggle();
});
renderSumToggle();
```

Важно: `summaryModeOn` объявлен `const`-стрелкой ниже `openChat` — hoisting её не поднимет. Либо переместить блок тумблера ВЫШЕ `openChat`, либо в `openChat` использовать прямое выражение `localStorage.getItem('chatSummary') !== '0'`. Выбрать первое (блок тумблера — сразу после объявления `startTurn`).

- [ ] **Step 5: Проверка синтаксиса**

Run: `node --check ui/renderer.js && node --check ui/bridge.js`
Expected: оба без вывода (exit 0).

- [ ] **Step 6: Commit**

```bash
git add ui/renderer.js ui/index.html
git commit -m "feat(chatsum): карточки сводок в чате — группировка ходов, файл-чипы, тумблер Сводка/Лента"
```

---

### Task 9: живая проверка и финал

**Files:** без новых правок (фиксы по результатам — отдельными коммитами).

- [ ] **Step 1: Полный тестовый прогон**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS, ноль упавших (было 281+, стало больше).

- [ ] **Step 2: Ручная проверка в живом приложении**

Run: `npm start` (dev-сборка с фичами и codesign — см. package.json).
Чек-лист:
1. Открыть чат сессии с историей → старые ходы схлопнуты в карточки (детерминированные сразу; LLM-проза дозаполняет последние 5 по мере готовности — событие `chat:summary`).
2. Клик по файл-чипу → файл открылся; ⌥-клик → Finder; несуществующий путь → тост «файл не найден».
3. Отправить сообщение агенту → живой стрим идёт как раньше; после Stop карточка появляется, сырьё уезжает под «развернуть».
4. «Развернуть/свернуть» работает; тумблер «Лента» возвращает сырую ленту целиком и переживает переоткрытие чата.
5. Codex-сессия: карточка с файлами из apply_patch.
6. Лог `[jarvis] chat:open … turns=… cards=…` в консоли демона согласуется с UI.

- [ ] **Step 3: Обновить статус спеки**

В `docs/superpowers/specs/2026-07-03-chat-turn-summaries-design.md` строку статуса заменить на:

```markdown
Дата: 2026-07-03. Статус: утверждено («давай дальше», 2026-07-04); реализация — docs/superpowers/plans/2026-07-04-chat-turn-summaries.md.
```

- [ ] **Step 4: Commit + ветка/PR**

```bash
git add docs/superpowers/specs/2026-07-03-chat-turn-summaries-design.md
git commit -m "docs(spec): саммари ходов — статус утверждено"
```

Дальше — superpowers:finishing-a-development-branch (PR в master; в общем каталоге локальные ветки без PR теряются — см. память о git-churn).

---

## Замечания для исполнителя

- **Не переформатировать чужой код**: CI fmt намеренно информационный. Правки — точечные.
- `ChatItem.role/kind` — `&'static str`; в тестах создавать литералами.
- `ellipsize`/`one_line` — из `crate::util`, работают по chars (не bytes) — безопасны для кириллицы.
- Служебный LLM может отвечать 15+ секунд (известная проблема, отдельный трек) — поэтому карточки всегда рисуются детерминированно сразу, LLM-проза приходит событием.
- В `parse_card` перечисление кандидатов идёт по `char_indices().rev()` — байтовые индексы `}` корректны и для строк с кириллицей.
- Кириллица-ретрай в `turn_generate` юнит-тестом не покрыт (завязан на живой
  `run_service_llm`); сам гейт (`ru::has_cyrillic`) уже проверен тестами `ru`,
  ветка ретрая проверяется вручную в Task 9.
