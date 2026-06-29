//! STT-блок настроек из ~/.jarvis/settings.json. Битый/нет блока → дефолты.

use serde_json::Value;

use crate::stt::engine::SttTask;

#[derive(Debug, Clone, PartialEq)]
pub struct SttConfig {
    /// Активный движок: "whisper-turbo" | "qwen3-0.6b" | "qwen3-1.7b"
    pub engine: String,
    /// Доминирующий язык (ISO 639-1, дефолт "ru" для RU/EN code-switching).
    pub dominant_lang: String,
    /// Режим задачи (транскрипция, не перевод).
    pub task: SttTask,
    /// Аудио-устройство захвата (None → системный дефолт).
    pub audio_device: Option<String>,
    /// Хоткей диктовки (дефолт "F8").
    pub hotkey: String,
}

impl Default for SttConfig {
    fn default() -> Self {
        SttConfig {
            engine: "whisper-turbo".into(),
            dominant_lang: "ru".into(),
            task: SttTask::Transcribe,
            audio_device: None,
            hotkey: "F8".into(),
        }
    }
}

impl SttConfig {
    /// Распарсить из корневого settings-объекта (его поле "stt"). Дефолты на дыры.
    pub fn from_settings(root: &Value) -> Self {
        let d = SttConfig::default();
        let stt = root.get("stt");
        let s = |k: &str, dv: &str| {
            stt.and_then(|v| v.get(k)).and_then(Value::as_str).unwrap_or(dv).to_string()
        };

        let task_str = s("task", "transcribe");
        let task = if task_str == "translate" { SttTask::Translate } else { SttTask::Transcribe };

        let audio_device = stt
            .and_then(|v| v.get("audioDevice"))
            .and_then(Value::as_str)
            .map(String::from);

        SttConfig {
            engine: s("engine", &d.engine),
            dominant_lang: s("dominantLang", &d.dominant_lang),
            task,
            audio_device,
            hotkey: s("hotkey", &d.hotkey),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // SttConfig::from_settings: missing → defaults
    #[test]
    fn missing_block_is_defaults() {
        let cfg = SttConfig::from_settings(&json!({}));
        assert_eq!(cfg, SttConfig::default());
    }

    // SttConfig::from_settings: parses stt object
    #[test]
    fn parses_stt_block() {
        let cfg = SttConfig::from_settings(&json!({
            "stt": {
                "engine": "whisper-turbo",
                "dominantLang": "en",
                "task": "transcribe",
                "audioDevice": "MacBook Pro Microphone",
                "hotkey": "F9"
            }
        }));
        assert_eq!(cfg.engine, "whisper-turbo");
        assert_eq!(cfg.dominant_lang, "en");
        assert_eq!(cfg.task, SttTask::Transcribe);
        assert_eq!(cfg.audio_device, Some("MacBook Pro Microphone".into()));
        assert_eq!(cfg.hotkey, "F9");
    }

    // SttConfig::from_settings: task=translate
    #[test]
    fn parses_task_translate() {
        let cfg = SttConfig::from_settings(&json!({ "stt": { "task": "translate" } }));
        assert_eq!(cfg.task, SttTask::Translate);
    }

    // SttConfig::from_settings: partial block → missing keys use defaults
    #[test]
    fn partial_block_merges_defaults() {
        let cfg = SttConfig::from_settings(&json!({ "stt": { "engine": "qwen3-1.7b" } }));
        assert_eq!(cfg.engine, "qwen3-1.7b");
        assert_eq!(cfg.dominant_lang, "ru", "missing dominantLang → default ru");
        assert_eq!(cfg.task, SttTask::Transcribe);
        assert_eq!(cfg.audio_device, None);
        assert_eq!(cfg.hotkey, "F8");
    }

    // SttConfig::from_settings: garbage types → defaults
    #[test]
    fn garbage_types_fall_back() {
        let cfg = SttConfig::from_settings(&json!({ "stt": { "engine": 42, "hotkey": true } }));
        assert_eq!(cfg.engine, "whisper-turbo", "wrong type → default engine");
        assert_eq!(cfg.hotkey, "F8", "wrong type → default hotkey");
    }

    // SttConfig::from_settings: audioDevice absent → None
    #[test]
    fn audio_device_absent_is_none() {
        let cfg = SttConfig::from_settings(&json!({ "stt": { "engine": "qwen3-0.6b" } }));
        assert_eq!(cfg.audio_device, None);
    }

    // SttConfig defaults
    #[test]
    fn default_engine_is_whisper_turbo() {
        assert_eq!(SttConfig::default().engine, "whisper-turbo");
    }

    #[test]
    fn default_dominant_lang_is_ru() {
        assert_eq!(SttConfig::default().dominant_lang, "ru");
    }

    #[test]
    fn default_task_is_transcribe() {
        assert_eq!(SttConfig::default().task, SttTask::Transcribe);
    }

    #[test]
    fn default_hotkey_is_f8() {
        assert_eq!(SttConfig::default().hotkey, "F8");
    }
}
