//! Wake-word сервис (инкр.10, стадия 1) — оркестрирует тап общего аудио-хаба →
//! движок-детектор → антидребезг/warm-up/cooldown → гейт верификации → действие.
//!
//! Дизайн §2. Always-on, но лёгкий: на каждый 80мс-кадр гоняется крошечная модель
//! (или инертный стаб). Срабатывание → снимок преролла → verifier (v1 NullVerifier,
//! пропускает) → `WakeAction`. Всё fail-safe: паника движка изолирована, демон жив.

pub mod action;
pub mod config;
pub mod engine;
#[cfg(feature = "wakeword-ort")]
pub mod engine_oww;
pub mod verify;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use action::WakeAction;
use config::{VerifyConfig, WakeConfig};
use engine::{build_engine, WakeWordEngine};
use verify::{build_verifier, SpeakerVerifier};

use crate::stt::hub::AudioHub;

/// Кадры прогрева: пока буферы детектора не наполнились — не будим (как в openWakeWord).
const WARMUP_FRAMES: u32 = 5;
/// Кулдаун после срабатывания: ~2с (25×80мс) — не перезапускаем на той же фразе.
const COOLDOWN_FRAMES: u32 = 25;

// ─── Чистый автомат решения (warm-up → debounce → cooldown) ──────────────────

/// Принимает скор кадра, решает «сработало ли». Чистый, без потоков/движка.
struct Detector {
    threshold: f32,
    debounce: u32,
    seen: u32,
    consec: u32,
    cooldown: u32,
}

impl Detector {
    fn new(threshold: f32, debounce: u32) -> Detector {
        Detector { threshold, debounce: debounce.max(1), seen: 0, consec: 0, cooldown: 0 }
    }

    /// Вернуть true ровно на кадре, где срабатывание подтверждено.
    fn feed(&mut self, score: f32) -> bool {
        self.seen = self.seen.saturating_add(1);
        if self.cooldown > 0 {
            self.cooldown -= 1;
            return false;
        }
        if self.seen <= WARMUP_FRAMES {
            return false; // прогрев
        }
        if score >= self.threshold {
            self.consec += 1;
            if self.consec >= self.debounce {
                self.consec = 0;
                self.cooldown = COOLDOWN_FRAMES;
                return true;
            }
        } else {
            self.consec = 0;
        }
        false
    }
}

// ─── Сессия детекции (движок + автомат + гейт + действие) ────────────────────

/// Объединяет движок и автомат на один проход кадра. Живёт в consumer-потоке;
/// в тестах создаётся напрямую с `FixtureEngine`/`CountingAction`.
struct WakeSession {
    engine: Box<dyn WakeWordEngine>,
    detector: Detector,
    verifier: Arc<dyn SpeakerVerifier>,
    verify_cfg: VerifyConfig,
    action: Arc<dyn WakeAction>,
    hub: Arc<AudioHub>,
}

impl WakeSession {
    fn on_frame(&mut self, frame: &[f32]) {
        // fail-safe: паника движка (напр. баг в ort-инференсе) не валит поток —
        // кадр трактуем как «тишина» и сбрасываем движок.
        let score = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.engine.push_frame(frame)
        })) {
            Ok(s) => s.unwrap_or(0.0),
            Err(_) => {
                crate::log::line("[wake] паника движка — кадр пропущен, сброс");
                self.engine.reset();
                0.0
            }
        };
        if self.detector.feed(score) {
            // снимок аудио срабатывания (преролл) — для верификации и STT-захвата
            let preroll = self.hub.preroll();
            if verify::passes(&*self.verifier, &self.verify_cfg, &preroll) {
                self.action.on_wake(preroll);
            } else {
                crate::log::line("[wake] отклонено верификацией говорящего");
            }
            self.engine.reset();
        }
    }
}

// ─── Сервис ──────────────────────────────────────────────────────────────────

