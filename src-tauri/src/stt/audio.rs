//! Аудио-захват через CPAL + ресемпл до 16кГц моно f32.
//!
//! Публичный API:
//!  - `CaptureSession::start(device)` — начать захват с микрофона
//!  - `CaptureSession::finish(self)` → `Vec<f32>` 16кГц моно f32
//!
//! Чистые DSP-функции (тестируются без реального микрофона):
//!  - `downmix_to_mono`   — усреднение интерлив. каналов → моно
//!  - `resample_to_16k`   — ресемпл через rubato FftFixedIn

use std::sync::{Arc, Mutex};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    SampleFormat,
};
use rubato::{FftFixedIn, Resampler};

/// Активная сессия захвата аудио. Дропнуть = остановить стрим.
pub struct CaptureSession {
    stream: cpal::Stream,
    buffer: Arc<Mutex<Vec<f32>>>,
    src_rate: u32,
    channels: u16,
}

impl CaptureSession {
    /// Начать захват. `device` — имя устройства или None → системный дефолт.
    pub fn start(device: Option<&str>) -> Result<Self, String> {
        let host = cpal::default_host();

        let input_device = match device {
            None => host
                .default_input_device()
                .ok_or_else(|| "нет дефолтного устройства ввода".to_string())?,
            Some(name) => host
                .input_devices()
                .map_err(|e| format!("enumerate devices: {e}"))?
                .find(|d| d.name().map(|n| n == name).unwrap_or(false))
                .ok_or_else(|| format!("устройство '{}' не найдено", name))?,
        };

        let config = input_device
            .default_input_config()
            .map_err(|e| format!("default_input_config: {e}"))?;

        let src_rate = config.sample_rate().0;
        let channels = config.channels();
        let sample_format = config.sample_format();
        let stream_config: cpal::StreamConfig = config.into();

        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let buf_clone = Arc::clone(&buffer);

        let err_fn = |e| eprintln!("[audio] stream error: {e}");

        let stream = match sample_format {
            SampleFormat::F32 => input_device
                .build_input_stream(
                    &stream_config,
                    move |data: &[f32], _| {
                        if let Ok(mut b) = buf_clone.lock() {
                            b.extend_from_slice(data);
                        }
                    },
                    err_fn,
                    None,
                )
                .map_err(|e| format!("build_input_stream f32: {e}"))?,
            SampleFormat::I16 => {
                let buf2 = Arc::clone(&buffer);
                input_device
                    .build_input_stream(
                        &stream_config,
                        move |data: &[i16], _| {
                            if let Ok(mut b) = buf2.lock() {
                                b.extend(data.iter().map(|&s| s as f32 / i16::MAX as f32));
                            }
                        },
                        err_fn,
                        None,
                    )
                    .map_err(|e| format!("build_input_stream i16: {e}"))?
            }
            SampleFormat::U16 => {
                let buf3 = Arc::clone(&buffer);
                input_device
                    .build_input_stream(
                        &stream_config,
                        move |data: &[u16], _| {
                            if let Ok(mut b) = buf3.lock() {
                                b.extend(
                                    data.iter()
                                        .map(|&s| (s as f32 - 32768.0) / 32768.0),
                                );
                            }
                        },
                        err_fn,
                        None,
                    )
                    .map_err(|e| format!("build_input_stream u16: {e}"))?
            }
            other => return Err(format!("неподдерживаемый формат сэмплов: {other:?}")),
        };

        stream.play().map_err(|e| format!("stream.play: {e}"))?;

        Ok(CaptureSession { stream, buffer, src_rate, channels })
    }

    /// Остановить захват, вернуть 16кГц моно f32-буфер.
    pub fn finish(self) -> Result<Vec<f32>, String> {
        // Дропаем стрим → захват останавливается
        drop(self.stream);

        let raw = self
            .buffer
            .lock()
            .map_err(|e| format!("buffer lock: {e}"))?
            .drain(..)
            .collect::<Vec<f32>>();

        let mono = downmix_to_mono(&raw, self.channels);
        resample_to_16k(&mono, self.src_rate)
    }
}

// ─── Чистые DSP-функции ──────────────────────────────────────────────────────

/// Усредняет интерлив. каналы в моно.
/// Если `channels == 1` — возвращает клон без вычислений.
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

