//! Трейт движка TTS + реализации. Piper — subprocess; Silero — заглушка (Фаза 2).
//! Любая ошибка движка — fail-safe: вернуть TtsError, демон не падает.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Debug)]
pub enum TtsError { NotInstalled(String), Synthesis(String) }

impl std::fmt::Display for TtsError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            TtsError::NotInstalled(s) => write!(f, "движок не установлен: {s}"),
            TtsError::Synthesis(s) => write!(f, "ошибка синтеза: {s}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct VoiceSel {
    pub speaker: String,
    pub voice_path: String,
    pub sample_rate: u32,
    /// темп речи Silero (SSML): x-slow|slow|medium|fast|x-fast
    pub rate: String,
}

pub trait TtsEngine: Send + Sync {
    fn synthesize(&self, text: &str, voice: &VoiceSel) -> Result<Vec<u8>, TtsError>;
    fn warmup(&self, _voice: &VoiceSel) {}
    fn available(&self) -> bool;
    fn name(&self) -> &'static str;
}

/// Piper: текст в stdin → WAV в stdout. Бинарь и модель — из ~/.jarvis/.
pub struct PiperEngine { pub bin: PathBuf }

impl PiperEngine {
    pub fn new(bin: PathBuf) -> Self { PiperEngine { bin } }
}

impl TtsEngine for PiperEngine {
    fn synthesize(&self, text: &str, voice: &VoiceSel) -> Result<Vec<u8>, TtsError> {
        if !self.available() {
            return Err(TtsError::NotInstalled("нет бинаря piper".into()));
        }
        if voice.voice_path.is_empty() || !PathBuf::from(&voice.voice_path).exists() {
            return Err(TtsError::NotInstalled(format!("нет модели голоса: {}", voice.voice_path)));
        }
        // Флаги piper подтверждаются на этапе установки (Task 9): текст stdin → WAV stdout.
        let mut child = Command::new(&self.bin)
            .arg("--model").arg(&voice.voice_path)
            .arg("--output_file").arg("-")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .spawn().map_err(|e| TtsError::Synthesis(format!("spawn piper: {e}")))?;
        child.stdin.take()
            .ok_or_else(|| TtsError::Synthesis("stdin не захвачен".into()))?
            .write_all(text.as_bytes())
            .map_err(|e| TtsError::Synthesis(format!("stdin: {e}")))?;
        let out = child.wait_with_output().map_err(|e| TtsError::Synthesis(format!("wait: {e}")))?;
        if !out.status.success() || out.stdout.is_empty() {
            return Err(TtsError::Synthesis(format!("piper rc={:?} bytes={}", out.status.code(), out.stdout.len())));
        }
        Ok(out.stdout)
    }
    fn available(&self) -> bool { self.bin.exists() }
    fn name(&self) -> &'static str { "piper" }
}

/// Silero — клиент к локальному сайдкару (FastAPI на 127.0.0.1). Блокирующий
/// reqwest (зовём из voice-воркера, отдельный поток). Любой сбой/недоступность
/// сайдкара — fail-safe: TtsError, демон не падает.
pub struct SileroEngine {
    base: String,
}

impl SileroEngine {
    pub fn new(base: String) -> Self {
        SileroEngine { base }
    }
    fn client(timeout: std::time::Duration) -> Result<reqwest::blocking::Client, TtsError> {
        reqwest::blocking::Client::builder()
            .timeout(timeout)
            .no_proxy() // сайдкар — localhost; системный HTTP_PROXY его не касается
            .build()
            .map_err(|e| TtsError::Synthesis(format!("http client: {e}")))
    }
}

impl TtsEngine for SileroEngine {
    fn synthesize(&self, text: &str, voice: &VoiceSel) -> Result<Vec<u8>, TtsError> {
        let client = Self::client(std::time::Duration::from_secs(20))?;
        let resp = client
            .post(format!("{}/tts", self.base))
            .json(&serde_json::json!({
                "text": text,
                "speaker": voice.speaker,
                "sample_rate": voice.sample_rate,
                "rate": voice.rate,
            }))
            .send()
            .map_err(|e| TtsError::Synthesis(format!("сайдкар недоступен: {e}")))?;
        if !resp.status().is_success() {
            return Err(TtsError::Synthesis(format!("сайдкар rc={}", resp.status())));
        }
        let bytes = resp
            .bytes()
            .map_err(|e| TtsError::Synthesis(format!("чтение WAV: {e}")))?;
        if bytes.is_empty() {
            return Err(TtsError::Synthesis("пустой WAV".into()));
        }
        Ok(bytes.to_vec())
    }
    /// Первый инференс греет модель — делаем короткой фразой на старте.
    fn warmup(&self, voice: &VoiceSel) {
        let _ = self.synthesize("Готово.", voice);
    }
    fn available(&self) -> bool {
        Self::client(std::time::Duration::from_millis(800))
            .and_then(|c| {
                c.get(format!("{}/health", self.base))
                    .send()
                    .map_err(|e| TtsError::Synthesis(e.to_string()))
            })
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
    fn name(&self) -> &'static str {
        "silero"
    }
}

/// Собрать движок по конфигу. Неизвестный engine → Piper (дефолт).
/// `silero_base` — URL сайдкара (`http://127.0.0.1:PORT`); для Piper игнорится.
pub fn build_engine(engine: &str, piper_bin: PathBuf, silero_base: String) -> Box<dyn TtsEngine> {
    match engine {
        "silero" => Box::new(SileroEngine::new(silero_base)),
        _ => Box::new(PiperEngine::new(piper_bin)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel() -> VoiceSel { VoiceSel { speaker: String::new(), voice_path: String::new(), sample_rate: 24000, rate: "fast".into() } }

    #[test]
    fn silero_unreachable_fails_safe() {
        // порт заведомо закрыт → и health, и синтез возвращают ошибку, не паникуют
        let e = SileroEngine::new("http://127.0.0.1:1".into());
        assert!(!e.available());
        assert!(e
            .synthesize("привет", &VoiceSel { speaker: "baya".into(), voice_path: String::new(), sample_rate: 24000, rate: "fast".into() })
            .is_err());
    }

    #[test]
    fn piper_missing_binary_is_not_installed() {
        let e = PiperEngine::new(PathBuf::from("/nonexistent/piper"));
        assert!(!e.available());
        assert!(matches!(e.synthesize("привет", &sel()), Err(TtsError::NotInstalled(_))));
    }

    #[test]
    fn build_engine_selects_by_name() {
        let b = || "http://127.0.0.1:1".to_string();
        assert_eq!(build_engine("silero", PathBuf::from("/x"), b()).name(), "silero");
        assert_eq!(build_engine("piper", PathBuf::from("/x"), b()).name(), "piper");
        assert_eq!(build_engine("???", PathBuf::from("/x"), b()).name(), "piper");
    }
}
