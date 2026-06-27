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

/// Простой TTS-сайдкара, после которого глушим процесс (вернуть ~38МБ модели +
/// питон). Озвучка частая в активные периоды; глушим только в долгую тишину.
const VOICE_IDLE_LIMIT: std::time::Duration = std::time::Duration::from_secs(600); // 10 минут

/// Ожидание готовности модели после ленивого подъёма (Silero грузится быстро).
const VOICE_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// Подождать готовности движка (модель загрузилась), но не дольше `timeout`.
/// Тёплый сайдкар проходит первую проверку мгновенно.
fn wait_ready(engine: &dyn TtsEngine, timeout: std::time::Duration) {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if engine.available() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

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
    duck: AtomicBool,        // настройка: паузить чужое медиа на время озвучки
    ducked: AtomicBool,      // сейчас держим чужое медиа на паузе (мы поставили)
    sidecar: Arc<sidecar::Sidecar>,
    app: tauri::AppHandle, // для удержания/продления тоста на время речи
    /// Воркер прямо сейчас проигрывает реплику (для полудуплекса разговора:
    /// цикл не открывает мик, пока true; speak_blocking не выходит по таймауту
    /// посреди звука).
    speaking: Arc<AtomicBool>,
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
            duck: AtomicBool::new(cfg.duck_others),
            ducked: AtomicBool::new(false),
            sidecar,
            app,
            speaking: Arc::new(AtomicBool::new(false)),
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

    /* ===== аудио-шторка: пауза чужого медиа на время озвучки ===== */

    pub fn set_duck(&self, on: bool) {
        self.duck.store(on, Ordering::SeqCst);
        if !on { self.force_unduck(); }
    }
    pub fn duck_enabled(&self) -> bool { self.duck.load(Ordering::SeqCst) }

    /// Включено и что-то играет → пауза (запоминаем, что паузили мы).
    fn ensure_ducked(&self) {
        if self.duck.load(Ordering::SeqCst)
            && !self.ducked.load(Ordering::SeqCst)
            && crate::macos::media_is_playing()
        {
            crate::macos::media_pause();
            self.ducked.store(true, Ordering::SeqCst);
        }
    }

    /// Немедленно вернуть медиа, если мы его паузили.
    fn force_unduck(&self) {
        if self.ducked.swap(false, Ordering::SeqCst) {
            crate::macos::media_play();
        }
    }

    /// После реплики: если очередь пуста — вернуть медиа с дебаунсом 400мс
    /// (чтобы между подряд идущими тостами не мигало пауза→плей→пауза).
    fn maybe_unduck(self: &Arc<Self>) {
        if !self.ducked.load(Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(400));
            if me.queue.0.lock().unwrap().is_empty() {
                me.force_unduck();
            }
        });
    }

    /// Тик супервизора: сперва глушим по простою (вернуть процесс/память в тихие
    /// периоды), иначе — перезапуск, если умер.
    pub fn tick(&self) {
        if !self.sidecar.idle_stop_if_due(VOICE_IDLE_LIMIT) {
            self.sidecar.restart_if_dead();
        }
    }

    /// Погасить Silero-сайдкар на выходе демона.
    pub fn dispose(&self) {
        self.force_unduck(); // не оставить чужое медиа на паузе
        self.sidecar.stop();
    }

    pub fn set_mute(&self, on: bool) {
        self.mute.store(on, Ordering::SeqCst);
        if on {
            self.player.stop(); // мгновенно глушим текущую
            self.force_unduck(); // замьютили Jarvis → вернуть чужое медиа
        }
    }
    pub fn is_muted(&self) -> bool { self.mute.load(Ordering::SeqCst) }

    /// Идёт ли сейчас проигрывание реплики (полудуплекс: цикл ждёт окончания).
    pub fn is_speaking(&self) -> bool { self.speaking.load(Ordering::SeqCst) }

    /// Барж-ин/прерывание (веха 2c): оборвать текущую озвучку И очистить очередь,
    /// БЕЗ mute и force_unduck (в отличие от set_mute). Иначе воркер после
    /// прерванного play_blocking заговорил бы следующую утту из очереди.
    pub fn stop(&self) {
        self.player.stop();
        self.queue.0.lock().unwrap().clear();
    }

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
            done: None,
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
            dedup_key: format!("test:{text}"), coalesce_group: None, toast_id: None, done: None,
        });
        cv.notify_one();
    }

    /// Разговорная реплика ассистента (п/п-2): БЕЗ контент-дедупа (повторные
    /// ответы — «не расслышал», дважды «сколько времени» — не глотаются), без
    /// тоста, Priority::Done. Уникальный dedup_key — счётчик.
    pub fn say(&self, text: &str) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let (m, cv) = &*self.queue;
        let added = m.lock().unwrap().enqueue(Utterance {
            text: text.to_string(),
            priority: Priority::Done,
            dedup_key: format!("say:{n}"),
            coalesce_group: None,
            toast_id: None,
            done: None,
        });
        if added {
            cv.notify_one();
        }
    }

    /// Сигнал «реплика отыграна» в канал `done` утты (если есть). Вызывается
    /// воркером на ЛЮБОМ исходе утты (сыграна/ошибка/мьют) — чтобы speak_blocking
    /// не завис.
    fn signal_done(u: &Utterance) {
        if let Some(done) = &u.done {
            let (m, cv) = &**done;
            *m.lock().unwrap() = true;
            cv.notify_all();
        }
    }

    /// Озвучить и ДОЖДАТЬСЯ конца (полудуплексный разговорный цикл): возвращает,
    /// когда речь отыграна/прервана/смьючена. Без контент-дедупа. Страховочный
    /// таймаут ~30с, чтобы цикл не завис, если воркер не сигналит.
    pub fn speak_blocking(&self, text: &str) {
        let done: composer::DoneSignal = Arc::new((Mutex::new(false), Condvar::new()));
        {
            let (m, cv) = &*self.queue;
            let added = m.lock().unwrap().enqueue(Utterance {
                text: text.to_string(),
                priority: Priority::Done,
                dedup_key: format!("blk:{}", crate::util::now_ms()),
                coalesce_group: None,
                toast_id: None,
                done: Some(done.clone()),
            });
            if !added {
                return; // дедуп отбил (крайне маловероятно с now_ms) → не ждём
            }
            cv.notify_one();
        }
        let (lock, c) = &*done;
        let mut g = lock.lock().unwrap();
        let start = std::time::Instant::now();
        while !*g {
            let (ng, _to) = c.wait_timeout(g, std::time::Duration::from_millis(500)).unwrap();
            g = ng;
            // Аварийный таймаут — только если воркер НЕ играет прямо сейчас (иначе
            // длинная реплика вышла бы из speak_blocking посреди звука → следующий
            // listen услышал бы хвост TTS; CONV-3). Пока играет — ждём дальше.
            if *g || (start.elapsed() > std::time::Duration::from_secs(30) && !self.is_speaking()) {
                break;
            }
        }
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
            // Тело утты в catch_unwind: паника синтеза/плеера не убивает воркер и
            // НЕ оставляет speak_blocking-ждущего висеть 30с (HD-2). signal_done —
            // ВСЕГДА после, на любом исходе (сыграна/ошибка/мьют/паника).
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if self.is_muted() {
                    return;
                }
                let vs = self.voice_sel();
                // Лениво поднять сайдкар + дождаться готовности; страж держит активность.
                self.sidecar.ensure_started();
                self.sidecar.touch();
                wait_ready(self.engine.as_ref(), VOICE_READY_TIMEOUT);
                let _use = self.sidecar.use_guard();
                let t_synth = crate::metrics::now();
                match self.engine.synthesize(&u.text, &vs) {
                    Ok(wav) => {
                        crate::metrics::record("tts_synth", t_synth, serde_json::json!({
                            "engine": self.engine.name(), "chars": u.text.chars().count(), "bytes": wav.len(),
                        }));
                        // мьют мог прийти во время долгого синтеза → не озвучиваем (HD-3)
                        if self.is_muted() {
                            return;
                        }
                        if let Some(tid) = &u.toast_id {
                            crate::windows::toast_hold(&self.app, tid);
                        }
                        self.ensure_ducked(); // зашторить чужое медиа на время речи
                        let t_play = crate::metrics::now();
                        self.speaking.store(true, Ordering::SeqCst); // полудуплекс: идёт звук
                        self.player.play_blocking(wav);
                        self.speaking.store(false, Ordering::SeqCst);
                        crate::metrics::record("tts_play", t_play, serde_json::json!({ "chars": u.text.chars().count() }));
                        self.maybe_unduck(); // очередь пуста → вернуть медиа (с дебаунсом)
                        if let Some(tid) = &u.toast_id {
                            crate::windows::toast_extend(&self.app, tid, 3500);
                        }
                    }
                    Err(e) => crate::log::line(&format!("[voice] {} молчит: {e}", self.engine.name())),
                }
            }));
            self.speaking.store(false, Ordering::SeqCst); // на случай паники во время play
            if res.is_err() {
                crate::log::line("[voice] паника в воркере озвучки — реплика пропущена (воркер жив)");
            }
            Self::signal_done(&u); // ВСЕГДА: реплика отыграна/прервана/паника → будим speak_blocking
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
