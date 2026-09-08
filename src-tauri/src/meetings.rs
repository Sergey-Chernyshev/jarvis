//! Explicit, local meeting recording. Audio is streamed to a private WAV file;
//! transcription reads bounded chunks only after capture has stopped. Meeting
//! text never enters the dictation/clipboard/keyboard-insertion path.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};

use crate::daemon::Daemon;
use crate::stt::hub::{AudioState, DST_RATE};

mod system_audio;

const MAX_RECORDING_SECONDS: u64 = 12 * 60 * 60;
const CHUNK_SAMPLES: usize = DST_RATE as usize * 30;
const SILENCE_SEARCH_SAMPLES: usize = DST_RATE as usize * 5;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);
static MICROPHONE_OPERATION: Mutex<()> = Mutex::new(());

/// Serializes the check-and-start transition with push-to-talk dictation.
pub(crate) fn microphone_operation_lock() -> MutexGuard<'static, ()> {
    MICROPHONE_OPERATION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Meeting {
    pub id: String,
    pub title: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub duration_ms: u64,
    pub status: String,
    /// "microphone" or explicitly selected "microphone-system".
    pub source: String,
    pub transcript: String,
    pub segments: Vec<MeetingSegment>,
    pub audio_path: String,
    pub error: Option<String>,
    #[serde(default)]
    pub warning: Option<String>,
}

struct ActiveRecording {
    id: String,
    // 0: recording, 1: user stop, 2: application shutdown/start timeout.
    stop: Arc<AtomicU8>,
    finished: Arc<(Mutex<bool>, Condvar)>,
}

#[derive(Default)]
struct State {
    items: HashMap<String, Meeting>,
    active: Option<ActiveRecording>,
    processing: HashSet<String>,
}

pub struct Meetings {
    root: PathBuf,
    state: Mutex<State>,
    shutting_down: AtomicU8,
}

impl Meetings {
    pub fn new() -> Arc<Self> {
        Self::load_from(crate::util::jarvis_dir().join("meetings"))
    }

