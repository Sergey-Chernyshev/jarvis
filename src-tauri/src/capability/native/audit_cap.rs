//! Капабилити аудита: чтение прошлых вызовов капабилити (прозрачность, §7).
//! Делегирует в `capability::audit::query`.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::capability::audit;
use crate::capability::contract::{CapabilityMeta, Provenance, RiskClass};
use crate::capability::registry::make_handler;
use crate::capability::DaemonRegistry;
use crate::daemon::Daemon;

pub fn register(reg: &mut DaemonRegistry) {
    reg.register(
        CapabilityMeta {
            id: "audit.query",
            class: RiskClass::Read,
            provenance: Provenance::Trusted,
            description: "Прошлые вызовы капабилити: кто/что/исход/время. Фильтры: consumer, id, outcome, limit.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "consumer": { "type": "string" },
                    "id": { "type": "string" },
                    "outcome": { "type": "string", "description": "ok|denied|rejected|failed|notfound" },
                    "limit": { "type": "integer", "description": "сколько последних записей, по умолчанию 200" }
                }
            }),
        },
        make_handler(|_d: Arc<Daemon>, args: Value| async move { Ok(json!(audit::query(&args))) }),
    );
}
