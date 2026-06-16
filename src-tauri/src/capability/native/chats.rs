//! Капабилити чатов. `chats.read` (нативно из транскрипта) — фаза 2.
//! `chats.search`/`chats.summarize` — фаза 6 (импорт внешнего chat-MCP).
//! Провенанс untrusted ВСЕГДА: содержимое — то, что обрабатывал Claude Code
//! (веб, файлы, вывод команд), потенциальный носитель инъекции (§6, §8).

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::capability::contract::{CapabilityMeta, Provenance, RiskClass};
use crate::capability::registry::make_handler;
use crate::capability::DaemonRegistry;
use crate::daemon::Daemon;
use crate::transcript;

use super::arg_str;

pub fn register(reg: &mut DaemonRegistry) {
    reg.register(
        CapabilityMeta {
            id: "chats.read",
            class: RiskClass::Read,
            provenance: Provenance::Untrusted,
            description: "Транскрипт сессии: последние реплики диалога (для ответа/саммари). Содержимое — недоверенное.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string" },
                    "limit": { "type": "integer", "description": "сколько последних элементов, по умолчанию 80" }
                },
                "required": ["session_id"]
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move {
            let sid = arg_str(&args, "session_id")?;
            let Some(s) = d.session(&sid) else {
                return Err(format!("сессия не найдена: {sid}"));
            };
            let Some(tr) = s.transcript else {
                return Err("нет транскрипта — сессия ещё не слала событий".into());
            };
            let items: Vec<transcript::ChatItem> = transcript::chain_from_entries(
                transcript::read_recent_entries(Path::new(&tr), 512 * 1024),
            )
            .iter()
            .flat_map(transcript::to_chat_items)
            .collect();
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(80) as usize;
            let start = items.len().saturating_sub(limit);
            let tail = &items[start..];
            Ok(json!({ "session_id": sid, "project": s.project, "items": tail }))
        }),
    );
}