    fn load_from(root: PathBuf) -> Arc<Self> {
        let mut state = State::default();
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                // Do not follow arbitrary files/symlinks found in the archive.
                if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if !valid_id(&id) {
                    continue;
                }
                let path = entry.path().join("meeting.json");
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                let Ok(mut meeting) = serde_json::from_slice::<Meeting>(&bytes) else {
                    continue;
                };
                if meeting.id != id {
                    continue;
                }
                meeting.audio_path = entry
                    .path()
                    .join("audio.wav")
                    .to_string_lossy()
                    .into_owned();
                if matches!(meeting.status.as_str(), "recording" | "transcribing") {
                    meeting.status = "interrupted".into();
                    meeting.error = Some("Jarvis завершился до окончания записи или расшифровки. Сохранённое аудио можно расшифровать повторно.".into());
                    let names: &[&str] = if meeting.source == "microphone-system" {
                        &["microphone.wav", "system.wav"]
                    } else {
                        &["audio.wav"]
                    };
                    meeting.duration_ms = names
                        .iter()
                        .filter_map(|name| hound::WavReader::open(entry.path().join(name)).ok())
                        .map(|reader| reader.duration() as u64 * 1000 / DST_RATE as u64)
                        .max()
                        .unwrap_or(meeting.duration_ms);
                    meeting
                        .ended_at
                        .get_or_insert(meeting.started_at + meeting.duration_ms as i64);
                    if let Ok(bytes) = serde_json::to_vec_pretty(&meeting) {
                        let _ = crate::stt::transcripts::write_private_atomic(&path, &bytes);
                    }
                }
                state.items.insert(id, meeting);
            }
        }
        Arc::new(Self {
            root,
            state: Mutex::new(state),
            shutting_down: AtomicU8::new(0),
        })
    }

    pub fn is_recording(&self) -> bool {
        self.state.lock().unwrap().active.is_some()
    }

    pub fn list(&self) -> Vec<Meeting> {
        let mut items: Vec<_> = self.state.lock().unwrap().items.values().cloned().collect();
        items.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        items
    }

    pub fn get(&self, id: &str) -> Result<Meeting, String> {
        self.state
            .lock()
            .unwrap()
            .items
            .get(id)
            .cloned()
            .ok_or_else(|| "Встреча не найдена".into())
    }

    pub fn status(&self) -> Option<Meeting> {
        let state = self.state.lock().unwrap();
        let id = state
            .active
            .as_ref()
            .map(|a| &a.id)
            .or_else(|| state.processing.iter().next())?;
        state
            .items
            .get(id)
            .filter(|m| matches!(m.status.as_str(), "recording" | "transcribing"))
            .cloned()
    }

    fn persist(&self, meeting: &Meeting) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(meeting).map_err(|e| e.to_string())?;
        crate::stt::transcripts::write_private_atomic(
            &self.root.join(&meeting.id).join("meeting.json"),
            &bytes,
        )
        .map_err(|e| format!("Не удалось сохранить встречу: {e}"))
    }

    fn update(
        &self,
        id: &str,
        app: &tauri::AppHandle,
        edit: impl FnOnce(&mut Meeting),
    ) -> Result<Meeting, String> {
        let meeting = {
            let mut state = self.state.lock().unwrap();
            let meeting = state.items.get_mut(id).ok_or("Встреча не найдена")?;
            edit(meeting);
            // Keep the current status available to the UI even if the disk is full.
            self.persist(meeting)?;
            meeting.clone()
        };
        let _ = app.emit("meetings_changed", &meeting);
        Ok(meeting)
    }

    fn fail(&self, id: &str, app: &tauri::AppHandle, error: String) {
        let result = self.update(id, app, |m| {
            m.status = "error".into();
            m.error = Some(error);
            m.ended_at.get_or_insert(crate::util::now_ms());
        });
        if let Err(error) = result {
            crate::log::line(&format!("[meetings] {error}"));
            if let Ok(meeting) = self.get(id) {
                let _ = app.emit("meetings_changed", &meeting);
            }
        }
    }

    pub fn start(
        self: &Arc<Self>,
        d: Arc<Daemon>,
        title: Option<String>,
        source: Option<String>,
    ) -> Result<Meeting, String> {
        if crate::native_smoke::enabled() {
            return Err("Запись живого аудио отключена в изолированном режиме проверки".into());
        }
        let _transition = microphone_operation_lock();
        if self.shutting_down.load(Ordering::SeqCst) != 0 {
            return Err("Jarvis завершает работу".into());
        }
        if d.dictation.is_capturing() || d.interaction.is_active() {
            return Err("Сначала завершите диктовку или голосовой разговор".into());
        }
        if d.audio.is_muted() {
            return Err("Микрофон выключен. Включите его перед записью встречи".into());
        }
        let title = clean_title(title)?;
        let source = system_audio::validate_source(source)?;
        crate::stt::mic_permission::require_authorized()?;
        let id = format!(
            "{}-{}-{}",
            crate::util::now_ms(),
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        );
        let dir = self.root.join(&id);
        let stop = Arc::new(AtomicU8::new(0));
        let finished = Arc::new((Mutex::new(false), Condvar::new()));
        let meeting = {
            let mut state = self.state.lock().unwrap();
            ensure_idle(&state)?;
            create_private_dir(&self.root)?;
            create_private_dir(&dir)?;
            let meeting = Meeting {
                id: id.clone(),
                title,
                started_at: crate::util::now_ms(),
                ended_at: None,
                duration_ms: 0,
                status: "recording".into(),
                source,
                transcript: String::new(),
                segments: Vec::new(),
                audio_path: dir.join("audio.wav").to_string_lossy().into_owned(),
                error: None,
                warning: None,
            };
            self.persist(&meeting)?;
            state.items.insert(id.clone(), meeting.clone());
            state.active = Some(ActiveRecording {
                id: id.clone(),
                stop: stop.clone(),
                finished: finished.clone(),
            });
            meeting
        };
        drop(_transition); // active reservation now protects the start transition
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let service = self.clone();
        let app = d.app.clone();
        let worker_id = id.clone();
        let worker_stop = stop.clone();
        let worker_finished = finished.clone();
        let spawn = std::thread::Builder::new()
            .name("jarvis-meeting-capture".into())
            .spawn(move || {
                service.record(d, worker_id, worker_stop, worker_finished, ready_tx);
            });
        if let Err(error) = spawn {
            self.state.lock().unwrap().active = None;
            self.fail(&id, &app, format!("Не удалось запустить запись: {error}"));
            return Err(format!("Не удалось запустить запись: {error}"));
        }
        match ready_rx.recv_timeout(Duration::from_secs(12)) {
            Ok(Ok(())) => Ok(self.get(&id).unwrap_or(meeting)),
            Ok(Err(error)) => Err(error),
            Err(_) => {
                stop.store(2, Ordering::SeqCst);
                Err("Не удалось запустить запись за 12 секунд. Проверьте разрешения на микрофон и системный звук, затем повторите запуск.".into())
            }
        }
    }

    fn record(
        self: &Arc<Self>,
        d: Arc<Daemon>,
        id: String,
        stop: Arc<AtomicU8>,
        finished: Arc<(Mutex<bool>, Condvar)>,
        ready: mpsc::SyncSender<Result<(), String>>,
    ) {
        // Always release both the voice interaction and capture reservation.
        struct CaptureGuard {
            meetings: Arc<Meetings>,
            d: Arc<Daemon>,
            id: String,
            finished: Arc<(Mutex<bool>, Condvar)>,
        }
        impl Drop for CaptureGuard {
            fn drop(&mut self) {
                {
                    let mut state = self.meetings.state.lock().unwrap();
                    if state.active.as_ref().is_some_and(|a| a.id == self.id) {
                        state.active = None;
                    }
                }
                self.d.interaction_leave_and_flush();
                *self.finished.0.lock().unwrap() = true;
                self.finished.1.notify_all();
            }
        }
        d.interaction.enter();
        let guard = CaptureGuard {
            meetings: self.clone(),
            d: d.clone(),
            id: id.clone(),
            finished,
        };
        let include_system = self.get(&id).is_ok_and(|m| m.source == "microphone-system");
        let dir = self.root.join(&id);
        let path = dir.join(if include_system {
            "microphone.wav"
        } else {
            "audio.wav"
        });
        let mut writer = match AudioWriter::create(&path) {
            Ok(writer) => writer,
            Err(error) => {
                let _ = ready.send(Err(error.clone()));
                self.fail(&id, &d.app, error);
                return;
            }
        };
        let anchor_ns = if include_system {
            system_audio::host_time_ns()
        } else {
            0
        };
        let mut system = if include_system {
            match system_audio::SystemCapture::start(
                &dir.join("system.wav"),
                anchor_ns,
                stop.clone(),
            ) {
                Ok(capture) => Some(capture),
                Err(error) => {
                    let _ = writer.finish();
                    self.fail(&id, &d.app, error.clone());
                    let _ = ready.send(Err(error));
                    return;
                }
            }
        } else {
            None
        };
        // A stop/exit that arrived while macOS displayed its permission dialog
        // must not open the microphone after the user has already cancelled.
        if stop.load(Ordering::SeqCst) != 0 {
            if let Some(system) = system.take() {
                let _ = system.finish();
            }
            let _ = writer.finish();
            self.fail(&id, &d.app, "Запуск записи отменён".into());
            let _ = ready.send(Err("Запуск записи отменён".into()));
            return;
        }
        let tap = d.audio.subscribe_wake(); // streaming subscription, with NO pre-roll
        // Opening CoreAudio is asynchronous. Idle/Starting immediately after
        // subscribe is not an error, and Listening alone does not prove samples.
        // Ack readiness only after the first real frame, preserving that frame.
        let first_frame = wait_for_first_frame(
            || tap.recv_timeout(Duration::from_millis(50)),
            || d.audio.state(),
            || stop.load(Ordering::SeqCst) != 0,
            Duration::from_secs(8),
        );
        let startup = first_frame.and_then(|frame| {
            if include_system { system_audio::align_microphone_start(&mut writer, anchor_ns, frame.len())?; }
            writer.push(&frame)
        });
        if let Err(error) = startup {
            if let Some(system) = system.take() {
                let _ = system.finish();
            }
            let _ = writer.finish();
            drop(tap);
            self.fail(&id, &d.app, error.clone());
            let _ = ready.send(Err(error));
            return;
        }
        if ready.send(Ok(())).is_err() || stop.load(Ordering::SeqCst) == 2 {
            stop.store(2, Ordering::SeqCst);
        }
        if let Ok(meeting) = self.get(&id) {
            let _ = d.app.emit("meetings_changed", &meeting);
        }
        let mut last_frame = Instant::now();
        let mut last_flush = Instant::now();
        let mut error = None;
        let mut warning = None;
        while stop.load(Ordering::SeqCst) == 0 {
            if d.audio.is_muted() {
                warning = Some("Запись остановлена, потому что микрофон был выключен. Аудио до этого момента сохранено.".into());
                break;
            }
            if let Some(frame) = tap.recv_timeout(Duration::from_millis(100)) {
                last_frame = Instant::now();
                if include_system {
                    if let Err(e) =
                        system_audio::align_microphone_start(&mut writer, anchor_ns, frame.len())
                    {
                        error = Some(e);
                        break;
                    }
                }
                if let Err(e) = writer.push(&frame) {
                    error = Some(e);
                    break;
                }
            } else if last_frame.elapsed() > Duration::from_secs(5) {
                error = Some("Микрофон перестал передавать звук. Запись остановлена; проверьте устройство ввода. Уже записанное аудио сохранено.".into());
                break;
            }
            if let Some(system) = system.as_mut() {
                if let Err(e) = system.drain() {
                    error = Some(e);
                    break;
                }
            }
            if writer.samples >= MAX_RECORDING_SECONDS * DST_RATE as u64 {
                warning = Some(
                    "Достигнут предел одной записи — 12 часов. Можно начать новую встречу.".into(),
                );
                break;
            }
            if last_flush.elapsed() >= Duration::from_secs(1) {
                if let Some(system) = system.as_mut() {
                    if let Err(e) = system.flush() {
                        error = Some(e);
                        break;
                    }
                }
                // hound updates the WAV header at every flush. A crash therefore
                // loses at most the latest second, not the entire meeting.
                let saved = writer.flush().and_then(|_| {
                    self.update(&id, &d.app, |m| {
                        m.duration_ms = writer.duration_ms();
                    })
                    .map(|_| ())
                });
                if let Err(e) = saved {
                    error = Some(e);
                    break;
                }
                last_flush = Instant::now();
            }
        }
        let duration_ms = writer.duration_ms();
        if let Some(system) = system.take() {
            if let Err(e) = system.finish() {
                error = Some(e);
            }
        }
        if let Err(e) = writer.finish() {
            error = Some(e);
        }
        drop(tap); // release the physical microphone before marking capture done
        if duration_ms == 0 && error.is_none() && stop.load(Ordering::SeqCst) != 2 {
            error = Some(
                "Микрофон не записал звук. Проверьте разрешение на микрофон и повторите запись."
                    .into(),
            );
        }
        let interrupted =
            stop.load(Ordering::SeqCst) == 2 || self.shutting_down.load(Ordering::SeqCst) != 0;
        if error.is_none() && !interrupted {
            self.state.lock().unwrap().processing.insert(id.clone());
        }
        let updated = self.update(&id, &d.app, |m| {
            m.ended_at = Some(crate::util::now_ms());
            m.duration_ms = duration_ms;
            m.warning = warning;
            m.error = error.clone();
            m.status = if interrupted { "interrupted" } else if error.is_some() { "error" } else { "transcribing" }.into();
            if interrupted {
                m.error = Some("Запись остановлена при завершении Jarvis. Сохранённое аудио можно расшифровать повторно.".into());
            }
        });
        drop(guard); // stop() is now free to return; long STT never blocks it
        if let Err(e) = updated {
            self.state.lock().unwrap().processing.remove(&id);
            self.fail(&id, &d.app, e);
            return;
        }
        if error.is_none() && !interrupted {
            self.transcribe(d, id);
        }
    }

    pub fn stop(&self) -> Result<Meeting, String> {
        let (id, finished) = {
            let state = self.state.lock().unwrap();
            let active = state.active.as_ref().ok_or("Нет активной записи встречи")?;
            active.stop.store(1, Ordering::SeqCst);
            (active.id.clone(), active.finished.clone())
        };
        let done = finished.0.lock().unwrap();
        let (_done, timeout) = finished
            .1
            .wait_timeout_while(done, Duration::from_secs(8), |done| !*done)
            .unwrap();
        if timeout.timed_out() {
            return Err("Остановка микрофона занимает больше времени. Запись уже останавливается; дождитесь обновления статуса.".into());
        }
        self.get(&id)
    }

    /// Flush on exit without starting an expensive transcription while quitting.
    pub fn dispose(&self) {
        self.shutting_down.store(1, Ordering::SeqCst);
        let finished = {
            let state = self.state.lock().unwrap();
            state.active.as_ref().map(|active| {
                active.stop.store(2, Ordering::SeqCst);
                active.finished.clone()
            })
        };
        if let Some(finished) = finished {
            let done = finished.0.lock().unwrap();
            let _ = finished
                .1
                .wait_timeout_while(done, Duration::from_secs(3), |done| !*done);
        }
    }

    pub fn retranscribe(self: &Arc<Self>, d: Arc<Daemon>, id: String) -> Result<Meeting, String> {
        {
            let mut state = self.state.lock().unwrap();
            ensure_idle(&state)?;
            let meeting = state.items.get(&id).ok_or("Встреча не найдена")?;
            let dir = self.root.join(&id);
            let has_audio = if meeting.source == "microphone-system" {
                dir.join("microphone.wav").is_file() && dir.join("system.wav").is_file()
            } else {
                dir.join("audio.wav").is_file()
            };
            if meeting.duration_ms == 0 || !has_audio {
                return Err("У этой встречи нет сохранённого аудио".into());
            }
            state.processing.insert(id.clone());
        }
        let meeting = match self.update(&id, &d.app, |m| {
            m.status = "transcribing".into();
            m.error = None;
            m.transcript.clear();
            m.segments.clear();
        }) {
            Ok(meeting) => meeting,
            Err(error) => {
                self.state.lock().unwrap().processing.remove(&id);
                return Err(error);
            }
        };
        let service = self.clone();
        let worker_id = id.clone();
        let app = d.app.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("jarvis-meeting-stt".into())
            .spawn(move || {
                service.transcribe(d, worker_id);
            })
        {
            self.state.lock().unwrap().processing.remove(&id);
            self.fail(
                &id,
                &app,
                format!("Не удалось запустить расшифровку: {error}"),
            );
            return Err(format!("Не удалось запустить расшифровку: {error}"));
        }
        Ok(meeting)
    }

    fn transcribe(&self, d: Arc<Daemon>, id: String) {
        if self.get(&id).is_ok_and(|m| m.source == "microphone-system") {
            match system_audio::mix_tracks(&self.root.join(&id)) {
                Ok(duration) => {
                    if let Err(error) = self.update(&id, &d.app, |m| m.duration_ms = duration) {
                        self.state.lock().unwrap().processing.remove(&id);
                        self.fail(&id, &d.app, error);
                        return;
                    }
                }
                Err(error) => {
                    self.state.lock().unwrap().processing.remove(&id);
                    self.fail(&id, &d.app, format!("Не удалось свести аудиодорожки: {error}. Исходные записи сохранены; можно повторить расшифровку."));
                    return;
                }
            }
        }
        let mut opts = d.stt.options();
        // A meeting transcript preserves the original language, regardless of
        // the dictation setting that may translate input into English.
        opts.task = crate::stt::engine::SttTask::Transcribe;
        let result = transcribe_file(
            &self.root.join(&id).join("audio.wav"),
            |pcm| {
                if self.shutting_down.load(Ordering::SeqCst) != 0 {
                    return Err("Расшифровка прервана завершением Jarvis. Аудио сохранено.".into());
                }
                d.stt.transcribe(pcm, &opts).map(|r| r.text)
            },
            |segment| {
                self.update(&id, &d.app, |m| {
                    if !m.transcript.is_empty() {
                        m.transcript.push('\n');
                    }
                    m.transcript.push_str(&segment.text);
                    m.segments.push(segment);
                })
                .map(|_| ())
            },
        );
        match result {
            Ok(()) => {
                if let Err(error) = self.update(&id, &d.app, |m| {
                    m.status = "ready".into();
                    m.error = None;
                    if m.transcript.is_empty() {
                        m.warning = Some("Речь не обнаружена. Аудиозапись сохранена; проверьте микрофон или попробуйте другую модель распознавания.".into());
                    }
                }) {
                    self.fail(&id, &d.app, error);
                }
            }
            Err(error) => self.fail(&id, &d.app, format!("Не удалось расшифровать встречу: {error}. Аудио и уже распознанные фрагменты сохранены; можно повторить расшифровку.")),
        }
        self.state.lock().unwrap().processing.remove(&id);
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 96 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn clean_title(title: Option<String>) -> Result<String, String> {
    let title = title.unwrap_or_default().trim().to_owned();
    if title.chars().count() > 200 {
        return Err("Название встречи должно быть не длиннее 200 символов".into());
    }
    if title.chars().any(char::is_control) {
        return Err("Название встречи не должно содержать управляющие символы".into());
    }
    Ok(if title.is_empty() {
        "Новая встреча".into()
    } else {
        title
    })
}

fn ensure_idle(state: &State) -> Result<(), String> {
    if state.active.is_some() {
        Err("Уже идёт запись встречи. Сначала остановите её".into())
    } else if !state.processing.is_empty() {
        Err("Дождитесь завершения текущей расшифровки встречи".into())
    } else {
        Ok(())
    }
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|e| format!("Не удалось создать каталог встречи: {e}"))
}

