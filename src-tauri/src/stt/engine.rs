//! Трейт движка STT + типы SttOptions/SttResult/SttSeg + build_engine.
//! Phase 1: NullEngine для всех arms — реальные движки добавляются в следующих фазах.
//! Любая ошибка движка — fail-safe: вернуть Err(String), демон не падает.

/// Задача распознавания: транскрипция (оригинальный язык) или перевод (в английский).
/// Дефолт — Transcribe (никогда не переводить; требование code-switching).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum SttTask {
    #[default]
    Transcribe,
    Translate,
}

/// Опции запроса к движку. Дефолт настроен под смешанную RU/EN речь:
/// dominant_lang="ru" (пин языка для Whisper), task=Transcribe (не перевод).
#[derive(Debug, Clone)]
pub struct SttOptions {
    /// Доминирующий язык (ISO 639-1, напр. "ru"). Движок пиннит Whisper-параметр language.
    pub dominant_lang: String,
    /// Режим задачи (транскрипция / перевод).
    pub task: SttTask,
    /// Подсказки для движка (терминология, имена — улучшают точность кодовых вкраплений).
    pub hints: Vec<String>,
}

impl Default for SttOptions {
    fn default() -> Self {
        SttOptions {
            dominant_lang: "ru".into(),
            task: SttTask::Transcribe,
            hints: vec![],
        }
    }
}

/// Один сегмент распознавания (временной отрезок или языковой кусок).
#[derive(Debug, Clone, PartialEq)]
pub struct SttSeg {
    pub text: String,
    /// Язык сегмента, если движок даёт пословную разметку (ISO 639-1).
    pub lang: Option<String>,
}

/// Результат транскрипции.
#[derive(Debug, Clone, PartialEq)]
pub struct SttResult {
    /// Полный текст (конкатенация сегментов или прямой ответ движка).
    pub text: String,
    /// Сегменты с пометкой языка (пусто, если движок не даёт разметку).
    pub segments: Vec<SttSeg>,
}

/// Трейт движка STT. Реализации: NullEngine (заглушка), WhisperEngine (фаза 2),
/// Qwen3Engine (фаза 3). Send+Sync — движок живёт в Arc<SttService>.
pub trait SttEngine: Send + Sync {
    fn name(&self) -> &'static str;
    fn transcribe(&self, pcm: &[f32], opts: &SttOptions) -> Result<SttResult, String>;
    fn available(&self) -> bool;
}

/// Заглушка: возвращает ошибку, не паникует. Используется пока движок не настроен/не установлен.
pub struct NullEngine;

impl SttEngine for NullEngine {
    fn name(&self) -> &'static str {
        "none"
    }
    fn transcribe(&self, _pcm: &[f32], _opts: &SttOptions) -> Result<SttResult, String> {
        Err("STT-движок не настроен".into())
    }
    fn available(&self) -> bool {
        false
    }
}

/// Собрать движок по конфигу. Phase 1: все arms → NullEngine (реальные движки — фазы 2-3).
pub fn build_engine(cfg: &crate::stt::config::SttConfig) -> Box<dyn SttEngine> {
    match cfg.engine.as_str() {
        "whisper-turbo" | "qwen3-0.6b" | "qwen3-1.7b" => Box::new(NullEngine),
        _ => Box::new(NullEngine),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::config::SttConfig;

    fn cfg(engine: &str) -> SttConfig {
        SttConfig { engine: engine.into(), ..SttConfig::default() }
    }

    // build_engine выбирает движок по конфигу — все arms → NullEngine (Phase 1)
    #[test]
    fn build_engine_whisper_turbo_is_null() {
        assert_eq!(build_engine(&cfg("whisper-turbo")).name(), "none");
    }

    #[test]
    fn build_engine_qwen3_06b_is_null() {
        assert_eq!(build_engine(&cfg("qwen3-0.6b")).name(), "none");
    }

    #[test]
    fn build_engine_qwen3_17b_is_null() {
        assert_eq!(build_engine(&cfg("qwen3-1.7b")).name(), "none");
    }

    #[test]
    fn build_engine_unknown_is_null() {
        assert_eq!(build_engine(&cfg("unknown-engine")).name(), "none");
    }

    // SttOptions::default() → ru/Transcribe
    #[test]
    fn stt_options_default_is_ru_transcribe() {
        let opts = SttOptions::default();
        assert_eq!(opts.dominant_lang, "ru");
        assert_eq!(opts.task, SttTask::Transcribe);
        assert!(opts.hints.is_empty());
    }

    // NullEngine: available()==false, transcribe → Err
    #[test]
    fn null_engine_not_available() {
        assert!(!NullEngine.available());
    }

    #[test]
    fn null_engine_transcribe_errors() {
        let result = NullEngine.transcribe(&[0.0f32; 16], &SttOptions::default());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("не настроен"));
    }

    #[test]
    fn null_engine_name_is_none() {
        assert_eq!(NullEngine.name(), "none");
    }
}
