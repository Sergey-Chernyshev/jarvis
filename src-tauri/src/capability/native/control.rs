//! Пишущие капабилити сессий (§6) — class Control, поэтому гейт ВСЕГДА требует
//! подтверждения для агента (§8). Делегируют в общие ядра `ipc::reply_core` /
//! `set_model_core` / `set_effort_core` / `rename_core` — тот же путь, что у
//! панели (no dup).
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

    // Класс Control, хотя по сути это правка отображения: в сессию ничего не
    // вставляется, но side-effect есть — имя ложится в конфиг и переживает
    // сессию, значит подтверждение обязательно. Settings взять нельзя: гейт
    // читает аргументы settings-капабилити как патч конфига и отклонил бы
    // 'session_id' по SETTINGS_ALLOWLIST (см. тест в capability/mod.rs).
    reg.register(
        CapabilityMeta {
            id: "sessions.rename",
            class: RiskClass::Control,
            provenance: Provenance::Trusted,
            description: "Дать чату (сессии) своё имя вместо автозаголовка: под ним чат будет виден в списке, \
уведомлениях, тултипах и в истории проектов. Зови, когда человек просит переименовать чат или навести порядок \
в списке («назови этот чат ‹БД›», «переименуй чат про миграции»); нужную сессию найди через sessions.list. \
Пустая строка в 'title' СНИМАЕТ имя и возвращает автозаголовок. Имя переживает конец сессии и перезапуск: \
если ту же сессию поднимут через --resume, она снова будет с ним. На работу агента в сессии не влияет — \
это только отображение. Потолок — 60 символов, длиннее вернётся отказом.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string", "description": "id сессии" },
                    "title": {
                        "type": "string",
                        "description": "новое имя чата; пустая строка — снять имя и вернуть автозаголовок"
                    }
                },
                "required": ["session_id", "title"]
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move {
            let sid = arg_str(&args, "session_id")?;
            // title обязателен, но пустая строка легальна: это снятие имени
            let title = args
                .get("title")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "нужен аргумент 'title' (строка; пустая — снять имя)".to_string())?;
            Ok(ipc::rename_core(&d, &sid, title))
        }),
    );
}
