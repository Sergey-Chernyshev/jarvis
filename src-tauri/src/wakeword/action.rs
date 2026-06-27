//! Действие при подтверждённом wake (после стадий 1+2). За интерфейсом
//! `WakeAction`, чтобы сервис wake тестировался моками, а прод-цепочка была
//! сменной.
//!
//! Ревизия 2 (под-проект «голосовая маршрутизация»): wake запускает Rust-роутер
//! (`crate::route`), а не одноразовый claude-агент. Цикл: видимая реакция →
//! STT-захват с прероллом → детерминированный скоринг → (tie-break) → (пикер) →
//! stage-then-send. Всё fail-safe; побочный эффект (`reply_core`) — только после
//! окна отмены или тапа пикера. Single-flight: один цикл за раз.

use std::sync::Arc;

use serde_json::json;

use crate::route::SingleFlight;
use crate::stt::hub::AudioHub;
use crate::stt::SttService;

/// Реакция на подтверждённый wake. `preroll` — снимок аудио ДО срабатывания
/// (16к моно), чтобы не потерять начало реплики.
pub trait WakeAction: Send + Sync {
    fn on_wake(&self, preroll: Vec<f32>);
}

/// Прод-действие: видимая реакция + запуск разговорного цикла (VAD/STT внутри).
pub struct AgentWakeAction {
    hub: Arc<AudioHub>,
    stt: Arc<SttService>,
    app: tauri::AppHandle,
    /// Один разговор за раз: повторный wake во время диалога игнорируется.
    sf: SingleFlight,
}

impl AgentWakeAction {
    pub fn new(hub: Arc<AudioHub>, stt: Arc<SttService>, app: tauri::AppHandle) -> Arc<Self> {
        Arc::new(AgentWakeAction { hub, stt, app, sf: SingleFlight::default() })
    }
}

impl WakeAction for AgentWakeAction {
    fn on_wake(&self, preroll: Vec<f32>) {
        // single-flight: цикл/разговор уже идёт — повторный wake игнорируем (он же
        // подавляет срабатывание детектора на собственный TTS Джарвиса).
        let Some(guard) = self.sf.try_enter() else {
            crate::log::line("[wake] уже слушаю — повторный wake проигнорирован");
            return;
        };

        let app = self.app.clone();
        let d = crate::daemon::Daemon::get(&app);

        // совместимость: панель по-прежнему получает «detected» (пилюля в настройках)
        crate::windows::emit_to_panel(&app, "wake", &json!({ "phase": "detected" }));

        // Разговорный мозг (п/п-2, веха 2b): start_conversation спавнит поток, держит
        // single-flight на весь диалог и крутит VAD-цикл (listen → STT → ход →
        // голосовой ответ), полудуплекс. guard уезжает внутрь.
        crate::convo::start_conversation(d, self.hub.clone(), self.stt.clone(), preroll, guard);
    }
}

#[cfg(test)]
pub mod test_support {
    use super::WakeAction;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Считающее действие — для тестов сервиса wake.
    pub struct CountingAction {
        pub count: AtomicUsize,
        pub last_preroll_len: AtomicUsize,
    }
    impl CountingAction {
        pub fn new() -> Self {
            CountingAction { count: AtomicUsize::new(0), last_preroll_len: AtomicUsize::new(0) }
        }
    }
    impl WakeAction for CountingAction {
        fn on_wake(&self, preroll: Vec<f32>) {
            self.count.fetch_add(1, Ordering::SeqCst);
            self.last_preroll_len.store(preroll.len(), Ordering::SeqCst);
        }
    }
}
