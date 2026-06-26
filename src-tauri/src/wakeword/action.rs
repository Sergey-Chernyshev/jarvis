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

/// Прод-действие: видимая реакция + STT-захват(преролл+окно) + маршрутизация.
pub struct AgentWakeAction {
    hub: Arc<AudioHub>,
    stt: Arc<SttService>,
    app: tauri::AppHandle,
    /// Длина фикс-окна записи после wake, мс (полный VAD — под-проект 2).
    window_ms: u64,
    /// Один голосовой цикл за раз: повторный wake во время цикла игнорируется.
    sf: SingleFlight,
}

impl AgentWakeAction {
    pub fn new(hub: Arc<AudioHub>, stt: Arc<SttService>, app: tauri::AppHandle) -> Arc<Self> {
        // 6с (а не 4с) — стоп-гэп до VAD: фикс-окно реже режет нормальную команду.
        Arc::new(AgentWakeAction { hub, stt, app, window_ms: 6000, sf: SingleFlight::default() })
    }
}

impl WakeAction for AgentWakeAction {
    fn on_wake(&self, preroll: Vec<f32>) {
        // single-flight: если цикл уже идёт — не плодим второй захват/агента
        let Some(guard) = self.sf.try_enter() else {
            crate::log::line("[wake] уже слушаю — повторный wake проигнорирован");
            return;
        };

        let app = self.app.clone();
        let hub = self.hub.clone();
        let stt = self.stt.clone();
        let window_ms = self.window_ms;
        let d = crate::daemon::Daemon::get(&app);

        // совместимость: панель по-прежнему получает «detected» (пилюля в настройках)
        crate::windows::emit_to_panel(&app, "wake", &json!({ "phase": "detected" }));
        // видимая реакция в оверлее: «Слушаю…» + длина окна для кольца отсчёта
        crate::route::hud::emit(
            &d,
            crate::route::hud::Phase::Listening { secs: (window_ms / 1000) as u32 },
        );

        // Тяжёлая часть (захват+STT+маршрутизация) — в отдельном потоке. Гард
        // живёт до конца цикла (Drop снимает single-flight на ЛЮБОМ выходе).
        std::thread::spawn(move || {
            // `guard` держим в области потока: ранние return (ошибка захвата/STT)
            // дропают его → single-flight снимается и цикл закрыт. На успешном
            // пути он УЕЗЖАЕТ в route_transcript → StageBuffer и держится всё окно
            // отмены (RC1), а не снимается тут же по выходу из потока.

            // STT-захват реплики (преролл уже снят сервисом — добавляем сами)
            let cap = hub.open_capture(false);
            std::thread::sleep(std::time::Duration::from_millis(window_ms));
            let live = match cap.finish() {
                Ok(p) => p,
                Err(e) => {
                    crate::log::line(&format!("[wake] capture finish: {e}"));
                    crate::route::hud::emit(
                        &d,
                        crate::route::hud::Phase::Error { msg: "захват не удался".into() },
                    );
                    return;
                }
            };
            let mut pcm = preroll;
            pcm.extend_from_slice(&live);
            if pcm.is_empty() {
                crate::log::line("[wake] пустой PCM после wake, пропуск");
                crate::route::hud::emit(&d, crate::route::hud::Phase::Empty);
                return;
            }

            // транскрипция активным STT-движком
            let opts = stt.options();
            let text = match stt.transcribe(&pcm, &opts) {
                Ok(r) => r.text.trim().to_string(),
                Err(e) => {
                    crate::log::line(&format!("[wake] transcribe: {e}"));
                    crate::route::hud::emit(
                        &d,
                        crate::route::hud::Phase::Error { msg: "распознавание не удалось".into() },
                    );
                    return;
                }
            };
            crate::log::line(&format!("[wake] реплика: «{}»", crate::util::ellipsize(&text, 80)));
            // история «что я говорил» (общая с диктовкой)
            d.transcripts.push(&text, "wake");

            // Маршрутизация в Rust. Источник недоверенный (открытый микрофон):
            // побочный эффект (reply_core) — только через stage-окно/пикер,
            // см. модель доверия в спеке. Блокируемся в этом потоке (не tokio).
            // guard едет внутрь — держит single-flight до пасты/отмены.
            tauri::async_runtime::block_on(crate::route::route_transcript(d.clone(), text, guard));
        });
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
