//! Скользящая память разговора. Хранит ТОЛЬКО реплику юзера, ответ ассистента и
//! КОРОТКУЮ структурную сводку действия (skill + санитизированные args + код) —
//! НИКОГДА сырой untrusted-текст (chats.read/документы), чтобы инъекция из одного
//! хода не переезжала во все следующие промпты (см. спеку §Безопасность).

use std::collections::VecDeque;

struct Turn {
    user: String,
    assistant: String,
    action_result: Option<String>,
}

pub struct Memory {
    turns: VecDeque<Turn>,
    max: usize,
}

impl Memory {
    pub fn new(max_turns: usize) -> Self {
        Self { turns: VecDeque::new(), max: max_turns.max(1) }
    }

    /// Добавить ход. `action_result` — КОРОТКАЯ сводка (не сырой контент).
    pub fn push(&mut self, user: &str, assistant: &str, action_result: Option<&str>) {
        self.turns.push_back(Turn {
            user: user.to_string(),
            assistant: assistant.to_string(),
            action_result: action_result.map(str::to_string),
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
}
