//! Silero-VAD гейт перед STT (Tier 3, feature `stt-vad`).
//!
//! Гоняем короткий PTT-клип через нейросетевой детектор речи (Silero V5). Если речи
//! нет — фон/музыка/тишина — STT пропускаем целиком, и Whisper/Qwen не «придумывают»
//! слова. Это сильнейший слой защиты от галлюцинаций (см. ресёрч).
//!
//! Модель ONNX бандлится внутри крейта `voice_activity_detector`; ort тот же
//! (=2.0.0-rc.10), что и у wake-word — одна onnxruntime на бинарь. Без фичи `stt-vad`
//! гейта нет (всегда «есть речь») — поведение как раньше.

#[cfg(feature = "stt-vad")]
mod imp {
    use voice_activity_detector::VoiceActivityDetector;

    const SAMPLE_RATE: i64 = 16_000; // hub отдаёт 16 кГц моно f32
    const CHUNK: usize = 512; // Silero V5: для 16 кГц допустим только chunk 512
    const SPEECH_PROB: f32 = 0.5; // порог «это речь» на один чанк (~32 мс)
    const MIN_SPEECH_CHUNKS: usize = 3; // ≥3 чанка (~96 мс) речи → считаем, что речь есть

    /// Есть ли речь в клипе? Бьём на чанки по 512 @16к, считаем чанки с p≥порога.
    /// Детектор создаём заново на вызов: onnx-сессия в крейте общая (дёшево), а
    /// LSTM-состояние сбрасывается — клипы независимы, без утечки между диктовками.
    /// Любая ошибка/слишком короткий клип → true (fail-open: лучше распознать, чем
    /// потерять живую речь — мусор дочистят Tier 1/2).
    pub fn has_speech(pcm: &[f32]) -> bool {
        if pcm.len() < CHUNK {
            return true; // короче одного чанка — могло быть быстрое слово, не гейтим
        }
        let Ok(mut vad) = VoiceActivityDetector::builder()
            .chunk_size(CHUNK)
            .sample_rate(SAMPLE_RATE)
            .build()
        else {
            crate::log::line("[vad] детектор не собрался — пропускаю гейт (fail-open)");
            return true;
        };
        let mut speech = 0usize;
        for chunk in pcm.chunks_exact(CHUNK) {
            if vad.predict(chunk.iter().copied()) >= SPEECH_PROB {
                speech += 1;
                if speech >= MIN_SPEECH_CHUNKS {
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(feature = "stt-vad")]
pub use imp::has_speech;

/// Без фичи `stt-vad` — гейта нет, всегда «есть речь» (старое поведение).
#[cfg(not(feature = "stt-vad"))]
pub fn has_speech(_pcm: &[f32]) -> bool {
    true
}

#[cfg(all(test, feature = "stt-vad"))]
mod tests {
    use super::has_speech;

    // Тишина (1с нулей) → VAD говорит «речи нет». Заодно ПРОВЕРЯЕТ, что onnxruntime
    // реально загрузился: при ошибке загрузки has_speech делает fail-open (true),
    // и этот тест упадёт — то есть упадёт, только если VAD по-настоящему не работает.
    #[test]
    fn silence_is_gated_as_no_speech() {
        let silence = vec![0.0f32; 16_000];
        assert!(
            !has_speech(&silence),
            "тишина должна гейтиться (если true — onnxruntime не загрузился, fail-open)"
        );
    }

    // Слишком короткий клип (< одного чанка) — fail-open, не гейтим (быстрое слово).
    #[test]
    fn tiny_clip_is_not_gated() {
        assert!(has_speech(&vec![0.0f32; 100]));
    }
}
