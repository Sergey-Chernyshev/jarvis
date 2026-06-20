//! Чистые DSP-функции аудио-фронтенда (тестируются без живого микрофона).
//!
//! С инкр.10 владельцем захвата стал единый `hub::AudioHub` (always-on источник
//! + веер потребителей + кольцевой преролл + жёсткий mute). Здесь остаётся только
//! переиспользуемая DSP-примитивка; нарезку/ресемпл/раздачу делает `hub::Pipeline`.

/// Усредняет интерлив. каналы в моно.
/// Если `channels <= 1` — возвращает клон без вычислений.
pub(crate) fn downmix_to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    let ch = channels as usize;
    if ch <= 1 {
        return interleaved.to_vec();
    }
    let frames = interleaved.len() / ch;
    let mut out = Vec::with_capacity(frames);
    for i in 0..frames {
        let sum: f32 = interleaved[i * ch..(i + 1) * ch].iter().sum();
        out.push(sum / ch as f32);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // downmix: стерео [0.1, 0.2, 0.3, 0.4] → [0.15, 0.35]
    #[test]
    fn test_downmix_stereo() {
        let stereo = vec![0.1f32, 0.2, 0.3, 0.4];
        let mono = downmix_to_mono(&stereo, 2);
        assert_eq!(mono.len(), 2);
        assert!((mono[0] - 0.15).abs() < 1e-6, "frame 0 = {}", mono[0]);
        assert!((mono[1] - 0.35).abs() < 1e-6, "frame 1 = {}", mono[1]);
    }

    // downmix: моно → passthrough (клон без изменений)
    #[test]
    fn test_downmix_mono() {
        let input = vec![0.1f32, 0.5, -0.3, 0.0];
        let out = downmix_to_mono(&input, 1);
        assert_eq!(out, input);
    }

    // downmix: усреднение 4 каналов
    #[test]
    fn test_downmix_quad() {
        let quad = vec![1.0f32, 2.0, 3.0, 4.0]; // один фрейм из 4 каналов
        let mono = downmix_to_mono(&quad, 4);
        assert_eq!(mono.len(), 1);
        assert!((mono[0] - 2.5).abs() < 1e-6);
    }
}
