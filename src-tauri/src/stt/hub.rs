//! Общий аудио-вход (инкр. 10) — ЕДИНЫЙ владелец захвата микрофона.
//!
//! Заменяет «поток-на-сессию» из инкр.9 на единый always-on источник с веерной
//! раздачей 16кГц-моно-кадров нескольким потребителям одновременно:
//!  - **WakeTap** — непрерывная подписка (wake-word слушает всегда, пока включён);
//!  - **CaptureSession** — подписка по требованию (PTT-диктовка, STT-захват по wake).
//!
//! Ключевые свойства (см. дизайн §1):
//!  - один `cpal::Stream` на устройство (CoreAudio не любит два input-стрима);
//!  - **жёсткий mute у источника** — кадр не входит в конвейер (доверие);
//!  - **кольцевой буфер-преролл** — начало реплики не теряется при wake;
//!  - чистое ядро `Pipeline` (downmix→ресемпл→нарезка→preroll→fan-out) тестируется
//!    на синтетике БЕЗ живого микрофона.
//!
//! Потоковая модель: `cpal::Stream` — `!Send`, поэтому живёт на отдельном
//! capture-потоке и НЕ покидает его; наружу (в `Daemon`) попадают только Send-ручки.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

use rubato::{FftFixedIn, Resampler};

use super::audio::downmix_to_mono;

/// Целевая частота для всех потребителей.
pub const DST_RATE: u32 = 16_000;
/// Размер кадра раздачи: 80 мс @16кГц = 1280 сэмплов (совпадает с шагом openWakeWord).
pub const FRAME_LEN: usize = 1280;
/// Преролл: 2 с 16кГц моно (32000 сэмплов) — с запасом покрывает начало реплики.
pub const PREROLL_SAMPLES: usize = (DST_RATE as usize) * 2;
/// Входной чанк ресемплера (native frames) — баланс латентности/качества.
const RESAMPLE_CHUNK_IN: usize = 1024;
/// Ёмкость native-канала (capture→proc): при застое proc realtime-callback
/// роняет кадр (try_send), а не растит память без предела.
const NATIVE_CHAN_CAP: usize = 256;
/// Ёмкость канала подписчика: ограничивает рост памяти при зависшем потребителе.
/// 4096 кадров ≈ 5.5 мин (≈21 МБ) — с запасом для диктовки; wake-тап вычитывает
/// быстро и до предела не доходит.
const SUB_CHAN_CAP: usize = 4096;

/// Видимое состояние источника (для индикатора «слушаю» и панели).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioState {
    Idle = 0,      // не слушаем
    Listening = 1, // активный захват, не заглушено
    Muted = 2,     // жёсткий mute
    Denied = 3,    // нет разрешения микрофона
    NoDevice = 4,  // нет устройства ввода / ошибка
}

impl AudioState {
    fn from_u8(v: u8) -> AudioState {
        match v {
            1 => AudioState::Listening,
            2 => AudioState::Muted,
            3 => AudioState::Denied,
            4 => AudioState::NoDevice,
            _ => AudioState::Idle,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            AudioState::Idle => "idle",
            AudioState::Listening => "listening",
            AudioState::Muted => "muted",
            AudioState::Denied => "denied",
            AudioState::NoDevice => "no-device",
        }
    }
}

// ─── Чистое ядро: Pipeline (downmix → стрим-ресемпл → нарезка → preroll) ──────

/// Состояние потоковой обработки одного capture-сеанса. Stateful (держит
/// резамплер и аккумуляторы), но БЕЗ потоков/cpal — поэтому полностью тестируем.
pub struct Pipeline {
    channels: u16,
    resampler: Option<FftFixedIn<f32>>, // None при src==16к (passthrough)
    in_buf: Vec<f32>,                   // аккумулятор native-моно под чанк ресемплера
    out_buf: Vec<f32>,                  // аккумулятор 16к-моно под нарезку на кадры
    preroll: VecDeque<f32>,             // кольцо последних PREROLL_SAMPLES @16к
}

impl Pipeline {
    pub fn new(src_rate: u32, channels: u16) -> Result<Pipeline, String> {
        let resampler = if src_rate == DST_RATE {
            None
        } else {
            Some(
                FftFixedIn::<f32>::new(src_rate as usize, DST_RATE as usize, RESAMPLE_CHUNK_IN, 2, 1)
                    .map_err(|e| format!("FftFixedIn::new: {e:?}"))?,
            )
        };
        Ok(Pipeline {
            channels,
            resampler,
            in_buf: Vec::new(),
            out_buf: Vec::new(),
            preroll: VecDeque::with_capacity(PREROLL_SAMPLES + FRAME_LEN),
        })
    }

    /// Скормить интерливнутый native-буфер; вернуть готовые кадры 16к моно (1280).
    pub fn push_native(&mut self, interleaved: &[f32]) -> Vec<Arc<[f32]>> {
        let mono = downmix_to_mono(interleaved, self.channels);
        match self.resampler.as_mut() {
            None => self.out_buf.extend_from_slice(&mono),
            Some(rs) => {
                self.in_buf.extend_from_slice(&mono);
                while self.in_buf.len() >= RESAMPLE_CHUNK_IN {
                    let chunk: Vec<f32> = self.in_buf.drain(..RESAMPLE_CHUNK_IN).collect();
                    if let Ok(out) = rs.process(&[chunk], None) {
                        self.out_buf.extend_from_slice(&out[0]);
                    }
                }
            }
        }
        self.cut_frames()
    }

