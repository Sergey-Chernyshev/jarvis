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
pub struct VoiceSel { pub speaker: String, pub voice_path: String, pub sample_rate: u32 }

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
        child.stdin.take().unwrap().write_all(text.as_bytes())
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

/// Silero — заглушка Фазы 1: всегда NotInstalled, чтобы engine="silero" молчал безопасно.
pub struct SileroStub;
impl TtsEngine for SileroStub {
    fn synthesize(&self, _t: &str, _v: &VoiceSel) -> Result<Vec<u8>, TtsError> {
        Err(TtsError::NotInstalled("Silero — Фаза 2, сайдкар ещё не реализован".into()))
    }
    fn available(&self) -> bool { false }
    fn name(&self) -> &'static str { "silero" }
}

/// Собрать движок по конфигу. Неизвестный engine → Piper (дефолт).
pub fn build_engine(engine: &str, piper_bin: PathBuf) -> Box<dyn TtsEngine> {
    match engine {
        "silero" => Box::new(SileroStub),
        _ => Box::new(PiperEngine::new(piper_bin)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel() -> VoiceSel { VoiceSel { speaker: String::new(), voice_path: String::new(), sample_rate: 24000 } }

    #[test]
    fn silero_stub_fails_safe() {
        let e = SileroStub;
        assert!(!e.available());
        assert!(matches!(e.synthesize("x", &sel()), Err(TtsError::NotInstalled(_))));
    }

    #[test]
    fn piper_missing_binary_is_not_installed() {
        let e = PiperEngine::new(PathBuf::from("/nonexistent/piper"));
        assert!(!e.available());
        assert!(matches!(e.synthesize("привет", &sel()), Err(TtsError::NotInstalled(_))));
    }

    #[test]
    fn build_engine_selects_by_name() {
        assert_eq!(build_engine("silero", PathBuf::from("/x")).name(), "silero");
        assert_eq!(build_engine("piper", PathBuf::from("/x")).name(), "piper");
        assert_eq!(build_engine("???", PathBuf::from("/x")).name(), "piper");
    }
}
