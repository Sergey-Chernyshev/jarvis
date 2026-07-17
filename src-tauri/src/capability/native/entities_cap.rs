//! Капабилити реестра сущностей (спека plugin-system §6.4). Тонкий фасад над
//! `entities::EntityStore`: логика в apply_publish (pure, тестируется там),
//! здесь — регистрация, эмит в панель и провенанс.
//!
//! Классы риска: обе — Read. publish = вход данных в ядро без side-effect'ов
//! на системе пользователя (confirm на каждом событии убил бы телеметрию);
//! query отдаёт данные чужих процессов → провенанс Untrusted.
//!
//! Фильтрация query по consumes-грантам читателя — инкремент 7 (спека §6.9).

use std::sync::Arc;

use serde_json::{json, Value};

use crate::capability::contract::{CapabilityMeta, Provenance, RiskClass};
use crate::capability::registry::make_handler;
use crate::capability::DaemonRegistry;
use crate::daemon::Daemon;

pub fn register(reg: &mut DaemonRegistry) {
    reg.register(
        CapabilityMeta {
            id: "entities.publish",
            class: RiskClass::Read,
            provenance: Provenance::Trusted,
            description: "Опубликовать (op=upsert) или убрать (op=remove) сущность в реестре ядра. Только для плагинов: владелец — личность вызывающего.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "op":    { "type": "string", "enum": ["upsert", "remove"] },
                    "kind":  { "type": "string", "description": "тип сущности, напр. vm" },
                    "id":    { "type": "string", "description": "object_id внутри kind" },
                    "state": { "type": "string" },
                    "attrs": { "type": "object" }
                },
                "required": ["kind", "id"]
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move {
            let out = crate::entities::apply_publish(&d.entities, &args)?;
            crate::windows::emit_to_panel(&d.app, "entities", &d.entities.snapshot());
            Ok(out)
        }),
    );

    reg.register(
        CapabilityMeta {
            id: "entities.query",
            class: RiskClass::Read,
            provenance: Provenance::Untrusted,
            description: "Сущности реестра ядра (vm.* и др.), опционально фильтры kind/owner.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "kind":  { "type": "string" },
                    "owner": { "type": "string" }
                }
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move {
            let kind = args.get("kind").and_then(|v| v.as_str());
            let owner = args.get("owner").and_then(|v| v.as_str());
            serde_json::to_value(d.entities.query(kind, owner)).map_err(|e| e.to_string())
        }),
    );
}
