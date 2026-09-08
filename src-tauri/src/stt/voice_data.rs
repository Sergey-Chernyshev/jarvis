//! Private, atomic storage for the voice dictionary and scratch draft.
//! Dictionary entries are explicit text substitutions after recognition; they
//! apply to every STT engine and never cascade into another replacement.
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static STORAGE: Mutex<()> = Mutex::new(());
const MAX_WORDS: usize = 500;
const MAX_DRAFT: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Word {
    pub word: String,
    pub replacement: String,
}
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Dictionary {
    pub words: Vec<Word>,
}
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Scratch {
    pub text: String,
}
#[derive(Serialize)]
pub struct ScratchSaved {
    pub ok: bool,
    pub text: String,
}

fn path(name: &str) -> PathBuf {
    crate::util::jarvis_dir().join(name)
}
fn read<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            format!(
                "Не удалось прочитать {}: {e}. Файл оставлен без изменений.",
                path.display()
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(format!("Не удалось прочитать {}: {e}", path.display())),
    }
}
fn save<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Не удалось создать каталог: {e}"))?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    super::transcripts::write_private_atomic(path, &bytes)
        .map_err(|e| format!("Не удалось сохранить {}: {e}", path.display()))
}
fn valid_word(word: &str, replacement: &str) -> Result<Word, String> {
    let word = word.trim();
    let replacement = replacement.trim();
    if word.is_empty() || replacement.is_empty() {
        return Err("Заполни распознанную фразу и замену".into());
    }
    if word.chars().count() > 128 || replacement.chars().count() > 512 {
        return Err("Фраза: до 128 символов; замена: до 512".into());
    }
    if word.chars().any(char::is_control) || replacement.chars().any(char::is_control) {
        return Err("Фраза и замена должны быть в одной строке".into());
    }
    Ok(Word {
        word: word.into(),
        replacement: replacement.into(),
    })
}
fn add_at(path: &Path, word: &str, replacement: &str) -> Result<Dictionary, String> {
    let entry = valid_word(word, replacement)?;
    let mut dictionary: Dictionary = read(path)?;
    if let Some(old) = dictionary
        .words
        .iter_mut()
        .find(|old| old.word.to_lowercase() == entry.word.to_lowercase())
    {
        *old = entry;
    } else {
        if dictionary.words.len() >= MAX_WORDS {
            return Err(format!("В словаре может быть не более {MAX_WORDS} замен"));
        }
        dictionary.words.push(entry);
    }
    save(path, &dictionary)?;
    Ok(dictionary)
}
fn remove_at(path: &Path, word: &str) -> Result<Dictionary, String> {
    let mut dictionary: Dictionary = read(path)?;
    dictionary
        .words
        .retain(|entry| entry.word.to_lowercase() != word.trim().to_lowercase());
    save(path, &dictionary)?;
    Ok(dictionary)
}
fn scratch_at(path: &Path, text: String) -> Result<ScratchSaved, String> {
    if text.len() > MAX_DRAFT {
        return Err("Черновик превышает 2 МБ. Сохрани часть текста в отдельный файл.".into());
    }
    save(path, &Scratch { text: text.clone() })?;
    Ok(ScratchSaved { ok: true, text })
}
fn locked<T>(action: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let _guard = STORAGE.lock().map_err(|_| "Хранилище голоса недоступно")?;
    action()
}
async fn storage_task<T: Send + 'static>(
    action: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    // File sync and serialization must never block the application UI thread.
    tauri::async_runtime::spawn_blocking(move || locked(action))
        .await
        .map_err(|e| format!("Хранилище голоса: {e}"))?
}
#[tauri::command]
pub async fn dictionary_get() -> Result<Dictionary, String> {
    storage_task(|| read(&path("voice-dictionary.json"))).await
}
#[tauri::command]
pub async fn dictionary_add(word: String, replacement: String) -> Result<Dictionary, String> {
    storage_task(move || add_at(&path("voice-dictionary.json"), &word, &replacement)).await
}
#[tauri::command]
pub async fn dictionary_remove(word: String) -> Result<Dictionary, String> {
    storage_task(move || remove_at(&path("voice-dictionary.json"), &word)).await
}
#[tauri::command]
pub async fn scratchpad_get() -> Result<Scratch, String> {
    storage_task(|| read(&path("voice-scratch.json"))).await
}
#[tauri::command]
pub async fn scratchpad_set(text: String) -> Result<ScratchSaved, String> {
    storage_task(move || scratch_at(&path("voice-scratch.json"), text)).await
}
fn word_character(character: char) -> bool {
    static WORD: OnceLock<Regex> = OnceLock::new();
    WORD.get_or_init(|| Regex::new(r"^\w$").expect("constant regex"))
        .is_match(&character.to_string())
}
/// Exact Unicode-aware whole words/phrases, ignoring case, with replacement
/// capitalization retained verbatim. Prefer the longest match at each offset.
pub fn replace_words(text: &str, words: &[Word]) -> String {
    let mut matches = Vec::new();
    for entry in words.iter().take(MAX_WORDS) {
        if valid_word(&entry.word, &entry.replacement).is_err() {
            continue;
        }
        let Ok(pattern) = RegexBuilder::new(&regex::escape(&entry.word))
            .case_insensitive(true)
            .build()
        else {
            continue;
        };
        for found in pattern.find_iter(text) {
            let before = text[..found.start()].chars().next_back();
            let after = text[found.end()..].chars().next();
            if before.is_some_and(word_character) || after.is_some_and(word_character) {
                continue;
            }
            matches.push((found.start(), found.end(), entry.replacement.as_str()));
        }
    }
    matches.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    let mut output = String::with_capacity(text.len());
    let mut offset = 0;
    for (start, end, replacement) in matches {
        if start < offset {
            continue;
        }
        output.push_str(&text[offset..start]);
        output.push_str(replacement);
        offset = end;
    }
    output.push_str(&text[offset..]);
    output
}
/// A broken dictionary must not discard a successful transcription. Its read
/// error remains visible in the dictionary UI for repair.
pub fn apply_dictionary_result(result: super::engine::SttResult) -> super::engine::SttResult {
    // Tests never read the developer's real dictionary. Explicit rules below
    // exercise the same transformation in isolation.
    #[cfg(not(test))]
    if let Ok(dictionary) = locked(|| read::<Dictionary>(&path("voice-dictionary.json"))) {
        return apply_result_rules(result, &dictionary.words);
    }
    result
}

