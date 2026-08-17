//! Кольцевой буфер событий — вся память узла между визитами ноута.
//!
//! Ноут спит и закрывается, а агент на VPS в это время работает: событие,
//! которое некому забрать прямо сейчас, должно дождаться. Курсор монотонный и
//! НЕ переиспользуется даже после вытеснения — только по нему ноут понимает,
//! что именно он пропустил.
//!
//! Переполнение не заминаем: отдаём `gap`, чтобы ноут перечитал транскрипты
//! целиком, а не собрал дырявую картину и не сделал вид, что ничего не терял
//! (дизайн 2026-08-05, §«Протокол узла»).

use serde_json::{json, Value};
use std::collections::VecDeque;

/// Событие в буфере: конверт от jarvis-hook как есть + метка приёма узлом.
#[derive(Debug, Clone, PartialEq)]
pub struct Recorded {
    pub cursor: u64,
    /// Когда узел ПРИНЯЛ событие (мс эпохи). Часы VPS и ноута расходятся,
    /// поэтому метка вспомогательная — порядок задаёт курсор, а не время.
    pub at: i64,
    pub envelope: Value,
}

impl Recorded {
    /// Конверт вкладываем, а не расплющиваем: у него свой ключ `event`,
    /// который иначе столкнулся бы с полями обёртки.
    pub fn to_json(&self) -> Value {
        json!({ "cursor": self.cursor, "at": self.at, "envelope": self.envelope })
    }
}

/// Ответ на /events: либо кусок ленты, либо честное «я потерял начало».
#[derive(Debug, PartialEq)]
pub enum Slice {
    Gap { cursor: u64 },
    Events { cursor: u64, events: Vec<Recorded> },
}

/// Согласованный слепок кольца для /hello — снимается под одним взятием
/// мьютекса, иначе числа в ответе противоречили бы друг другу.
#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    pub cursor: u64,
    pub buffered: usize,
    pub oldest: Option<u64>,
    pub capacity: usize,
}

pub struct Ring {
    capacity: usize,
    items: VecDeque<Recorded>,
    /// Курсор, который получит СЛЕДУЮЩЕЕ событие; он же — «спрашивай с него»
    /// в ответе /events. При вытеснении не сдвигается: монотонность и есть
    /// весь контракт курсора.
    next: u64,
}

impl Ring {
    pub fn new(capacity: usize) -> Ring {
        Ring {
            // ноль ёмкости означал бы «всё теряем молча» — худший из возможных
            // режимов, поэтому минимум одно событие держим всегда
            capacity: capacity.max(1),
            items: VecDeque::new(),
            next: 0,
        }
    }

    pub fn push(&mut self, envelope: Value, at: i64) -> u64 {
        let cursor = self.next;
        self.next += 1;
        self.items.push_back(Recorded { cursor, at, envelope });
        while self.items.len() > self.capacity {
            self.items.pop_front();
        }
        cursor
    }

    pub fn stats(&self) -> Stats {
        Stats {
            cursor: self.next,
            buffered: self.items.len(),
            oldest: self.items.front().map(|e| e.cursor),
            capacity: self.capacity,
        }
    }

    /// События с курсора `since` включительно.
    pub fn since(&self, since: u64) -> Slice {
        // Курсор из будущего = узел перезапускался и начал ленту заново.
        // Промолчать нельзя: ноут решил бы, что «ничего не происходило».
        if since > self.next {
            return Slice::Gap { cursor: self.next };
        }
        match self.items.front().map(|e| e.cursor) {
            // начало запрошенного отрезка уже вытеснено — врать нечем
            Some(oldest) if since < oldest => Slice::Gap { cursor: self.next },
            _ => Slice::Events {
                cursor: self.next,
                events: self
                    .items
                    .iter()
                    .filter(|e| e.cursor >= since)
                    .cloned()
                    .collect(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(capacity: usize, n: u64) -> Ring {
        let mut r = Ring::new(capacity);
        for i in 0..n {
            r.push(json!({ "event": "prompt", "n": i }), 1_000 + i as i64);
        }
        r
    }

    fn cursors(slice: &Slice) -> Vec<u64> {
        match slice {
            Slice::Events { events, .. } => events.iter().map(|e| e.cursor).collect(),
            Slice::Gap { .. } => panic!("ожидались события, пришёл gap"),
        }
    }

    // Курсор нумерует события с нуля и не переиспользуется.
    #[test]
    fn cursor_is_monotonic_and_slice_starts_at_requested() {
        let r = ring(10, 3);
        assert_eq!(r.stats().cursor, 3, "следующий курсор = сколько принято");
        assert_eq!(cursors(&r.since(0)), vec![0, 1, 2]);
        assert_eq!(cursors(&r.since(2)), vec![2]);
        // «спроси с курсора, который вернули» — пустой ответ, а не gap
        assert_eq!(r.since(3), Slice::Events { cursor: 3, events: vec![] });
    }

    // Свежий узел: ноут приходит с нуля и не должен получить gap на пустом месте.
    #[test]
    fn fresh_buffer_has_no_gap() {
        let r = Ring::new(10);
        assert_eq!(r.since(0), Slice::Events { cursor: 0, events: vec![] });
        assert_eq!(r.stats().oldest, None);
    }

    // Переполнение: старое вытеснено, ёмкость соблюдена, курсор продолжает расти.
    #[test]
    fn overflow_drops_oldest_and_keeps_capacity() {
        let r = ring(3, 5);
        let s = r.stats();
        assert_eq!(s.buffered, 3);
        assert_eq!(s.capacity, 3);
        assert_eq!(s.cursor, 5);
        assert_eq!(s.oldest, Some(2), "0 и 1 вытеснены");
        assert_eq!(cursors(&r.since(2)), vec![2, 3, 4]);
    }

    // Запрос за вытесненным началом — честный gap, а не тихая выдача огрызка.
    #[test]
    fn request_before_oldest_is_a_gap() {
        let r = ring(3, 5);
        assert_eq!(r.since(0), Slice::Gap { cursor: 5 });
        assert_eq!(r.since(1), Slice::Gap { cursor: 5 });
        // ровно на границе дырки нет
        assert!(matches!(r.since(2), Slice::Events { .. }));
    }

    // Узел перезапустили, у ноута курсор из прошлой жизни — тоже gap.
    #[test]
    fn cursor_from_the_future_is_a_gap() {
        let r = ring(10, 2);
        assert_eq!(r.since(99), Slice::Gap { cursor: 2 });
    }

    // Ёмкость 0 схлопнулась бы в «теряем всё молча» — держим хотя бы одно.
    #[test]
    fn zero_capacity_is_clamped_to_one() {
        let r = ring(0, 3);
        assert_eq!(r.stats().capacity, 1);
        assert_eq!(cursors(&r.since(2)), vec![2]);
        assert_eq!(r.since(0), Slice::Gap { cursor: 3 });
    }

    // Конверт доезжает до ноута нетронутым и не путается с полями обёртки.
    #[test]
    fn envelope_is_nested_not_flattened() {
        let mut r = Ring::new(4);
        r.push(json!({ "agent": "claude", "event": "stop" }), 7);
        let Slice::Events { events, .. } = r.since(0) else {
            panic!("ожидались события");
        };
        let out = events[0].to_json();
        assert_eq!(out["cursor"], 0);
        assert_eq!(out["at"], 7);
        assert_eq!(out["envelope"]["event"], "stop");
        assert_eq!(out["envelope"]["agent"], "claude");
    }
}
