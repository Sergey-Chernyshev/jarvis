//! Скользящая память разговора. Хранит ТОЛЬКО реплику юзера, ответ ассистента и
//! КОРОТКУЮ структурную сводку действия (skill + санитизированные args + код) —
//! НИКОГДА сырой untrusted-текст (chats.read/документы), чтобы инъекция из одного
//! хода не переезжала во все следующие промпты (см. спеку §Безопасность).

use std::collections::VecDeque;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
struct Turn {
    user: String,
    assistant: String,
    action_result: Option<String>,
}

/// Персист памяти на диске: ходы + момент сохранения (для проверки свежести —
/// контекст «проблемы» не должен протекать в несвязанный разговор спустя сутки).
#[derive(Serialize, Deserialize)]
struct Persisted {
    saved_at_ms: i64,
    turns: Vec<Turn>,
}

/// Сквозной контекст «протух» спустя 6 часов простоя — тогда грузим пустую память.
const STALE_AFTER_MS: i64 = 6 * 60 * 60 * 1000;

/// Свеж ли персист (момент сохранения не старше окна) — чистая, тестируема.
fn is_fresh(saved_at_ms: i64, now_ms: i64) -> bool {
    now_ms - saved_at_ms <= STALE_AFTER_MS && now_ms >= saved_at_ms
}

pub struct Memory {
    turns: VecDeque<Turn>,
    max: usize,
}

impl Memory {
    pub fn new(max_turns: usize) -> Self {
        Self { turns: VecDeque::new(), max: max_turns.max(1) }
    }

    /// Путь персиста сквозного контекста (в каталоге демона: dev/prod раздельно).
    pub fn persisted_path() -> PathBuf {
        crate::util::jarvis_dir().join("convo-memory.json")
    }

    /// Загрузить сквозной контекст из дефолтного пути (последние max ходов, если свеж).
    pub fn load(max_turns: usize) -> Self {
        Self::load_from(&Self::persisted_path(), max_turns, chrono::Utc::now().timestamp_millis())
    }

