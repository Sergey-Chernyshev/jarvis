//! Whisper-движок (whisper-rs 0.16, Metal, whisper.cpp под капотом).
//!
//! Компилируется только при feature "whisper-native" (требует CMake + Xcode CLT).
//! Без feature движок существует в виде заглушки (см. `#[cfg(not(...))]` ниже),
//! чтобы `cargo test` без feature работал и build_engine мог вернуть правильное имя.
//!
//! ## Путь к модели
//! По умолчанию `~/.jarvis/stt/ggml-large-v3-turbo-q5_0.bin`.
//! Переопределяется через `WhisperEngine::with_path` (для тестов и кастомной установки).
//!
//! ## Параметры Whisper
//! - `SamplingStrategy::Greedy { best_of: 1 }` — минимальная задержка.
//! - `set_language(Some(&opts.dominant_lang))` — пин языка "ru" (не авто-детект).
//! - `set_translate(false)` — транскрипция в оригинальном языке; code-switching EN
//!   сохраняется в латинице.
//! - Все print-флаги выключены → тихий вывод.
//!
//! ## API whisper-rs 0.16 (реальный, проверено по исходникам)
//! - `WhisperContext::new_with_params(path, WhisperContextParameters::default())`
//! - `ctx.create_state()` → `WhisperState`
//! - `FullParams::new(SamplingStrategy::Greedy { best_of: 1 })`
//! - `params.set_language(Option<&str>)`, `params.set_translate(bool)`
//! - `params.set_print_progress(bool)`, `params.set_print_special(bool)` и т.д.
//! - `state.full(params, &[f32])` → `Result<(), WhisperError>`
//! - `state.as_iter()` → итератор `WhisperSegment`; сегмент реализует `Display`
//!   (корректный текст); `segment.to_str_lossy()` → `Cow<str>`

use std::path::PathBuf;

use crate::stt::engine::{SttEngine, SttOptions, SttResult, SttTask};

#[cfg(feature = "whisper-native")]
use crate::stt::engine::SttSeg;

// ---------------------------------------------------------------------------
// Чистая вспомогательная функция — не зависит от whisper-rs, тестируема всегда.
// ---------------------------------------------------------------------------

/// Вычислить пару (язык, нужен ли перевод) из SttOptions.
///
/// Возвращает `(dominant_lang, translate)`:
/// - `translate = true` только при `SttTask::Translate` (перевод в английский).
/// - По умолчанию (Transcribe) → `translate = false`, язык = "ru" (code-switching сохранён).
pub fn whisper_lang_and_translate(opts: &SttOptions) -> (String, bool) {
    let translate = matches!(opts.task, SttTask::Translate);
    (opts.dominant_lang.clone(), translate)
}

/// Минимум сэмплов PCM (16кГц моно) перед подачей в whisper — ~1с. Короче этого
/// whisper.cpp нестабилен (жёсткий 100-мс гейт, шаткий avg_logprob по горстке
/// токенов), из-за чего короткие фразы иногда декодируются в пустоту.
const MIN_PCM_SAMPLES: usize = 16_000;

/// Пунктуированная затравка декодера (initial_prompt) по языку. Whisper
/// мимикрирует под стиль затравки: без неё greedy-декод на длинных диктовках
/// почти не ставит знаки препинания. По исходнику whisper.cpp затравка попадает
/// в rolling-контекст первого окна и её стиль распространяется по цепочке окон;
/// с нашим no_context совместимо (тот чистит контекст только на старте вызова,
/// а state создаётся свежий на каждый transcribe). Фразы затравок добавлены в
/// блоклист dehallucinate — если модель «протечёт» затравкой на тишине, сегмент
/// будет отброшен.
pub fn punctuation_seed(lang: &str) -> &'static str {
    match lang {
        // Два предложения: пример точки И вопроса (whisper мимикрирует стиль,
        // инструкции не выполняет). Фразы намеренно «нечеловеческие» — их можно
        // безопасно держать в блоклисте, не рискуя выкинуть живую речь.
        "ru" => "Диктовка началась, говорю обычным тоном. Знаки препинания — запятые, точки, вопросы — расставляем правильно, верно?",
        "en" => "The dictation has started, I am speaking normally. Punctuation marks — commas, periods, questions — are placed correctly, right?",
        _ => "", // не навязываем языковой стиль неизвестному языку
    }
}

