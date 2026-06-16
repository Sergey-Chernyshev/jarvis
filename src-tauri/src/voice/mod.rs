//! Голосовой/TTS модуль: синтез русских фраз.

pub mod composer;
pub mod config;
pub mod engine;
pub mod numerals;
pub mod player;
pub mod queue;
pub mod sidecar;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use composer::{Composer, Priority, SpeechSignals, TemplateComposer, Utterance};
use config::VoiceConfig;
use engine::{build_engine, TtsEngine, VoiceSel};
use player::{Play, RodioPlayer};
use queue::SpeechQueue;

/// Фиксированный порт Silero-сайдкара на localhost.
const SILERO_PORT: u16 = 8731;

/// Голосовой сервис: композитор + очередь + движок + проигрыватель на фоне.
/// Владеет супервизором Silero-сайдкара (старт/перезапуск/стоп).
pub struct Voice {
    composer: Box<dyn Composer>,
    engine: Box<dyn TtsEngine>,
    player: Arc<dyn Play>,
    // спикер живой (Silero берёт его per-запрос) → меняется из настроек без
    // перезапуска; частота фиксирована на старте
    speaker: Mutex<String>,
    rate: Mutex<String>, // темп речи — тоже живой
    sample_rate: u32,
    queue: Arc<(Mutex<SpeechQueue>, Condvar)>,
    mute: Arc<AtomicBool>,
    sidecar: Arc<sidecar::Sidecar>,
    app: tauri::AppHandle, // для удержания/продления тоста на время речи
}

impl Voice {
    pub fn new(
        cfg: &VoiceConfig,
        silero_dir: std::path::PathBuf,
        app: tauri::AppHandle,
    ) -> Arc<Self> {
        // Silero — единственный движок: поднимаем сайдкар и берём его base.
        let speaker = if cfg.speaker.is_empty() { "xenia".to_string() } else { cfg.speaker.clone() };
        let sidecar = Arc::new(sidecar::Sidecar::new(silero_dir, speaker.clone(), "v4_ru".into(), SILERO_PORT));
        sidecar.ensure_started();
        let engine = build_engine(sidecar.base());
        let v = Arc::new(Voice {
            composer: Box::new(TemplateComposer),
            engine,
            player: Arc::new(RodioPlayer::new()),
            speaker: Mutex::new(speaker),
            rate: Mutex::new(if cfg.rate.is_empty() { "fast".to_string() } else { cfg.rate.clone() }),
            sample_rate: cfg.sample_rate,
            queue: Arc::new((Mutex::new(SpeechQueue::new()), Condvar::new())),
            mute: Arc::new(AtomicBool::new(cfg.mute)),
            sidecar,
            app,
        });
        v.clone().spawn_worker();
        v
    }

    /// Текущий выбор голоса для движка (спикер живой).
    fn voice_sel(&self) -> VoiceSel {
        VoiceSel {
            speaker: self.speaker.lock().unwrap().clone(),
            sample_rate: self.sample_rate,
            rate: self.rate.lock().unwrap().clone(),
        }
    }

    /// Сменить спикера на лету (без перезапуска): Silero берёт его per-запрос.
    pub fn set_speaker(&self, speaker: &str) {
        if !speaker.is_empty() {
            *self.speaker.lock().unwrap() = speaker.to_string();
        }
    }
    pub fn speaker(&self) -> String { self.speaker.lock().unwrap().clone() }

    /// Сменить темп речи на лету (x-slow|slow|medium|fast|x-fast).
    pub fn set_rate(&self, rate: &str) {
        if matches!(rate, "x-slow" | "slow" | "medium" | "fast" | "x-fast") {
            *self.rate.lock().unwrap() = rate.to_string();
        }
    }
    pub fn rate(&self) -> String { self.rate.lock().unwrap().clone() }

    /// Тик супервизора: перезапустить Silero-сайдкар, если он умер.
    pub fn tick(&self) {
        self.sidecar.restart_if_dead();
    }

    /// Погасить Silero-сайдкар на выходе демона.
    pub fn dispose(&self) {
        self.sidecar.stop();
    }

    pub fn set_mute(&self, on: bool) {
        self.mute.store(on, Ordering::SeqCst);
        if on { self.player.stop(); } // мгновенно глушим текущую
    }
    pub fn is_muted(&self) -> bool { self.mute.load(Ordering::SeqCst) }

