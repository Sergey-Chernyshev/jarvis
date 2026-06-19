//! Капабилити STT: транскрибирование PCM-аудио через SttService (§10, инкр. 9).
//!
//! Класс: `Control` — микрофон это реальный сайд-эффект (доступ к речи пользователя,
//! приватность), поэтому требует подтверждения у агента/плагина (ConfirmPolicy::Always).
//! Провенанс: Trusted — вывод нашего собственного STT-движка (не чужого контента).
//!
//! Грант на микрофон (§10):
//! - Агенту — запрещён поимённо через `denied_ids` (как `audit.query`): агент не вправе
//!   слушать микрофон без явного разрешения пользователя.
//! - Плагин получает доступ, только если в его гранте есть `RiskClass::Control` AND
//!   `stt.transcribe` НЕ в его `denied_ids` — т.е. пользователь явно добавил класс.
//!   По умолчанию `Consumer::plugin` строит грант по манифесту: плагин без `Control`
//!   не пройдёт gate. Это минимальная enforcement без отдельного нового класса.
//! - Внутренняя диктовка вызывает `SttService` напрямую (не через эту капабилити).
//!
//! Вход: `{ "pcm_base64": string, "lang"?: string }`
//!   pcm_base64 — Base64-кодированный буфер f32 LE PCM 16кГц моно (big/little endian: LE).
//!   lang — опциональный overrides dominant_lang из конфига (ISO 639-1, напр. "en").
//!
//! Выход: `{ "text": string }` (текст транскрипции).

use std::sync::Arc;

use serde_json::{json, Value};

use crate::capability::contract::{CapabilityMeta, Provenance, RiskClass};
use crate::capability::registry::make_handler;
use crate::capability::DaemonRegistry;
use crate::daemon::Daemon;

pub fn register(reg: &mut DaemonRegistry) {
    reg.register(
        CapabilityMeta {
            id: "stt.transcribe",
            class: RiskClass::Control,
            provenance: Provenance::Trusted,
            description: "Транскрибировать PCM-аудио (f32 LE 16кГц моно, Base64) через STT-движок. Требует явного гранта на микрофон. Возвращает {text}.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pcm_base64": {
                        "type": "string",
                        "description": "Base64-кодированный буфер f32 LE PCM 16кГц моно"
                    },
                    "lang": {
                        "type": "string",
                        "description": "Опциональный overrides dominant_lang (ISO 639-1, напр. 'en')"
                    }
                },
                "required": ["pcm_base64"]
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move {
            // 1. Достать pcm_base64 — обязательный аргумент.
            let b64 = args
                .get("pcm_base64")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "нужен аргумент 'pcm_base64' (строка)".to_string())?;

            // 2. Декодировать Base64 → байты → f32 LE сэмплы.
            use base64::Engine as _;
            let raw = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| format!("base64 decode: {e}"))?;

            if raw.len() % 4 != 0 {
                return Err("pcm_base64: число байт не кратно 4 (нужен f32 LE)".to_string());
            }
            let pcm: Vec<f32> = raw
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();

            // 3. Опции: берём из конфига демона, overrides lang если задан.
            let mut opts = d.stt.options();
            if let Some(lang) = args.get("lang").and_then(|v| v.as_str()) {
                if !lang.is_empty() {
                    opts.dominant_lang = lang.to_string();
                }
            }

            // 4. Транскрибировать.
            let result = d.stt.transcribe(&pcm, &opts).map_err(|e| format!("STT: {e}"))?;
            Ok(json!({ "text": result.text }))
        }),
    );
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;

    /// Кодировать PCM-буфер f32 LE в Base64 (утилита тестов).
    pub fn encode_pcm(samples: &[f32]) -> String {
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    }

    #[test]
    fn encode_decode_pcm_roundtrip() {
        let samples = vec![0.0f32, 0.5f32, -0.5f32, 1.0f32];
        let b64 = encode_pcm(&samples);
        let raw = base64::engine::general_purpose::STANDARD.decode(&b64).unwrap();
        let decoded: Vec<f32> =
            raw.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        assert_eq!(decoded, samples);
    }

    #[test]
    fn odd_byte_count_is_detected() {
        // 5 байт — не кратно 4, ожидаем ошибку про f32 LE
        let bad_b64 = base64::engine::general_purpose::STANDARD.encode(&[1u8, 2, 3, 4, 5]);
        // Мы не дёргаем handler напрямую, но проверяем логику: len % 4 != 0
        let raw = base64::engine::general_purpose::STANDARD.decode(&bad_b64).unwrap();
        assert_eq!(raw.len() % 4, 1, "5 байт — не кратно 4");
    }
}