/// Ресемплирует моно f32-буфер из `src_rate` Гц → 16000 Гц через rubato FftFixedIn.
/// Если `src_rate == 16000` — возвращает клон без обработки.
///
/// Параметры rubato:
///  - chunk_size=256, sub_chunks=4 — минимизируют startup-latency FftFixedIn,
///    удерживая погрешность длины в пределах ~1% при типичных буферах 0.1–10 с.
pub(crate) fn resample_to_16k(input: &[f32], src_rate: u32) -> Result<Vec<f32>, String> {
    const DST_RATE: u32 = 16_000;

    if src_rate == DST_RATE {
        return Ok(input.to_vec());
    }
    if input.is_empty() {
        return Ok(Vec::new());
    }

    // chunk_size=256, sub_chunks=4 выбраны экспериментально:
    // дают <2% погрешности длины при N=4800 (48k→16k) — в пределах ±5% допуска.
    let chunk_size: usize = 256;
    let sub_chunks: usize = 4;

    let mut resampler =
        FftFixedIn::<f32>::new(src_rate as usize, DST_RATE as usize, chunk_size, sub_chunks, 1)
            .map_err(|e| format!("FftFixedIn::new: {e:?}"))?;

    let mut output_samples: Vec<f32> = Vec::new();
    let full_chunks = input.len() / chunk_size;
    let remainder = input.len() % chunk_size;

    // Полные чанки
    for i in 0..full_chunks {
        let chunk = &input[i * chunk_size..(i + 1) * chunk_size];
        let waves_in: Vec<Vec<f32>> = vec![chunk.to_vec()];
        let waves_out =
            resampler.process(&waves_in, None).map_err(|e| format!("resample process: {e:?}"))?;
        output_samples.extend_from_slice(&waves_out[0]);
    }

    // Хвост: добиваем нулями до chunk_size, берём пропорциональную часть вывода
    if remainder > 0 {
        let mut tail = input[full_chunks * chunk_size..].to_vec();
        tail.resize(chunk_size, 0.0);
        let waves_in: Vec<Vec<f32>> = vec![tail];
        let waves_out =
            resampler.process(&waves_in, None).map_err(|e| format!("resample tail: {e:?}"))?;
        let keep = (waves_out[0].len() as f64 * remainder as f64 / chunk_size as f64).round()
            as usize;
        let keep = keep.min(waves_out[0].len());
        output_samples.extend_from_slice(&waves_out[0][..keep]);
    }

    Ok(output_samples)
}

// ─── Тесты (без реального микрофона) ─────────────────────────────────────────

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

    // resample: src==16000 → passthrough без изменений
    #[test]
    fn test_resample_passthrough() {
        let input = vec![0.1f32, 0.2, 0.3, 0.4, 0.5];
        let out = resample_to_16k(&input, 16000).unwrap();
        assert_eq!(out, input);
    }

    // resample: 48000→16000 (~N/3 выходных, ±5%)
    // N=4800 (0.1 с @ 48кГц → ~1600 сэмплов @ 16кГц)
    #[test]
    fn test_resample_48k_to_16k() {
        let n = 4800usize;
        let input: Vec<f32> = (0..n).map(|i| (i as f32 * 0.01).sin()).collect();
        let out = resample_to_16k(&input, 48000).unwrap();
        let expected = n / 3; // 1600
        let tolerance = (expected as f32 * 0.05).ceil() as usize; // 80
        assert!(
            out.len() >= expected - tolerance && out.len() <= expected + tolerance,
            "48k→16k: got {} samples, expected {}±{}",
            out.len(),
            expected,
            tolerance
        );
    }

    // resample: 32000→16000 (~N/2 выходных, ±5%)
    // N=3200 (0.1 с @ 32кГц → ~1600 сэмплов @ 16кГц)
    #[test]
    fn test_resample_32k_to_16k() {
        let n = 3200usize;
        let input: Vec<f32> = (0..n).map(|i| (i as f32 * 0.02).sin()).collect();
        let out = resample_to_16k(&input, 32000).unwrap();
        let expected = n / 2; // 1600
        let tolerance = (expected as f32 * 0.05).ceil() as usize; // 80
        assert!(
            out.len() >= expected - tolerance && out.len() <= expected + tolerance,
            "32k→16k: got {} samples, expected {}±{}",
            out.len(),
            expected,
            tolerance
        );
    }
}