    /// Композирует сигналы в реплику и кладёт в очередь (fail-safe).
    pub fn speak(&self, signals: SpeechSignals) {
        if self.is_muted() { return; }
        let Some(u) = self.composer.compose(&signals) else { return; };
        let high = u.priority == Priority::NeedHuman;
        let (m, cv) = &*self.queue;
        let added = m.lock().unwrap().enqueue(u);
        if added {
            if high { self.player.stop(); } // прерываем текущую низкоприоритетную
            cv.notify_one();
        }
    }

    /// Озвучить РЕАЛЬНОЕ уведомление: тот же текст, что показал тост (title+body).
    /// kind: "done"|"waiting"|"limit"|… → приоритет и дедуп. `toast_id` — карточку
    /// держим открытой, пока говорим, и продлеваем после. Fail-safe.
    pub fn speak_text(&self, title: &str, body: &str, kind: &str, toast_id: Option<&str>) {
        if self.is_muted() {
            return;
        }
        let text = notif_tts_text(title, body);
        if text.is_empty() {
            return;
        }
        let high = matches!(kind, "waiting" | "limit");
        let u = Utterance {
            text,
            priority: if high { Priority::NeedHuman } else { Priority::Done },
            // дедуп по содержимому: повтор того же тоста не читаем дважды,
            // но разные «что сделано» — каждое озвучиваем
            dedup_key: format!("{kind}:{title}:{body}"),
            coalesce_group: None,
            toast_id: toast_id.map(String::from),
        };
        let (m, cv) = &*self.queue;
        if m.lock().unwrap().enqueue(u) {
            if high {
                self.player.stop(); // «нужен ты»/лимит прерывают текущую «готово»
            }
            cv.notify_one();
        }
    }

    pub fn test_phrase(&self, text: &str) {
        let (m, cv) = &*self.queue;
        m.lock().unwrap().enqueue(Utterance {
            text: text.to_string(), priority: Priority::Done,
            dedup_key: format!("test:{text}"), coalesce_group: None, toast_id: None,
        });
        cv.notify_one();
    }

    pub fn warmup(&self) { self.engine.warmup(&self.voice_sel()); }
    pub fn engine_name(&self) -> &'static str { self.engine.name() }
    /// PID Silero-сайдкара (для метрик диагностики); None, если ещё не запущен.
    pub fn sidecar_pid(&self) -> Option<u32> { self.sidecar.pid() }
    /// Глубина очереди речи (для метрик).
    pub fn queue_len(&self) -> usize { self.queue.0.lock().unwrap().len() }
    pub fn engine_available(&self) -> bool { self.engine.available() }

    fn spawn_worker(self: Arc<Self>) {
        std::thread::spawn(move || loop {
            let next = {
                let (m, cv) = &*self.queue;
                let mut g = m.lock().unwrap();
                while g.is_empty() { g = cv.wait(g).unwrap(); }
                g.next()
            };
            let Some(u) = next else { continue; };
            if self.is_muted() { continue; }
            let vs = self.voice_sel();
            let t_synth = crate::metrics::now();
            match self.engine.synthesize(&u.text, &vs) {
                Ok(wav) => {
                    crate::metrics::record("tts_synth", t_synth, serde_json::json!({
                        "engine": self.engine.name(), "chars": u.text.chars().count(), "bytes": wav.len(),
                    }));
                    // держим тост открытым на время речи
                    if let Some(tid) = &u.toast_id {
                        crate::windows::toast_hold(&self.app, tid);
                    }
                    let t_play = crate::metrics::now();
                    self.player.play_blocking(wav);
                    crate::metrics::record("tts_play", t_play, serde_json::json!({ "chars": u.text.chars().count() }));
                    // речь закончилась → закрыть карточку через 3.5с
                    if let Some(tid) = &u.toast_id {
                        crate::windows::toast_extend(&self.app, tid, 3500);
                    }
                }
                Err(e) => crate::log::line(&format!("[voice] {} молчит: {e}", self.engine.name())),
            }
        });
    }
}

/// Текст уведомления → фраза для TTS. title «Проект — закончил» разворачиваем,
/// body чистим от markdown/кода/списков (squeeze_reply), режем до ~240 символов.
fn notif_tts_text(title: &str, body: &str) -> String {
    use crate::util::{ellipsize, one_line};
    let head = title.replace(" — ", ", ").replace('—', ",");
    let body = crate::transcript::squeeze_reply(body);
    let joined = if body.trim().is_empty() { head } else { format!("{head}. {body}") };
    ellipsize(&one_line(&joined), 240)
}
