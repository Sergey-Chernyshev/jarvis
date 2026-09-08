//! Native system output capture and disk-backed track alignment. No permission
//! query starts capture. The Objective-C callback copies into a bounded queue;
//! file I/O and mixing run on the meeting worker, never that callback.

use super::{AudioWriter, DST_RATE, MAX_RECORDING_SECONDS};
use std::path::{Path, PathBuf};

#[derive(Clone, serde::Serialize)]
pub struct Source {
    pub id: &'static str,
    pub label: &'static str,
    pub available: bool,
    pub reason: Option<&'static str>,
}

pub fn sources() -> Vec<Source> {
    let available = native::available();
    vec![
        Source {
            id: "microphone",
            label: "В комнате · микрофон",
            available: true,
            reason: None,
        },
        Source {
            id: "microphone-system",
            label: "Онлайн · микрофон и звук приложений",
            available,
            reason: if available {
                None
            } else {
                Some("Системный звук доступен на macOS 13 и новее")
            },
        },
    ]
}

pub fn validate_source(source: Option<String>) -> Result<String, String> {
    let source = source.unwrap_or_else(|| "microphone".into());
    match source.as_str() {
        "microphone" => Ok(source),
        "microphone-system" if native::available() => Ok(source),
        "microphone-system" => Err("Запись системного звука доступна только на macOS 13 и новее. Выберите запись микрофона.".into()),
        _ => Err("Неизвестный источник записи встречи".into()),
    }
}

pub fn host_time_ns() -> u64 {
    native::host_time_ns()
}

struct Packet {
    samples: Vec<f32>,
    timestamp_ns: u64,
    sample_rate: u32,
}

pub struct SystemCapture {
    native: native::Capture,
    writer: Option<AudioWriter>,
    anchor_ns: u64,
}

impl SystemCapture {
    pub fn start(
        path: &Path,
        anchor_ns: u64,
        stop: std::sync::Arc<std::sync::atomic::AtomicU8>,
    ) -> Result<Self, String> {
        let writer = AudioWriter::create(path)?;
        let native = native::Capture::start(stop)?;
        Ok(Self {
            native,
            writer: Some(writer),
            anchor_ns,
        })
    }

    pub fn drain(&mut self) -> Result<(), String> {
        while let Some(packet) = self.native.next() {
            if packet.sample_rate != DST_RATE {
                return Err(
                    "Системный аудиопоток не соответствует запрошенному формату 16 кГц".into(),
                );
            }
            // Preserve every native packet, including a final tail shorter than
            // an AudioHub frame. Timestamp gaps are represented as silence.
            let offset = time_offset_samples(packet.timestamp_ns, self.anchor_ns)?;
            append_at(self.writer.as_mut().unwrap(), offset, &packet.samples)?;
        }
        self.native.error()
    }

    pub fn flush(&mut self) -> Result<(), String> {
        self.drain()?;
        self.writer.as_mut().unwrap().flush()
    }

    pub fn finish(mut self) -> Result<(), String> {
        self.native.stop();
        let drained = self.drain();
        let finalized = self.writer.take().unwrap().finish();
        drained.and(finalized)
    }
}

fn time_offset_samples(timestamp_ns: u64, anchor_ns: u64) -> Result<u64, String> {
    let elapsed = timestamp_ns.saturating_sub(anchor_ns);
    if elapsed > (MAX_RECORDING_SECONDS + 60) * 1_000_000_000 {
        return Err("Системный аудиопоток вернул неверную временную метку".into());
    }
    Ok(elapsed * DST_RATE as u64 / 1_000_000_000)
}

/// Keep packet timing on disk: gaps become silence, overlaps are trimmed. No
/// allocation grows with the meeting length.
fn append_at(writer: &mut AudioWriter, offset: u64, samples: &[f32]) -> Result<(), String> {
    if offset > writer.samples {
        writer.push_silence(offset - writer.samples)?;
    }
    let skip = writer
        .samples
        .saturating_sub(offset)
        .min(samples.len() as u64) as usize;
    writer.push(&samples[skip..])
}

/// First mic packet arrives after its audio interval. AudioHub has no native
/// timestamps, so its initial alignment uses callback arrival minus duration;
/// following packets remain contiguous, avoiding drift from scheduler jitter.
pub fn align_microphone_start(
    writer: &mut AudioWriter,
    anchor_ns: u64,
    frame_len: usize,
) -> Result<(), String> {
    if writer.samples == 0 {
        let packet_start =
            host_time_ns().saturating_sub(frame_len as u64 * 1_000_000_000 / DST_RATE as u64);
        writer.push_silence(time_offset_samples(packet_start, anchor_ns)?)?;
    }
    Ok(())
}