    fn cut_frames(&mut self) -> Vec<Arc<[f32]>> {
        let mut frames = Vec::new();
        while self.out_buf.len() >= FRAME_LEN {
            let frame: Vec<f32> = self.out_buf.drain(..FRAME_LEN).collect();
            // преролл: кольцо последних PREROLL_SAMPLES
            self.preroll.extend(frame.iter().copied());
            while self.preroll.len() > PREROLL_SAMPLES {
                self.preroll.pop_front();
            }
            frames.push(Arc::from(frame.into_boxed_slice()));
        }
        frames
    }

    /// Снимок преролла (последние ≤2с 16к моно) — отдаётся STT-захвату на wake.
    pub fn preroll_snapshot(&self) -> Vec<f32> {
        self.preroll.iter().copied().collect()
    }

    /// Очистить преролл (на mute — не держим пред-mute аудио).
    pub fn clear_preroll(&mut self) {
        self.preroll.clear();
    }
}

// ─── Подписчики (веер) ───────────────────────────────────────────────────────

struct Subscriber {
    id: u64,
    tx: SyncSender<Arc<[f32]>>,
}

struct HubSession {
    pipeline: Pipeline,
    src_rate: u32,
    channels: u16,
}

struct HubThreads {
    stop: Arc<AtomicBool>,
    capture: JoinHandle<()>,
    proc: JoinHandle<()>,
}

struct Inner {
    device: Option<String>,
    subs: Vec<Subscriber>,
    /// Счётчик «спроса»: wake (0/1) + число активных capture-сессий.
    demand: u32,
    session: Option<HubSession>,
    threads: Option<HubThreads>,
}

/// Единый владелец захвата. Живёт в `Arc<AudioHub>` внутри `Daemon`.
pub struct AudioHub {
    inner: Mutex<Inner>,
    /// Shared с realtime-callback: `set_muted` мгновенно глушит захват у источника.
    muted: Arc<AtomicBool>,
    state: AtomicU8,
    next_id: AtomicU64,
    /// Сериализует переходы захвата (subscribe/unsubscribe/restart), чтобы рост
    /// спроса 0→1 не вклинился между падением 1→0 и его stop_capture (иначе
    /// подписчик остался бы без захвата — «глухой»). Порядок: lifecycle → inner.
    lifecycle: Mutex<()>,
    /// Сколько 16к-кадров выдано всего — растёт, пока устройство живо (даже в
    /// тишине cpal шлёт нули). Застой при работающем не-mute захвате = устройство
    /// отвалилось → watchdog (`tick`) перезапускает захват.
    frames_seen: AtomicU64,
    last_frames: AtomicU64,
    stalls: AtomicU32,
    /// Кадры с РЕАЛЬНОЙ энергией (не цифровая тишина). Если захват жив
    /// (`frames_seen` растёт), но этот счётчик стоит — микрофон отдаёт тишину
    /// (типично для неподписанного бинаря без доступа TCC) → watchdog поднимет
    /// `mic_silent`, чтобы провал был виден, а не молчал.
    nonzero_frames: AtomicU64,
    last_nonzero: AtomicU64,
    silent_ticks: AtomicU32,
    mic_silent: AtomicBool,
    app: Option<tauri::AppHandle>,
}

impl AudioHub {
    pub fn new(device: Option<String>, app: Option<tauri::AppHandle>) -> Arc<AudioHub> {
        Arc::new(AudioHub {
            inner: Mutex::new(Inner {
                device,
                subs: Vec::new(),
                demand: 0,
                session: None,
                threads: None,
            }),
            muted: Arc::new(AtomicBool::new(false)),
            state: AtomicU8::new(AudioState::Idle as u8),
            next_id: AtomicU64::new(1),
            lifecycle: Mutex::new(()),
            frames_seen: AtomicU64::new(0),
            last_frames: AtomicU64::new(0),
            stalls: AtomicU32::new(0),
            nonzero_frames: AtomicU64::new(0),
            last_nonzero: AtomicU64::new(0),
            silent_ticks: AtomicU32::new(0),
            mic_silent: AtomicBool::new(false),
            app,
        })
    }

    /// Сменить устройство ввода. Если захват идёт — перезапустить на новом
    /// устройстве (иначе смена применится при следующем старте).
    pub fn set_device(self: &Arc<Self>, device: Option<String>) {
        let _lc = self.lifecycle.lock().unwrap();
        let running = {
            let mut g = self.inner.lock().unwrap();
            g.device = device;
            g.threads.is_some()
        };
        if running {
            self.restart_capture();
        }
    }

