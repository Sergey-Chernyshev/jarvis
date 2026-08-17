//! Сторож главного потока.
//!
//! «Зависло» — самый бесполезный баг-репорт из возможных: он не говорит ни где,
//! ни надолго ли. Сторож превращает его в строку лога с длительностью. Раз в
//! пару секунд он просит главный поток отметиться; если отметка не обновляется,
//! значит поток чем-то занят — а занят он может быть только синхронной командой
//! или замком, за которым та встала.
//!
//! Сам сторож главный поток не трогает ничем тяжёлым: одна атомарная запись.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;

/// Насколько долгая задержка считается зависанием, а не просто занятостью.
///
/// Отрисовка кадра, открытие окна и подобное укладываются в доли секунды.
/// Три секунды — это уже то, что человек называет «встало».
const STUCK_MS: i64 = 3_000;

/// Как часто спрашивать. Чаще незачем: сторож ищет секунды, а не миллисекунды.
const BEAT: Duration = Duration::from_secs(2);

pub fn start(app: AppHandle) {
    let beat = Arc::new(AtomicI64::new(crate::util::now_ms()));
    std::thread::spawn(move || {
        let mut reported = false;
        loop {
            std::thread::sleep(BEAT);
            let b = beat.clone();
            let now = crate::util::now_ms();
            // Просьба отметиться уходит в очередь главного потока. Пока он
            // занят, она там и лежит — именно это нам и нужно измерить.
            let _ = app.run_on_main_thread(move || {
                b.store(crate::util::now_ms(), Ordering::SeqCst);
            });
            let behind = now - beat.load(Ordering::SeqCst);
            if behind >= STUCK_MS && !reported {
                crate::log::line(&format!(
                    "[watchdog] главный поток не отвечает {} с — окно стоит",
                    behind / 1000
                ));
                reported = true;
            } else if behind < STUCK_MS && reported {
                crate::log::line(&format!(
                    "[watchdog] главный поток отпустило, простой {} с",
                    behind / 1000
                ));
                reported = false;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Порог должен отделять обычную занятость от того, что человек называет
    /// зависанием. Кадр — это миллисекунды, «встало» — это секунды.
    #[test]
    fn threshold_is_about_seconds_not_frames() {
        assert!(STUCK_MS >= 1_000, "меньше секунды — это ещё не зависание");
        assert!(STUCK_MS <= 10_000, "больше десяти секунд человек уже не дождётся");
        assert!(BEAT.as_millis() as i64 <= STUCK_MS, "спрашивать реже порога бессмысленно");
    }
}
