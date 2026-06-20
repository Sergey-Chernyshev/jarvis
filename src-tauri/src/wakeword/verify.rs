//! Верификация говорящего (стадия 2) — ШОВ. v1: только интерфейс + заглушка.
//!
//! Запускается СТРОГО после wake-word, на аудио срабатывания (не always-on).
//! `NullVerifier` всегда проходит (verification по умолчанию выключена → будит
//! любой голос, нет false-reject пользователя). Реальный эмбеддер подключается
//! позже ТОЙ ЖЕ сигнатурой, не трогая wake-слой/конвейер (см. дизайн §3).
//!
//! Честно: верификация — это снижение случайных срабатываний на чужой голос,
//! НЕ security (запись/имитатор пройдут).

use super::config::VerifyConfig;

/// Единый интерфейс верификатора. `Sync` — вызывается из consumer-потока wake.
pub trait SpeakerVerifier: Send + Sync {
    /// Включена ли реальная проверка. false → гейт всегда пропускает.
    fn enabled(&self) -> bool;
    /// Сверить аудио срабатывания (16к моно) с зачисленным профилем → 0..1.
    fn verify(&self, audio_16k: &[f32]) -> f32;
    /// Зачислить профиль из нескольких произнесений фразы.
    fn enroll(&self, utterances: &[Vec<f32>]) -> Result<(), String>;
}

/// Заглушка-проход: всегда совпадение 1.0, verification считается выключенной.
pub struct NullVerifier;

impl SpeakerVerifier for NullVerifier {
    fn enabled(&self) -> bool {
        false
    }
    fn verify(&self, _audio_16k: &[f32]) -> f32 {
        1.0
    }
    fn enroll(&self, _utterances: &[Vec<f32>]) -> Result<(), String> {
        Err("верификация говорящего ещё не реализована (v1: только шов)".into())
    }
}

/// Собрать верификатор по конфигу. v1: всегда `NullVerifier` (реализация позже,
/// той же сигнатурой). Гейт принимает решение в `passes()`.
pub fn build_verifier(_cfg: &VerifyConfig) -> Box<dyn SpeakerVerifier> {
    Box::new(NullVerifier)
}

/// Решение гейта верификации: пропустить ли срабатывание.
/// Выключена ИЛИ verifier не enabled → пропускаем (любой голос). Иначе сверяем
/// скор с порогом конфига.
pub fn passes(verifier: &dyn SpeakerVerifier, cfg: &VerifyConfig, audio_16k: &[f32]) -> bool {
    if !cfg.enabled || !verifier.enabled() {
        return true;
    }
    verifier.verify(audio_16k) >= cfg.threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_verifier_always_passes_value() {
        let v = NullVerifier;
        assert!(!v.enabled());
        assert_eq!(v.verify(&[0.0; 16000]), 1.0);
    }

    #[test]
    fn null_verifier_enroll_is_unsupported() {
        assert!(NullVerifier.enroll(&[vec![0.0; 100]]).is_err());
    }

    #[test]
    fn gate_passes_when_disabled() {
        let v = NullVerifier;
        let cfg = VerifyConfig { enabled: false, ..VerifyConfig::default() };
        assert!(passes(&v, &cfg, &[0.0; 16000]), "verification выкл → пропуск");
    }

    #[test]
    fn gate_passes_when_verifier_inert_even_if_cfg_enabled() {
        // Конфиг включён, но NullVerifier.enabled()==false → всё равно пропуск
        // (v1: реальной проверки нет, false-reject пользователя недопустим).
        let v = NullVerifier;
        let cfg = VerifyConfig { enabled: true, threshold: 0.9, profile: None };
        assert!(passes(&v, &cfg, &[0.0; 16000]));
    }

    // Мок включённого верификатора — проверяем сравнение с порогом.
    struct MockVerifier(f32);
    impl SpeakerVerifier for MockVerifier {
        fn enabled(&self) -> bool {
            true
        }
        fn verify(&self, _a: &[f32]) -> f32 {
            self.0
        }
        fn enroll(&self, _u: &[Vec<f32>]) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn gate_threshold_compare_when_enabled() {
        let cfg = VerifyConfig { enabled: true, threshold: 0.6, profile: None };
        assert!(passes(&MockVerifier(0.7), &cfg, &[0.0; 10]), "0.7 ≥ 0.6 → пропуск");
        assert!(!passes(&MockVerifier(0.5), &cfg, &[0.0; 10]), "0.5 < 0.6 → отказ");
    }
}
