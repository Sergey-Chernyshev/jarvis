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
/// При engine="silero" владеет супервизором сайдкара (старт/перезапуск/стоп).
pub struct Voice {
    composer: Box<dyn Composer>,
    engine: Box<dyn TtsEngine>,
    player: Arc<dyn Play>,
    voice: VoiceSel,
    queue: Arc<(Mutex<SpeechQueue>, Condvar)>,
    mute: Arc<AtomicBool>,
    sidecar: Option<Arc<sidecar::Sidecar>>,
}

impl Voice {
    pub fn new(
        cfg: &VoiceConfig,
        piper_bin: std::path::PathBuf,
        silero_dir: std::path::PathBuf,
    ) -> Arc<Self> {
        // для silero — поднимаем сайдкар и берём его base; для piper sidecar=None
        let (sidecar, silero_base) = if cfg.engine == "silero" {
            let speaker = if cfg.speaker.is_empty() { "baya".to_string() } else { cfg.speaker.clone() };
            let sc = Arc::new(sidecar::Sidecar::new(silero_dir, speaker, "v4_ru".into(), SILERO_PORT));
            sc.ensure_started();
            let base = sc.base();
            (Some(sc), base)
        } else {
            (None, format!("http://127.0.0.1:{SILERO_PORT}"))
        };
        let engine = build_engine(&cfg.engine, piper_bin, silero_base);
        let speaker = if cfg.speaker.is_empty() && cfg.engine == "silero" { "baya".to_string() } else { cfg.speaker.clone() };
        let v = Arc::new(Voice {
            composer: Box::new(TemplateComposer),
            engine,
            player: Arc::new(RodioPlayer::new()),
            voice: VoiceSel { speaker, voice_path: cfg.voice_path.clone(), sample_rate: cfg.sample_rate },
            queue: Arc::new((Mutex::new(SpeechQueue::new()), Condvar::new())),
            mute: Arc::new(AtomicBool::new(cfg.mute)),
            sidecar,
        });
        v.clone().spawn_worker();
        v
    }

    /// Тик супервизора: перезапустить сайдкар, если он умер (no-op для piper).
    pub fn tick(&self) {
        if let Some(sc) = &self.sidecar {
            sc.restart_if_dead();
        }
    }

    /// Погасить сайдкар на выходе демона (no-op для piper).
    pub fn dispose(&self) {
        if let Some(sc) = &self.sidecar {
            sc.stop();
        }
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

    pub fn test_phrase(&self, text: &str) {
        let (m, cv) = &*self.queue;
        m.lock().unwrap().enqueue(Utterance {
            text: text.to_string(), priority: Priority::Done,
            dedup_key: format!("test:{text}"), coalesce_group: None,
        });
        cv.notify_one();
    }

    pub fn warmup(&self) { self.engine.warmup(&self.voice); }
    pub fn engine_name(&self) -> &'static str { self.engine.name() }
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
            match self.engine.synthesize(&u.text, &self.voice) {
                Ok(wav) => { self.player.play_blocking(wav); }
                Err(e) => crate::log::line(&format!("[voice] {} молчит: {e}", self.engine.name())),
            }
        });
    }
}
