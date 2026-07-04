//! Сегментация транскрипта на «ходы» (юзер-текст → всё до следующего юзер-текста)
//! и детерминированные факты хода (файлы, команды) для карточек сводки.
//! Принцип extract-then-abstract: пути/команды достаёт ЭТОТ код, LLM только
//! аннотирует — см. docs/superpowers/specs/2026-07-03-chat-turn-summaries-design.md.

use serde::{Deserialize, Serialize};

use crate::transcript::ChatItem;

/// Диапазон одного хода в плоском списке элементов чата.
/// `key` — ts юзер-реплики (мс, строкой); "pre" — частичный головной ход,
/// у которого юзер-реплика обрезана окном чтения (такой не суммаризируем).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSpan {
    pub key: String,
    pub start: usize,
    pub end: usize, // эксклюзивно
    pub complete: bool,
}

/// Границы ходов по плоскому списку элементов: граница — юзер-текст
/// (tool_result-записи Claude в ленту не попадают — см. to_chat_items).
pub fn spans(items: &[ChatItem]) -> Vec<TurnSpan> {
    let mut out: Vec<TurnSpan> = Vec::new();
    for (i, it) in items.iter().enumerate() {
        if it.role == "user" && it.kind == "text" {
            if let Some(last) = out.last_mut() {
                last.end = i;
            }
            out.push(TurnSpan { key: it.ts.to_string(), start: i, end: i, complete: true });
        } else if out.is_empty() {
            out.push(TurnSpan { key: "pre".into(), start: 0, end: 0, complete: false });
        }
    }
    if let Some(last) = out.last_mut() {
        last.end = items.len();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(role: &'static str, kind: &'static str, text: &str, ts: i64) -> ChatItem {
        ChatItem { role, kind, text: text.into(), ts }
    }

    #[test]
    fn spans_split_on_user_text() {
        let items = vec![
            it("user", "text", "сделай A", 100),
            it("assistant", "tool", "Edit · a.rs", 101),
            it("assistant", "text", "готово A", 102),
            it("user", "text", "теперь B", 200),
            it("assistant", "text", "готово B", 201),
        ];
        let s = spans(&items);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].key.as_str(), s[0].start, s[0].end, s[0].complete), ("100", 0, 3, true));
        assert_eq!((s[1].key.as_str(), s[1].start, s[1].end, s[1].complete), ("200", 3, 5, true));
    }

    #[test]
    fn spans_head_without_user_is_partial() {
        let items = vec![
            it("assistant", "tool", "Bash · ls", 50),
            it("assistant", "text", "хвост прошлого хода", 51),
            it("user", "text", "новый ход", 100),
            it("assistant", "text", "ок", 101),
        ];
        let s = spans(&items);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].key.as_str(), s[0].start, s[0].end, s[0].complete), ("pre", 0, 2, false));
        assert!(s[1].complete);
    }

    #[test]
    fn spans_empty_input() {
        assert!(spans(&[]).is_empty());
    }
}
