//! Сжатое хранилище аудио диктовок — чтобы ПЕРЕГЕНЕРИРОВАТЬ распознавание, если
//! анализ дал ошибку/мусор. Поток: f32 PCM (16кГц моно, как у `transcribe`) →
//! i16 WAV (`hound`) → gzip (`flate2`) → файл `stt/dictations/<id>.wav.gz`.
//!
//! Храним только последние `RETAIN` диктовок (аудио тяжелее текста) — на каждое
//! сохранение чистим выпавшее из окна. Всё best-effort: ошибка не валит диктовку.

use std::io::{Read, Write};
use std::path::PathBuf;

/// Частота PCM, который приходит в `SttService::transcribe` (см. `hub::DST_RATE`).
const RATE: u32 = 16_000;
/// Сколько последних диктовок держать с аудио (старее — удаляем).
const RETAIN: u64 = 300;

fn dir() -> PathBuf {
    crate::util::jarvis_dir().join("stt").join("dictations")
}
fn path(id: u64) -> PathBuf {
    dir().join(format!("{id}.wav.gz"))
}

/// Закодировать PCM (f32, моно) в gzip-сжатый WAV (i16). Чистая — тестируема.
pub fn encode(pcm: &[f32]) -> Result<Vec<u8>, String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav: Vec<u8> = Vec::new();
    {
        let mut w = hound::WavWriter::new(std::io::Cursor::new(&mut wav), spec)
            .map_err(|e| format!("wav writer: {e}"))?;
        for &s in pcm {
            let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            w.write_sample(v).map_err(|e| format!("wav write: {e}"))?;
        }
        w.finalize().map_err(|e| format!("wav finalize: {e}"))?;
    }
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&wav).map_err(|e| format!("gzip: {e}"))?;
    enc.finish().map_err(|e| format!("gzip finish: {e}"))
}

/// Декодировать gzip-WAV обратно в PCM (f32, моно 16кГц). Чистая — тестируема.
pub fn decode(gz: &[u8]) -> Result<Vec<f32>, String> {
    let mut dec = flate2::read::GzDecoder::new(gz);
    let mut wav = Vec::new();
    dec.read_to_end(&mut wav).map_err(|e| format!("gunzip: {e}"))?;
    let r = hound::WavReader::new(std::io::Cursor::new(wav)).map_err(|e| format!("wav reader: {e}"))?;
    Ok(r.into_samples::<i16>()
        .map(|s| s.unwrap_or(0) as f32 / i16::MAX as f32)
        .collect())
}

/// Сохранить аудио диктовки по id (best-effort). Чистит выпавшее из окна `RETAIN`.
pub fn save(id: u64, pcm: &[f32]) -> Result<(), String> {
    let d = dir();
    std::fs::create_dir_all(&d).map_err(|e| format!("mkdir: {e}"))?;
    let bytes = encode(pcm)?;
    std::fs::write(path(id), bytes).map_err(|e| format!("запись: {e}"))?;
    if id > RETAIN {
        let _ = std::fs::remove_file(path(id - RETAIN));
    }
    Ok(())
}

/// Загрузить и декодировать аудио диктовки по id.
pub fn load(id: u64) -> Result<Vec<f32>, String> {
    let gz = std::fs::read(path(id)).map_err(|e| format!("аудио диктовки {id} не найдено: {e}"))?;
    decode(&gz)
}

/// Есть ли сохранённое аудио для id.
pub fn exists(id: u64) -> bool {
    path(id).exists()
}

/// Удалить аудио диктовки (при выселении/удалении реплики).
pub fn delete(id: u64) {
    let _ = std::fs::remove_file(path(id));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip_preserves_signal() {
        // синус ~440Гц, 0.5с @16кГц
        let n = 8000;
        let pcm: Vec<f32> = (0..n)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / RATE as f32).sin())
            .collect();
        let gz = encode(&pcm).expect("encode");
        let back = decode(&gz).expect("decode");
        assert_eq!(back.len(), pcm.len(), "длина сохраняется");
        // i16-квантование: расхождение не больше шага 1/32767
        let max_err = pcm.iter().zip(&back).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(max_err < 1.0 / 32767.0 + 1e-4, "сигнал сохранён в пределах i16 ({max_err})");
    }

    #[test]
    fn gzip_actually_compresses_silence() {
        // тишина сжимается сильно — проверяем, что это реально gzip (не голый WAV)
        let pcm = vec![0.0f32; 16_000]; // 1с тишины = 32КБ i16 WAV
        let gz = encode(&pcm).expect("encode");
        assert!(gz.len() < 2000, "тишина ужимается gzip'ом (got {} байт)", gz.len());
    }

    #[test]
    fn decode_garbage_errors_not_panics() {
        assert!(decode(b"not a gzip").is_err(), "битый ввод — Err, не паника");
    }

    #[test]
    fn empty_pcm_roundtrips() {
        let gz = encode(&[]).expect("encode empty");
        assert_eq!(decode(&gz).expect("decode empty").len(), 0);
    }
}
