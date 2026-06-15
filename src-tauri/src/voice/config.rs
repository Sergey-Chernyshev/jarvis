//! voice-блок настроек из ~/.jarvis/settings.json. Битый/нет блока → дефолты.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct VoiceConfig {
    pub engine: String,         // "piper" | "silero"
    pub speaker: String,
    pub voice_path: String,
    pub sample_rate: u32,
    /// темп речи Silero: x-slow|slow|medium|fast|x-fast
    pub rate: String,
    pub mute: bool,
    pub verbosity: String,      // "short" | "descriptive"
    pub ev_stop: bool,
    pub ev_notification: bool,
    pub ev_stop_failure: bool,
    pub ev_subagent_stop: bool,
    pub ev_session_end: bool,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        VoiceConfig {
            engine: "piper".into(), speaker: String::new(), voice_path: String::new(),
            // лучшие дефолты Silero: 48 кГц + темп «fast» (×1.2 бодрее, не тараторит)
            sample_rate: 48000, rate: "fast".into(), mute: false, verbosity: "short".into(),
            ev_stop: true, ev_notification: true, ev_stop_failure: true,
            ev_subagent_stop: false, ev_session_end: false,
        }
    }
}

impl VoiceConfig {
    /// Распарсить из корневого settings-объекта (его поле "voice"). Дефолты на дыры.
    pub fn from_settings(root: &Value) -> Self {
        let d = VoiceConfig::default();
        let v = root.get("voice");
        let s = |k: &str, dv: &str| v.and_then(|v| v.get(k)).and_then(Value::as_str).unwrap_or(dv).to_string();
        let b = |k: &str, dv: bool| v.and_then(|v| v.get(k)).and_then(Value::as_bool).unwrap_or(dv);
        let ev = |k: &str, dv: bool| v.and_then(|v| v.get("events")).and_then(|e| e.get(k)).and_then(Value::as_bool).unwrap_or(dv);
        VoiceConfig {
            engine: s("engine", &d.engine),
            speaker: s("speaker", &d.speaker),
            voice_path: s("voicePath", &d.voice_path),
            sample_rate: v.and_then(|v| v.get("sampleRate")).and_then(Value::as_u64).unwrap_or(d.sample_rate as u64) as u32,
            rate: s("rate", &d.rate),
            mute: b("mute", d.mute),
            verbosity: s("verbosity", &d.verbosity),
            ev_stop: ev("stop", d.ev_stop),
            ev_notification: ev("notification", d.ev_notification),
            ev_stop_failure: ev("stopFailure", d.ev_stop_failure),
            ev_subagent_stop: ev("subagentStop", d.ev_subagent_stop),
            ev_session_end: ev("sessionEnd", d.ev_session_end),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_block_is_defaults() {
        assert_eq!(VoiceConfig::from_settings(&json!({})), VoiceConfig::default());
    }

    #[test]
    fn partial_block_merges_defaults() {
        let cfg = VoiceConfig::from_settings(&json!({ "voice": { "engine": "silero", "events": { "stop": false } } }));
        assert_eq!(cfg.engine, "silero");
        assert!(!cfg.ev_stop);
        assert!(cfg.ev_notification, "не заданное событие — дефолт вкл");
        assert_eq!(cfg.sample_rate, 48000);
    }

    #[test]
    fn garbage_types_fall_back() {
        let cfg = VoiceConfig::from_settings(&json!({ "voice": { "sampleRate": "oops", "mute": "yes" } }));
        assert_eq!(cfg.sample_rate, 48000);
        assert!(!cfg.mute);
    }
}