    /// Загрузка из заданного пути с инъекцией «сейчас» (для тестов). Протухший или
    /// битый персист → пустая память. Хранится ТОЛЬКО санированное (как и в push).
    pub fn load_from(path: &std::path::Path, max_turns: usize, now_ms: i64) -> Self {
        let mut m = Self::new(max_turns);
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(p) = serde_json::from_slice::<Persisted>(&bytes) {
                if is_fresh(p.saved_at_ms, now_ms) {
                    for t in p.turns {
                        m.turns.push_back(t);
                    }
                    while m.turns.len() > m.max {
                        m.turns.pop_front();
                    }
                }
            }
        }
        m
    }

    /// Сохранить текущие ходы в дефолтный путь (best-effort).
    pub fn save(&self) {
        self.save_to(&Self::persisted_path(), chrono::Utc::now().timestamp_millis());
    }

    /// Сохранение в заданный путь с инъекцией «сейчас» (для тестов).
    pub fn save_to(&self, path: &std::path::Path, now_ms: i64) {
        let p = Persisted { saved_at_ms: now_ms, turns: self.turns.iter().cloned().collect() };
        if let Ok(bytes) = serde_json::to_vec(&p) {
            let _ = crate::stt::transcripts::write_private_atomic(path, &bytes);
        }
    }

    /// Добавить ход. `action_result` — КОРОТКАЯ сводка (не сырой контент). Поля
    /// клампим (ответ из Data-пути ограничен лишь краткостью модели — не даём
    /// раздуть последующие промпты / усилить инъекцию; SEC-2/SEC-3).
    pub fn push(&mut self, user: &str, assistant: &str, action_result: Option<&str>) {
        self.turns.push_back(Turn {
            user: crate::util::ellipsize(user, 200),
            assistant: crate::util::ellipsize(assistant, 200),
            action_result: action_result.map(|a| crate::util::ellipsize(a, 120)),
        });
        while self.turns.len() > self.max {
            self.turns.pop_front();
        }
    }

    /// Рендер для промпта (пусто, если ходов нет).
    pub fn render(&self) -> String {
        if self.turns.is_empty() {
            return String::new();
        }
        let mut out = String::from("Контекст разговора (старые→новые):\n");
        for t in &self.turns {
            out.push_str(&format!("Юзер: {}\nДжарвис: {}\n", t.user, t.assistant));
            if let Some(ar) = &t.action_result {
                out.push_str(&format!("(действие: {ar})\n"));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn file_mode(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;

        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn keeps_last_n_turns() {
        let mut m = Memory::new(2);
        m.push("a", "ra", None);
        m.push("b", "rb", None);
        m.push("c", "rc", None);
        let r = m.render();
        assert!(!r.contains("ra"), "старый ход вытеснен");
        assert!(r.contains("rb") && r.contains("rc"));
    }

    #[test]
    fn render_includes_short_action_result() {
        let mut m = Memory::new(4);
        m.push("сколько ждут", "две сессии", Some("sessions_status: 2 waiting"));
        let r = m.render();
        assert!(r.contains("сколько ждут"));
        assert!(r.contains("две сессии"));
        assert!(r.contains("sessions_status"));
    }

    #[test]
    fn empty_render_is_empty() {
        assert_eq!(Memory::new(3).render(), "");
    }

    fn tmp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("jarvis-convo-mem-{}-{}.json", std::process::id(), tag))
    }

    #[test]
    fn persist_round_trip_preserves_recent_turns() {
        let path = tmp_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        let mut m = Memory::new(8);
        m.push("в чём проблема с билдом", "падает линковка", Some("assistant: answer"));
        m.push("а теперь", "проверь флаги", None);
        m.save_to(&path, 1_000_000);

        let loaded = Memory::load_from(&path, 8, 1_000_000 + 1000);
        let r = loaded.render();
        assert!(r.contains("в чём проблема с билдом"));
        assert!(r.contains("проверь флаги"));
        assert!(r.contains("assistant: answer"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn stale_persist_loads_empty() {
        let path = tmp_path("stale");
        let _ = std::fs::remove_file(&path);
        let mut m = Memory::new(8);
        m.push("старый контекст", "ответ", None);
        m.save_to(&path, 0);
        // спустя > 6 часов
        let loaded = Memory::load_from(&path, 8, STALE_AFTER_MS + 1);
        assert_eq!(loaded.render(), "", "протухший контекст не грузится");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_clamps_to_max_keeping_recent() {
        let path = tmp_path("clamp");
        let _ = std::fs::remove_file(&path);
        let mut m = Memory::new(10);
        for i in 0..6 {
            m.push(&format!("ход {i}"), &format!("ответ {i}"), None);
        }
        m.save_to(&path, 100);
        // грузим с окном 2 → остаются последние 2 хода
        let loaded = Memory::load_from(&path, 2, 200);
        let r = loaded.render();
        assert!(r.contains("ответ 5") && r.contains("ответ 4"));
        assert!(!r.contains("ответ 0"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_loads_empty() {
        let loaded = Memory::load_from(std::path::Path::new("/nonexistent/jarvis/x.json"), 4, 100);
        assert_eq!(loaded.render(), "");
    }

    #[test]
    fn is_fresh_window() {
        assert!(is_fresh(100, 100)); // тот же момент
        assert!(is_fresh(0, STALE_AFTER_MS)); // ровно на границе
        assert!(!is_fresh(0, STALE_AFTER_MS + 1)); // за границей
        assert!(!is_fresh(1000, 500)); // часы назад (битое будущее) → не свеж
    }

    #[cfg(unix)]
    #[test]
    fn persisted_memory_replaces_public_file_with_private_complete_json() {
        use std::os::unix::fs::PermissionsExt;

        let path = tmp_path("private-atomic");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, br#"{"saved_at_ms":0,"turns":[]}"#).expect("seed memory");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("public mode");
        let mut memory = Memory::new(4);
        memory.push("секретный вопрос", "приватный ответ", None);

        memory.save_to(&path, 123);

        assert_eq!(
            file_mode(&path),
            0o600,
            "память разговора доступна только владельцу"
        );
        let persisted: Persisted =
            serde_json::from_slice(&std::fs::read(&path).expect("read memory"))
                .expect("complete JSON");
        assert_eq!(persisted.turns.len(), 1);
        assert_eq!(persisted.turns[0].user, "секретный вопрос");
        let _ = std::fs::remove_file(&path);
    }
}
