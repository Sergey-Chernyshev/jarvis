//! Капабилити метрик/usage — делегирует в `usage.rs` (как IPC `usage_summary`).

use std::sync::Arc;

use serde_json::{json, Value};

use crate::capability::contract::{CapabilityMeta, Provenance, RiskClass};
use crate::capability::registry::make_handler;
use crate::capability::DaemonRegistry;
use crate::daemon::Daemon;

pub fn register(reg: &mut DaemonRegistry) {
    reg.register(
        CapabilityMeta {
            id: "metrics.query",
            class: RiskClass::Read,
            provenance: Provenance::Trusted,
            description: "Usage: токены, деньги и число запросов за период ('today' или 'week'), с разбивкой и сериями.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "period": { "type": "string", "enum": ["today", "week"], "description": "период, по умолчанию today" }
                }
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move {
            let period = args.get("period").and_then(|v| v.as_str()).unwrap_or("today");
            Ok(d.usage.stats(period))
        }),
    );

    reg.register(
        CapabilityMeta {
            id: "metrics.session",
            class: RiskClass::Read,
            provenance: Provenance::Trusted,
            description: "Usage по конкретной сессии (токены/стоимость).",
            input_schema: json!({
                "type": "object",
                "properties": { "session_id": { "type": "string" } },
                "required": ["session_id"]
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move {
            let sid = super::arg_str(&args, "session_id")?;
            Ok(d.usage.for_session(&sid).unwrap_or(Value::Null))
        }),
    );
}