/// Оставлять ли сегмент whisper, судя по его `no_speech_probability`. Порог
/// намеренно высокий: whisper.cpp роняет сегмент как «не речь» только когда
/// `nsp > 0.6` И `avg_logprob < -1.0`; мы же раньше роняли по одному `nsp > 0.6`
/// и выкидывали валидные короткие фразы в пустоту. Оставляем всё, кроме случаев
/// экстремальной уверенности в тишине — по логарифм-вероятности решает сам движок.
fn keep_by_no_speech(no_speech_prob: f32) -> bool {
    const NO_SPEECH_MAX: f32 = 0.95;
    no_speech_prob <= NO_SPEECH_MAX
}

/// Дополнить короткий PCM хвостовой тишиной до `min_samples`. Если буфер уже не
/// короче — вернуть как есть (без лишней аллокации). Хвостовая (а не ведущая)
/// тишина не сдвигает онсет и не рискует обрезать начало фразы.
fn pad_pcm_to_min(pcm: &[f32], min_samples: usize) -> std::borrow::Cow<'_, [f32]> {
    if pcm.len() >= min_samples {
        std::borrow::Cow::Borrowed(pcm)
    } else {
        let mut v = Vec::with_capacity(min_samples);
        v.extend_from_slice(pcm);
        v.resize(min_samples, 0.0);
        std::borrow::Cow::Owned(v)
    }
}

// ---------------------------------------------------------------------------
// WhisperEngine: реализация с whisper-rs (только при feature "whisper-native")
// ---------------------------------------------------------------------------

/// Путь к модели Whisper по умолчанию.
fn default_model_path() -> PathBuf {
    crate::util::jarvis_dir().join("stt").join("ggml-large-v3-turbo-q5_0.bin")
}

#[cfg(feature = "whisper-native")]
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Движок Whisper: грузит `ggml-large-v3-turbo-q5_0.bin` и запускает транскрипцию
/// через whisper-rs с Metal-ускорением.
///
/// Контекст (`WhisperContext`) грузится лениво при первом вызове `transcribe`.
/// Хранится в `Mutex<Option<...>>` — `WhisperContext` не `Send` по умолчанию, но
/// доступ всегда serialized через мьютекс.
pub struct WhisperEngine {
    model_path: PathBuf,
    #[cfg(feature = "whisper-native")]
    ctx: std::sync::Mutex<Option<WhisperContext>>,
}

impl WhisperEngine {
    /// Создать движок с путём к модели по умолчанию (`~/.jarvis/stt/ggml-...bin`).
    pub fn new() -> Self {
        Self::with_path(default_model_path())
    }

    /// Создать движок с явным путём к модели (для тестов / кастомной установки).
    pub fn with_path(model_path: PathBuf) -> Self {
        WhisperEngine {
            model_path,
            #[cfg(feature = "whisper-native")]
            ctx: std::sync::Mutex::new(None),
        }
    }

    /// Получить или инициализировать WhisperContext (ленивая загрузка).
    #[cfg(feature = "whisper-native")]
    fn ensure_ctx(&self) -> Result<(), String> {
        let mut guard = self.ctx.lock().map_err(|e| format!("whisper mutex: {e}"))?;
        if guard.is_none() {
            let ctx = WhisperContext::new_with_params(
                &self.model_path,
                WhisperContextParameters::default(),
            )
            .map_err(|e| format!("whisper: не удалось загрузить модель: {e}"))?;
            *guard = Some(ctx);
        }
        Ok(())
    }
}

