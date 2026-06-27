//! Барж-ин (веха 2c, спайк): онсет-детектор речи поверх TTS + шов эхо-подавления.
//!
//! ВАЖНО (честная граница): без РЕАЛЬНОГО AEC (см. `NoopAec`) энергия микрофона
//! содержит собственный TTS Джарвиса — энергетический детектор сработал бы на него
//! (ложный барж-ин). Поэтому акустический барж-ин включается флагом `bargeIn`
//! (дефолт OFF) лишь когда `EchoCanceller` — настоящий backend (macOS
//! VoiceProcessingIO / webrtc-audio-processing) и оттюнен на микрофоне.
//! Эти куски — чистые/тестируемые; нативный AEC-backend — мик-зависимый остаток.

/// Шов эхо-подавления: убрать из mic-кадра опорный сигнал (то, что играем в TTS).
/// На выходе — «очищенный» mic-кадр для VAD/онсет-детектора.
pub trait EchoCanceller: Send {
    fn process(&mut self, mic: &[f32], reference: &[f32]) -> Vec<f32>;
}

/// Заглушка: passthrough (эхо НЕ подавляется) — текущее поведение стека. Барж-ин
/// с ней самосрабатывает, поэтому при NoopAec флаг `bargeIn` держим выключенным.
pub struct NoopAec;

impl EchoCanceller for NoopAec {
    fn process(&mut self, mic: &[f32], _reference: &[f32]) -> Vec<f32> {
        mic.to_vec()
    }
}

/// Автомат «началась речь юзера поверх TTS»: N кадров подряд выше порога → онсет.
/// Энергия подаётся УЖЕ после AEC (иначе ловит собственный TTS).
pub struct OnsetDetector {
    threshold: f32,
    need: u32,
    consec: u32,
}

impl OnsetDetector {
    pub fn new(threshold: f32, need_frames: u32) -> Self {
        Self { threshold, need: need_frames.max(1), consec: 0 }
    }

    /// Подать энергию (RMS) очередного кадра. true — онсет подтверждён (пора рвать TTS).
    pub fn push(&mut self, energy: f32) -> bool {
        if energy >= self.threshold {
            self.consec += 1;
            if self.consec >= self.need {
                self.consec = 0;
                return true;
            }
        } else {
            self.consec = 0;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_aec_is_passthrough() {
        let mut a = NoopAec;
        let mic = [0.1, -0.2, 0.3];
        assert_eq!(a.process(&mic, &[0.5, 0.5, 0.5]), mic.to_vec());
    }

    #[test]
    fn onset_after_n_consecutive_loud_frames() {
        let mut d = OnsetDetector::new(0.05, 3);
        assert!(!d.push(0.1)); // 1
        assert!(!d.push(0.1)); // 2
        assert!(d.push(0.1)); // 3 → онсет
    }

    #[test]
    fn quiet_frame_resets_streak() {
        let mut d = OnsetDetector::new(0.05, 3);
        d.push(0.1);
        d.push(0.1);
        assert!(!d.push(0.01), "тихий кадр сбрасывает счётчик");
        assert!(!d.push(0.1));
        assert!(!d.push(0.1));
        assert!(d.push(0.1), "снова 3 подряд → онсет");
    }

    #[test]
    fn echo_level_below_threshold_does_not_trigger() {
        // с реальным AEC остаточное эхо ниже порога → не триггерит барж-ин
        let mut d = OnsetDetector::new(0.05, 2);
        for _ in 0..10 {
            assert!(!d.push(0.02), "эхо-уровень не должен триггерить");
        }
    }
}
