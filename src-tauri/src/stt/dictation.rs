//! Push-to-talk диктовка: хранит активную сессию захвата, транскрибирует и
//! вставляет текст при отпускании хоткея.
//!
//! Жизненный цикл:
//!   on_press()   → запустить микрофон (идемпотентно; двойное нажатие — no-op)
//!   on_release() → остановить, транскрибировать PCM → вставить текст (async, spawn)
//!
//! Всё fail-safe: любой шаг пишет в лог и возвращается без паники.

use std::sync::{Arc, Mutex};

use super::hub::{AudioHub, CaptureSession};
use super::SttService;

/// PTT-потребитель диктовки. Живёт в Arc внутри Daemon.
/// С инкр.10 захват идёт через общий `AudioHub` (единая зона ответственности),
/// а не через собственный cpal-стрим.
pub struct Dictation {
    service: Arc<SttService>,
    hub: Arc<AudioHub>,
    /// Активная сессия захвата аудио (None = не пишем).
    capturing: Mutex<Option<CaptureSession>>,
}

impl Dictation {
    pub fn new(service: Arc<SttService>, hub: Arc<AudioHub>) -> Arc<Self> {
        Arc::new(Dictation { service, hub, capturing: Mutex::new(None) })
    }

    /// Начать захват аудио при нажатии хоткея. Идемпотентно: если захват уже
    /// идёт (двойное срабатывание или авто-повтор клавиши) — пропуск.
    pub fn on_press(&self) {
        {
            let mut guard = match self.capturing.lock() {
                Ok(g) => g,
                Err(e) => {
                    crate::log::line(&format!("[dictation] on_press lock: {e}"));
                    return;
                }
            };
            if guard.is_some() {
                // Уже пишем — идемпотентный пропуск.
                return;
            }
            // Захват через общий хаб (без преролла — PTT пишет с момента нажатия).
            *guard = Some(self.hub.open_capture(false));
        } // лок захвата отпущен ДО прогрева (spawn питона его не держит)
        // Греем STT-модель ПОКА человек говорит: к отпусканию клавиши она уже
        // загружена (прячет cold-start после idle-stop). Неблокирующий вызов.
        self.service.warm();
        crate::log::line("[dictation] запись начата");
    }

    /// Остановить захват, транскрибировать и вставить текст. Если захват не
    /// шёл — no-op. Тяжёлая работа (transcribe) выполняется в отдельном потоке.
    pub fn on_release(&self) {
        let session = {
            let mut guard = match self.capturing.lock() {
                Ok(g) => g,
                Err(e) => {
                    crate::log::line(&format!("[dictation] on_release lock: {e}"));
                    return;
                }
            };
            guard.take()
        };

        let Some(session) = session else {
            // Нет активного захвата — no-op.
            return;
        };

        let service = self.service.clone();
        std::thread::spawn(move || {
            // ── finish() → PCM ───────────────────────────────────────────────
            let pcm = match session.finish() {
                Ok(p) => p,
                Err(e) => {
                    crate::log::line(&format!("[dictation] finish: {e}"));
                    return;
                }
            };
            if pcm.is_empty() {
                crate::log::line("[dictation] пустой PCM-буфер, пропуск");
                return;
            }

            // ── transcribe() → text ──────────────────────────────────────────
            let opts = service.options();
            let text = match service.transcribe(&pcm, &opts) {
                Ok(r) => r.text,
                Err(e) => {
                    crate::log::line(&format!("[dictation] transcribe: {e}"));
                    return;
                }
            };
            let text = text.trim().to_string();
            if text.is_empty() {
                crate::log::line("[dictation] пустой результат транскрипции, пропуск");
                return;
            }
            crate::log::line(&format!(
                "[dictation] транскрипция: «{}»",
                crate::util::ellipsize(&text, 80)
            ));

            // ── insert_text() → ⌘V ──────────────────────────────────────────
            if let Err(e) = super::insert::insert_text(&text) {
                crate::log::line(&format!("[dictation] insert_text: {e}"));
            }
        });
    }

    /// Вспомогательный предикат: возвращает true, если захват активен.
    /// Используется в тестах для проверки state machine.
    pub fn is_capturing(&self) -> bool {
        self.capturing.lock().map(|g| g.is_some()).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::config::SttConfig;

    fn make_dictation() -> Arc<Dictation> {
        // SttService с дефолтным конфигом (qwen3-0.6b, но сайдкар не запущен).
        let svc = SttService::new(SttConfig::default());
        // Хаб без AppHandle; в тестах ensure_running() — no-op (живой микрофон не трогаем).
        let hub = super::super::hub::AudioHub::new(None, None);
        Dictation::new(svc, hub)
    }

    // on_release без предшествующего on_press — no-op (не паникует)
    #[test]
    fn on_release_without_press_is_noop() {
        let d = make_dictation();
        assert!(!d.is_capturing());
        d.on_release(); // не должен паниковать
        assert!(!d.is_capturing());
    }

    // on_release с None-сессией идемпотентен: повторный вызов тоже no-op
    #[test]
    fn double_on_release_is_noop() {
        let d = make_dictation();
        d.on_release();
        d.on_release(); // второй — тоже нормально
        assert!(!d.is_capturing());
    }

    // Начальное состояние: захват не активен
    #[test]
    fn initial_state_not_capturing() {
        let d = make_dictation();
        assert!(!d.is_capturing());
    }

    // Двойной on_press не паникует (идемпотентный guard)
    // Реальный CaptureSession::start в тестах не открываем (нет микрофона CI),
    // тест проверяет только что is_capturing() не ломается при повторном вызове.
    #[test]
    fn double_press_guard_logic_no_panic() {
        let d = make_dictation();
        // Первый on_press может завершиться с ошибкой (нет реального микрофона),
        // но не должен паниковать.
        d.on_press();
        // Второй on_press: если первый не поставил сессию — всё равно no-op.
        d.on_press();
        // Независимо от результата — on_release не должен паниковать.
        d.on_release();
    }
}
