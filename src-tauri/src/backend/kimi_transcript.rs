//! Парсер транскрипта Kimi Code CLI (`wire.jsonl`) → общий `ChatItem`.
//!
//! Формат: `<дом>/sessions/<wd_*>/<sid>/agents/main/wire.jsonl`, JSONL, append-only,
//! линейный — `chain_from_entries` не нужен (как у Codex). Первая строка всегда
//! `metadata`, у остальных записей есть `type` и `time` (мс эпохи).
//!
//! В ленту идут только два типа: `context.append_message` (реплики юзера) и
//! `context.append_loop_event` (всё, что делает ассистент: текст, размышления,
//! тулы). `turn.prompt` игнорируем намеренно — он дубль следующего
//! `context.append_message` с тем же текстом. Defensive: неизвестное → пропуск,
//! битая строка отсеивается ещё на разборе JSONL, не паникуем.
//!
//! `turnId` в логе разнотипный (строка в loop-событиях, число в `turn.ended`) —
//! поэтому мы его не читаем вообще: для ленты он не нужен.

use serde_json::Value;

use crate::transcript::ChatItem;
use crate::util::{basename, ellipsize, now_ms, one_line};

/// Одна строка `wire.jsonl` → элементы чата (обычно 0–1).
pub fn to_chat_items(entry: &Value) -> Vec<ChatItem> {
    let ts = entry.get("time").and_then(Value::as_i64).unwrap_or_else(now_ms);
    match entry.get("type").and_then(Value::as_str) {
        Some("context.append_message") => user_items(entry.get("message"), ts),
        Some("context.append_loop_event") => loop_items(entry.get("event"), ts),
        // metadata, llm.request, usage.record, turn.*, permission.*, tools.* … — телеметрия
        _ => vec![],
    }
}

/// Реплика юзера. `role` тут всегда "user", различает записи `origin.kind`:
/// в ленту берём ТОЛЬКО живой ввод человека, а `injection` / `system_trigger` /
/// `background_task` / `task` / `skill_activation` / `cron_job` — служебные
/// впрыски (там же живут `<system-reminder>`), им в чате не место.
fn user_items(message: Option<&Value>, ts: i64) -> Vec<ChatItem> {
    let Some(m) = message else {
        return vec![];
    };
    if m.pointer("/origin/kind").and_then(Value::as_str) != Some("user") {
        return vec![];
    }
    let mut out = Vec::new();
    for t in content_texts(m.get("content")) {
        push_text(&mut out, "user", &t, ts);
    }
    out
}

/// Событие «петли» агента: текст, размышление или тул.
fn loop_items(event: Option<&Value>, ts: i64) -> Vec<ChatItem> {
    let Some(e) = event else {
        return vec![];
    };
    match e.get("type").and_then(Value::as_str) {
        Some("content.part") => {
            let Some(part) = e.get("part") else {
                return vec![];
            };
            // Текст лежит в поле, ИМЯ которого совпадает с типом: text → part.text,
            // think → part.think. Размышления прячем (аналог reasoning у Codex).
            if part.get("type").and_then(Value::as_str) != Some("text") {
                return vec![];
            }
            let mut out = Vec::new();
            let text = part.get("text").and_then(Value::as_str).unwrap_or("");
            push_text(&mut out, "assistant", text, ts);
            out
        }
        Some("tool.call") => vec![ChatItem {
            role: "assistant",
            kind: "tool",
            text: tool_label(
                e.get("name").and_then(Value::as_str).unwrap_or("tool"),
                e.get("args"),
                e.get("description").and_then(Value::as_str),
                e.get("display"),
            ),
            ts,
        }],
        Some("tool.result") => error_item(e.get("result"), ts).into_iter().collect(),
        _ => vec![], // step.begin / step.end
    }
}

/// Успешный результат тула в ленте не нужен (это простыни вывода), а ошибка —
/// нужна: без неё непонятно, почему агент вдруг сменил курс.
fn error_item(result: Option<&Value>, ts: i64) -> Option<ChatItem> {
    let r = result?;
    if !r.get("isError").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    // output — либо строка, либо массив ContentPart (картинки приезжают
    // ссылкой `blobref:` — не разворачиваем, берём только текстовые части).
    let out = match r.get("output") {
        Some(Value::String(s)) => s.clone(),
        other => content_texts(other).join("\n"),
    };
    let tail = tail_chars(&one_line(&out), 200);
    Some(ChatItem {
        role: "assistant",
        kind: "tool",
        text: if tail.is_empty() {
            "Ошибка".to_string()
        } else {
            format!("Ошибка · {tail}")
        },
        ts,
    })
}