    /// Жёсткий mute: при включении кадр НЕ входит в конвейер ни для кого, и
    /// накопленный преролл (до 2с до mute) очищается — приватность.
    pub fn set_muted(&self, on: bool) {
        self.muted.store(on, Ordering::SeqCst);
        if on {
            if let Ok(mut g) = self.inner.lock() {
                if let Some(s) = g.session.as_mut() {
                    s.pipeline.clear_preroll();
                }
            }
        }
        self.refresh_state();
    }
    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::SeqCst)
    }
    pub fn state(&self) -> AudioState {
        AudioState::from_u8(self.state.load(Ordering::SeqCst))
    }

    fn set_state(&self, s: AudioState) {
        let prev = self.state.swap(s as u8, Ordering::SeqCst);
        if prev != s as u8 {
            self.notify_panel();
        }
    }

    /// Текущее аудио-состояние одним payload (для эмита И для pull-on-load из тоста).
    pub fn audio_state_payload(&self) -> serde_json::Value {
        serde_json::json!({
            "state": self.state().as_str(),
            "muted": self.is_muted(),
            "mic_silent": self.is_mic_silent(),
        })
    }

    /// Отправить панели текущее аудио-состояние (+ mute, + флаг «микрофон молчит»).
    /// Дублируем в окно `toast`: оверлей виден, когда панель скрыта (норм. режим),
    /// и показывает «слышу / тихо / нет доступа» — фикс «говорю Hey Jarvis, ничего».
    /// audio_state эмитится лишь НА ИЗМЕНЕНИИ; ранний terminal-state (denied при
    /// старте) мог уйти до загрузки webview тоста — тост дотягивает его сам через
    /// `voice_audio_state` (VR-3).
    fn notify_panel(&self) {
        if let Some(app) = self.app.as_ref() {
            let payload = self.audio_state_payload();
            crate::windows::emit_to_panel(app, "audio_state", &payload);
            crate::windows::emit_to_toast_window(app, "audio_state", &payload);
        }
    }

    /// Захват «жив», но микрофон отдаёт цифровую тишину (нет реального доступа).
    pub fn is_mic_silent(&self) -> bool {
        self.mic_silent.load(Ordering::Relaxed)
    }

    /// Пересчитать видимое состояние из факта работы + mute.
    fn refresh_state(&self) {
        let running = self.inner.lock().map(|g| g.threads.is_some()).unwrap_or(false);
        let s = if !running {
            AudioState::Idle
        } else if self.is_muted() {
            AudioState::Muted
        } else {
            AudioState::Listening
        };
        // не перетираем терминальные Denied/NoDevice, выставленные стартом
        let cur = self.state();
        if !matches!(cur, AudioState::Denied | AudioState::NoDevice) || running {
            self.set_state(s);
        }
    }

    fn new_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Подписаться на поток кадров. Возвращает (id, Receiver). Поднимает «спрос»
    /// и при необходимости запускает захват. Под `lifecycle` — чтобы subscribe и
    /// unsubscribe не разъехались (см. поле `lifecycle`).
    fn subscribe(self: &Arc<Self>) -> (u64, Receiver<Arc<[f32]>>) {
        let _lc = self.lifecycle.lock().unwrap();
        let (tx, rx) = mpsc::sync_channel(SUB_CHAN_CAP);
        let id = self.new_id();
        {
            let mut g = self.inner.lock().unwrap();
            g.subs.push(Subscriber { id, tx });
            g.demand += 1;
        }
        self.ensure_running();
        (id, rx)
    }

    fn unsubscribe(self: &Arc<Self>, id: u64) {
        let _lc = self.lifecycle.lock().unwrap();
        let stop_now = {
            let mut g = self.inner.lock().unwrap();
            g.subs.retain(|s| s.id != id);
            if g.demand > 0 {
                g.demand -= 1;
            }
            g.demand == 0
        };
        if stop_now {
            self.stop_capture();
        }
    }

    /// Открыть сессию захвата (для PTT/STT). `with_preroll` — приклеить снимок
    /// преролла к началу записи (бесшовный wake→STT).
    pub fn open_capture(self: &Arc<Self>, with_preroll: bool) -> CaptureSession {
        // Подписываемся ПЕРЕД снимком преролла: кадры между снимком и подпиской
        // не теряются (их ловит rx), начало реплики сохранно.
        let (id, rx) = self.subscribe();
        let preroll = if with_preroll { self.preroll() } else { Vec::new() };
        CaptureSession { hub: self.clone(), id, rx, preroll }
    }

    /// Непрерывная подписка для wake-word.
    pub fn subscribe_wake(self: &Arc<Self>) -> WakeTap {
        let (id, rx) = self.subscribe();
        WakeTap { hub: self.clone(), id, rx }
    }

    /// Снимок преролла (16к моно). Пусто, если сессия не идёт.
    pub fn preroll(&self) -> Vec<f32> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.session.as_ref().map(|s| s.pipeline.preroll_snapshot()))
            .unwrap_or_default()
    }

    /// Прогнать native-буфер через конвейер и разослать кадры подписчикам.
    /// Вызывается proc-потоком; в тестах — напрямую. Уважает mute (drop у источника).
    fn ingest(&self, interleaved: &[f32], src_rate: u32, channels: u16) {
        if self.muted.load(Ordering::SeqCst) {
            return; // жёсткий mute: ничего не обрабатываем и не раздаём
        }
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        // пересоздать конвейер при смене формата
        let need_new = match g.session.as_ref() {
            Some(s) => s.src_rate != src_rate || s.channels != channels,
            None => true,
        };
        if need_new {
            match Pipeline::new(src_rate, channels) {
                Ok(p) => g.session = Some(HubSession { pipeline: p, src_rate, channels }),
                Err(e) => {
                    crate::log::line(&format!("[audio] pipeline: {e}"));
                    return;
                }
            }
        }
        let frames = g.session.as_mut().unwrap().pipeline.push_native(interleaved);
        if frames.is_empty() {
            return;
        }
        // Перепроверка mute перед раздачей: закрывает суб-кадровую гонку (mute
        // включили, пока буфер был в обработке) — заглушённое аудио не уходит никому.
        if self.muted.load(Ordering::SeqCst) {
            return;
        }
        // watchdog-счётчик живости (растёт и в тишине — cpal шлёт нули)
        self.frames_seen.fetch_add(frames.len() as u64, Ordering::Relaxed);
        // счётчик кадров с РЕАЛЬНОЙ энергией: отличает живой микрофон от цифровой
        // тишины (нет доступа TCC у неподписанного бинаря — macOS шлёт нули).
        if interleaved.iter().any(|&s| s.abs() > 1e-4) {
            self.nonzero_frames.fetch_add(frames.len() as u64, Ordering::Relaxed);
        }
        for frame in frames {
            // try_send: отставший потребитель теряет кадр (не растим память без
            // предела), отвалившийся (Receiver уронен) — выбывает из веера.
            g.subs.retain(|s| !matches!(s.tx.try_send(frame.clone()), Err(mpsc::TrySendError::Disconnected(_))));
        }
    }

    /// Watchdog: при работающем не-заглушённом захвате кадры обязаны расти.
    /// Два тика без новых кадров ⇒ устройство отвалилось (macOS не сигналит
    /// дисконнект явно) ⇒ перезапуск захвата с текущего дефолтного устройства.
    pub fn tick(self: &Arc<Self>) {
        // под lifecycle: рестарт/стоп watchdog не разъезжается с subscribe/unsubscribe
        let _lc = self.lifecycle.lock().unwrap();
        let running = self.inner.lock().map(|g| g.threads.is_some()).unwrap_or(false);
        if !running || self.is_muted() {
            self.last_frames.store(self.frames_seen.load(Ordering::Relaxed), Ordering::Relaxed);
            self.last_nonzero.store(self.nonzero_frames.load(Ordering::Relaxed), Ordering::Relaxed);
            self.stalls.store(0, Ordering::Relaxed);
            self.silent_ticks.store(0, Ordering::Relaxed);
            if self.mic_silent.swap(false, Ordering::Relaxed) {
                self.notify_panel();
            }
            return;
        }
        // Runtime-отзыв разрешения микрофона: cpal продолжает слать ТИШИНУ (нули),
        // поэтому watchdog по кадрам это не поймает — проверяем TCC явно.
        if matches!(
            crate::stt::mic_permission::status(),
            crate::stt::mic_permission::MicAuth::Denied | crate::stt::mic_permission::MicAuth::Restricted
        ) {
            crate::log::line("[audio] разрешение микрофона отозвано — останавливаю захват");
            self.stop_capture();
            self.set_state(AudioState::Denied);
            return;
        }
        let now = self.frames_seen.load(Ordering::Relaxed);
        let prev = self.last_frames.swap(now, Ordering::Relaxed);
        if now == prev {
            // устройство не шлёт даже тишину ⇒ отвалилось → перезапуск
            let s = self.stalls.fetch_add(1, Ordering::Relaxed) + 1;
            if s >= 2 {
                crate::log::line("[audio] захват завис (устройство отвалилось?) — перезапуск");
                self.stalls.store(0, Ordering::Relaxed);
                self.restart_capture();
            }
            return;
        }
        self.stalls.store(0, Ordering::Relaxed);

        // Детектор тишины: захват жив (кадры растут), но реальной энергии нет
        // несколько тиков подряд ⇒ микрофон отдаёт цифровую тишину (типично для
        // неподписанного бинаря без доступа TCC). Рестарт не поможет — поднимаем
        // видимый флаг и пишем понятную причину в лог (провал перестаёт молчать).
        let now_nz = self.nonzero_frames.load(Ordering::Relaxed);
        let prev_nz = self.last_nonzero.swap(now_nz, Ordering::Relaxed);
        let got_audio = now_nz != prev_nz;
        let (ticks, silent) = silence_decide(self.silent_ticks.load(Ordering::Relaxed), got_audio);
        self.silent_ticks.store(ticks, Ordering::Relaxed);
        if silent && !self.mic_silent.swap(true, Ordering::Relaxed) {
            crate::log::line(
                "[audio] микрофон не даёт звука (тишина) — вероятно нет доступа к микрофону (TCC). \
                 Wake-word и диктовка не услышат. Выдай доступ: Системные настройки → \
                 Конфиденциальность → Микрофон (или запусти подписанное приложение).",
            );
            self.notify_panel();
        } else if got_audio && self.mic_silent.swap(false, Ordering::Relaxed) {
            crate::log::line("[audio] микрофон снова даёт звук");
            self.notify_panel();
        }
    }

    /// Остановить и (если есть спрос) поднять захват заново — для watchdog.
    /// Подписки/спрос не трогаем: их получатели продолжат жить, новые кадры
    /// потекут после рестарта.
    fn restart_capture(self: &Arc<Self>) {
        self.stop_capture();
        let demand = self.inner.lock().map(|g| g.demand).unwrap_or(0);
        if demand > 0 {
            self.ensure_running();
        }
    }

    fn ensure_running(self: &Arc<Self>) {
        // Юнит-тесты не открывают живой микрофон: гоняем `ingest()` напрямую.
        if cfg!(test) {
            return;
        }
        let mut g = self.inner.lock().unwrap();
        if g.threads.is_some() {
            return; // уже работает
        }
        let device = g.device.clone();

        // Проверка разрешения микрофона ДО открытия стрима (иначе SIGABRT/тишина).
        match crate::stt::mic_permission::status() {
            crate::stt::mic_permission::MicAuth::Denied
            | crate::stt::mic_permission::MicAuth::Restricted => {
                drop(g);
                self.set_state(AudioState::Denied);
                crate::log::line("[audio] разрешение микрофона не выдано — захват не открыт");
                return;
            }
            crate::stt::mic_permission::MicAuth::NotDetermined => {
                // ещё не спрашивали — показать системный диалог (нужен встроенный
                // NSMicrophoneUsageDescription: .app или dev-бинарь с Info.plist).
                // Старт НЕ блокируем: разрешит — кадры пойдут; нет — watchdog
                // поймает тишину и поднимет mic_silent.
                crate::stt::mic_permission::request();
            }
            crate::stt::mic_permission::MicAuth::Authorized => {}
        }

        let stop = Arc::new(AtomicBool::new(false));
        let (native_tx, native_rx) = mpsc::sync_channel::<Vec<f32>>(NATIVE_CHAN_CAP);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(u32, u16), String>>();
        let muted_flag = self.muted_handle();

        // capture-поток: владеет !Send cpal::Stream, не покидает поток.
        let stop_cap = stop.clone();
        let capture = std::thread::spawn(move || {
            capture_thread(device.as_deref(), native_tx, ready_tx, stop_cap, muted_flag);
        });

        // proc-поток: native → ingest (downmix/ресемпл/нарезка/fan-out).
        let hub = self.clone();
        let proc = std::thread::spawn(move || {
            // ждём формат (или ошибку старта)
            let (rate, channels) = match ready_rx.recv() {
                Ok(Ok(fmt)) => fmt,
                Ok(Err(e)) => {
                    crate::log::line(&format!("[audio] старт захвата: {e}"));
                    hub.set_state(AudioState::NoDevice);
                    return;
                }
                Err(_) => return,
            };
            hub.refresh_state();
            while let Ok(buf) = native_rx.recv() {
                // fail-safe: паника в DSP/ресемпле не убивает поток (кадр пропущен)
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    hub.ingest(&buf, rate, channels)
                }));
                if res.is_err() {
                    crate::log::line("[audio] паника в ingest — кадр пропущен (демон жив)");
                }
            }
        });

        g.threads = Some(HubThreads { stop, capture, proc });
        // подтверждение/ошибка старта приходит в proc-поток (ready_rx переехал туда);
        // индикатор состояния выставит proc-поток после получения формата.
    }

    fn stop_capture(self: &Arc<Self>) {
        let threads = {
            let mut g = self.inner.lock().unwrap();
            g.session = None;
            g.threads.take()
        };
        if let Some(t) = threads {
            crate::log::line("[audio] захват остановлен (спрос=0)");
            t.stop.store(true, Ordering::SeqCst);
            // НЕ джойним под локом: cpal/CoreAudio-teardown может зависнуть, а
            // `unsubscribe`/`set_device` держат `lifecycle` — это морозило ВЕСЬ
            // аудио-хаб («бесконечный анализ» на любом движке). Detach: потоки сами
            // завершатся по stop-флагу (capture уронит native_tx → proc выйдет из recv).
            // JoinHandle'ы дропаются без join — поток продолжает teardown в фоне.
            drop(t);
        }
        self.refresh_state();
    }

    /// Тот же shared mute-флаг для realtime-callback (drop у источника).
    fn muted_handle(&self) -> Arc<AtomicBool> {
        self.muted.clone()
    }

    /// Погасить захват на выходе демона.
    pub fn dispose(self: &Arc<Self>) {
        self.stop_capture();
    }
}

