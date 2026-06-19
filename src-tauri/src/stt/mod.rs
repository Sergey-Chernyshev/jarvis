//! STT-сервис: владеет движком + конфигом. Единственная точка входа для
//! внутренних потребителей (диктовка, голосовой агент). Fail-safe: ошибки
//! движка — не паника, а Err-результат.

pub mod config;
pub mod engine;
pub mod engine_whisper;

use std::sync::Arc;

use config::SttConfig;
use engine::{SttEngine, SttOptions, SttResult};

/// Сервис распознавания речи. Владеет движком и конфигом.
/// Создаётся один раз через `SttService::new`, живёт в Arc.
pub struct SttService {
    engine: Box<dyn SttEngine>,
    config: SttConfig,
}

impl SttService {
    /// Создать сервис с заданной конфигурацией (движок инициализируется немедленно).
    pub fn new(cfg: SttConfig) -> Arc<Self> {
        let engine = engine::build_engine(&cfg);
        Arc::new(SttService { engine, config: cfg })
    }

    /// Транскрибировать буфер PCM (16кГц моно f32). Опции — из `options()` или явные.
    pub fn transcribe(&self, pcm: &[f32], opts: &SttOptions) -> Result<SttResult, String> {
        self.engine.transcribe(pcm, opts)
    }

    /// Имя активного движка (для UI/логов).
    pub fn engine_name(&self) -> &'static str {
        self.engine.name()
    }

    /// Движок доступен (модель/сайдкар на месте).
    pub fn available(&self) -> bool {
        self.engine.available()
    }

    /// Опции из конфига (dominant_lang + task; hints пусты).
    pub fn options(&self) -> SttOptions {
        SttOptions {
            dominant_lang: self.config.dominant_lang.clone(),
            task: self.config.task.clone(),
            hints: vec![],
        }
    }

    /// Активный конфиг (для диагностики).
    pub fn config(&self) -> &SttConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{SttEngine, SttOptions, SttResult, SttSeg, SttTask};

    /// Мок-движок для тестов: всегда возвращает фиксированный результат.
    struct MockEngine {
        result_text: String,
    }

    impl SttEngine for MockEngine {
        fn name(&self) -> &'static str {
            "mock"
        }
        fn transcribe(&self, _pcm: &[f32], _opts: &SttOptions) -> Result<SttResult, String> {
            Ok(SttResult {
                text: self.result_text.clone(),
                segments: vec![SttSeg { text: self.result_text.clone(), lang: Some("ru".into()) }],
            })
        }
        fn available(&self) -> bool {
            true
        }
    }

    /// SttService с мок-движком через Box<dyn SttEngine> напрямую.
    fn service_with_mock(text: &str) -> Arc<SttService> {
        // Строим сервис с дефолтным конфигом, потом подменяем движок.
        // Так как конструктор Arc::new(SttService{engine, config}), используем
        // специальный приватный конструктор-путь: создаём SttService вручную.
        let cfg = SttConfig::default();
        Arc::new(SttService {
            engine: Box::new(MockEngine { result_text: text.to_string() }),
            config: cfg,
        })
    }

    // SttService с мок-движком: transcribe возвращает мок-результат
    #[test]
    fn mock_engine_transcribe_returns_result() {
        let svc = service_with_mock("привет мир");
        let result = svc.transcribe(&[0.0f32; 16000], &SttOptions::default());
        assert!(result.is_ok());
        let r = result.unwrap();
        assert_eq!(r.text, "привет мир");
        assert_eq!(r.segments.len(), 1);
        assert_eq!(r.segments[0].lang, Some("ru".into()));
    }

    // SttService с мок-движком: available()==true
    #[test]
    fn mock_engine_is_available() {
        let svc = service_with_mock("test");
        assert!(svc.available());
    }

    // SttService с мок-движком: engine_name() == "mock"
    #[test]
    fn mock_engine_name() {
        let svc = service_with_mock("test");
        assert_eq!(svc.engine_name(), "mock");
    }

    // SttService с NullEngine (через build_engine с дефолтным конфигом): available()==false
    #[test]
    fn null_engine_service_not_available() {
        let svc = SttService::new(SttConfig::default());
        assert!(!svc.available());
    }

    // SttService с NullEngine: transcribe → Err
    #[test]
    fn null_engine_service_transcribe_errors() {
        let svc = SttService::new(SttConfig::default());
        let result = svc.transcribe(&[0.0f32; 16], &SttOptions::default());
        assert!(result.is_err());
    }

    // options() из конфига: dominant_lang и task передаются правильно
    #[test]
    fn options_from_config_default() {
        let svc = SttService::new(SttConfig::default());
        let opts = svc.options();
        assert_eq!(opts.dominant_lang, "ru");
        assert_eq!(opts.task, SttTask::Transcribe);
        assert!(opts.hints.is_empty());
    }

    // options() из конфига с кастомными настройками
    #[test]
    fn options_from_config_custom() {
        let cfg = SttConfig {
            dominant_lang: "en".into(),
            task: SttTask::Translate,
            ..SttConfig::default()
        };
        let svc = SttService::new(cfg);
        let opts = svc.options();
        assert_eq!(opts.dominant_lang, "en");
        assert_eq!(opts.task, SttTask::Translate);
    }

    // SttService::new строит правильно из конфига
    #[test]
    fn new_service_engine_name_is_none_for_default_config() {
        let svc = SttService::new(SttConfig::default());
        assert_eq!(svc.engine_name(), "none");
    }
}
