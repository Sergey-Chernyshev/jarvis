//! Нативные фасады капабилити — тонкие обёртки над существующими сервисами
//! демона (§4, §12). Логику НЕ переписываем, только делегируем.

use serde_json::Value;

use super::DaemonRegistry;

mod audit_cap;
mod chats;
mod metrics;
mod notifications;
mod sessions;
mod settings_cap;
mod tasks;

/// Зарегистрировать все нативные капабилити в боевом реестре.
pub fn register_all(reg: &mut DaemonRegistry) {
    // фаза 2 — read
    sessions::register(reg);
    metrics::register(reg);
    notifications::register(reg);
    tasks::register(reg);
    settings_cap::register(reg);
    audit_cap::register(reg);
    chats::register(reg);
    // фаза 3 — control/settings (sessions.reply/queue/control/launch/interrupt,
    // settings.set) добавятся здесь же.
}

/// Достать строковый аргумент из JSON-объекта или вернуть внятную ошибку.
pub(crate) fn arg_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("нужен аргумент '{key}' (строка)"))
}
