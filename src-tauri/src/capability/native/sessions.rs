//! Капабилити сессий. Read-часть (list/get) — фаза 2; control-часть
//! (reply/queue/control/launch/interrupt) — фаза 3. Делегирует в реестр
//! сессий демона (`daemon.rs`), ничего не дублируя.

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
            id: "sessions.list",
            class: RiskClass::Read,
            provenance: Provenance::Trusted,
            description: "Список живых сессий Claude Code с их статусом (что сейчас запущено, что работает/ждёт/закончило).",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        make_handler(|d: Arc<Daemon>, _args: Value| async move {
            serde_json::to_value(d.snapshot()).map_err(|e| e.to_string())
        }),
    );

    reg.register(
        CapabilityMeta {
            id: "sessions.get",
            class: RiskClass::Read,
            provenance: Provenance::Trusted,
            description: "Состояние одной сессии по её id. Принимает и талон запуска ('spawn-…', см. sessions.spawn): \
пока сессия поднимается, вернётся её состояние 'launching', а как только CLI пришлёт первый хук — сама сессия.",
            input_schema: json!({
                "type": "object",
                "properties": { "session_id": { "type": "string", "description": "id сессии или талон 'spawn-…'" } },
                "required": ["session_id"]
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move {
            let sid = arg_str(&args, "session_id")?;
            if let Some(s) = d.session(&sid) {
                return serde_json::to_value(s).map_err(|e| e.to_string());
            }
            // Талон: `spawn` отдаёт его сразу, до появления сессии, — иначе
            // вернувшийся id было бы нечем спросить первые секунды.
            match d.spawns.find(&sid) {
                Some(rec) => match rec.session_id.as_deref().and_then(|s| d.session(s)) {
                    Some(s) => serde_json::to_value(s).map_err(|e| e.to_string()),
                    None => Ok(super::spawn::pending_json(&rec)),
                },
                None => Err(format!("сессия не найдена: {sid}")),
            }
        }),
    );
}
