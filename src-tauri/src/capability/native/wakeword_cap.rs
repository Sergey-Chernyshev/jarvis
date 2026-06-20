//! Капабилити wake-word (инкр. 10): только статус (Read). Включение always-on
//! микрофона — НЕ капабилити (приватно-критично, делается из панели/настроек);
//! wake-события плагинам отложены (спека §9). Приватный доступ плагина к речи —
//! через уже существующую `stt.transcribe` (грант на микрофон).

use std::sync::Arc;

use serde_json::{json, Value};

use crate::capability::contract::{CapabilityMeta, Provenance, RiskClass};
use crate::capability::registry::make_handler;
use crate::capability::DaemonRegistry;
use crate::daemon::Daemon;

pub fn register(reg: &mut DaemonRegistry) {
    reg.register(
        CapabilityMeta {
            id: "wakeword.status",
            class: RiskClass::Read,
            provenance: Provenance::Trusted,
            description: "Статус wake-word: включён/слушает/заглушён, модель на месте, \
                          порог, состояние аудио-входа. Только чтение.",
            input_schema: json!({ "type": "object" }),
        },
        make_handler(|d: Arc<Daemon>, _args: Value| async move { Ok(d.wake.status()) }),
    );
}
