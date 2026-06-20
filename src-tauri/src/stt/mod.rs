//! STT-сервис: владеет движком + конфигом. Единственная точка входа для
//! внутренних потребителей (диктовка, голосовой агент). Fail-safe: ошибки
//! движка — не паника, а Err-результат.

pub mod audio;
pub mod config;
pub mod dictation;
pub mod engine;
pub mod engine_qwen3;
pub mod engine_whisper;
pub mod hub; // инкр.10: единый владелец захвата + веер + преролл + жёсткий mute
pub mod insert;
pub mod mic_permission; // инкр.10: безопасная проверка разрешения микрофона (TCC)
pub mod sidecar;

use std::sync::Arc;

use config::SttConfig;
use engine::{SttEngine, SttOptions, SttResult};
use sidecar::SttSidecar;

/// Фиксированный порт STT-сайдкара (Qwen3-ASR MLX) на localhost.
const STT_PORT: u16 = 8732;

/// Сервис распознавания речи. Владеет движком и конфигом.
/// Для qwen3-движков также владеет супервизором MLX-сайдкара (старт/тик/стоп):
/// движок — лишь HTTP-клиент, кто-то должен поднять процесс на 127.0.0.1:8732.
/// Создаётся один раз через `SttService::new`, живёт в Arc.
pub struct SttService {
    engine: Box<dyn SttEngine>,
    config: SttConfig,
    /// Сайдкар: Some только для qwen3-* (None для whisper — там движок из файла модели).
    sidecar: Option<Arc<SttSidecar>>,
}

impl SttService {
    /// Создать сервис с заданной конфигурацией (движок инициализируется немедленно).
    /// Для qwen3-движка поднимает MLX-сайдкар и подключает движок к его base().
    pub fn new(cfg: SttConfig) -> Arc<Self> {
        if cfg.engine.starts_with("qwen3") {
            let dir = crate::util::jarvis_dir().join("stt-mlx");
            let sidecar = Arc::new(SttSidecar::new(
                &dir.to_string_lossy(),
                &cfg.engine,
                STT_PORT,
            ));
            // Запуск fail-safe: не установлен/не стартовал → движок просто вернёт Err.
            let _ = sidecar.ensure_started();
            let engine = engine::build_qwen3_engine(sidecar.base(), &cfg.engine);
            return Arc::new(SttService { engine, config: cfg, sidecar: Some(sidecar) });
        }
        let engine = engine::build_engine(&cfg);
        Arc::new(SttService { engine, config: cfg, sidecar: None })
    }

    /// Тик супервизора: перезапустить MLX-сайдкар, если он умер (qwen3-only).
    pub fn tick(&self) {
        if let Some(s) = &self.sidecar {
            s.restart_if_dead();
        }
    }

    /// Погасить MLX-сайдкар на выходе демона (qwen3-only; no-op для whisper).
    pub fn dispose(&self) {
        if let Some(s) = &self.sidecar {
            s.stop();
        }
    }

    /// PID MLX-сайдкара (для метрик диагностики); None, если не qwen3 или не запущен.
    pub fn sidecar_pid(&self) -> Option<u32> {
        self.sidecar.as_ref().and_then(|s| s.pid())
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
            sidecar: None,
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

    // SttService с Qwen3Engine (дефолтный конфиг engine=qwen3-0.6b): available()==false
    // когда сайдкар не запущен (порт 8732 закрыт в тестах)
    #[test]
    fn qwen3_engine_service_not_available() {
        let svc = SttService::new(SttConfig::default());
        assert!(!svc.available());
    }

    // SttService с Qwen3Engine: transcribe → Err когда сайдкар не запущен
    #[test]
    fn qwen3_engine_service_transcribe_errors() {
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

    // SttService::new строит правильно из конфига — дефолт = qwen3-0.6b (Phase 3)
    #[test]
    fn new_service_engine_name_is_qwen3_for_default_config() {
        let svc = SttService::new(SttConfig::default());
        assert_eq!(svc.engine_name(), "qwen3-0.6b");
    }
}
