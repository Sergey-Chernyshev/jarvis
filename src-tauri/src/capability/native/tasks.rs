//! Капабилити task-досок сессий — читает `Session.board` (инкр. 6) из реестра.
//! Провенанс untrusted: текст задач приходит из сессии, потенциально с инъекцией.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::capability::contract::{CapabilityMeta, Provenance, RiskClass};
use crate::capability::registry::make_handler;
use crate::capability::DaemonRegistry;
use crate::daemon::Daemon;

use super::arg_str;

pub fn register(reg: &mut DaemonRegistry) {
    reg.register(
        CapabilityMeta {
            id: "tasks.get",
            class: RiskClass::Read,
            // структура trusted, но текст задач — из сессии (untrusted, §6)
            provenance: Provenance::Untrusted,
            description: "Доска задач сессии (TodoWrite/Task): статус плана, прогресс.",
            input_schema: json!({
                "type": "object",
                "properties": { "session_id": { "type": "string" } },
                "required": ["session_id"]
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move {
            let sid = arg_str(&args, "session_id")?;
            let Some(s) = d.session(&sid) else {
                return Err(format!("сессия не найдена: {sid}"));
            };
            Ok(json!({
                "session_id": sid,
                "project": s.project,
                "board": s.board,
                "subagents": s.subagents,
                "task": s.task,
                "task_progress": s.task_progress,
            }))
        }),
    );
}