struct AudioWriter {
    writer: hound::WavWriter<BufWriter<File>>,
    samples: u64,
}

impl AudioWriter {
    fn create(path: &Path) -> Result<Self, String> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(path)
            .map_err(|e| format!("Не удалось создать аудиозапись: {e}"))?;
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: DST_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::new(BufWriter::new(file), spec)
            .map_err(|e| format!("Не удалось создать WAV: {e}"))?;
        Ok(Self { writer, samples: 0 })
    }

    fn push(&mut self, samples: &[f32]) -> Result<(), String> {
        for &sample in samples {
            let sample = if sample.is_finite() {
                (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
            } else {
                0
            };
            self.writer.write_sample(sample).map_err(|e| {
                format!("Не удалось записать аудио (проверьте свободное место): {e}")
            })?;
            self.samples += 1;
        }
        Ok(())
    }

    fn duration_ms(&self) -> u64 {
        self.samples * 1000 / DST_RATE as u64
    }

    fn push_silence(&mut self, mut count: u64) -> Result<(), String> {
        if self.samples.saturating_add(count) > (MAX_RECORDING_SECONDS + 60) * DST_RATE as u64 {
            return Err("Превышена максимальная длительность аудиозаписи".into());
        }
        let silence = [0.0; 1600];
        while count > 0 {
            let length = count.min(silence.len() as u64) as usize;
            self.push(&silence[..length])?;
            count -= length as u64;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), String> {
        self.writer
            .flush()
            .map_err(|e| format!("Не удалось сохранить WAV: {e}"))
    }

    fn finish(self) -> Result<(), String> {
        self.writer
            .finalize()
            .map_err(|e| format!("Не удалось завершить WAV: {e}"))
    }
}

/// Prefer a 200 ms quiet interval near a chunk's end to avoid cutting a word.
/// No overlap or deduplication heuristic can accidentally remove repeated speech.
fn chunk_boundary(pcm: &[f32]) -> usize {
    if pcm.len() < CHUNK_SAMPLES {
        return pcm.len();
    }
    let window = DST_RATE as usize / 5;
    let start = CHUNK_SAMPLES - SILENCE_SEARCH_SAMPLES;
    for end in (start + window..=CHUNK_SAMPLES).rev().step_by(window) {
        let mean_square = pcm[end - window..end].iter().map(|s| s * s).sum::<f32>() / window as f32;
        if mean_square < 0.000_004 {
            return end - window / 2;
        }
    }
    CHUNK_SAMPLES
}

fn transcribe_file(
    path: &Path,
    mut transcribe: impl FnMut(&[f32]) -> Result<String, String>,
    mut emit: impl FnMut(MeetingSegment) -> Result<(), String>,
) -> Result<(), String> {
    let mut reader =
        hound::WavReader::open(path).map_err(|e| format!("Не удалось прочитать WAV: {e}"))?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.sample_rate != DST_RATE
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        return Err("Ожидается аудиозапись Jarvis: WAV PCM16, 16 кГц, моно".into());
    }
    let mut samples = reader.samples::<i16>();
    let mut pcm = Vec::with_capacity(CHUNK_SAMPLES);
    let mut offset = 0u64;
    loop {
        while pcm.len() < CHUNK_SAMPLES {
            match samples.next() {
                Some(Ok(sample)) => pcm.push(sample as f32 / i16::MAX as f32),
                Some(Err(error)) => return Err(format!("Повреждён аудиофайл: {error}")),
                None => break,
            }
        }
        if pcm.is_empty() {
            break;
        }
        let boundary = chunk_boundary(&pcm);
        let chunk = &pcm[..boundary];
        // Digital silence must not be fed into an ASR model as speech.
        if chunk.iter().any(|s| s.abs() > 0.0001) {
            let text = transcribe(chunk)?.trim().to_owned();
            if !text.is_empty() {
                emit(MeetingSegment {
                    start_ms: offset * 1000 / DST_RATE as u64,
                    end_ms: (offset + boundary as u64) * 1000 / DST_RATE as u64,
                    text,
                })?;
            }
        }
        offset += boundary as u64;
        pcm.drain(..boundary);
    }
    Ok(())
}

