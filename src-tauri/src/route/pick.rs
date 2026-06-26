//! Реестр ожидающих выборов пикера. Зеркало `PendingConfirms`, но несёт ВЫБОР
//! (`session_id`), а не bool. Резолвится ТОЛЬКО из in-process IPC
//! (`voice_pick_resolve`), не из MCP-реестра — голосовой агент не может «сам
//! себя выбрать» (то же свойство изоляции, что у `agent_confirm`).

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::oneshot;

pub struct PendingPicks {
    map: Mutex<HashMap<String, oneshot::Sender<Option<String>>>>,
}

impl Default for PendingPicks {
    fn default() -> Self {
        Self { map: Mutex::new(HashMap::new()) }
    }
}

impl PendingPicks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Зарегистрировать ожидание; вернуть приёмник выбора.
    pub fn register(&self, nonce: String) -> oneshot::Receiver<Option<String>> {
        let (tx, rx) = oneshot::channel();
        self.map.lock().unwrap().insert(nonce, tx);
        rx
    }

    /// Доставить выбор (одноразово: запись удаляется). true — если nonce был.
    pub fn resolve(&self, nonce: &str, choice: Option<String>) -> bool {
        if let Some(tx) = self.map.lock().unwrap().remove(nonce) {
            let _ = tx.send(choice);
            true
        } else {
            false
        }
    }

    /// Снять ожидание → None (таймаут/Drop/закрытие тоста) без утечки записи.
    pub fn cancel(&self, nonce: &str) {
        if let Some(tx) = self.map.lock().unwrap().remove(nonce) {
            let _ = tx.send(None);
        }
    }
}

/// Генератор nonce — переиспользуем у боевого confirmer'а (16 байт urandom → hex).
pub use crate::capability::confirm_panel::gen_nonce;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolve_delivers_choice_single_use() {
        let p = PendingPicks::new();
        let rx = p.register("n1".into());
        assert!(p.resolve("n1", Some("sid-7".into())));
        assert_eq!(rx.await.unwrap(), Some("sid-7".to_string()));
        assert!(!p.resolve("n1", Some("x".into())), "повтор того же nonce — нет записи");
    }

    #[tokio::test]
    async fn cancel_resolves_none() {
        let p = PendingPicks::new();
        let rx = p.register("n2".into());
        p.cancel("n2");
        assert_eq!(rx.await.unwrap(), None);
    }

    #[test]
    fn unknown_nonce_false() {
        assert!(!PendingPicks::new().resolve("nope", Some("x".into())));
    }
}
