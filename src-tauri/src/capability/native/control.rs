//! Control-капабилити сессий (§6) — class Control, поэтому гейт ВСЕГДА требует
//! подтверждения для агента (§8). Делегируют в общие ядра `ipc::reply_core` /
//! `set_model_core` / `set_effort_core` — тот же путь, что у панели (no dup).

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
            panel_result(ipc::reply_core(&d, sid, text).await)
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
            panel_result(res)
        }),
    );
}

/// Привести панельный ответ ({ok:bool,…}) к Result капабилити: ok→значение,
/// иначе — внятная ошибка (включая needsTmux).
fn panel_result(v: Value) -> Result<Value, String> {
    if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        return Ok(v);
    }
    let msg = v
        .get("error")
        .and_then(|e| e.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            v.get("needsTmux")
                .and_then(|b| b.as_bool())
                .filter(|&b| b)
                .map(|_| "сессия не в tmux — управление недоступно".to_string())
        })
        .unwrap_or_else(|| "не удалось выполнить".to_string());
    Err(msg)
}
