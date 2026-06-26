//! Буфер отложенной отправки. Уверенный голосовой роут стейджит текст с видимым
//! окном отмены; необратимый tmux-`Enter` (в колбэке `send`) случается ТОЛЬКО
//! если окно истекло без `cancel`. Отмена снимает «живость» записи до вызова
//! колбэка — побочного эффекта не происходит. Это единственный корректный undo:
//! после `reply_core`→`tmux::reply` текст уже отправлен в сессию (см. спеку §3).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::time::Duration;

pub struct StageBuffer {
    /// nonce → флаг «жив». Колбэк проверяет (и гасит) флаг перед отправкой;
    /// `cancel` гасит и удаляет — гонку выигрывает первый `swap(false)`.
    live: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl Default for StageBuffer {
    fn default() -> Self {
        Self { live: Mutex::new(HashMap::new()) }
    }
}

impl StageBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Поставить отправку через `window`; по истечении без отмены — `send(session_id, text)`
    /// ровно один раз. Если до срабатывания вызвали `cancel(nonce)` — `send` не зовётся.
    pub fn stage<F>(
        self: &Arc<Self>,
        nonce: String,
        session_id: String,
        text: String,
        window: Duration,
        send: F,
    ) where
        F: FnOnce(String, String) + Send + 'static,
    {
        let alive = Arc::new(AtomicBool::new(true));
        self.live.lock().unwrap().insert(nonce.clone(), alive.clone());
        let this = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(window).await;
            // выигравший swap(false) исполняет отправку; проигравший (cancel) — нет
            if alive.swap(false, Ordering::SeqCst) {
                this.live.lock().unwrap().remove(&nonce);
                send(session_id, text);
            }
        });
    }

    /// Отменить отложенную отправку. true — если запись была жива (успели до пасты).
    pub fn cancel(&self, nonce: &str) -> bool {
        if let Some(alive) = self.live.lock().unwrap().remove(nonce) {
            alive.swap(false, Ordering::SeqCst)
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tokio::time::sleep;

    #[tokio::test]
    async fn fires_after_window_when_not_cancelled() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let buf = Arc::new(StageBuffer::new());
        buf.stage(
            "n1".into(),
            "sid".into(),
            "txt".into(),
            Duration::from_millis(40),
            move |_sid, _txt| {
                c.fetch_add(1, Ordering::SeqCst);
            },
        );
        sleep(Duration::from_millis(120)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancel_before_window_prevents_send() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let buf = Arc::new(StageBuffer::new());
        buf.stage(
            "n1".into(),
            "sid".into(),
            "txt".into(),
            Duration::from_millis(80),
            move |_s, _t| {
                c.fetch_add(1, Ordering::SeqCst);
            },
        );
        assert!(buf.cancel("n1"));
        sleep(Duration::from_millis(140)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn double_cancel_is_safe_and_unknown_nonce_false() {
        let buf = Arc::new(StageBuffer::new());
        buf.stage("n1".into(), "s".into(), "t".into(), Duration::from_millis(30), move |_, _| {});
        assert!(buf.cancel("n1"));
        assert!(!buf.cancel("n1"));
        assert!(!buf.cancel("nope"));
    }
}
