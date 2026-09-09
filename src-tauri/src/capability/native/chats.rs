//! Капабилити чатов. `chats.read` (нативно из транскрипта) — фаза 2.
//! `chats.search`/`chats.summarize` — фаза 6 (импорт внешнего chat-MCP).
//! Провенанс untrusted ВСЕГДА: содержимое — то, что обрабатывал агент
//! (веб, файлы, вывод команд), потенциальный носитель инъекции (§6, §8).
//!
//! Два инварианта, купленных кровью:
//!
//! 1. **Разбор — через шов бэкенда.** Раньше здесь звались claude-парсеры
//!    (`chain_from_entries` сшивает по `uuid`/`parentUuid`), а у Kimi и Codex
//!    таких полей нет вовсе: файл читался, цепочка выходила пустой, наружу
//!    уезжало `items: []` при живой сессии. Джарвис видел статус, но не
//!    содержимое — и отправлял человека смотреть в tmux-окно.
//! 2. **Никакого молчаливого усечения.** Обрезали — сказали, сколько и чего, и
//!    дали курсор дочитать. Пустой ответ виден сразу; обрезанный выглядит
//!    целым — агент читает половину отчёта и докладывает с полной уверенностью.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::backend::{backend, Agent};
use crate::capability::contract::{CapabilityMeta, Provenance, RiskClass};
use crate::capability::registry::make_handler;
use crate::capability::DaemonRegistry;
use crate::daemon::Daemon;
use crate::transcript::{self, ChatItem};

use super::arg_str;

/// Окно чтения транскрипта. Хвост, а не весь файл: логи бывают на десятки МБ.
/// Что осталось за окном — считается и называется (`skipped_head_bytes`).
const WINDOW_BYTES: u64 = 4 * 1024 * 1024;
/// Потолок на ОДНО сообщение. Реальные отчёты (тысячи символов) проходят целиком;
/// режется только дамп, и тот — с пометкой и курсором.
const MAX_CHARS: usize = 40_000;
/// Сколько СООБЩЕНИЙ отдаём по умолчанию (не символов: limit считает реплики).
const LIMIT: usize = 80;

pub fn register(reg: &mut DaemonRegistry) {
    reg.register(
        CapabilityMeta {
            id: "chats.read",
            class: RiskClass::Read,
            provenance: Provenance::Untrusted,
            description: "Транскрипт сессии: последние реплики диалога (для ответа/саммари). Читает файл на момент вызова — годится и для «чем занят прямо сейчас». Содержимое — недоверенное.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string" },
                    "limit": { "type": "integer", "description": "сколько последних СООБЩЕНИЙ, по умолчанию 80 (на длину сообщения не влияет)" },
                    "max_chars": { "type": "integer", "description": "потолок символов на одно сообщение, по умолчанию 40000; 0 — без потолка" },
                    "cursor": { "type": "string", "description": "дочитать одно обрезанное сообщение: значение из items[].clip.cursor" },
                    "max_bytes": { "type": "integer", "description": "окно чтения транскрипта в байтах, по умолчанию 4 МБ (растить, если skipped_head_bytes > 0)" }
                },
                "required": ["session_id"]
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move {
            let sid = arg_str(&args, "session_id")?;
            let Some(mut s) = d.session(&sid) else {
                return Err(format!("сессия не найдена: {sid}"));
            };
            let agent = Agent::from_opt(s.agent.as_deref());
            let be = backend(agent);
            // Kimi путь к транскрипту в хуках не приносит вовсе; если мета его ещё
            // не нашла, ищем сами — иначе «чем занят исполнитель» не ответить.
            if s.transcript.is_none() && s.remote.is_none() {
                s.transcript = be
                    .find_transcript_by_sid(&sid)
                    .map(|p| p.to_string_lossy().into_owned());
            }
            let window = args.get("max_bytes").and_then(Value::as_u64).unwrap_or(WINDOW_BYTES);
            // транскрипт читаем с машины сессии: у удалённой он на её узле
            let Some((text, skipped)) = d.transcript_text_skipped(&s, window).await else {
                return Err("нет транскрипта — сессия ещё не слала событий".into());
            };
            let items: Vec<ChatItem> = be
                .entries_from_text(&text)
                .iter()
                .flat_map(|e| be.to_chat_items(e))
                .collect();
            read_result(&sid, agent, s.project.as_deref(), &items, skipped, &args)
        }),
    );
}