#[tauri::command]
pub fn meetings_list(app: tauri::AppHandle) -> Vec<Meeting> {
    app.state::<Arc<Meetings>>().list()
}

#[tauri::command]
pub fn meetings_status(app: tauri::AppHandle) -> Option<Meeting> {
    app.state::<Arc<Meetings>>().status()
}

#[tauri::command]
pub fn meetings_get(app: tauri::AppHandle, id: String) -> Result<Meeting, String> {
    app.state::<Arc<Meetings>>().get(&id)
}

#[tauri::command]
pub fn meetings_sources() -> Vec<system_audio::Source> {
    system_audio::sources()
}

#[tauri::command]
pub async fn meetings_start(
    app: tauri::AppHandle,
    title: Option<String>,
    source: Option<String>,
) -> Result<Meeting, String> {
    let meetings = app.state::<Arc<Meetings>>().inner().clone();
    let d = Daemon::get(&app);
    tauri::async_runtime::spawn_blocking(move || meetings.start(d, title, source))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn meetings_stop(app: tauri::AppHandle) -> Result<Meeting, String> {
    let meetings = app.state::<Arc<Meetings>>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || meetings.stop())
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn meetings_retranscribe(app: tauri::AppHandle, id: String) -> Result<Meeting, String> {
    let meetings = app.state::<Arc<Meetings>>().inner().clone();
    let d = Daemon::get(&app);
    tauri::async_runtime::spawn_blocking(move || meetings.retranscribe(d, id))
        .await
        .map_err(|e| e.to_string())?
}

/// The receiver supplies actual frames in production. Tests can delay that
/// supply without opening the host microphone or bypassing startup decisions.
fn wait_for_first_frame(
    mut receive: impl FnMut() -> Option<Arc<[f32]>>,
    state: impl Fn() -> AudioState,
    cancelled: impl Fn() -> bool,
    timeout: Duration,
) -> Result<Arc<[f32]>, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if cancelled() { return Err("Запуск записи отменён".into()); }
        if let Some(error) = state().capture_error() { return Err(error.into()); }
        if Instant::now() >= deadline {
            return Err("Микрофон не начал передавать звук. Проверьте устройство и разрешения, затем начните запись ещё раз.".into());
        }
        if let Some(frame) = receive() {
            if cancelled() { return Err("Запуск записи отменён".into()); }
            if let Some(error) = state().capture_error() { return Err(error.into()); }
            if !frame.is_empty() { return Ok(frame); }
        }
    }
}

#[cfg(test)]
mod tests;
