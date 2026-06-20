//! Движок wake-word за единым трейтом (стадия 1).
//!
//! v1: дефолтный `StubEngine` (инертен — никогда не будит, безопасно по
//! умолчанию) + реальный `OwwEngine` (openWakeWord через `ort`) за фичей
//! `wakeword-ort`. Смена движка не трогает сервис/конвейер.

use super::config::WakeConfig;

/// Кадр для детектора — ровно 80 мс @16кГц моно (1280 сэмплов).
pub const WAKE_FRAME_LEN: usize = crate::stt::hub::FRAME_LEN;

/// Единый интерфейс детектора фразы. Stateful: кормится кадрами по 80 мс.
pub trait WakeWordEngine: Send {
    fn name(&self) -> &str;
    /// Скормить один кадр (1280 f32 @16к). Вернуть текущий скор 0..1, или None,
    /// пока окно контекста не набралось (warm-up).
    fn push_frame(&mut self, frame: &[f32]) -> Option<f32>;
    /// Сбросить внутренние буферы (после срабатывания / при рестарте).
    fn reset(&mut self);
}

/// Инертный движок: присутствует, но НИКОГДА не будит (скор всегда 0.0).
/// Дефолт без фичи `wakeword-ort` — always-on микрофон не приводит к ложным
/// срабатываниям при отсутствии моделей.
pub struct StubEngine;

impl WakeWordEngine for StubEngine {
    fn name(&self) -> &str {
        "stub"
    }
    fn push_frame(&mut self, _frame: &[f32]) -> Option<f32> {
        Some(0.0)
    }
    fn reset(&mut self) {}
}

/// Собрать движок по конфигу. При `wakeword-ort` и engine=="openwakeword"
/// пытается поднять `OwwEngine`; модели нет/ошибка → лог + инертный стаб (fail-safe).
pub fn build_engine(cfg: &WakeConfig) -> Box<dyn WakeWordEngine> {
    #[cfg(feature = "wakeword-ort")]
    {
        if cfg.engine == "openwakeword" {
            match super::engine_oww::OwwEngine::load(&cfg.model) {
                Ok(e) => {
                    crate::log::line(&format!("[wake] движок openWakeWord готов: {}", cfg.model));
                    return Box::new(e);
                }
                Err(e) => {
                    crate::log::line(&format!(
                        "[wake] openWakeWord недоступен ({e}) — инертный стаб"
                    ));
                    return Box::new(StubEngine);
                }
            }
        }
    }
    let _ = cfg;
    Box::new(StubEngine)
}

/// Детерминированный тест-движок: «срабатывает» по маркеру в первом сэмпле кадра
/// (frame[0] трактуется как скор). Позволяет тестировать конвейер
/// wake→debounce→verify→action без ONNX/моделей/микрофона.
#[cfg(test)]
pub struct FixtureEngine;

#[cfg(test)]
impl WakeWordEngine for FixtureEngine {
    fn name(&self) -> &str {
        "fixture"
    }
    fn push_frame(&mut self, frame: &[f32]) -> Option<f32> {
        Some(frame.first().copied().unwrap_or(0.0).clamp(0.0, 1.0))
    }
    fn reset(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_is_inert() {
        let mut e = StubEngine;
        for _ in 0..100 {
            assert_eq!(e.push_frame(&[1.0; WAKE_FRAME_LEN]), Some(0.0), "стаб никогда не будит");
        }
    }

    #[test]
    fn build_engine_default_is_stub_without_feature() {
        // Без фичи wakeword-ort всегда стаб.
        let e = build_engine(&WakeConfig::default());
        #[cfg(not(feature = "wakeword-ort"))]
        assert_eq!(e.name(), "stub");
        let _ = e;
    }

    #[test]
    fn fixture_returns_first_sample_as_score() {
        let mut e = FixtureEngine;
        let mut f = vec![0.9f32; WAKE_FRAME_LEN];
        assert_eq!(e.push_frame(&f), Some(0.9));
        f[0] = 0.1;
        assert_eq!(e.push_frame(&f), Some(0.1));
    }
}
