//! Капабилити конфига. Read-часть (get) — фаза 2. Write (settings.set) — фаза 3
//! (гейт уже запрещает там security-ключи). Делегирует в `settings::Store`.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::capability::contract::{CapabilityMeta, Provenance, RiskClass};
use crate::capability::registry::make_handler;
use crate::capability::DaemonRegistry;
use crate::daemon::Daemon;

pub fn register(reg: &mut DaemonRegistry) {
    reg.register(
        CapabilityMeta {
            id: "settings.get",
            class: RiskClass::Read,
            provenance: Provenance::Trusted,
            description: "Чтение незащищённого конфига Jarvis (~/.jarvis/settings.json).",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        make_handler(|d: Arc<Daemon>, _args: Value| async move { Ok(d.settings.load()) }),
    );
}