/// Чистое решение детектора тишины (тестируемо без потоков/микрофона). Вход —
/// текущий счётчик «тихих» тиков и был ли звук в этом тике; выход — (новый
/// счётчик, поднимать ли флаг «тишина»). 2 тика (≈10с при 5с-тике) чистой тишины
/// при живом захвате ⇒ микрофон молчит.
fn silence_decide(silent_ticks: u32, got_audio: bool) -> (u32, bool) {
    if got_audio {
        (0, false)
    } else {
        let t = silent_ticks.saturating_add(1);
        (t, t >= 2)
    }
}

/// Сессия захвата (PTT/STT-потребитель). На `Drop`/`finish` — отписка.
pub struct CaptureSession {
    hub: Arc<AudioHub>,
    id: u64,
    rx: Receiver<Arc<[f32]>>,
    preroll: Vec<f32>,
}

impl CaptureSession {
    /// Совместимый со старым API конструктор: открыть захват через хаб.
    /// `device` игнорируется (устройство — на уровне хаба); сохранён ради
    /// минимального диффа потребителей инкр.9.
    pub fn start_on(hub: &Arc<AudioHub>, _device: Option<&str>) -> Result<CaptureSession, String> {
        Ok(hub.open_capture(false))
    }

    /// Остановить захват, вернуть накопленный 16к моно PCM (с прероллом впереди).
    /// Отписку выполняет `Drop` (ниже) — здесь её НЕ дублируем, иначе спрос
    /// декрементился бы дважды и остановил бы захват для других подписчиков.
    pub fn finish(mut self) -> Result<Vec<f32>, String> {
        let mut out = std::mem::take(&mut self.preroll);
        while let Ok(frame) = self.rx.try_recv() {
            out.extend_from_slice(&frame);
        }
        Ok(out)
        // self выходит из области видимости здесь → Drop::drop → unsubscribe (ровно раз)
    }
}

