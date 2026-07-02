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
    // — затравки initial_prompt (см. engine_whisper::punctuation_seed): если
    // декодер «протёк» затравкой на тишине — это не речь пользователя.
    // Нормализация сохраняет ВНУТРЕННЮЮ пунктуацию; протечка бывает и целиком,
    // и по предложениям — покрываем оба варианта —
    "диктовка началась, говорю обычным тоном. знаки препинания — запятые, точки, вопросы — расставляем правильно, верно",
    "диктовка началась, говорю обычным тоном",
    "знаки препинания — запятые, точки, вопросы — расставляем правильно, верно",
    "the dictation has started, i am speaking normally. punctuation marks — commas, periods, questions — are placed correctly, right",
    "the dictation has started, i am speaking normally",
    "punctuation marks — commas, periods, questions — are placed correctly, right",
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

/// Схлопнуть дегенеративные петли декодера: whisper (жадный best_of:1) иногда
/// зацикливается и печатает одно «предложение» подряд много раз («Писать. Писать.
/// Писать…», «Минт, витамин. Минт, витамин.») — это не речь. Встроенный детектор
/// повторов whisper.cpp (`entropy_thold`) заперт за `result_len > 32` токенов и
/// короткие петли пропускает, а блоклист их не ловит (фразы осмысленные) — поэтому
/// чистим тут. Консервативно: схлопываем ТОЛЬКО прогон одинаковых (нормализованных)
/// подряд длиной ≥3 до одного вхождения; пары (эмфатическое «Нет. Нет.») не трогаем.
pub fn collapse_repeats(text: &str) -> String {
    const MIN_RUN: usize = 3;
    let sentences = split_sentences(text);
    if sentences.is_empty() {
        return String::new();
    }
    let mut kept: Vec<&str> = Vec::with_capacity(sentences.len());
    let mut i = 0;
    while i < sentences.len() {
        // длина прогона одинаковых по норме подряд
        let mut j = i + 1;
        while j < sentences.len() && norm(&sentences[j]) == norm(&sentences[i]) {
            j += 1;
        }
        let run = j - i;
        // прогон ≥ MIN_RUN → оставить одно вхождение; иначе оставить все
        let take = if run >= MIN_RUN { 1 } else { run };
        for s in &sentences[i..i + take] {
            kept.push(s.as_str());
        }
        i = j;
    }
    if kept.len() == sentences.len() {
        return text.trim().to_string(); // ничего не схлопнули — формат сохранён
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

    // --- collapse_repeats: дегенеративные петли декодера Whisper ---

    #[test]
    fn collapse_run_of_identical_sentences() {
        // Классическая петля жадного декодера: одно «предложение» подряд ≥3 раз.
        assert_eq!(collapse_repeats("Писать. Писать. Писать. Писать."), "Писать.");
    }

    #[test]
    fn collapse_keeps_unique_prefix_then_collapses_loop() {
        assert_eq!(
            collapse_repeats("Минт, витамин, кафе. Минт, витамин. Минт, витамин. Минт, витамин."),
            "Минт, витамин, кафе. Минт, витамин."
        );
    }

    #[test]
    fn collapse_leaves_double_repeat_alone() {
        // Пара повторов — законный эмфатический приём, НЕ трогаем (консервативно).
        let t = "Нет. Нет.";
        assert_eq!(collapse_repeats(t), t);
    }

    #[test]
    fn collapse_clean_text_unchanged() {
        let t = "Обычная фраза диктовки без повторов.";
        assert_eq!(collapse_repeats(t), t);
        assert_eq!(collapse_repeats(""), "");
    }
}