/// Atomically create the combined STT WAV. Original tracks survive all failure
/// paths, including an interrupted mix, and remain available for retranscription.
pub fn mix_tracks(dir: &Path) -> Result<u64, String> {
    let microphone = dir.join("microphone.wav");
    let system = dir.join("system.wav");
    let temporary = dir.join(format!(
        ".mix-{}-{}.wav",
        std::process::id(),
        super::NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _cleanup = Cleanup(temporary.clone());
    let mut mic = open_track(&microphone)?;
    let mut sys = open_track(&system)?;
    let mut writer = AudioWriter::create(&temporary)?;
    let mut mic_samples = mic.samples::<i16>();
    let mut sys_samples = sys.samples::<i16>();
    let mut block = Vec::with_capacity(4096);
    loop {
        let a = mic_samples
            .next()
            .transpose()
            .map_err(|e| format!("Повреждена запись микрофона: {e}"))?;
        let b = sys_samples
            .next()
            .transpose()
            .map_err(|e| format!("Повреждена запись системного звука: {e}"))?;
        if a.is_none() && b.is_none() {
            break;
        }
        // Mix with headroom; retain full level when just one track is audible.
        let a = a.unwrap_or(0) as f32 / i16::MAX as f32;
        let b = b.unwrap_or(0) as f32 / i16::MAX as f32;
        block.push(if a == 0.0 {
            b
        } else if b == 0.0 {
            a
        } else {
            (a + b) * 0.5
        });
        if block.len() == 4096 {
            writer.push(&block)?;
            block.clear();
        }
    }
    writer.push(&block)?;
    let duration = writer.duration_ms();
    writer.finish()?;
    std::fs::rename(&temporary, dir.join("audio.wav"))
        .map_err(|e| format!("Не удалось сохранить сведение встречи: {e}"))?;
    Ok(duration)
}

fn open_track(path: &Path) -> Result<hound::WavReader<std::io::BufReader<std::fs::File>>, String> {
    let reader = hound::WavReader::open(path)
        .map_err(|e| format!("Не удалось открыть {}: {e}", path.display()))?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.sample_rate != DST_RATE
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
        || reader.duration() as u64 > (MAX_RECORDING_SECONDS + 60) * DST_RATE as u64
    {
        return Err("Неверный формат или длительность аудиодорожки встречи".into());
    }
    Ok(reader)
}

#[cfg(target_os = "macos")]
mod native {
    use super::Packet;
    use std::ffi::{c_char, c_void, CStr};
    use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
    use std::sync::mpsc::{self, Receiver, SyncSender};
    use std::sync::Arc;

    extern "C" {
        fn jarvis_system_audio_available() -> i32;
        fn jarvis_system_audio_host_time_ns() -> u64;
        fn jarvis_system_audio_start(
            callback: unsafe extern "C" fn(*mut c_void, *const f32, usize, u64, u32),
            cancelled: unsafe extern "C" fn(*mut c_void) -> i32,
            context: *mut c_void,
            error: *mut c_char,
            capacity: usize,
        ) -> *mut c_void;
        fn jarvis_system_audio_error(
            handle: *mut c_void,
            error: *mut c_char,
            capacity: usize,
        ) -> i32;
        fn jarvis_system_audio_stop(handle: *mut c_void);
    }

    struct CallbackState {
        tx: SyncSender<Packet>,
        overflow: AtomicBool,
        stop: Arc<AtomicU8>,
    }
    unsafe extern "C" fn is_cancelled(context: *mut c_void) -> i32 {
        if context.is_null() {
            return 1;
        }
        let state = &*(context as *const CallbackState);
        i32::from(state.stop.load(Ordering::SeqCst) != 0)
    }
    unsafe extern "C" fn on_audio(
        context: *mut c_void,
        samples: *const f32,
        count: usize,
        timestamp_ns: u64,
        sample_rate: u32,
    ) {
        if context.is_null() || samples.is_null() || count == 0 {
            return;
        }
        let state = &*(context as *const CallbackState);
        // Defensive length bound on a native callback, and no blocking work.
        if count > 192_000 {
            state.overflow.store(true, Ordering::Relaxed);
            return;
        }
        let packet = Packet {
            samples: std::slice::from_raw_parts(samples, count).to_vec(),
            timestamp_ns,
            sample_rate,
        };
        if state.tx.try_send(packet).is_err() {
            state.overflow.store(true, Ordering::Relaxed);
        }
    }

    pub fn available() -> bool {
        unsafe { jarvis_system_audio_available() != 0 }
    }
    pub fn host_time_ns() -> u64 {
        unsafe { jarvis_system_audio_host_time_ns() }
    }

    pub struct Capture {
        handle: *mut c_void,
        context: Box<CallbackState>,
        rx: Receiver<Packet>,
        last_error: Option<String>,
    }

    impl Capture {
        pub fn start(stop: Arc<AtomicU8>) -> Result<Self, String> {
            let (tx, rx) = mpsc::sync_channel(512);
            let mut context = Box::new(CallbackState {
                tx,
                overflow: AtomicBool::new(false),
                stop,
            });
            let mut error = [0 as c_char; 1024];
            let handle = unsafe {
                jarvis_system_audio_start(
                    on_audio,
                    is_cancelled,
                    (&mut *context) as *mut CallbackState as *mut c_void,
                    error.as_mut_ptr(),
                    error.len(),
                )
            };
            if handle.is_null() {
                let reason = unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy();
                return Err(format!("Системный звук не запущен: {reason}. Проверьте разрешение Jarvis на запись экрана и системного аудио в Системных настройках."));
            }
            Ok(Self {
                handle,
                context,
                rx,
                last_error: None,
            })
        }

        pub fn next(&self) -> Option<Packet> {
            self.rx.try_recv().ok()
        }
        pub fn error(&self) -> Result<(), String> {
            if self.context.overflow.load(Ordering::Relaxed) {
                return Err("Не удалось вовремя сохранить системный звук: аудиоочередь переполнена. Проверьте свободное место и нагрузку на диск.".into());
            }
            if let Some(error) = &self.last_error {
                return Err(error.clone());
            }
            if !self.handle.is_null() {
                let mut error = [0 as c_char; 1024];
                if unsafe {
                    jarvis_system_audio_error(self.handle, error.as_mut_ptr(), error.len())
                } != 0
                {
                    return Err(unsafe { CStr::from_ptr(error.as_ptr()) }
                        .to_string_lossy()
                        .into_owned());
                }
            }
            Ok(())
        }
        pub fn stop(&mut self) {
            if !self.handle.is_null() {
                self.last_error = self.error().err();
                unsafe {
                    jarvis_system_audio_stop(self.handle);
                }
                self.handle = std::ptr::null_mut();
            }
        }
    }
    impl Drop for Capture {
        fn drop(&mut self) {
            self.stop();
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod native {
    use super::Packet;
    pub fn available() -> bool {
        false
    }
    pub fn host_time_ns() -> u64 {
        0
    }
    pub struct Capture;
    impl Capture {
        pub fn start(_stop: std::sync::Arc<std::sync::atomic::AtomicU8>) -> Result<Self, String> {
            Err("Системный звук поддерживается на macOS 13 и новее".into())
        }
        pub fn next(&self) -> Option<Packet> {
            None
        }
        pub fn error(&self) -> Result<(), String> {
            Ok(())
        }
        pub fn stop(&mut self) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meetings::tests::TestDir;

    #[test]
    fn packet_gaps_and_overlap_keep_a_single_continuous_timeline() {
        let dir = TestDir::new();
        let path = dir.0.join("timeline.wav");
        let mut writer = AudioWriter::create(&path).unwrap();
        append_at(&mut writer, 3, &[0.2, 0.3, 0.4]).unwrap();
        append_at(&mut writer, 5, &[0.8, 0.5, 0.6]).unwrap();
        writer.finish().unwrap();
        let pcm: Vec<i16> = hound::WavReader::open(&path)
            .unwrap()
            .samples()
            .map(Result::unwrap)
            .collect();
        assert_eq!(pcm.len(), 8);
        assert_eq!(&pcm[..3], &[0, 0, 0]);
        assert_eq!(
            pcm[5],
            (0.4 * i16::MAX as f32) as i16,
            "overlap must not replace the already saved sample"
        );
        assert_eq!(pcm[6], (0.5 * i16::MAX as f32) as i16);
    }

    #[test]
    fn mixing_retains_both_speakers_and_different_track_lengths() {
        let dir = TestDir::new();
        for (name, pcm) in [
            ("microphone.wav", vec![0.5, 0.0, 0.5]),
            ("system.wav", vec![0.0, 0.5, 0.5, 0.25]),
        ] {
            let mut writer = AudioWriter::create(&dir.0.join(name)).unwrap();
            writer.push(&pcm).unwrap();
            writer.finish().unwrap();
        }
        mix_tracks(&dir.0).unwrap();
        let pcm: Vec<i16> = hound::WavReader::open(dir.0.join("audio.wav"))
            .unwrap()
            .samples()
            .map(Result::unwrap)
            .collect();
        assert_eq!(pcm.len(), 4);
        assert!(pcm[..3]
            .iter()
            .all(|s| (*s - (0.5 * i16::MAX as f32) as i16).abs() <= 1));
        assert!((pcm[3] - (0.25 * i16::MAX as f32) as i16).abs() <= 1);
        assert!(dir.0.join("microphone.wav").exists() && dir.0.join("system.wav").exists());
    }

    #[test]
    fn invalid_track_does_not_replace_previous_mix() {
        let dir = TestDir::new();
        std::fs::write(dir.0.join("audio.wav"), b"previous mix").unwrap();
        assert!(mix_tracks(&dir.0).is_err());
        assert_eq!(
            std::fs::read(dir.0.join("audio.wav")).unwrap(),
            b"previous mix"
        );
    }

    #[test]
    fn timeline_rejects_unbounded_silence_allocation_and_preserves_pre_start_packets() {
        assert_eq!(time_offset_samples(50, 100).unwrap(), 0);
        assert_eq!(time_offset_samples(1_000_000_100, 100).unwrap(), 16000);
        assert!(time_offset_samples(u64::MAX, 0).is_err());
        assert!(validate_source(Some("unknown".into())).is_err());
        assert_eq!(validate_source(None).unwrap(), "microphone");
    }
}