fn apply_result_rules(
    mut result: super::engine::SttResult,
    words: &[Word],
) -> super::engine::SttResult {
    let corrected = replace_words(&result.text, words);
    let mut changed = corrected != result.text;
    result.text = corrected;
    for segment in &mut result.segments {
        let corrected = replace_words(&segment.text, words);
        changed |= corrected != segment.text;
        segment.text = corrected;
    }
    // A phrase can cross segment boundaries. Segments have only optional
    // language labels, no timing; omit contradictory segmentation in this
    // case instead of returning old words beside the corrected full text.
    if changed && !result.segments.is_empty() {
        let joined = result
            .segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let normalized = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized(&joined) != normalized(&result.text) {
            result.segments.clear();
        }
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    fn word(a: &str, b: &str) -> Word {
        Word {
            word: a.into(),
            replacement: b.into(),
        }
    }
    fn temporary(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("jarvis-voice-data-{}-{name}", std::process::id()))
    }
    #[test]
    fn replacements_are_whole_unicode_words_and_keep_explicit_case() {
        let words = [
            word("джарвис", "Jarvis"),
            word("код", "code"),
            word("таури", "Tauri"),
        ];
        assert_eq!(
            replace_words("ДЖАРВИС, таури! код кодер подкод _код", &words),
            "Jarvis, Tauri! code кодер подкод _код"
        );
        assert_eq!(replace_words("код\u{301} код", &words), "код\u{301} code");
        assert_eq!(
            replace_words("C++ и C++17", &[word("C++", "cpp")]),
            "cpp и C++17"
        );
    }
    #[test]
    fn phrases_prefer_longest_and_do_not_cascade() {
        let words = [
            word("alpha", "beta"),
            word("alpha beta", "project"),
            word("beta", "gamma"),
        ];
        assert_eq!(
            replace_words("alpha beta; alpha; beta", &words),
            "project; beta; gamma"
        );
        assert_eq!(replace_words("unchanged", &[]), "unchanged");
        assert_eq!(
            replace_words("Tauri. (tauri)", &[word("tauri", "ТАУРИ")]),
            "ТАУРИ. (ТАУРИ)"
        );
    }

    #[test]
    fn common_result_applies_one_dictionary_to_text_and_segments() {
        let input = super::super::engine::SttResult {
            text: "таури работает".into(),
            segments: vec![super::super::engine::SttSeg {
                text: "таури работает".into(),
                lang: Some("ru".into()),
            }],
        };
        let output = apply_result_rules(input, &[word("таури", "Tauri")]);
        assert_eq!(output.text, "Tauri работает");
        assert_eq!(output.segments[0].text, output.text);
        assert_eq!(output.segments[0].lang.as_deref(), Some("ru"));
    }

    #[test]
    fn phrase_across_segments_never_returns_conflicting_segment_text() {
        let input = super::super::engine::SttResult {
            text: "тайп скрипт".into(),
            segments: vec![
                super::super::engine::SttSeg {
                    text: "тайп".into(),
                    lang: Some("ru".into()),
                },
                super::super::engine::SttSeg {
                    text: "скрипт".into(),
                    lang: Some("ru".into()),
                },
            ],
        };
        let output = apply_result_rules(input, &[word("тайп скрипт", "TypeScript")]);
        assert_eq!(output.text, "TypeScript");
        assert!(output.segments.is_empty());
    }
    #[test]
    fn dictionary_roundtrip_upsert_remove_and_private_permissions() {
        let p = temporary("dictionary.json");
        let _ = std::fs::remove_file(&p);
        add_at(&p, "таури", "Tauri").unwrap();
        add_at(&p, "ТАУРИ", "TAURI").unwrap();
        let saved: Dictionary = read(&p).unwrap();
        assert_eq!(saved.words.len(), 1);
        assert_eq!(saved.words[0].replacement, "TAURI");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(remove_at(&p, "таури").unwrap().words.is_empty());
        let _ = std::fs::remove_file(p);
    }
    #[test]
    fn malformed_dictionary_is_not_overwritten_by_edits() {
        let p = temporary("corrupt.json");
        std::fs::write(&p, "corrupt").unwrap();
        assert!(add_at(&p, "a", "b").is_err());
        assert!(remove_at(&p, "a").is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "corrupt");
        let _ = std::fs::remove_file(p);
    }
    #[test]
    fn scratch_roundtrip_and_write_failures_are_explicit() {
        let p = temporary("scratch.json");
        let text = "строка\nsecond line";
        assert!(scratch_at(&p, text.into()).unwrap().ok);
        assert_eq!(read::<Scratch>(&p).unwrap().text, text);
        assert!(scratch_at(&p, "x".repeat(MAX_DRAFT + 1)).is_err());
        assert_eq!(read::<Scratch>(&p).unwrap().text, text);
        let blocker = temporary("blocker");
        std::fs::write(&blocker, "file").unwrap();
        assert!(scratch_at(&blocker.join("draft.json"), "draft".into()).is_err());
        assert!(add_at(&blocker.join("dict.json"), "a", "b").is_err());
        assert!(valid_word(" ", "x").is_err());
        assert!(valid_word("a", "").is_err());
        let _ = std::fs::remove_file(p);
        let _ = std::fs::remove_file(blocker);
    }
}
