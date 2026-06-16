//! Нативные фасады капабилити — тонкие обёртки над существующими сервисами
//! демона (§4, §12). Логику НЕ переписываем, только делегируем. Заполняется
//! в фазе 2 (read) и фазе 3 (control/settings).

use super::DaemonRegistry;

/// Зарегистрировать все нативные капабилити в боевом реестре.
pub fn register_all(_reg: &mut DaemonRegistry) {
    // фаза 2: sessions/metrics/notifications/tasks/settings/audit/chats (read)
    // фаза 3: sessions.reply/queue/control/launch/interrupt, settings.set
}
