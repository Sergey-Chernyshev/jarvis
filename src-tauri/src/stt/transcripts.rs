//! Кольцевая история диктовки/реплик: последние N распознанных фраз с временем
//! и источником. Для UI «что я говорил» + копирование. In-memory (живёт, пока
//! жив демон) — не пишем на диск, чтобы голос не оставлял следов без спроса.

use std::collections::VecDeque;
use std::sync::Mutex;

/// Максимум хранимых реплик (старые вытесняются).
const CAP: usize = 100;

/// Одна распознанная реплика.
#[derive(Clone, serde::Serialize)]
pub struct Transcript {
    /// Распознанный текст.
    pub text: String,
    /// Unix-время (секунды) распознавания.
    pub ts: u64,
    /// Источник: "dictation" (F8) | "wake" (Hey Jarvis).
    pub source: String,
}

/// Потокобезопасный кольцевой буфер реплик. Новые — в начало (front).
#[derive(Default)]
pub struct Transcripts {
    items: Mutex<VecDeque<Transcript>>,
}

impl Transcripts {
    pub fn new() -> Self {
        Transcripts { items: Mutex::new(VecDeque::new()) }
    }

    /// Добавить реплику (с текущим временем). Пустой текст игнорируется.
    pub fn push(&self, text: &str, source: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut g = match self.items.lock() {
            Ok(g) => g,
            Err(_) => return, // отравленный лок — fail-safe, не паникуем
        };
        g.push_front(Transcript { text: text.to_string(), ts, source: source.to_string() });
        while g.len() > CAP {
            g.pop_back();
        }
    }

    /// Все реплики (новые первыми) — для UI.
    pub fn list(&self) -> Vec<Transcript> {
        self.items.lock().map(|g| g.iter().cloned().collect()).unwrap_or_default()
    }

    /// Очистить историю.
    pub fn clear(&self) {
        if let Ok(mut g) = self.items.lock() {
            g.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_then_list_newest_first() {
        let t = Transcripts::new();
        t.push("привет", "dictation");
        t.push("мир", "wake");
        let l = t.list();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].text, "мир"); // новые первыми
        assert_eq!(l[0].source, "wake");
        assert_eq!(l[1].text, "привет");
    }

    #[test]
    fn empty_text_ignored() {
        let t = Transcripts::new();
        t.push("   ", "dictation");
        t.push("", "dictation");
        assert!(t.list().is_empty());
    }

    #[test]
    fn cap_evicts_oldest() {
        let t = Transcripts::new();
        for i in 0..(CAP + 20) {
            t.push(&format!("реплика {i}"), "dictation");
        }
        let l = t.list();
        assert_eq!(l.len(), CAP, "не превышаем CAP");
        assert_eq!(l[0].text, format!("реплика {}", CAP + 19), "новейшая — первой");
    }

    #[test]
    fn clear_empties() {
        let t = Transcripts::new();
        t.push("x", "dictation");
        t.clear();
        assert!(t.list().is_empty());
    }
}