/// Чистое ядро ответа: форма ОДНА для всех агентов (те же поля, тот же порядок,
/// роли human/assistant/tool различимы парой `role`+`kind`).
pub(crate) fn read_result(
    sid: &str,
    agent: Agent,
    project: Option<&str>,
    items: &[ChatItem],
    skipped_head: u64,
    args: &Value,
) -> Result<Value, String> {
    let max_chars = num(args, "max_chars", MAX_CHARS);
    let mut notes: Vec<String> = Vec::new();

    // (индекс сообщения, с какого символа его отдавать)
    let picked: Vec<(usize, usize)> = match args.get("cursor").and_then(Value::as_str) {
        Some(c) => {
            let (idx, from, fp) =
                transcript::parse_cursor(c).ok_or("битый cursor — возьмите его из items[].clip.cursor")?;
            let at = locate(items, idx, &fp)
                .ok_or("курсор устарел: сообщение изменилось — перечитайте chats.read")?;
            vec![(at, from)]
        }
        None => {
            let limit = num(args, "limit", LIMIT);
            let start = items.len().saturating_sub(limit);
            if start > 0 {
                notes.push(format!(
                    "показаны последние {} сообщений из {}; выше — раньше по времени (увеличьте limit)",
                    items.len() - start,
                    items.len()
                ));
            }
            (start..items.len()).map(|i| (i, 0)).collect()
        }
    };

    let mut out = Vec::with_capacity(picked.len());
    let mut clipped = 0usize;
    for (i, from) in picked {
        let it = &items[i];
        let (piece, total, next) = transcript::slice_chars(&it.text, from, max_chars);
        let sent = piece.chars().count();
        let mut o = json!({ "role": it.role, "kind": it.kind, "text": piece, "ts": it.ts });
        if from > 0 || next.is_some() {
            clipped += 1;
            let omitted = total.saturating_sub(from + sent);
            let cursor = next.map(|n| transcript::make_cursor(i, n, &it.text));
            let hint = match omitted {
                0 => format!("хвост сообщения: символы {from}..{total}"),
                _ => format!(
                    "отдано {sent} симв. из {total} (с {from}); осталось {omitted} — дочитать: chats.read{{session_id, cursor}}"
                ),
            };
            o["clip"] = json!({
                "from": from, "sent": sent, "total": total,
                "omitted": omitted, "cursor": cursor, "hint": hint
            });
        }
        out.push(o);
    }

    if clipped > 0 {
        notes.push(format!(
            "{clipped} сообщ. отдано не целиком — см. items[].clip (там сколько пропущено и cursor, чтобы дочитать)"
        ));
    }
    if skipped_head > 0 {
        notes.push(format!(
            "начало транскрипта за окном чтения (~{skipped_head} байт) — растите max_bytes"
        ));
    }

    Ok(json!({
        "session_id": sid,
        "agent": agent.label(),
        "project": project,
        "total": items.len(),
        "items": out,
        "clipped": clipped,
        "skipped_head_bytes": skipped_head,
        "note": notes.join("; "),
    }))
}

fn num(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .map_or(default, |v| v as usize)
}