struct Running {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

struct Inner {
    wake_cfg: WakeConfig,
    verify_cfg: VerifyConfig,
    running: Option<Running>,
}

/// Сервис wake-word. Живёт в `Arc<WakeWord>` внутри `Daemon`.
pub struct WakeWord {
    hub: Arc<AudioHub>,
    verifier: Arc<dyn SpeakerVerifier>,
    action: Arc<dyn WakeAction>,
    inner: Mutex<Inner>,
    /// Сериализует переходы жизненного цикла (start/stop/tick/set_enabled/
    /// reconfigure), чтобы супервизорный tick не разъехался с IPC-вызовом и не
    /// оставил «осиротевший» consumer-поток с включённым микрофоном после выключения.
    transition: Mutex<()>,
}

impl WakeWord {
    pub fn new(
        hub: Arc<AudioHub>,
        wake_cfg: WakeConfig,
        verify_cfg: VerifyConfig,
        action: Arc<dyn WakeAction>,
    ) -> Arc<WakeWord> {
        let verifier: Arc<dyn SpeakerVerifier> = Arc::from(build_verifier(&verify_cfg));
        let svc = Arc::new(WakeWord {
            hub,
            verifier,
            action,
            inner: Mutex::new(Inner {
                wake_cfg: wake_cfg.clone(),
                verify_cfg,
                running: None,
            }),
            transition: Mutex::new(()),
        });
        if wake_cfg.enabled {
            svc.start();
        }
        svc
    }

    /// Запустить consumer-поток (идемпотентно). Публичная обёртка под lock перехода.
    pub fn start(self: &Arc<Self>) {
        let _t = self.transition.lock().unwrap();
        self.start_locked();
    }

