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
pub mod transcripts; // история «что я говорил» (диктовка/wake) + копирование

use std::sync::{Arc, Mutex};
use std::time::Duration;

use config::SttConfig;
use engine::{SttEngine, SttOptions, SttResult};
use sidecar::SttSidecar;

/// Фиксированный порт STT-сайдкара (Qwen3-ASR MLX) на localhost.
const STT_PORT: u16 = 8732;

/// Простой STT-сайдкара, после которого глушим процесс и возвращаем память
/// модели (~1.3 ГБ для qwen3-0.6b). На следующей диктовке поднимаем лениво.
const IDLE_LIMIT: Duration = Duration::from_secs(600); // 10 минут

/// Максимум ожидания готовности модели после ленивого подъёма (cold-start).
const READY_TIMEOUT: Duration = Duration::from_secs(20);

/// Подождать, пока движок не станет доступен (модель загрузилась), но не дольше
/// `timeout`. Тёплый сайдкар проходит первую проверку мгновенно.
fn wait_ready(engine: &dyn SttEngine, timeout: Duration) {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if engine.available() {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Сервис распознавания речи. Владеет движком и конфигом.
/// Для qwen3-движков также владеет супервизором MLX-сайдкара (старт/тик/стоп):
/// движок — лишь HTTP-клиент, кто-то должен поднять процесс на 127.0.0.1:8732.
/// Создаётся один раз через `SttService::new`, живёт в Arc.
pub struct SttService {
    /// Активный движок за `Mutex<Arc<dyn>>`: `transcribe` берёт `Arc` по короткому
    /// локу и работает на клоне (лок не держится во время распознавания), а
    /// `set_engine` подменяет `Arc` под локом. In-flight транскрипции доживают на
    /// старом движке. Так горячая смена не блокирует и не рвёт текущие запросы.
    engine: Mutex<Arc<dyn SttEngine>>,
    config: Mutex<SttConfig>,
    /// Сайдкар: Some только для qwen3-* (None для whisper). Меняется при set_engine.
    sidecar: Mutex<Option<Arc<SttSidecar>>>,
    /// Сериализует переходы жизненного цикла (set_engine ↔ tick супервизора), чтобы
    /// супервизор не воскресил только что остановленный сайдкар. Калька wakeword.
    transition: Mutex<()>,
}

impl SttService {
    /// Создать сервис с заданной конфигурацией (движок инициализируется немедленно).
    /// Для qwen3-движка поднимает MLX-сайдкар и подключает движок к его base().
    pub fn new(cfg: SttConfig) -> Arc<Self> {
        let (engine, sidecar) = Self::build_from_cfg(&cfg);
        if let Some(s) = &sidecar {
            // Запуск fail-safe: не установлен/не стартовал → движок просто вернёт Err.
            let _ = s.ensure_started();
        }
        Arc::new(SttService {
            engine: Mutex::new(engine),
            config: Mutex::new(cfg),
            sidecar: Mutex::new(sidecar),
            transition: Mutex::new(()),
        })
    }

    /// Построить пару (движок, сайдкар) из конфига. Общий код для `new` и
    /// `set_engine` — чтобы пути сборки (qwen-сайдкар на STT_PORT / whisper из
    /// файла модели) не разъехались.
    fn build_from_cfg(cfg: &SttConfig) -> (Arc<dyn SttEngine>, Option<Arc<SttSidecar>>) {
        if cfg.engine.starts_with("qwen3") {
            let dir = crate::util::jarvis_dir().join("stt-mlx");
            let sidecar = Arc::new(SttSidecar::new(&dir.to_string_lossy(), &cfg.engine, STT_PORT));
            let engine = engine::build_qwen3_engine(sidecar.base(), &cfg.engine);
            (engine, Some(sidecar))
        } else {
            (engine::build_engine(cfg), None)
        }
    }

    /// Горячо сменить движок/модель без перезапуска демона. Под `transition`-локом:
    /// останавливаем старый сайдкар (освобождаем порт 8732 и память модели),
    /// собираем новый движок+сайдкар, атомарно подменяем. In-flight транскрипции
    /// доживают на старом `Arc` движка. Готовность модели — лениво в `transcribe`
    /// (без блокирующего ожидания здесь, чтобы не морозить IPC-команду).
    pub fn set_engine(&self, cfg: SttConfig) {
        let _t = self.transition.lock().unwrap();
        // снять старый сайдкар ДО подмены (active=false внутри stop → tick не воскресит)
        if let Some(old) = self.sidecar.lock().unwrap().take() {
            old.stop();
        }
        let (engine, sidecar) = Self::build_from_cfg(&cfg);
        if let Some(s) = &sidecar {
            let _ = s.ensure_started();
        }
        *self.engine.lock().unwrap() = engine;
        *self.sidecar.lock().unwrap() = sidecar;
        *self.config.lock().unwrap() = cfg;
    }

    /// Прогреть сайдкар заранее (на нажатии клавиши диктовки), чтобы модель
    /// грузилась, ПОКА человек говорит — к отпусканию она уже готова и cold-start
    /// после idle-stop незаметен. Неблокирующий: `ensure_started` спавнит питон,
    /// модель грузится в нём асинхронно; повторный/тёплый вызов — no-op.
    pub fn warm(&self) {
        let sidecar = self.sidecar.lock().unwrap().clone();
        if let Some(s) = &sidecar {
            let _ = s.ensure_started();
        }
    }

    /// Тик супервизора (qwen3-only): сначала пробуем заглушить по простою
    /// (вернуть ~1.3 ГБ резидентной модели), иначе — перезапуск, если умер.
    /// Под `transition` — атомарно к `set_engine` (не воскрешаем снятый сайдкар).
    pub fn tick(&self) {
        let _t = self.transition.lock().unwrap();
        let sidecar = self.sidecar.lock().unwrap().clone();
        if let Some(s) = &sidecar {
            if !s.idle_stop_if_due(IDLE_LIMIT) {
                s.restart_if_dead();
            }
        }
    }

    /// Погасить MLX-сайдкар на выходе демона (qwen3-only; no-op для whisper).
    pub fn dispose(&self) {
        let sidecar = self.sidecar.lock().unwrap().clone();
        if let Some(s) = &sidecar {
            s.stop();
        }
    }

    /// PID MLX-сайдкара (для метрик диагностики); None, если не qwen3 или не запущен.
    pub fn sidecar_pid(&self) -> Option<u32> {
        self.sidecar.lock().unwrap().as_ref().and_then(|s| s.pid())
    }

    /// Транскрибировать буфер PCM (16кГц моно f32). Опции — из `options()` или явные.
    ///
    /// Лениво поднимает сайдкар, если он был заглушён по простою (idle-stop), и
    /// ждёт загрузки модели (cold-start). Когда сайдкар уже тёплый — задержки нет.
    pub fn transcribe(&self, pcm: &[f32], opts: &SttOptions) -> Result<SttResult, String> {
        // Снимок движка и сайдкара под короткими локами — дальше работаем на клонах
        // (Arc), не держа лок во время распознавания. set_engine может подменить их
        // параллельно; текущий запрос доживёт на снятых здесь Arc (старый движок).
        let engine = self.engine.lock().unwrap().clone();
        let sidecar = self.sidecar.lock().unwrap().clone();
        // Страж «в полёте» на весь transcribe (включая cold-start ожидание):
        // пока он жив, idle-stop из tick не убьёт сайдкар под нами (анти-гонка).
        let _guard = sidecar.as_ref().map(|s| s.use_guard());
        if let Some(s) = &sidecar {
            // поднять после idle-stop (no-op, если уже работает) + отметить использование
            s.ensure_started()?;
            // дождаться готовности модели: питон грузит её синхронно при старте,
            // до этого HTTP не отвечает. Тёплый сайдкар проходит проверку сразу.
            wait_ready(engine.as_ref(), READY_TIMEOUT);
            s.touch();
        }
        let r = engine.transcribe(pcm, opts);
        // длинная транскрипция тоже считается использованием — продлеваем активность
        if let Some(s) = &sidecar {
            s.touch();
        }
        r
    }

    /// Имя активного движка (для UI/логов). `name()` — статический литерал; лок
    /// держится лишь на момент вызова, после set_engine сразу видно новое имя.
    pub fn engine_name(&self) -> &'static str {
        self.engine.lock().unwrap().name()
    }

    /// Движок доступен (модель/сайдкар на месте). Клонируем Arc и проверяем без
    /// лока (qwen available() ходит по HTTP до 3с — не держим лок под этим).
    pub fn available(&self) -> bool {
        let engine = self.engine.lock().unwrap().clone();
        engine.available()
    }

    /// Опции из конфига (dominant_lang + task; hints пусты).
    pub fn options(&self) -> SttOptions {
        let cfg = self.config.lock().unwrap();
        SttOptions {
            dominant_lang: cfg.dominant_lang.clone(),
            task: cfg.task.clone(),
            hints: vec![],
        }
    }

    /// Активный конфиг (клон — вернуть ссылку из-под лока нельзя).
    pub fn config(&self) -> SttConfig {
        self.config.lock().unwrap().clone()
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
        // Строим сервис с мок-движком напрямую (минуя build_from_cfg/сайдкар).
        let cfg = SttConfig::default();
        let engine: Arc<dyn SttEngine> = Arc::new(MockEngine { result_text: text.to_string() });
        Arc::new(SttService {
            engine: Mutex::new(engine),
            config: Mutex::new(cfg),
            sidecar: Mutex::new(None),
            transition: Mutex::new(()),
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

    // warm() безопасен для whisper-движка (sidecar=None → no-op, без spawn)
    #[test]
    fn warm_noop_for_sidecarless_engine() {
        let svc = service_with_mock("x"); // sidecar=None
        svc.warm(); // не паникует, ничего не поднимает
        svc.warm();
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

    // Инкр.6: set_engine горячо подменяет движок без пересоздания SttService.
    // На whisper-turbo сайдкар не поднимается (None) — смена мгновенна, без процессов.
    #[test]
    fn set_engine_hot_swaps_to_whisper() {
        let svc = service_with_mock("x");
        assert_eq!(svc.engine_name(), "mock");
        svc.set_engine(SttConfig { engine: "whisper-turbo".into(), ..SttConfig::default() });
        assert_eq!(svc.engine_name(), "whisper-turbo", "имя движка обновилось");
        assert!(svc.sidecar_pid().is_none(), "whisper не держит сайдкар");
        // конфиг тоже обновился (клон отражает новый движок)
        assert_eq!(svc.config().engine, "whisper-turbo");
    }

    // set_engine не отравляет локи: транскрипция после смены работает (мок→мок-симметрия
    // через whisper-заглушку без модели → Err fail-safe, но не паника).
    #[test]
    fn transcribe_after_set_engine_does_not_panic() {
        let svc = service_with_mock("привет");
        svc.set_engine(SttConfig { engine: "whisper-turbo".into(), ..SttConfig::default() });
        let _ = svc.transcribe(&[0.0f32; 16], &SttOptions::default()); // не паникует
        assert!(svc.available() || !svc.available()); // лок не отравлен — вызов проходит
    }
}