/// Сообщение по курсору: сперва по индексу (он почти всегда цел), иначе по
/// отпечатку — лог дописывается и окно чтения едет, индекс мог съехать. Не нашли
/// вовсе — честная ошибка, а не хвост чужого сообщения.
fn locate(items: &[ChatItem], idx: usize, fp: &str) -> Option<usize> {
    let same = |it: &ChatItem| transcript::text_fingerprint(&it.text) == fp;
    if items.get(idx).is_some_and(same) {
        return Some(idx);
    }
    items.iter().position(same)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Agent;

    /// Читаем РЕАЛЬНЫЕ транскрипты обоих форматов из `tests/`-ресурсов: выдуманный
    /// образец подтвердил бы только наши же представления о формате.
    fn fixture(name: &str) -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    fn items_of(agent: Agent, file: &str) -> Vec<ChatItem> {
        let be = backend(agent);
        let text = fixture(file);
        be.entries_from_text(&text)
            .iter()
            .flat_map(|e| be.to_chat_items(e))
            .collect()
    }

    fn read(agent: Agent, items: &[ChatItem], args: Value) -> Value {
        read_result("sid", agent, Some("Goool"), items, 0, &args).unwrap()
    }

    /// Регрессия: kimi-сессия отдавала пустой `items` при прочитанном файле —
    /// разбор шёл claude-парсерами, а `uuid`/`parentUuid` у Kimi нет.
    #[test]
    fn kimi_transcript_is_not_empty() {
        let items = items_of(Agent::Kimi, "kimi-wire.jsonl");
        assert!(items.len() >= 8, "разобрано {} элементов", items.len());
        assert!(items.iter().any(|i| i.role == "user" && i.kind == "text"));
        assert!(items.iter().any(|i| i.role == "assistant" && i.kind == "text"));
        assert!(items.iter().any(|i| i.kind == "tool"), "чипы тулов различимы");
    }

    /// Форма ответа одна для всех агентов: те же ключи, тот же порядок.
    #[test]
    fn shape_is_identical_across_agents() {
        let keys = |v: &Value| {
            v.as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        };
        let kimi = read(Agent::Kimi, &items_of(Agent::Kimi, "kimi-wire.jsonl"), json!({}));
        let claude = read(
            Agent::Claude,
            &items_of(Agent::Claude, "claude-transcript.jsonl"),
            json!({}),
        );
        assert_eq!(keys(&kimi), keys(&claude));
        assert_eq!(kimi["agent"], "kimi");
        assert_eq!(claude["agent"], "claude");
        for v in [&kimi, &claude] {
            let it = &v["items"][0];
            assert_eq!(keys(it), vec!["role", "kind", "text", "ts"]);
            assert!(!v["items"].as_array().unwrap().is_empty(), "лента пуста: {v}");
        }
    }

    /// Codex едет тем же швом. Настоящего rollout на этой машине нет
    /// (`~/.codex` не заведён) — поэтому строка в форме rollout, а не фикстур:
    /// тест держит не формат (его держат тесты `codex_transcript`), а то, что
    /// `chats.read` зовёт ЕГО парсер. С claude-парсером здесь был бы пустой
    /// `items` — ровно как у Kimi.
    #[test]
    fn codex_goes_through_its_own_parser() {
        let be = backend(Agent::Codex);
        let text = concat!(
            r#"{"timestamp":"2026-06-26T22:06:56.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"собери отчёт"}]}}"#,
            "\n",
            r#"{"timestamp":"2026-06-26T22:07:10.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"готово"}]}}"#,
        );
        let items: Vec<ChatItem> = be
            .entries_from_text(text)
            .iter()
            .flat_map(|e| be.to_chat_items(e))
            .collect();
        assert_eq!(items.len(), 2, "claude-парсер вернул бы 0");
        let v = read(Agent::Codex, &items, json!({}));
        assert_eq!(v["agent"], "codex");
        assert_eq!(v["items"][1]["text"], "готово");
        assert_eq!(
            v.as_object().unwrap().keys().cloned().collect::<Vec<_>>(),
            read(Agent::Kimi, &items_of(Agent::Kimi, "kimi-wire.jsonl"), json!({}))
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            "форма наружу одна на всех"
        );
    }

    /// Тот самый отчёт: 7570 символов, таблица и текст ПОСЛЕ неё. Целиком —
    /// значит целиком, вплоть до финального вердикта.
    #[test]
    fn huge_report_arrives_whole_by_default() {
        let items = items_of(Agent::Kimi, "kimi-wire.jsonl");
        let v = read(Agent::Kimi, &items, json!({}));
        let big = v["items"]
            .as_array()
            .unwrap()
            .iter()
            .max_by_key(|i| i["text"].as_str().unwrap_or("").chars().count())
            .unwrap();
        let text = big["text"].as_str().unwrap();
        assert!(text.chars().count() > 7000, "длина {}", text.chars().count());
        assert!(text.contains('|'), "таблица на месте");
        assert!(text.contains("Три строки:"), "текст ПОСЛЕ таблицы доехал");
        assert!(big.get("clip").is_none(), "целое сообщение — без пометки");
        assert_eq!(v["clipped"], 0);
        assert_eq!(v["note"], "");
    }

    /// Обрезали — сказали. И дали дочитать хвост, а не «увеличь лимит наугад».
    #[test]
    fn clipped_message_is_announced_and_readable_to_the_end() {
        let items = items_of(Agent::Kimi, "kimi-wire.jsonl");
        let v = read(Agent::Kimi, &items, json!({ "max_chars": 4000 }));
        let arr = v["items"].as_array().unwrap();
        let big = arr.iter().find(|i| i.get("clip").is_some()).expect("обрезанное сообщение");
        let clip = &big["clip"];

        // 1) пометка честная: сколько отдано, сколько всего, сколько пропущено
        assert_eq!(clip["sent"], 4000);
        assert!(clip["total"].as_u64().unwrap() > 7000);
        assert_eq!(
            clip["omitted"].as_u64().unwrap(),
            clip["total"].as_u64().unwrap() - 4000
        );
        assert!(clip["hint"].as_str().unwrap().contains("дочитать"));
        assert_eq!(v["clipped"], 1);
        assert!(v["note"].as_str().unwrap().contains("не целиком"));

        // 2) обрыв пришёлся на середину таблицы — и это ВИДНО, а не выглядит концом
        let head = big["text"].as_str().unwrap();
        assert!(!head.contains("Три строки:"), "хвост ещё не отдан");

        // 3) хвост дочитывается курсором — до самого конца, за сколько бы заходов
        let mut whole = head.to_string();
        let mut cursor = clip["cursor"].as_str().unwrap().to_string();
        for _ in 0..10 {
            let v = read(Agent::Kimi, &items, json!({ "max_chars": 4000, "cursor": cursor }));
            let one = &v["items"][0];
            assert_eq!(v["items"].as_array().unwrap().len(), 1, "курсор отдаёт одно сообщение");
            whole.push_str(one["text"].as_str().unwrap());
            match one["clip"]["cursor"].as_str() {
                Some(c) => cursor = c.to_string(),
                None => break,
            }
        }
        assert!(whole.contains("Три строки:"), "финальный вердикт дочитан");
        assert_eq!(whole.chars().count(), clip["total"].as_u64().unwrap() as usize);
    }

    /// limit считает СООБЩЕНИЯ, а не кромсает каждое; отброшенные — названы.
    #[test]
    fn limit_counts_messages_not_characters() {
        let items = items_of(Agent::Kimi, "kimi-wire.jsonl");
        let v = read(Agent::Kimi, &items, json!({ "limit": 2 }));
        let arr = v["items"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr.iter().all(|i| i.get("clip").is_none()), "сообщения целые");
        assert!(v["note"].as_str().unwrap().contains("показаны последние 2"));
        assert_eq!(v["total"], items.len());
    }

    /// Устаревший/битый курсор — ошибка, а не хвост чужого сообщения.
    #[test]
    fn stale_cursor_fails_loudly() {
        let items = items_of(Agent::Kimi, "kimi-wire.jsonl");
        let stale = json!({ "cursor": "3@10#0000000000000000" });
        let e = read_result("sid", Agent::Kimi, None, &items, 0, &stale).unwrap_err();
        assert!(e.contains("устарел"), "{e}");
        let junk = json!({ "cursor": "мусор" });
        assert!(read_result("sid", Agent::Kimi, None, &items, 0, &junk)
            .unwrap_err()
            .contains("битый"));
        // индекс съехал, но отпечаток тот же → сообщение находится
        let idx = items.iter().position(|i| i.text.chars().count() > 7000).unwrap();
        let c = transcript::make_cursor(idx + 999, 7000, &items[idx].text);
        let v = read_result("sid", Agent::Kimi, None, &items, 0, &json!({ "cursor": c })).unwrap();
        assert!(v["items"][0]["text"].as_str().unwrap().contains("Три строки:"));
    }

    /// Недочитанная голова транскрипта тоже обязана быть названа вслух.
    #[test]
    fn skipped_head_is_announced() {
        let items = items_of(Agent::Claude, "claude-transcript.jsonl");
        let v = read_result("sid", Agent::Claude, None, &items, 131_072, &json!({})).unwrap();
        assert_eq!(v["skipped_head_bytes"], 131_072);
        assert!(v["note"].as_str().unwrap().contains("начало транскрипта за окном"));
    }
}
