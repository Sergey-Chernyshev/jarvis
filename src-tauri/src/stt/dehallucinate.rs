//! Anti-hallucination фильтр для STT.
//!
//! Whisper (и в меньшей степени Qwen3-ASR) на ТИШИНЕ/ШУМЕ/МУЗЫКЕ сваливаются в
//! языковую модель и выдают частые фразы из обучающих данных — субтитры ютуба,
//! «Продолжение следует», «Thanks for watching» и т.п. Это не речь пользователя.
//!
//! Чистые функции (без зависимостей) — общий блоклист для обоих движков:
//!  - `is_hallucination(seg)` — посегментный дроп в whisper-движке;
//!  - `scrub(text)` — пост-чистка финального текста (whisper + qwen + safety net).
//!
//! Консервативно: дропаем только ТОЧНОЕ совпадение нормализованной фразы с
//! блоклистом или bracket-маркеры тишины — чтобы НИКОГДА не выкинуть живую речь.
//! Основную работу делает VAD-гейт перед STT; это — сетка безопасности.

/// Нормализация для сравнения: lower, без краевой пунктуации/кавычек, схлопнутые
/// пробелы. «Продолжение следует…» и « Продолжение, следует. » → одно и то же.
fn norm(s: &str) -> String {
    let lowered = s.trim().to_lowercase();
    let trimmed = lowered.trim_matches(|c: char| {
        c.is_whitespace() || ".,!?…\"'«»()[]{}-—–:;".contains(c)
    });
    trimmed.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Известные галлюцинации (УЖЕ нормализованные). Сегмент — галлюцинация, если его
/// нормализованный текст ТОЧНО равен одной из этих фраз.
const BLOCKLIST: &[&str] = &[
    // — RU (субтитры/концовки роликов) —
    "продолжение следует",
    "субтитры сделал dimatorzok",
    "субтитры создавал dimatorzok",
    "субтитры делал dimatorzok",
    "редактор субтитров а.синецкая",
    "корректор а.кулакова",
    "спасибо за просмотр",
    "спасибо за внимание",
    "подписывайтесь на канал",
    "ставьте лайки и подписывайтесь",
    "до новых встреч",
    "всем пока",
    // — EN (Whisper youtube-subtitle priors) —
    "thanks for watching",
    "thank you for watching",
    "thank you",
    "please subscribe",
    "subscribe to my channel",
    "like and subscribe",
    "see you next time",
    "bye",
];

/// bracket-маркеры тишины/звука, которые модель иногда печатает дословно.
const BRACKET_MARKERS: &[&str] = &[
    "[blank_audio]",
    "[ silence ]",
    "[silence]",
    "[ pause ]",
    "(silence)",
    "(music)",
    "(музыка)",
    "[музыка]",
    "[тишина]",
    "[ инструментальная музыка ]",
    "[аплодисменты]",
    "(applause)",
];

/// Один фрагмент (сегмент/предложение) — галлюцинация? Пусто → НЕ галлюцинация
/// (пустое отсекут вызывающие отдельно). Только точное совпадение — без догадок.
pub fn is_hallucination(s: &str) -> bool {
    let raw = s.trim().to_lowercase();
    if raw.is_empty() {
        return false;
    }
    if BRACKET_MARKERS.contains(&raw.as_str()) {
        return true;
    }
    BLOCKLIST.contains(&norm(s).as_str())
}

/// Разбить текст на «предложения» по `.!?…` и переводам строк (для пост-чистки).
fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        cur.push(ch);
        if matches!(ch, '.' | '!' | '?' | '…' | '\n') {
            let t = cur.trim();
            if !t.is_empty() {
                out.push(t.to_string());
            }
            cur.clear();
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        out.push(t.to_string());
    }
    out
}

/// Вычистить галлюцинации из финального текста (общая сетка для whisper и qwen).
/// Если НИЧЕГО не выкинули — возвращаем оригинал как есть (сохраняем форматирование).
/// Если весь текст оказался галлюцинацией — пустая строка.
pub fn scrub(text: &str) -> String {
    let sentences = split_sentences(text);
    if sentences.is_empty() {
        return String::new();
    }
    let kept: Vec<&str> = sentences
        .iter()
        .filter(|s| !is_hallucination(s))
        .map(String::as_str)
        .collect();
    if kept.len() == sentences.len() {
        return text.trim().to_string(); // чистый текст — без изменений
    }
    kept.join(" ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocklisted_phrases_are_hallucinations() {
        assert!(is_hallucination("Продолжение следует."));
        assert!(is_hallucination("  спасибо за просмотр  "));
        assert!(is_hallucination("Thanks for watching"));
        assert!(is_hallucination("[BLANK_AUDIO]"));
        assert!(is_hallucination("(музыка)"));
        assert!(is_hallucination("Субтитры сделал DimaTorzok"));
    }

    #[test]
    fn real_speech_is_not_a_hallucination() {
        assert!(!is_hallucination("привет как дела"));
        assert!(!is_hallucination("добавь сервис нетифайер"));
        assert!(!is_hallucination("спасибо тебе огромное за помощь с кодом")); // не точный матч
        assert!(!is_hallucination(""));
    }

    #[test]
    fn scrub_drops_trailing_hallucination_sentence() {
        assert_eq!(
            scrub("Реальный текст диктовки. Спасибо за просмотр."),
            "Реальный текст диктовки."
        );
    }

    #[test]
    fn scrub_whole_text_hallucination_to_empty() {
        assert_eq!(scrub("Продолжение следует"), "");
        assert_eq!(scrub("[BLANK_AUDIO]"), "");
    }

    #[test]
    fn scrub_clean_text_unchanged() {
        let t = "Просто обычный текст без проблем.";
        assert_eq!(scrub(t), t);
        assert_eq!(scrub(""), "");
    }
}
