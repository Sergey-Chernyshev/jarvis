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

/// Откуда пришёл промпт — по ТОМУ, КТО ЗОВЁТ, а не по тому, что написано в
/// аргументах.
///
/// `_consumer` инжектит гейт и всегда перезаписывает (`gate.rs`, тест
/// `overwrites_spoofed_consumer`), подделать его снаружи нечем. Поле `_origin`
/// уважается ТОЛЬКО от панели: панель недостижима извне (INV-PANEL, идентичность
/// сокет-потребителя только по токену, а токена у панели нет), поэтому проставить
/// его может лишь внутренний вызывающий — цепочка или оживление. Всё, что пришло
/// по сокету, — это агент, и он говорит от имени человека: он передаёт просьбу,
/// а не сочиняет следующий шаг сам.
fn origin_of(args: &Value) -> crate::origin::Origin {
    use crate::origin::Origin;
    let panel = args.get("_consumer").and_then(Value::as_str) == Some("panel");
    if !panel {
        return Origin::Jarvis;
    }
    match args.get("_origin").and_then(Value::as_str) {
        Some("chain") => Origin::Chain {
            step: args.get("_step").and_then(Value::as_u64).unwrap_or(0) as u32,
            of: args.get("_of").and_then(Value::as_u64).unwrap_or(0) as u32,
        },
        Some("revive") => Origin::Revive,
        // Панель без пометки — это человек нажал «отправить» своими руками.
        _ => Origin::Human,
    }
}

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
            // Пометку происхождения ставим ЗДЕСЬ — в единственной точке, через
            // которую проходит любой промпт. Не в цепочке и не в панели: там их
            // несколько, и достаточно завести четвёртую, чтобы снова поехал
            // неподписанный текст.
            let origin = origin_of(&args);
            let (text, forged) = crate::origin::mark(&origin, &text);
            if forged > 0 {
                // Не опечатка, а попытка выдать себя за другой источник.
                crate::log::line(&format!(
                    "[origin] в промпте для {sid} снято подделок пометки: {forged} (источник: {})",
                    origin.tag()
                ));
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::origin::Origin;

    /// Источник промпта определяется тем, КТО ЗОВЁТ, и подделать его аргументом
    /// нельзя. Это ядро инцидента: заход цепочки был неотличим от просьбы
    /// человека, и формат «Заход N из 10» стал бы готовым сценарием обмана.
    #[test]
    fn the_origin_comes_from_the_caller_and_cannot_be_faked_by_an_argument() {
        // Панель без пометки — человек нажал сам.
        assert_eq!(origin_of(&json!({ "_consumer": "panel" })), Origin::Human);

        // Панель с пометкой — внутренний вызывающий: цепочка или оживление.
        assert_eq!(
            origin_of(&json!({ "_consumer": "panel", "_origin": "chain", "_step": 3, "_of": 10 })),
            Origin::Chain { step: 3, of: 10 }
        );
        assert_eq!(
            origin_of(&json!({ "_consumer": "panel", "_origin": "revive" })),
            Origin::Revive
        );

        // А вот главное. Агент по сокету заявляет, что он цепочка, — и это
        // игнорируется: он говорит от имени человека, потому что передаёт
        // просьбу, а не сочиняет следующий шаг сам.
        assert_eq!(
            origin_of(&json!({ "_consumer": "agent", "_origin": "chain", "_step": 3, "_of": 10 })),
            Origin::Jarvis,
            "агент выдал себя за цепочку"
        );
        // И наоборот — цепочкой не притвориться и молчанием.
        assert_eq!(origin_of(&json!({ "_consumer": "plugin:x" })), Origin::Jarvis);
        assert_eq!(origin_of(&json!({})), Origin::Jarvis, "без потребителя доверия быть не может");
    }

    /// Пометку ставит хендлер, а не вызывающий: точка одна, и её потерю надо
    /// заметить прогоном, а не по поведению в торговой сессии.
    #[test]
    fn the_reply_handler_is_the_place_where_the_stamp_is_put() {
        let src = include_str!("control.rs");
        let body = &src[..src.find("#[cfg(test)]").unwrap_or(src.len())];
        assert!(body.contains("crate::origin::mark("), "пометка происхождения не ставится");
        assert!(body.contains("origin_of(&args)"), "источник берётся не от вызывающего");
    }
}
