//! Буфер отложенной отправки. Уверенный голосовой роут стейджит текст с видимым
//! окном отмены; необратимый tmux-`Enter` (в колбэке `send`) случается ТОЛЬКО
//! если окно истекло без `cancel`. Отмена снимает «живость» записи до вызова
//! колбэка — побочного эффекта не происходит. Это единственный корректный undo:
//! после `reply_core`→`tmux::reply` текст уже отправлен в сессию (см. спеку §3).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::time::Duration;

/// Запись стейджа: флаг «жив» + непрозрачный держатель (`_hold`). `_hold` несёт
/// single-flight-гард цикла: пока запись в карте — голосовой цикл считается
/// активным; запись удаляется ровно на срабатывании ИЛИ отмене, тогда гард
/// дропается и single-flight освобождается (см. RC1 — иначе re-wake в окне
/// отмены сносил staged-карточку, а паста всё равно срабатывала).
struct Entry {
    alive: Arc<AtomicBool>,
    _hold: Box<dyn Send>,
}

pub struct StageBuffer {
    /// nonce → запись. Колбэк проверяет (и гасит) флаг перед отправкой;
    /// `cancel` гасит и удаляет — гонку выигрывает первый `swap(false)`.
    live: Mutex<HashMap<String, Entry>>,
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
    /// `hold` живёт ровно пока запись в карте (срабатывание/отмена дропают её) —
    /// сюда передаётся single-flight-гард цикла, чтобы он держался всё окно отмены.
    pub fn stage<F>(
        self: &Arc<Self>,
        nonce: String,
        session_id: String,
        text: String,
        window: Duration,
        hold: Box<dyn Send>,
        send: F,
    ) where
        F: FnOnce(String, String) + Send + 'static,
    {
        let alive = Arc::new(AtomicBool::new(true));
        self.live.lock().unwrap().insert(nonce.clone(), Entry { alive: alive.clone(), _hold: hold });
        let this = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(window).await;
            // выигравший swap(false) исполняет отправку; проигравший (cancel) — нет.
            // remove дропает Entry (а с ним _hold/гард) — single-flight снимается.
            if alive.swap(false, Ordering::SeqCst) {
                this.live.lock().unwrap().remove(&nonce);
                send(session_id, text);
            }
        });
    }

    /// Отменить отложенную отправку. true — если запись была жива (успели до пасты).
    /// Удаление записи дропает держатель → single-flight освобождается сразу.
    pub fn cancel(&self, nonce: &str) -> bool {
        if let Some(entry) = self.live.lock().unwrap().remove(nonce) {
            entry.alive.swap(false, Ordering::SeqCst)
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

    /// Маркер «гард жив»: Drop ставит флаг — так проверяем, что держатель
    /// (single-flight) дропается ровно на срабатывании/отмене.
    struct DropFlag(Arc<AtomicBool>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

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
            Box::new(()),
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
            Box::new(()),
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
        buf.stage("n1".into(), "s".into(), "t".into(), Duration::from_millis(30), Box::new(()), move |_, _| {});
        assert!(buf.cancel("n1"));
        assert!(!buf.cancel("n1"));
        assert!(!buf.cancel("nope"));
    }

    #[tokio::test]
    async fn hold_dropped_on_cancel_releases_guard() {
        let dropped = Arc::new(AtomicBool::new(false));
        let buf = Arc::new(StageBuffer::new());
        buf.stage(
            "n1".into(),
            "s".into(),
            "t".into(),
            Duration::from_millis(200),
            Box::new(DropFlag(dropped.clone())),
            move |_, _| {},
        );
        assert!(!dropped.load(Ordering::SeqCst), "гард жив, пока окно открыто");
        assert!(buf.cancel("n1"));
        assert!(dropped.load(Ordering::SeqCst), "отмена дропает гард сразу");
    }

    #[tokio::test]
    async fn hold_dropped_on_fire_releases_guard() {
        let dropped = Arc::new(AtomicBool::new(false));
        let buf = Arc::new(StageBuffer::new());
        buf.stage(
            "n1".into(),
            "s".into(),
            "t".into(),
            Duration::from_millis(30),
            Box::new(DropFlag(dropped.clone())),
            move |_, _| {},
        );
        sleep(Duration::from_millis(90)).await;
        assert!(dropped.load(Ordering::SeqCst), "срабатывание дропает гард");
    }
}