/// `content` бывает строкой и массивом `[{type:"text",text:"…"}]` — и у сообщения
/// юзера, и у результата тула.
fn content_texts(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .map(ToString::to_string)
            .collect(),
        _ => vec![],
    }
}

fn push_text(out: &mut Vec<ChatItem>, role: &'static str, text: &str, ts: i64) {
    let t = text.trim();
    // служебные вставки (`<system-reminder>` и прочие) в чат не показываем
    if t.is_empty() || t.starts_with('<') {
        return;
    }
    out.push(ChatItem {
        role,
        kind: "text",
        text: ellipsize(t, 4000),
        ts,
    });
}

/// Хвост строки: у ошибок суть обычно в конце (traceback → «Command failed…»).
fn tail_chars(s: &str, max: usize) -> String {
    let ch: Vec<char> = s.trim().chars().collect();
    if ch.len() <= max {
        return ch.into_iter().collect();
    }
    format!("…{}", ch[ch.len() - max..].iter().collect::<String>())
}

/// Чип инструмента в общем формате «Tool · аргумент». Приоритет подсказки:
/// `display` (структурированный рендер самого CLI) → `description` (человеческая
/// фраза от модели) → сырые `args`. Имена тулов у Kimi уже читаемые
/// (Bash/Read/Edit/Grep), переименовывать нечего.
fn tool_label(
    name: &str,
    args: Option<&Value>,
    description: Option<&str>,
    display: Option<&Value>,
) -> String {
    let hint = display_hint(display)
        .or_else(|| description.map(ToString::to_string))
        .or_else(|| args_hint(args));
    match hint {
        Some(h) if !h.trim().is_empty() => {
            format!("{name} · {}", ellipsize(&one_line(&h), 96))
        }
        _ => name.to_string(),
    }
}

/// 8 видов `display`. Поля внутри вида опциональны — реальные логи бывают беднее
/// схемы (у `search` встречается один `query`, у `command` нет `description`).
fn display_hint(display: Option<&Value>) -> Option<String> {
    let d = display?;
    let s = |k: &str| d.get(k).and_then(Value::as_str).map(ToString::to_string);
    match d.get("kind").and_then(Value::as_str)? {
        // path абсолютный — в чипе полезно только имя файла
        "file_io" => s("path").map(|p| basename(&p)).or_else(|| s("detail")),
        "command" => s("command"),
        "search" => s("query").or_else(|| s("path")),
        "url_fetch" => s("url"),
        "todo_list" => todo_hint(d.get("items")),
        "agent_call" => s("agent_name").or_else(|| s("prompt")),
        "plan_review" => s("plan").map(|p| first_line(&p)),
        "skill_call" => s("skill_name"),
        _ => None,
    }
}

/// В чипе TodoList полезен текущий пункт, а не число задач.
fn todo_hint(items: Option<&Value>) -> Option<String> {
    let arr = items?.as_array()?;
    let pick = arr
        .iter()
        .find(|i| i.get("status").and_then(Value::as_str) == Some("in_progress"))
        .or_else(|| arr.first())?;
    pick.get("title").and_then(Value::as_str).map(ToString::to_string)
}

/// Первая непустая строка (у плана это markdown-заголовок).
fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .trim_start_matches('#')
        .trim()
        .to_string()
}

