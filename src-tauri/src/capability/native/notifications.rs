//! Капабилити истории/уведомлений и лимитов — делегирует в `history.rs`,
//! `limits.rs` (как IPC `history_get`/`limit_get`).

use std::sync::Arc;

use serde_json::{json, Value};

use crate::capability::contract::{CapabilityMeta, Provenance, RiskClass};
use crate::capability::registry::make_handler;
use crate::capability::DaemonRegistry;
use crate::daemon::Daemon;

pub fn register(reg: &mut DaemonRegistry) {
    reg.register(
        CapabilityMeta {
            id: "notifications.history",
            class: RiskClass::Read,
            provenance: Provenance::Trusted,
            description: "История проектов/чатов с агрегатами по сессиям (что было, когда, сколько токенов).",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        make_handler(|d: Arc<Daemon>, _args: Value| async move { Ok(d.history.projects(&d.usage)) }),
    );

    reg.register(
        CapabilityMeta {
            id: "limits.get",
            class: RiskClass::Read,
            provenance: Provenance::Trusted,
            description: "Лимиты провайдеров: когда сброс, упёрлись ли, доля недели и окна, \
                          норма в сутки, прогноз запаса хода, ступень лестницы, резерв, \
                          свежесть чисел и ночной режим.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        make_handler(|d: Arc<Daemon>, _args: Value| async move { Ok(crate::limits::state_json(&d)) }),
    );
}