impl Drop for CaptureSession {
    /// Освобождение захвата на ЛЮБОМ пути выхода — включая потерянный key-up PTT
    /// или панику потребителя, когда `finish()` не вызвали. Без этого спрос тёк и
    /// микрофон держался бессрочно («индикатор вечно слушает»). Ровно один
    /// декремент спроса на сессию: `finish()` больше сам не отписывается.
    fn drop(&mut self) {
        self.hub.unsubscribe(self.id);
    }
}

/// Непрерывный тап для wake-word. На `Drop` — отписка.
pub struct WakeTap {
    hub: Arc<AudioHub>,
    id: u64,
    rx: Receiver<Arc<[f32]>>,
}

impl WakeTap {
    /// Блокирующий приём следующего кадра (None — источник закрылся).
    pub fn recv(&self) -> Option<Arc<[f32]>> {
        self.rx.recv().ok()
    }
    /// Приём с таймаутом — чтобы consumer-поток мог проверять stop-флаг.
    pub fn recv_timeout(&self, dur: std::time::Duration) -> Option<Arc<[f32]>> {
        match self.rx.recv_timeout(dur) {
            Ok(f) => Some(f),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => None,
        }
    }
}

impl Drop for WakeTap {
    fn drop(&mut self) {
        self.hub.unsubscribe(self.id);
    }
}