impl Default for WhisperEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "whisper-native")]
impl SttEngine for WhisperEngine {
    fn name(&self) -> &'static str {
        "whisper-turbo"
    }

    fn available(&self) -> bool {
        self.model_path.exists()
    }

    fn transcribe(&self, pcm: &[f32], opts: &SttOptions) -> Result<SttResult, String> {
        if !self.model_path.exists() {
            return Err("модель whisper не установлена".into());
        }

        self.ensure_ctx()?;

        let guard = self.ctx.lock().map_err(|e| format!("whisper mutex: {e}"))?;
        let ctx = guard.as_ref().expect("ctx должен быть загружен после ensure_ctx");

        let mut state = ctx.create_state().map_err(|e| format!("whisper: create_state: {e}"))?;

        let (lang, translate) = whisper_lang_and_translate(opts);

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(&lang));
        params.set_translate(translate);
        // Anti-hallucination для коротких PTT-клипов (Tier 1, см. ресёрч): на тишине/
        // шуме Whisper сваливается в языковую модель и печатает субтитры-фразы.
        params.set_no_context(true); // клипы независимы — не «засевать» след. галлюцинацией
        // Пунктуированная затравка: без неё greedy-декод на длинных диктовках
        // выдаёт сплошной поток слов без знаков препинания.
        let seed = punctuation_seed(&lang);
        if !seed.is_empty() {
            params.set_initial_prompt(seed);
        }
        params.set_temperature(0.0); // без сэмплинга
        params.set_temperature_inc(0.2); // fallback против повторов
        params.set_logprob_thold(-1.0); // низкий avg logprob → провал декода
        params.set_entropy_thold(2.6); // аналог compression_ratio: ловит повторы
        params.set_suppress_blank(true);
        params.set_print_progress(false);
        params.set_print_special(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        // Короткий клип дополняем хвостовой тишиной до ~1с — иначе whisper.cpp
        // нестабилен и короткая фраза иногда декодируется в пустоту.
        let pcm = pad_pcm_to_min(pcm, MIN_PCM_SAMPLES);
        state.full(params, &pcm).map_err(|e| format!("whisper: full: {e}"))?;

        let mut text_parts: Vec<String> = Vec::new();
        let mut segments: Vec<SttSeg> = Vec::new();

        // Tier 2: посегментный фильтр. Очень высокая no_speech_probability → модель
        // «придумала» текст на не-речи; блок-фраза → известная галлюцинация.
        for seg in state.as_iter() {
            if !keep_by_no_speech(seg.no_speech_probability()) {
                continue;
            }
            let seg_text = seg.to_str_lossy().map_err(|e| format!("whisper: seg text: {e}"))?;
            let seg_text = seg_text.trim().to_string();
            if seg_text.is_empty() || crate::stt::dehallucinate::is_hallucination(&seg_text) {
                continue;
            }
            text_parts.push(seg_text.clone());
            segments.push(SttSeg { text: seg_text, lang: None });
        }

        let text = text_parts.join(" ");
        Ok(SttResult { text, segments })
    }
}

// ---------------------------------------------------------------------------
// Заглушка: когда feature "whisper-native" ВЫКЛЮЧЕН.
// Позволяет build_engine("whisper-turbo") всегда возвращать правильный name().
// Без CMake тесты всё равно компилируются и проходят.
// ---------------------------------------------------------------------------

#[cfg(not(feature = "whisper-native"))]
impl SttEngine for WhisperEngine {
    fn name(&self) -> &'static str {
        "whisper-turbo"
    }

    fn available(&self) -> bool {
        self.model_path.exists()
    }

    fn transcribe(&self, _pcm: &[f32], _opts: &SttOptions) -> Result<SttResult, String> {
        if !self.model_path.exists() {
            return Err("модель whisper не установлена".into());
        }
        Err(
            "whisper-native feature не включён — пересоберите с --features whisper-native".into(),
        )
    }
}

