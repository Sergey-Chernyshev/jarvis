//! Control-капабилити сессий (§6) — class Control, поэтому гейт ВСЕГДА требует
//! подтверждения для агента (§8). Делегируют в общие ядра `ipc::reply_core` /
//! `set_model_core` / `set_effort_core` — тот же путь, что у панели (no dup).
//!
//! Мягкие провалы бизнес-логики ({ok:false, needsTmux, …}) возвращаются как
//! Ok(value) — структура сохраняется нетронутой (нужна рендереру). Только
//! невалидные входные данные дают Err (до исполнения).

use std::sync::Arc;

use serde_json::{json, Value};

use crate::capability::contract::{CapabilityMeta, Provenance, RiskClass};
use crate::capability::registry::make_handler;
use crate::capability::DaemonRegistry;
use crate::daemon::Daemon;
use crate::ipc;

use super::arg_str;

pub fn register(reg: &mut DaemonRegistry) {
    reg.register(
        CapabilityMeta {
            id: "sessions.reply",
            class: RiskClass::Control,
            provenance: Provenance::Trusted,
            description: "Отправить текст (промпт/ответ) в сессию Claude Code. ОПАСНО: инжект в сессию с доступом к ФС — требует подтверждения пользователя.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string" },
                    "text": { "type": "string", "description": "что отправить в сессию" }
                },
                "required": ["session_id", "text"]
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move {
            let sid = arg_str(&args, "session_id")?;
            let text = arg_str(&args, "text")?;
            Ok(ipc::reply_core(&d, sid, text).await)
        }),
    );

    reg.register(
        CapabilityMeta {
            id: "sessions.control",
            class: RiskClass::Control,
            provenance: Provenance::Trusted,
            description: "Сменить модель или effort сессии. Передай поле 'model' ИЛИ 'effort'.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string" },
                    "model": { "type": "string", "description": "напр. opus / sonnet" },
                    "effort": { "type": "string", "description": "напр. low / high / max" }
                },
                "required": ["session_id"]
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move {
            let sid = arg_str(&args, "session_id")?;
            let res = if let Some(m) = args.get("model").and_then(|v| v.as_str()) {
                ipc::set_model_core(&d, &sid, m).await
            } else if let Some(e) = args.get("effort").and_then(|v| v.as_str()) {
                ipc::set_effort_core(&d, &sid, e).await
            } else {
                return Err("нужно поле 'model' или 'effort'".into());
            };
            Ok(res)
        }),
    );
}