    /// Спавн consumer-потока. Вызывать ТОЛЬКО держа `transition`.
    fn start_locked(self: &Arc<Self>) {
        let mut g = self.inner.lock().unwrap();
        if g.running.is_some() {
            return;
        }
        // защита: не поднимать поток, если выключено (tick/гонка)
        if !g.wake_cfg.enabled {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let wake_cfg = g.wake_cfg.clone();
        let verify_cfg = g.verify_cfg.clone();
        let hub = self.hub.clone();
        let verifier = self.verifier.clone();
        let action = self.action.clone();
        let stop_t = stop.clone();
        let join = std::thread::spawn(move || {
            run_loop(hub, wake_cfg, verify_cfg, verifier, action, stop_t);
        });
        g.running = Some(Running { stop, join: Some(join) });
        crate::log::line("[wake] детектор запущен (always-on)");
    }

    /// Остановить consumer-поток (идемпотентно).
    pub fn stop(&self) {
        let _t = self.transition.lock().unwrap();
        self.stop_locked();
    }

    /// Останов. Вызывать ТОЛЬКО держа `transition`.
    fn stop_locked(&self) {
        let running = { self.inner.lock().unwrap().running.take() };
        if let Some(mut r) = running {
            r.stop.store(true, Ordering::SeqCst);
            if let Some(j) = r.join.take() {
                let _ = j.join();
            }
            crate::log::line("[wake] детектор остановлен");
        }
    }

    /// Вкл/выкл из настроек/панели. Поднимает/гасит поток (атомарно к tick).
    pub fn set_enabled(self: &Arc<Self>, on: bool) {
        let _t = self.transition.lock().unwrap();
        self.inner.lock().unwrap().wake_cfg.enabled = on;
        if on {
            self.start_locked();
        } else {
            self.stop_locked();
        }
    }

    /// Переконфигурировать вживую (порог/модель/верификация). Если включён —
    /// перезапускаем поток, чтобы подхватить новый движок/порог (атомарно к tick).
    pub fn reconfigure(self: &Arc<Self>, wake_cfg: WakeConfig, verify_cfg: VerifyConfig) {
        let _t = self.transition.lock().unwrap();
        let enabled = wake_cfg.enabled;
        {
            let mut g = self.inner.lock().unwrap();
            g.wake_cfg = wake_cfg;
            g.verify_cfg = verify_cfg;
        }
        self.stop_locked();
        if enabled {
            self.start_locked();
        }
    }

    /// Тик-супервизор: если включён, но поток умер (паника/выход) — поднять заново.
    /// Под `transition`, поэтому не разъезжается с конкурентным set_enabled/reconfigure
    /// (перепроверяем enabled внутри критической секции перед рестартом).
    pub fn tick(self: &Arc<Self>) {
        let _t = self.transition.lock().unwrap();
        let need_restart = {
            let g = self.inner.lock().unwrap();
            g.wake_cfg.enabled
                && g.running.as_ref().map(|r| r.join.as_ref().map(|j| j.is_finished()).unwrap_or(true)).unwrap_or(true)
        };
        if need_restart {
            self.stop_locked();
            self.start_locked(); // start_locked сам перепроверит enabled
        }
    }

    pub fn dispose(&self) {
        self.stop();
    }

    /// Статус для панели/капабилити.
    pub fn status(&self) -> serde_json::Value {
        let g = self.inner.lock().unwrap();
        let running = g.running.is_some();
        serde_json::json!({
            "enabled": g.wake_cfg.enabled,
            "running": running,
            "listening": running && self.hub.state() == crate::stt::hub::AudioState::Listening,
            "muted": self.hub.is_muted(),
            "engine": g.wake_cfg.engine,
            "threshold": g.wake_cfg.threshold,
            "model_present": models_present(),
            "verification_enabled": g.verify_cfg.enabled,
            "audio_state": self.hub.state().as_str(),
            "mic": crate::stt::mic_permission::status().as_str(),
            "mic_silent": self.hub.is_mic_silent(),
        })
    }
}

/// Папка моделей wake-word (~/.jarvis/wakeword).
pub fn models_dir() -> std::path::PathBuf {
    crate::util::jarvis_dir().join("wakeword")
}

/// Все ли 3 ONNX-модели на месте (мел + эмбеддер + детектор фразы).
pub fn models_present() -> bool {
    let d = models_dir();
    ["melspectrogram.onnx", "embedding_model.onnx", "hey_jarvis_v0.1.onnx"]
        .iter()
        .all(|f| d.join(f).exists())
}

/// Тело consumer-потока: тап хаба → on_frame, пока не попросят стоп.
fn run_loop(
    hub: Arc<AudioHub>,
    wake_cfg: WakeConfig,
    verify_cfg: VerifyConfig,
    verifier: Arc<dyn SpeakerVerifier>,
    action: Arc<dyn WakeAction>,
    stop: Arc<AtomicBool>,
) {
    let tap = hub.subscribe_wake();
    let engine = build_engine(&wake_cfg);
    crate::log::line(&format!(
        "[wake] consumer-поток: движок «{}», порог {:.2}",
        engine.name(),
        wake_cfg.threshold
    ));
    let mut session = WakeSession {
        engine,
        detector: Detector::new(wake_cfg.threshold, wake_cfg.debounce),
        verifier,
        verify_cfg,
        action,
        hub: hub.clone(),
    };
    while !stop.load(Ordering::SeqCst) {
        if let Some(frame) = tap.recv_timeout(Duration::from_millis(200)) {
            session.on_frame(&frame);
        }
    }
    // tap уронится здесь → отписка от хаба (Drop), спрос падает.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wakeword::action::test_support::CountingAction;
    use crate::wakeword::engine::{FixtureEngine, WAKE_FRAME_LEN};
    use crate::wakeword::verify::NullVerifier;
    use std::sync::atomic::Ordering;

    // ── Detector: чистый автомат ─────────────────────────────────────────────

    #[test]
    fn detector_warmup_suppresses_first_frames() {
        let mut d = Detector::new(0.5, 1);
        // первые WARMUP_FRAMES высоких кадров — не будят
        for _ in 0..WARMUP_FRAMES {
            assert!(!d.feed(1.0), "во время прогрева не будим");
        }
        // следующий — будит (debounce=1)
        assert!(d.feed(1.0));
    }

    #[test]
    fn detector_debounce_requires_consecutive() {
        let mut d = Detector::new(0.5, 3);
        for _ in 0..WARMUP_FRAMES {
            d.feed(0.0);
        }
        assert!(!d.feed(1.0)); // 1/3
        assert!(!d.feed(1.0)); // 2/3
        assert!(d.feed(1.0)); // 3/3 → сработало
    }

    #[test]
    fn detector_low_frame_resets_consec() {
        let mut d = Detector::new(0.5, 2);
        for _ in 0..WARMUP_FRAMES {
            d.feed(0.0);
        }
        assert!(!d.feed(1.0)); // 1/2
        assert!(!d.feed(0.1)); // сброс
        assert!(!d.feed(1.0)); // снова 1/2
        assert!(d.feed(1.0)); // 2/2
    }

    #[test]
    fn detector_cooldown_blocks_retrigger() {
        let mut d = Detector::new(0.5, 1);
        for _ in 0..WARMUP_FRAMES {
            d.feed(0.0);
        }
        assert!(d.feed(1.0), "первое срабатывание");
        // в кулдаун — даже высокие кадры не будят
        for _ in 0..COOLDOWN_FRAMES {
            assert!(!d.feed(1.0), "кулдаун блокирует повтор");
        }
        assert!(d.feed(1.0), "после кулдауна снова можно");
    }

    #[test]
    fn detector_threshold_respected() {
        let mut d = Detector::new(0.8, 1);
        for _ in 0..WARMUP_FRAMES {
            d.feed(0.0);
        }
        assert!(!d.feed(0.79), "ниже порога — нет");
        assert!(d.feed(0.81), "выше порога — да");
    }

    // ── WakeSession: движок + гейт + действие ────────────────────────────────

    fn frame_with_score(score: f32) -> Vec<f32> {
        let mut f = vec![0.0f32; WAKE_FRAME_LEN];
        f[0] = score; // FixtureEngine читает frame[0] как скор
        f
    }

    fn make_session(action: Arc<CountingAction>) -> WakeSession {
        WakeSession {
            engine: Box::new(FixtureEngine),
            detector: Detector::new(0.5, 2),
            verifier: Arc::new(NullVerifier),
            verify_cfg: VerifyConfig::default(),
            action,
            hub: AudioHub::new(None, None),
        }
    }

    #[test]
    fn session_fires_action_after_warmup_and_debounce() {
        let action = Arc::new(CountingAction::new());
        let mut s = make_session(action.clone());
        // прогрев
        for _ in 0..WARMUP_FRAMES {
            s.on_frame(&frame_with_score(0.0));
        }
        assert_eq!(action.count.load(Ordering::SeqCst), 0);
        // debounce=2 высоких кадра → срабатывание
        s.on_frame(&frame_with_score(0.9));
        s.on_frame(&frame_with_score(0.9));
        assert_eq!(action.count.load(Ordering::SeqCst), 1, "действие вызвано ровно раз");
    }

    #[test]
    fn session_stub_never_fires() {
        let action = Arc::new(CountingAction::new());
        let mut s = WakeSession {
            engine: Box::new(engine::StubEngine), // инертный
            detector: Detector::new(0.5, 1),
            verifier: Arc::new(NullVerifier),
            verify_cfg: VerifyConfig::default(),
            action: action.clone(),
            hub: AudioHub::new(None, None),
        };
        for _ in 0..200 {
            s.on_frame(&frame_with_score(1.0));
        }
        assert_eq!(action.count.load(Ordering::SeqCst), 0, "стаб не будит никогда");
    }

    #[test]
    fn session_verification_reject_blocks_action() {
        // включённый мок-верификатор, который всегда отвергает
        struct Reject;
        impl SpeakerVerifier for Reject {
            fn enabled(&self) -> bool {
                true
            }
            fn verify(&self, _a: &[f32]) -> f32 {
                0.0
            }
            fn enroll(&self, _u: &[Vec<f32>]) -> Result<(), String> {
                Ok(())
            }
        }
        let action = Arc::new(CountingAction::new());
        let mut s = WakeSession {
            engine: Box::new(FixtureEngine),
            detector: Detector::new(0.5, 1),
            verifier: Arc::new(Reject),
            verify_cfg: VerifyConfig { enabled: true, threshold: 0.5, profile: None },
            action: action.clone(),
            hub: AudioHub::new(None, None),
        };
        for _ in 0..WARMUP_FRAMES {
            s.on_frame(&frame_with_score(0.0));
        }
        s.on_frame(&frame_with_score(0.9)); // детект, но верификация отвергнет
        assert_eq!(action.count.load(Ordering::SeqCst), 0, "верификация заблокировала действие");
    }

    // ── Сервис: вкл/выкл/статус не паникует ──────────────────────────────────

    #[test]
    fn service_start_stop_idempotent() {
        let hub = AudioHub::new(None, None);
        let action: Arc<dyn WakeAction> = Arc::new(CountingAction::new());
        let svc = WakeWord::new(hub, WakeConfig::default(), VerifyConfig::default(), action);
        svc.start();
        svc.start(); // идемпотентно
        svc.stop();
        svc.stop(); // идемпотентно
    }

    #[test]
    fn service_disabled_by_default_not_running() {
        let hub = AudioHub::new(None, None);
        let action: Arc<dyn WakeAction> = Arc::new(CountingAction::new());
        let svc = WakeWord::new(hub, WakeConfig::default(), VerifyConfig::default(), action);
        let st = svc.status();
        assert_eq!(st["enabled"], false);
        assert_eq!(st["running"], false);
    }

    #[test]
    fn service_set_enabled_toggles_running() {
        let hub = AudioHub::new(None, None);
        let action: Arc<dyn WakeAction> = Arc::new(CountingAction::new());
        let svc = WakeWord::new(hub, WakeConfig::default(), VerifyConfig::default(), action);
        svc.set_enabled(true);
        assert_eq!(svc.status()["running"], true);
        svc.set_enabled(false);
        assert_eq!(svc.status()["running"], false);
    }
}