// ---------------------------------------------------------------------------
// Тесты — работают без whisper-rs (без cmake), проверяют нашу логику.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::engine::{SttOptions, SttTask};

    // --- whisper_lang_and_translate ---

    #[test]
    fn transcribe_ru_gives_no_translate() {
        let opts = SttOptions { dominant_lang: "ru".into(), task: SttTask::Transcribe, hints: vec![] };
        let (lang, translate) = whisper_lang_and_translate(&opts);
        assert_eq!(lang, "ru");
        assert!(!translate, "Transcribe → translate должен быть false (не переводить)");
    }

    #[test]
    fn translate_task_gives_translate_true() {
        let opts = SttOptions { dominant_lang: "ru".into(), task: SttTask::Translate, hints: vec![] };
        let (lang, translate) = whisper_lang_and_translate(&opts);
        assert_eq!(lang, "ru");
        assert!(translate, "Translate → translate должен быть true");
    }

    #[test]
    fn default_opts_give_ru_no_translate() {
        let opts = SttOptions::default();
        let (lang, translate) = whisper_lang_and_translate(&opts);
        assert_eq!(lang, "ru", "дефолтный язык — ru");
        assert!(!translate, "дефолтная задача — Transcribe, не переводить");
    }

    #[test]
    fn lang_pin_passthrough() {
        let opts = SttOptions { dominant_lang: "en".into(), task: SttTask::Transcribe, hints: vec![] };
        let (lang, translate) = whisper_lang_and_translate(&opts);
        assert_eq!(lang, "en");
        assert!(!translate);
    }

    // --- WhisperEngine::available() ---

    #[test]
    fn available_false_when_model_missing() {
        let engine = WhisperEngine::with_path(PathBuf::from("/nonexistent/model.bin"));
        assert!(!engine.available(), "available() → false если файл отсутствует");
    }

    #[test]
    fn available_true_when_model_exists() {
        // Создать временный файл-заглушку
        let dir = std::env::temp_dir();
        let path = dir.join("fake-ggml-test-model.bin");
        std::fs::write(&path, b"fake").unwrap();
        let engine = WhisperEngine::with_path(path.clone());
        assert!(engine.available(), "available() → true если файл существует");
        let _ = std::fs::remove_file(&path);
    }

    // --- WhisperEngine::name() ---

    #[test]
    fn name_is_whisper_turbo() {
        let engine = WhisperEngine::new();
        assert_eq!(engine.name(), "whisper-turbo");
    }

    // --- punctuation_seed: затравка стиля пунктуации ---

    #[test]
    fn seed_is_punctuated_for_known_langs() {
        for lang in ["ru", "en"] {
            let s = punctuation_seed(lang);
            assert!(!s.is_empty(), "{lang}: затравка есть");
            assert!(s.contains(',') && s.contains('?'), "{lang}: есть запятая и вопрос");
        }
    }

    #[test]
    fn seed_empty_for_unknown_lang() {
        assert_eq!(punctuation_seed("de"), "");
        assert_eq!(punctuation_seed(""), "");
    }

    #[test]
    fn seed_leak_is_caught_by_blocklist() {
        // Протечка затравки на тишине обязана отбрасываться как галлюцинация.
        for lang in ["ru", "en"] {
            assert!(
                crate::stt::dehallucinate::is_hallucination(punctuation_seed(lang)),
                "{lang}: затравка в блоклисте"
            );
        }
    }

    // --- keep_by_no_speech: не ронять валидные короткие фразы в пустоту ---

    #[test]
    fn no_speech_keeps_moderate_segments() {
        // Раньше порог 0.6 ронял такой сегмент — теперь оставляем (это чинило
        // «короткая фраза → пустой результат»).
        assert!(keep_by_no_speech(0.6), "0.6 больше не считается тишиной");
        assert!(keep_by_no_speech(0.9), "даже 0.9 оставляем — движок сам решает по avg_logprob");
    }

    #[test]
    fn no_speech_drops_only_extreme() {
        assert!(!keep_by_no_speech(0.99), "экстремальная уверенность в тишине → дроп");
        assert!(keep_by_no_speech(0.95), "граница включительно — оставляем");
    }

    // --- pad_pcm_to_min: стабилизировать короткий декод ---

    #[test]
    fn pad_extends_short_pcm_with_trailing_silence() {
        let pcm = vec![0.3f32; 100];
        let out = pad_pcm_to_min(&pcm, 16_000);
        assert_eq!(out.len(), 16_000, "дополнено до минимума");
        assert_eq!(&out[..100], &pcm[..], "исходное аудио в начале сохранено");
        assert!(out[100..].iter().all(|&s| s == 0.0), "хвост — тишина");
    }

    #[test]
    fn pad_leaves_long_pcm_untouched() {
        let pcm = vec![0.2f32; 20_000];
        let out = pad_pcm_to_min(&pcm, 16_000);
        assert_eq!(out.len(), 20_000, "длинный буфер не трогаем");
        assert_eq!(&out[..], &pcm[..]);
    }

    // --- transcribe без модели → Err ---

    #[test]
    fn transcribe_without_model_returns_err() {
        let engine = WhisperEngine::with_path(PathBuf::from("/nonexistent/model.bin"));
        let result = engine.transcribe(&[0.0f32; 16], &SttOptions::default());
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("не установлена"),
            "ошибка должна сообщать об отсутствии модели"
        );
    }
}