fn args_hint(args: Option<&Value>) -> Option<String> {
    let a = args?;
    for k in [
        "command",
        "path",
        "file_path",
        "pattern",
        "query",
        "url",
        "description",
        "skill",
        "prompt",
        "task_id",
    ] {
        if let Some(v) = a.get(k).and_then(Value::as_str) {
            let v = if k == "path" || k == "file_path" {
                basename(v)
            } else {
                v.to_string()
            };
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// Модель сессии: последний `llm.request.modelAlias` — полный алиас
/// (`kimi-code/k3`), короткое `model` не берём, оно теряет вендора. Фолбэки для
/// сессий, где запросов ещё не было: `config.update`, затем `profile.bind`.
pub fn extract_model(entries: &[Value]) -> Option<String> {
    for typ in ["llm.request", "config.update", "profile.bind"] {
        let found = entries.iter().rev().find_map(|e| {
            (e.get("type").and_then(Value::as_str) == Some(typ))
                .then(|| e.get("modelAlias").and_then(Value::as_str))
                .flatten()
                .filter(|s| !s.is_empty())
                .map(String::from)
        });
        if found.is_some() {
            return found;
        }
    }
    None
}

/// Заголовок: первая реплика юзера (укорочена). Человеческое имя сессии лежит в
/// `state.json` рядом с `wire.jsonl` — его читает демон отдельно, это фолбэк.
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

/// Финальный ответ ассистента: последняя СВЯЗНАЯ серия текстовых частей — та,
/// после которой не было ни тула, ни реплики юзера. Kimi печатает по части на
/// шаг, и «сейчас поищу…» перед десятком тулов — это тоже `content.part{text}`;
/// брать надо только хвост. (Демон предпочитает `last_assistant_message` из
/// хука — файл может быть не сфлашен; это фолбэк.)
pub fn full_final_reply(entries: &[Value]) -> Option<String> {
    let mut run: Vec<String> = Vec::new();
    for e in entries {
        for item in to_chat_items(e) {
            match (item.role, item.kind) {
                ("assistant", "text") => run.push(item.text),
                _ => run.clear(),
            }
        }
    }
    let reply = run.join("\n");
    let reply = reply.trim();
    if reply.is_empty() {
        None
    } else {
        Some(ellipsize(reply, 6000))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::entries_from_text;
    use serde_json::json;

    fn msg(kind: &str, text: &str) -> Value {
        json!({
            "type": "context.append_message", "time": 1_755_000_000_000i64,
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": text}],
                "origin": {"kind": kind}
            }
        })
    }

    fn loop_event(event: Value) -> Value {
        json!({ "type": "context.append_loop_event", "time": 1_755_000_000_001i64, "event": event })
    }

    fn part(typ: &str, text: &str) -> Value {
        loop_event(json!({
            "type": "content.part", "uuid": "u1", "turnId": "0", "step": 1,
            "part": { "type": typ, typ: text }
        }))
    }

    #[test]
    fn user_message_in_feed_injection_out() {
        let items = to_chat_items(&msg("user", "как считаешь кто сильнее?"));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].role, "user");
        assert_eq!(items[0].kind, "text");
        assert_eq!(items[0].text, "как считаешь кто сильнее?");
        assert_eq!(items[0].ts, 1_755_000_000_000);

        for kind in [
            "injection",
            "system_trigger",
            "background_task",
            "task",
            "skill_activation",
            "cron_job",
        ] {
            assert!(
                to_chat_items(&msg(kind, "<system-reminder>дела</system-reminder>")).is_empty(),
                "origin.kind={kind} — служебный впрыск, не в ленту"
            );
        }
    }

    #[test]
    fn text_part_is_assistant_think_is_hidden() {
        let t = to_chat_items(&part("text", "Если коротко: я бы поставил на себя"));
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].role, "assistant");
        assert_eq!(t[0].kind, "text");
        assert_eq!(t[0].text, "Если коротко: я бы поставил на себя");

        let th = to_chat_items(&part("think", "We need answer user in Russian."));
        assert!(th.is_empty(), "размышления в ленту не идут");
    }

    #[test]
    fn tool_call_label_prefers_display_then_description_then_args() {
        let with_display = loop_event(json!({
            "type": "tool.call", "toolCallId": "c1", "name": "Read",
            "description": "Reading CONTEXT.md",
            "args": {"path": "CONTEXT.md", "n_lines": 200},
            "display": {"kind": "file_io", "operation": "read", "path": "/Users/x/proj/CONTEXT.md"}
        }));
        let items = to_chat_items(&with_display);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "tool");
        assert_eq!(items[0].role, "assistant");
        assert_eq!(items[0].text, "Read · CONTEXT.md");

        let cmd = loop_event(json!({
            "type": "tool.call", "toolCallId": "c2", "name": "Bash",
            "args": {"command": "cargo test"},
            "display": {"kind": "command", "command": "cargo test", "cwd": "/x", "language": "bash"}
        }));
        assert_eq!(to_chat_items(&cmd)[0].text, "Bash · cargo test");

        // display нет (старые сессии) → description
        let desc = loop_event(json!({
            "type": "tool.call", "toolCallId": "c3", "name": "Grep",
            "description": "Searching for 'fn main' in src",
            "args": {"pattern": "fn main", "path": "src"}
        }));
        assert_eq!(to_chat_items(&desc)[0].text, "Grep · Searching for 'fn main' in src");

        // ни display, ни description → args
        let bare = loop_event(json!({
            "type": "tool.call", "toolCallId": "c4", "name": "WebSearch",
            "args": {"query": "Kimi K3 benchmark"}
        }));
        assert_eq!(to_chat_items(&bare)[0].text, "WebSearch · Kimi K3 benchmark");

        // совсем пусто → голое имя
        let empty = loop_event(json!({"type":"tool.call","toolCallId":"c5","name":"EnterPlanMode","args":{}}));
        assert_eq!(to_chat_items(&empty)[0].text, "EnterPlanMode");
    }

    #[test]
    fn todo_and_agent_and_skill_displays() {
        let todo = loop_event(json!({
            "type": "tool.call", "name": "TodoList", "toolCallId": "t1", "args": {},
            "display": {"kind": "todo_list", "items": [
                {"title": "Прочитать отчёты", "status": "completed"},
                {"title": "Синтезировать критику", "status": "in_progress"}
            ]}
        }));
        assert_eq!(to_chat_items(&todo)[0].text, "TodoList · Синтезировать критику");

        let agent = loop_event(json!({
            "type": "tool.call", "name": "Agent", "toolCallId": "t2", "args": {},
            "display": {"kind": "agent_call", "agent_name": "coder", "prompt": "длинный промпт"}
        }));
        assert_eq!(to_chat_items(&agent)[0].text, "Agent · coder");

        let skill = loop_event(json!({
            "type": "tool.call", "name": "Skill", "toolCallId": "t3", "args": {},
            "display": {"kind": "skill_call", "skill_name": "check-kimi-code-docs", "args": "почему"}
        }));
        assert_eq!(to_chat_items(&skill)[0].text, "Skill · check-kimi-code-docs");

        let plan = loop_event(json!({
            "type": "tool.call", "name": "ExitPlanMode", "toolCallId": "t4", "args": {},
            "display": {"kind": "plan_review", "plan": "# Перенос BC-алертов\n\n## Цель\n\nВместо…"}
        }));
        assert_eq!(to_chat_items(&plan)[0].text, "ExitPlanMode · Перенос BC-алертов");
    }

    #[test]
    fn tool_result_only_errors_reach_feed() {
        let ok = loop_event(json!({
            "type": "tool.result", "toolCallId": "c1", "parentUuid": "u1",
            "result": {"output": "всё хорошо, длинная простыня вывода", "note": "truncated"}
        }));
        assert!(to_chat_items(&ok).is_empty(), "успешный результат — не в ленте");

        let err = loop_event(json!({
            "type": "tool.result", "toolCallId": "c2", "parentUuid": "u2",
            "result": {"output": "ERROR: must be owner of schema public\nCommand failed with exit code: 1.", "isError": true}
        }));
        let items = to_chat_items(&err);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "tool");
        assert_eq!(
            items[0].text,
            "Ошибка · ERROR: must be owner of schema public Command failed with exit code: 1."
        );

        // output массивом ContentPart (картинки приезжают ссылкой blobref:)
        let arr = loop_event(json!({
            "type": "tool.result", "toolCallId": "c3",
            "result": {"isError": true, "output": [
                {"type": "text", "text": "файл не найден"},
                {"type": "image_url", "imageUrl": {"url": "blobref:image/png;deadbeef"}}
            ]}
        }));
        assert_eq!(to_chat_items(&arr)[0].text, "Ошибка · файл не найден");
    }

    #[test]
    fn steps_and_telemetry_are_skipped() {
        for e in [
            loop_event(json!({"type": "step.begin", "step": 1})),
            loop_event(json!({"type": "step.end", "step": 1})),
            json!({"type": "metadata", "protocol_version": "1.5", "created_at": 1_755_000_000_000i64}),
            json!({"type": "usage.record", "time": 1i64, "usage": {"input": 10}}),
            // turn.prompt — дубль следующего context.append_message, в ленту не идёт
            json!({"type": "turn.prompt", "time": 1i64, "input": [{"type":"text","text":"привет"}]}),
            // turnId числом (в turn.ended) не должен ничего ломать
            json!({"type": "turn.ended", "time": 1i64, "turnId": 0}),
        ] {
            assert!(to_chat_items(&e).is_empty(), "не в ленту: {e}");
        }
    }

    #[test]
    fn extract_model_takes_last_alias_across_sources() {
        let entries = vec![
            json!({"type": "profile.bind", "time": 1i64, "modelAlias": "kimi-code/kimi-for-coding"}),
            json!({"type": "config.update", "time": 2i64, "modelAlias": "kimi-code/k3-256k"}),
            json!({"type": "llm.request", "time": 3i64, "model": "k3", "modelAlias": "kimi-code/k3", "thinkingEffort": "high"}),
            json!({"type": "config.update", "time": 4i64, "modelAlias": "kimi-code/k3-256k"}),
        ];
        assert_eq!(
            extract_model(&entries).as_deref(),
            Some("kimi-code/k3"),
            "llm.request главнее config.update, даже если тот позже"
        );
        // без запросов — фолбэк на config.update, затем на profile.bind
        assert_eq!(extract_model(&entries[..2]).as_deref(), Some("kimi-code/k3-256k"));
        assert_eq!(
            extract_model(&entries[..1]).as_deref(),
            Some("kimi-code/kimi-for-coding")
        );
        assert_eq!(extract_model(&[]), None);
    }

    #[test]
    fn title_is_first_human_message() {
        let entries = vec![
            json!({"type": "metadata", "protocol_version": "1.5"}),
            msg("injection", "<system-reminder>The TodoList tool…</system-reminder>"),
            msg("background_task", "фоновая задача завершилась"),
            msg("user", "сделай рефактор парсера транскрипта"),
            msg("user", "и ещё вот это"),
        ];
        assert_eq!(
            extract_title(&entries).as_deref(),
            Some("сделай рефактор парсера транскрипта")
        );
        assert_eq!(extract_title(&entries[..3]), None, "одни впрыски — заголовка нет");
    }

    #[test]
    fn final_reply_takes_last_connected_run_of_text() {
        let entries = vec![
            msg("user", "погугли"),
            part("think", "нужно поискать"),
            part("text", "Сейчас поищу свежие бенчмарки."),
            loop_event(json!({"type":"tool.call","name":"WebSearch","toolCallId":"c1","args":{"query":"x"}})),
            loop_event(json!({"type":"tool.result","toolCallId":"c1","result":{"output":"…"}})),
            part("text", "Итог: вот что нашёл."),
            part("text", "И ещё одна деталь."),
        ];
        assert_eq!(
            full_final_reply(&entries).as_deref(),
            Some("Итог: вот что нашёл.\nИ ещё одна деталь."),
            "берём хвост из связных text-частей, а не всё подряд"
        );
        assert_eq!(full_final_reply(&entries[..1]), None, "ответа ассистента ещё нет");
    }

    #[test]
    fn broken_lines_are_skipped_silently() {
        let text = concat!(
            "{\"type\":\"metadata\",\"protocol_version\":\"1.5\",\"created_at\":1755000000000}\n",
            "{\"type\":\"context.append_message\",\"time\":1,\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"привет\"}],\"origin\":{\"kind\":\"user\"}}}\n",
            "{\"type\":\"context.append_loop_event\",\"time\":2,\"event\":{\"type\":\"content.part\",\"tur\n",
            "не json вовсе\n",
            "{\"type\":\"context.append_loop_event\",\"time\":3,\"event\":{\"type\":\"content.part\",\"part\":{\"type\":\"text\",\"text\":\"здравствуй\"}}}\n",
        );
        let entries = entries_from_text(text);
        assert_eq!(entries.len(), 3, "две битые строки отсеяны на разборе");
        let items: Vec<ChatItem> = entries.iter().flat_map(to_chat_items).collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "привет");
        assert_eq!(items[1].text, "здравствуй");
        // и на мусорных значениях полей парсер не паникует
        assert!(to_chat_items(&json!({"type": "context.append_message", "message": 42})).is_empty());
        assert!(to_chat_items(&json!({"type": "context.append_loop_event"})).is_empty());
        assert!(to_chat_items(&json!(["не объект"])).is_empty());
    }
}
