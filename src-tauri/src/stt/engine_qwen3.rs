//! Qwen3-ASR движок STT — клиент к локальному stt-server.py (FastAPI на 127.0.0.1).
//! Блокирующий reqwest (зовём из STT-воркера, отдельный поток). Любой сбой/
//! недоступность сайдкара — fail-safe: Err(String), демон не падает.

use crate::stt::engine::{SttEngine, SttOptions, SttResult, SttSeg};

pub struct Qwen3Engine {
    base: String,
    name: String,
}

impl Qwen3Engine {
    pub fn new(base: String, name: String) -> Self {
        Qwen3Engine { base, name }
    }

    fn client(timeout: std::time::Duration) -> Result<reqwest::blocking::Client, String> {
        reqwest::blocking::Client::builder()
            .timeout(timeout)
            .no_proxy() // сайдкар — localhost; системный HTTP_PROXY его не касается
            .build()
            .map_err(|e| format!("http client: {e}"))
    }
}

impl SttEngine for Qwen3Engine {
    fn name(&self) -> &'static str {
        // Возвращаем статический литерал по значению поля.
        match self.name.as_str() {
            "qwen3-1.7b" => "qwen3-1.7b",
            _ => "qwen3-0.6b",
        }
    }

    fn available(&self) -> bool {
        Self::client(std::time::Duration::from_secs(3))
            .and_then(|c| {
                c.get(format!("{}/health", self.base))
                    .send()
                    .map_err(|e| format!("сайдкар недоступен: {e}"))
            })
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    fn transcribe(&self, pcm: &[f32], opts: &SttOptions) -> Result<SttResult, String> {
        let body = pcm_to_le_bytes(pcm);
        let client = Self::client(std::time::Duration::from_secs(30))?;
        let resp = client
            .post(format!("{}/transcribe", self.base))
            .header("Content-Type", "application/octet-stream")
            .header("lang", &opts.dominant_lang)
            .body(body)
            .send()
            .map_err(|e| format!("сайдкар недоступен: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("сайдкар rc={}", resp.status()));
        }
        let text = resp.text().map_err(|e| format!("чтение ответа: {e}"))?;
        parse_transcribe_resp(&text)
    }
}

// ---------------------------------------------------------------------------
// Чистые вспомогательные функции — тестируемы без сети.
// ---------------------------------------------------------------------------

/// Конвертировать PCM-сэмплы f32 в little-endian байты.
pub fn pcm_to_le_bytes(pcm: &[f32]) -> Vec<u8> {
    pcm.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Парсить JSON-ответ `/transcribe` в `SttResult`.
pub fn parse_transcribe_resp(json: &str) -> Result<SttResult, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("JSON-ошибка: {e}"))?;

    let text = v
        .get("text")
        .and_then(|t| t.as_str())
        .ok_or_else(|| "ответ не содержит поле 'text'".to_string())?
        .to_string();

    let segments = if let Some(segs) = v.get("segments").and_then(|s| s.as_array()) {
        segs.iter()
            .filter_map(|seg| {
                let seg_text = seg.get("text").and_then(|t| t.as_str())?.to_string();
                let lang = seg.get("lang").and_then(|l| l.as_str()).map(String::from);
                Some(SttSeg { text: seg_text, lang })
            })
            .collect()
    } else {
        vec![]
    };

    Ok(SttResult { text, segments })
}

// ---------------------------------------------------------------------------
// Тесты — работают без сайдкара, без сети.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::engine::{SttOptions, SttEngine};

    // --- pcm_to_le_bytes ---

    #[test]
    fn pcm_encoding_length() {
        let bytes = pcm_to_le_bytes(&[0.0_f32, 1.0_f32]);
        // 2 сэмпла × 4 байта = 8 байт
        assert_eq!(bytes.len(), 8);
    }

    #[test]
    fn pcm_encoding_roundtrip() {
        let input = vec![0.0_f32, 1.0_f32, -1.0_f32, 0.5_f32];
        let bytes = pcm_to_le_bytes(&input);
        // Восстановить из little-endian байт
        let decoded: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        assert_eq!(input, decoded);
    }

    // --- parse_transcribe_resp ---

    #[test]
    fn parse_resp_basic_text() {
        let r = parse_transcribe_resp(r#"{"text":"привет"}"#).unwrap();
        assert_eq!(r.text, "привет");
        assert!(r.segments.is_empty(), "без segments → пустой Vec");
    }

    #[test]
    fn parse_resp_empty_text() {
        let r = parse_transcribe_resp(r#"{"text":""}"#).unwrap();
        assert_eq!(r.text, "");
        assert!(r.segments.is_empty());
    }

    #[test]
    fn parse_resp_bad_json_is_err() {
        assert!(parse_transcribe_resp("not-json").is_err());
    }

    #[test]
    fn parse_resp_missing_text_is_err() {
        assert!(parse_transcribe_resp(r#"{"foo":"bar"}"#).is_err());
    }

    #[test]
    fn parse_resp_with_segments() {
        let json = r#"{"text":"привет мир","segments":[{"text":"привет","lang":"ru"},{"text":"мир","lang":"ru"}]}"#;
        let r = parse_transcribe_resp(json).unwrap();
        assert_eq!(r.text, "привет мир");
        assert_eq!(r.segments.len(), 2);
        assert_eq!(r.segments[0].text, "привет");
        assert_eq!(r.segments[0].lang, Some("ru".into()));
    }

    // --- available() без сервера ---

    #[test]
    fn available_unreachable_returns_false() {
        let e = Qwen3Engine::new("http://127.0.0.1:19999".into(), "qwen3-0.6b".into());
        assert!(!e.available(), "недостижимый порт → false, не паника");
    }

    // --- build_engine → Qwen3Engine ---

    #[test]
    fn build_engine_qwen3_name() {
        use crate::stt::config::SttConfig;
        let cfg = SttConfig { engine: "qwen3-0.6b".into(), ..SttConfig::default() };
        let e = crate::stt::engine::build_engine(&cfg);
        assert!(e.name().starts_with("qwen3"), "движок qwen3 → name начинается с qwen3");
    }
}
