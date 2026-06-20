//! Конфиг wake-word и верификации из ~/.jarvis/settings.json. Дыры → дефолты.

use serde_json::Value;

/// Настройки wake-word детектора (стадия 1).
#[derive(Debug, Clone, PartialEq)]
pub struct WakeConfig {
    /// Включён ли always-on детектор (по умолчанию ВЫКЛ — приватность).
    pub enabled: bool,
    /// Движок детектора: "openwakeword" (v1) | "stub".
    pub engine: String,
    /// Имя/путь модели фразы. "hey_jarvis" → бандл; иначе абсолютный путь .onnx.
    pub model: String,
    /// Порог срабатывания 0..1.
    pub threshold: f32,
    /// Сколько кадров подряд ≥ порога нужно для срабатывания (антидребезг).
    pub debounce: u32,
}

impl Default for WakeConfig {
    fn default() -> Self {
        WakeConfig {
            enabled: false,
            engine: "openwakeword".into(),
            model: "hey_jarvis".into(),
            threshold: 0.5,
            debounce: 2,
        }
    }
}

impl WakeConfig {
    pub fn from_settings(root: &Value) -> Self {
        let d = WakeConfig::default();
        let w = root.get("wake");
        let s = |k: &str, dv: &str| {
            w.and_then(|v| v.get(k)).and_then(Value::as_str).unwrap_or(dv).to_string()
        };
        let f = |k: &str, dv: f32| {
            w.and_then(|v| v.get(k)).and_then(Value::as_f64).map(|x| x as f32).unwrap_or(dv)
        };
        WakeConfig {
            enabled: w.and_then(|v| v.get("enabled")).and_then(Value::as_bool).unwrap_or(d.enabled),
            engine: s("engine", &d.engine),
            model: s("model", &d.model),
            // порог зажимаем в [0,1] — мусор/выход за диапазон → дефолт-безопасно
            threshold: f("threshold", d.threshold).clamp(0.0, 1.0),
            debounce: w
                .and_then(|v| v.get("debounce"))
                .and_then(Value::as_u64)
                .map(|x| x.clamp(1, 50) as u32)
                .unwrap_or(d.debounce),
        }
    }
}

/// Настройки верификации говорящего (стадия 2). v1 — только шов.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifyConfig {
    /// Включена ли верификация (по умолчанию ВЫКЛ — будит любой голос).
    pub enabled: bool,
    /// Порог совпадения 0..1.
    pub threshold: f32,
    /// Путь к голосовому профилю (None — нет профиля).
    pub profile: Option<String>,
}

impl Default for VerifyConfig {
    fn default() -> Self {
        VerifyConfig { enabled: false, threshold: 0.5, profile: None }
    }
}

impl VerifyConfig {
    pub fn from_settings(root: &Value) -> Self {
        let d = VerifyConfig::default();
        let v = root.get("verification");
        VerifyConfig {
            enabled: v.and_then(|x| x.get("enabled")).and_then(Value::as_bool).unwrap_or(d.enabled),
            threshold: v
                .and_then(|x| x.get("threshold"))
                .and_then(Value::as_f64)
                .map(|x| x as f32)
                .unwrap_or(d.threshold)
                .clamp(0.0, 1.0),
            profile: v.and_then(|x| x.get("profile")).and_then(Value::as_str).map(String::from),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn wake_missing_block_is_defaults() {
        assert_eq!(WakeConfig::from_settings(&json!({})), WakeConfig::default());
    }

    #[test]
    fn wake_default_is_disabled() {
        assert!(!WakeConfig::default().enabled, "always-on по умолчанию ВЫКЛ");
    }

    #[test]
    fn wake_parses_block() {
        let c = WakeConfig::from_settings(&json!({
            "wake": { "enabled": true, "threshold": 0.7, "debounce": 3, "model": "/x/hey.onnx" }
        }));
        assert!(c.enabled);
        assert!((c.threshold - 0.7).abs() < 1e-6);
        assert_eq!(c.debounce, 3);
        assert_eq!(c.model, "/x/hey.onnx");
    }

    #[test]
    fn wake_threshold_clamped() {
        let c = WakeConfig::from_settings(&json!({ "wake": { "threshold": 9.0 } }));
        assert!((c.threshold - 1.0).abs() < 1e-6, "порог зажат в [0,1]");
        let c2 = WakeConfig::from_settings(&json!({ "wake": { "threshold": -3.0 } }));
        assert!((c2.threshold - 0.0).abs() < 1e-6);
    }

    #[test]
    fn wake_debounce_clamped_min_1() {
        let c = WakeConfig::from_settings(&json!({ "wake": { "debounce": 0 } }));
        assert_eq!(c.debounce, 1, "debounce минимум 1");
    }

    #[test]
    fn wake_garbage_types_fall_back() {
        let c = WakeConfig::from_settings(&json!({ "wake": { "enabled": "yes", "threshold": "hi" } }));
        assert!(!c.enabled);
        assert!((c.threshold - 0.5).abs() < 1e-6);
    }

    #[test]
    fn verify_default_disabled() {
        let d = VerifyConfig::default();
        assert!(!d.enabled, "верификация по умолчанию ВЫКЛ");
        assert_eq!(d.profile, None);
    }

    #[test]
    fn verify_parses_block() {
        let v = VerifyConfig::from_settings(&json!({
            "verification": { "enabled": true, "threshold": 0.8, "profile": "/p.bin" }
        }));
        assert!(v.enabled);
        assert!((v.threshold - 0.8).abs() < 1e-6);
        assert_eq!(v.profile, Some("/p.bin".into()));
    }
}