/// Имена доступных устройств ввода (микрофонов) для селектора в настройках.
/// Дедуп + сортировка; пустой список — если хост не отдал устройства. Совпадает по
/// имени с тем, что ищет `capture_thread` (точный матч `device.name()`).
pub fn input_device_names() -> Vec<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let mut names: Vec<String> = host
        .input_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default();
    names.sort();
    names.dedup();
    names
}

// ─── cpal capture-поток (вызывается только на своём потоке; Stream !Send) ─────

fn capture_thread(
    device: Option<&str>,
    native_tx: SyncSender<Vec<f32>>,
    ready_tx: Sender<Result<(u32, u16), String>>,
    stop: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
) {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::SampleFormat;

    let host = cpal::default_host();
    let dev = match device {
        None => host.default_input_device(),
        Some(name) => {
            let found = host
                .input_devices()
                .ok()
                .and_then(|mut it| it.find(|d| d.name().map(|n| n == name).unwrap_or(false)));
            if found.is_none() {
                // Выбранное устройство пропало (отключили / неверное имя) — НЕ роняем
                // захват, а откатываемся на системное по умолчанию (запись продолжит идти).
                crate::log::line(&format!(
                    "[audio] устройство «{name}» не найдено (есть: {}) — откат на системное по умолчанию",
                    input_device_names().join(", ")
                ));
            }
            found.or_else(|| host.default_input_device())
        }
    };
    let Some(dev) = dev else {
        let _ = ready_tx.send(Err("нет устройства ввода".into()));
        return;
    };
    crate::log::line(&format!(
        "[audio] захват с устройства: {}",
        dev.name().unwrap_or_else(|_| "<неизвестно>".into())
    ));
    let config = match dev.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("default_input_config: {e}")));
            return;
        }
    };
    let src_rate = config.sample_rate().0;
    let channels = config.channels();
    let fmt = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();
    let err_fn = |e| crate::log::line(&format!("[audio] stream error: {e}"));

    // realtime callback: mute → drop у источника; иначе конверсия в f32 + send.
    macro_rules! build {
        ($t:ty, $conv:expr) => {{
            let m = muted.clone();
            let tx = native_tx.clone();
            dev.build_input_stream(
                &stream_config,
                move |data: &[$t], _: &cpal::InputCallbackInfo| {
                    if m.load(Ordering::Relaxed) {
                        return; // жёсткий mute у источника
                    }
                    let buf: Vec<f32> = data.iter().map($conv).collect();
                    // try_send: при застое proc роняем кадр (realtime-safe), не блокируемся
                    let _ = tx.try_send(buf);
                },
                err_fn,
                None,
            )
        }};
    }
    let stream = match fmt {
        SampleFormat::F32 => build!(f32, |&s| s),
        SampleFormat::I16 => build!(i16, |&s| s as f32 / i16::MAX as f32),
        SampleFormat::U16 => build!(u16, |&s| (s as f32 - 32768.0) / 32768.0),
        other => {
            let _ = ready_tx.send(Err(format!("формат не поддержан: {other:?}")));
            return;
        }
    };
    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("build_input_stream: {e}")));
            return;
        }
    };
    if let Err(e) = stream.play() {
        let _ = ready_tx.send(Err(format!("stream.play: {e}")));
        return;
    }
    let _ = ready_tx.send(Ok((src_rate, channels)));

    // держим стрим живым, пока не попросят остановиться
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // Сначала pause (глушит realtime-callback), потом drop — teardown AudioUnit
    // при живом callback'e умеет зависать, а зависший drop = микрофон (и оранжевый
    // индикатор) держатся бессрочно. Лог ПОСЛЕ drop — маркер, что teardown прошёл:
    // если строки нет после «захват остановлен», значит CoreAudio завис здесь.
    let _ = stream.pause();
    drop(stream); // остановить захват — на этом же потоке
    crate::log::line("[audio] стрим микрофона освобождён");
    // native_tx уронится здесь → proc-поток завершит цикл recv()
}

