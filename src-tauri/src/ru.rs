//! Русификация заголовков задач/сессий.
//!
//! Системные сообщения Claude Code — статикой; не-русские строки — ленивым
//! haiku-переводом с кэшем на диске (~/.jarvis/translations.json): каждая
//! уникальная строка стоит один вызов за всю жизнь кэша.
//!
//! Модуль — чистое состояние (кэш + очередь); запуск haiku и доливку переводов
//! в реестр оркестрирует Daemon, чтобы не плодить циклические зависимости.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::util::{ellipsize, jarvis_dir, one_line};

/// Статическая русификация уведомлений Claude Code.
pub fn ru_notification(msg: &str) -> String {
    use regex::RegexBuilder;
    let m = msg.to_string();
    let ci = |p: &str| RegexBuilder::new(p).case_insensitive(true).build().unwrap();
    if ci("waiting for your input").is_match(&m) {
        return "Ждёт твоего ввода".into();
    }
    if let Some(c) = ci(r"needs your permission to use\s+(.+)").captures(&m) {
        return format!("Нужен пермишен: {}", one_line(&c[1]));
    }
    if ci("needs your permission").is_match(&m) {
        return "Нужен пермишен".into();
    }
    m
}

pub fn has_cyrillic(s: &str) -> bool {
    s.chars().any(|c| matches!(c, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё'))
}

pub struct Translator {
    cache: Mutex<HashMap<String, String>>,
    queue: Mutex<HashSet<String>>,
    busy: AtomicBool,
}

fn file() -> std::path::PathBuf {
    jarvis_dir().join("translations.json")
}

impl Translator {
    pub fn load() -> Self {
        let cache = fs::read_to_string(file())
            .ok()
            .and_then(|raw| serde_json::from_str::<HashMap<String, String>>(&raw).ok())
            .unwrap_or_default();
        Self {
            cache: Mutex::new(cache),
            queue: Mutex::new(HashSet::new()),
            busy: AtomicBool::new(false),
        }
    }

    /// Русская версия строки. Не-русское без перевода → в очередь; вернётся
    /// оригинал (перевод догонит реестр позже). `true` — пора качать очередь.
    pub fn ru(&self, text: &str) -> (String, bool) {
        let t = one_line(text);
        if t.is_empty() || has_cyrillic(&t) {
            return (t, false);
        }
        if let Some(tr) = self.cache.lock().unwrap().get(&t) {
            return (tr.clone(), false);
        }
        self.queue.lock().unwrap().insert(t.clone());
        (t, true)
    }

    pub fn lookup(&self, text: &str) -> Option<String> {
        self.cache.lock().unwrap().get(text).cloned()
    }

    /// Захватить пачку на перевод (≤6 строк) — или None, если перевод уже идёт.
    pub fn take_batch(&self) -> Option<Vec<String>> {
        if self.busy.swap(true, Ordering::SeqCst) {
            return None;
        }
        let mut queue = self.queue.lock().unwrap();
        if queue.is_empty() {
            self.busy.store(false, Ordering::SeqCst);
            return None;
        }
        let batch: Vec<String> = queue.iter().take(6).cloned().collect();
        for t in &batch {
            queue.remove(t);
        }
        Some(batch)
    }

    /// Готовые переводы из ответа haiku: пронумерованные строки по одной на исходную.
    pub fn finish_batch(&self, batch: &[String], output: Option<&str>) -> bool {
        self.busy.store(false, Ordering::SeqCst);
        let Some(out) = output else { return false };
        let re = regex::Regex::new(r"^\s*\d+[.)]\s*").unwrap();
        let lines: Vec<String> = out
            .lines()
            .map(|l| re.replace(l, "").trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        let mut changed = false;
        {
            let mut cache = self.cache.lock().unwrap();
            for (src, line) in batch.iter().zip(lines.iter()) {
                if has_cyrillic(line) {
                    cache.insert(src.clone(), ellipsize(&one_line(line), 120));
                    changed = true;
                }
            }
            if changed {
                let _ = fs::create_dir_all(jarvis_dir());
                let _ = fs::write(file(), serde_json::to_string_pretty(&*cache).unwrap_or_default());
            }
        }
        changed
    }

    pub fn queue_len(&self) -> usize {
        self.queue.lock().unwrap().len()
    }

    /// Промпт перевода — дословно из Electron-версии.
    pub fn prompt_for(batch: &[String]) -> String {
        let numbered = batch
            .iter()
            .enumerate()
            .map(|(i, t)| format!("{}. {}", i + 1, t))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Переведи строки на русский. Это заголовки задач разработки: технические термины, имена файлов и команд не переводи. Ответь только пронумерованными переводами, по одному на строку.\n\n{numbered}"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_mapping() {
        assert_eq!(ru_notification("Claude is waiting for your input"), "Ждёт твоего ввода");
        assert_eq!(
            ru_notification("Claude needs your permission to use Bash"),
            "Нужен пермишен: Bash"
        );
        assert_eq!(ru_notification("что-то своё"), "что-то своё");
    }

    #[test]
    fn cyrillic_detection() {
        assert!(has_cyrillic("привет"));
        assert!(has_cyrillic("Ёлки"));
        assert!(!has_cyrillic("hello fix tests"));
    }
}
