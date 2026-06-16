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

    reg.register(
        CapabilityMeta {
            id: "settings.set",
            class: RiskClass::Settings,
            provenance: Provenance::Trusted,
            description: "Изменить незащищённый конфиг. Поле 'patch' — объект ключ→значение. Security-ключи (гранты/плагины/политика) запрещены гейтом.",
            input_schema: json!({
                "type": "object",
                "properties": { "patch": { "type": "object", "description": "ключи и новые значения" } },
                "required": ["patch"]
            }),
        },
        // gate уже отклонит security-ключи ДО хендлера (запрет самоэскалации, §7)
        make_handler(|d: Arc<Daemon>, args: Value| async move {
            let patch = args
                .get("patch")
                .and_then(|p| p.as_object())
                .cloned()
                .ok_or_else(|| "нужен объект 'patch'".to_string())?;
            Ok(d.settings.save(patch))
        }),
    );
}