// ─── Тесты (без живого микрофона) ────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(n: usize, step: f32) -> Vec<f32> {
        (0..n).map(|i| (i as f32 * step).sin()).collect()
    }

    #[test]
    fn pipeline_passthrough_16k_frames() {
        let mut p = Pipeline::new(16_000, 1).unwrap();
        // 3 кадра ровно
        let input = sine(FRAME_LEN * 3, 0.01);
        let frames = p.push_native(&input);
        assert_eq!(frames.len(), 3);
        assert!(frames.iter().all(|f| f.len() == FRAME_LEN));
    }

    #[test]
    fn pipeline_accumulates_partial_then_emits() {
        let mut p = Pipeline::new(16_000, 1).unwrap();
        assert!(p.push_native(&sine(FRAME_LEN - 10, 0.01)).is_empty(), "недокадр не отдаётся");
        let frames = p.push_native(&sine(20, 0.01)); // добиваем за границу
        assert_eq!(frames.len(), 1);
    }

    #[test]
    fn pipeline_downmix_stereo_to_mono_frame_len() {
        let mut p = Pipeline::new(16_000, 2).unwrap();
        // стерео: 2*FRAME_LEN сэмплов интерлива → 1 моно-кадр
        let stereo = sine(FRAME_LEN * 2, 0.01);
        let frames = p.push_native(&stereo);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), FRAME_LEN);
    }

    #[test]
    fn pipeline_resamples_48k_to_16k_roughly_third() {
        let mut p = Pipeline::new(48_000, 1).unwrap();
        // 48000 сэмплов (1с @48к) → ~16000 @16к → ~12 кадров по 1280
        let frames = p.push_native(&sine(48_000, 0.005));
        let total: usize = frames.iter().map(|f| f.len()).sum();
        let expected = 16_000;
        let tol = (expected as f32 * 0.05) as usize;
        assert!(
            total >= expected - tol - FRAME_LEN && total <= expected + tol,
            "48к→16к: {total} сэмплов, ждали ~{expected}"
        );
    }

    #[test]
    fn pipeline_preroll_capped() {
        let mut p = Pipeline::new(16_000, 1).unwrap();
        // вливаем 5с — преролл должен держать только последние 2с
        let _ = p.push_native(&sine(DST_RATE as usize * 5, 0.001));
        let pre = p.preroll_snapshot();
        assert!(pre.len() <= PREROLL_SAMPLES, "преролл {} > cap {}", pre.len(), PREROLL_SAMPLES);
        assert!(pre.len() >= PREROLL_SAMPLES - FRAME_LEN, "преролл слишком мал: {}", pre.len());
    }

    #[test]
    fn hub_fanout_delivers_same_frames_to_all_subs() {
        let hub = AudioHub::new(None, None);
        let s1 = hub.subscribe_wake();
        let s2 = hub.subscribe_wake();
        hub.ingest(&sine(FRAME_LEN * 2, 0.01), 16_000, 1);
        let f1 = s1.recv_timeout(std::time::Duration::from_millis(50)).unwrap();
        let f2 = s2.recv_timeout(std::time::Duration::from_millis(50)).unwrap();
        assert_eq!(f1.len(), FRAME_LEN);
        assert_eq!(&f1[..], &f2[..], "оба подписчика получают идентичные кадры");
    }

    #[test]
    fn hub_hard_mute_drops_everything() {
        let hub = AudioHub::new(None, None);
        let tap = hub.subscribe_wake();
        hub.set_muted(true);
        hub.ingest(&sine(FRAME_LEN * 4, 0.01), 16_000, 1);
        assert!(
            tap.recv_timeout(std::time::Duration::from_millis(30)).is_none(),
            "при mute ни один кадр не доходит"
        );
        // снятие mute — снова слышим
        hub.set_muted(false);
        hub.ingest(&sine(FRAME_LEN, 0.01), 16_000, 1);
        assert!(tap.recv_timeout(std::time::Duration::from_millis(50)).is_some());
    }

    #[test]
    fn hub_unsubscribe_drops_demand() {
        let hub = AudioHub::new(None, None);
        let tap = hub.subscribe_wake();
        assert_eq!(hub.inner.lock().unwrap().demand, 1);
        drop(tap);
        assert_eq!(hub.inner.lock().unwrap().demand, 0, "Drop тапа снимает спрос");
    }

    #[test]
    fn capture_session_prepends_preroll() {
        let hub = AudioHub::new(None, None);
        // наполнить преролл
        let wake = hub.subscribe_wake();
        hub.ingest(&sine(FRAME_LEN * 3, 0.01), 16_000, 1);
        // выгрести кадры из wake, чтобы преролл сформировался в pipeline
        while wake.recv_timeout(std::time::Duration::from_millis(10)).is_some() {}
        let cap = hub.open_capture(true);
        let pcm = cap.finish().unwrap();
        assert!(!pcm.is_empty(), "захват с прероллом не пуст");
    }

    #[test]
    fn capture_session_finish_unsubscribes() {
        let hub = AudioHub::new(None, None);
        let cap = hub.open_capture(false);
        let d_before = hub.inner.lock().unwrap().demand;
        let _ = cap.finish();
        assert_eq!(hub.inner.lock().unwrap().demand, d_before - 1);
    }

    // Потерянный key-up PTT / паника: сессию дропнули БЕЗ finish() — микрофон
    // всё равно обязан освободиться (иначе индикатор «вечно слушает»).
    #[test]
    fn capture_session_drop_without_finish_unsubscribes() {
        let hub = AudioHub::new(None, None);
        let cap = hub.open_capture(false);
        assert_eq!(hub.inner.lock().unwrap().demand, 1);
        drop(cap);
        assert_eq!(hub.inner.lock().unwrap().demand, 0, "Drop сессии освобождает захват");
    }

    // finish()+Drop не должны декрементить спрос ДВАЖДЫ: завершение одной сессии
    // не имеет права остановить захват для другого активного подписчика.
    #[test]
    fn finish_one_session_leaves_other_subscribed() {
        let hub = AudioHub::new(None, None);
        let a = hub.open_capture(false);
        let b = hub.open_capture(false);
        assert_eq!(hub.inner.lock().unwrap().demand, 2);
        let _ = a.finish();
        assert_eq!(hub.inner.lock().unwrap().demand, 1, "ровно один декремент на сессию");
        drop(b);
        assert_eq!(hub.inner.lock().unwrap().demand, 0);
    }

    #[test]
    fn audio_state_strings() {
        assert_eq!(AudioState::Listening.as_str(), "listening");
        assert_eq!(AudioState::Muted.as_str(), "muted");
        assert_eq!(AudioState::from_u8(3), AudioState::Denied);
    }

    #[test]
    fn watchdog_tick_noop_when_not_running() {
        let hub = AudioHub::new(None, None);
        // не работает захват → tick безопасен, состояние не ломается
        hub.tick();
        hub.tick();
        assert_eq!(hub.state(), AudioState::Idle);
    }

    #[test]
    fn silence_decide_flags_after_two_silent_ticks() {
        let (t1, f1) = silence_decide(0, false);
        assert_eq!((t1, f1), (1, false), "первый тихий тик — ещё не флаг");
        let (t2, f2) = silence_decide(t1, false);
        assert_eq!((t2, f2), (2, true), "две тишины подряд → флаг");
        let (t3, f3) = silence_decide(t2, true);
        assert_eq!((t3, f3), (0, false), "звук сбрасывает счётчик и флаг");
    }

    #[test]
    fn nonzero_frames_only_grows_on_real_energy() {
        let hub = AudioHub::new(None, None);
        let _tap = hub.subscribe_wake();
        let before = hub.nonzero_frames.load(Ordering::Relaxed);
        // цифровая тишина (нули) — nonzero НЕ растёт, frames_seen растёт
        hub.ingest(&vec![0.0f32; FRAME_LEN * 2], 16_000, 1);
        assert_eq!(hub.nonzero_frames.load(Ordering::Relaxed), before, "тишина не растит nonzero");
        assert!(hub.frames_seen.load(Ordering::Relaxed) > 0, "frames_seen растёт даже в тишине");
        // реальная энергия — nonzero растёт
        hub.ingest(&sine(FRAME_LEN * 2, 0.05), 16_000, 1);
        assert!(hub.nonzero_frames.load(Ordering::Relaxed) > before, "звук растит nonzero");
    }

    #[test]
    fn frames_seen_grows_on_ingest() {
        let hub = AudioHub::new(None, None);
        let _tap = hub.subscribe_wake();
        let before = hub.frames_seen.load(Ordering::Relaxed);
        hub.ingest(&sine(FRAME_LEN * 3, 0.01), 16_000, 1);
        assert!(hub.frames_seen.load(Ordering::Relaxed) > before, "ingest растит счётчик живости");
    }
}
