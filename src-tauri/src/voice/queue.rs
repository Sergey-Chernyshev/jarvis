//! Сериализованная очередь речи: одна реплика за раз, приоритет (нужен-человек >
//! готово), дедуп повторов, коалесцирование Stop-бэклога, прерывание текущей.

use crate::voice::composer::Utterance;
use std::collections::VecDeque;

#[derive(Default)]
pub struct SpeechQueue {
    items: VecDeque<Utterance>,
    recent_dedup: VecDeque<String>, // последние dedup_key, чтобы не повторять
}

impl SpeechQueue {
    pub fn new() -> Self { Self::default() }

    /// Поставить реплику. Дедуп повторов. NeedHuman лезет вперёд Done.
    /// Возвращает true, если что-то реально добавлено.
    pub fn enqueue(&mut self, u: Utterance) -> bool {
        if self.recent_dedup.contains(&u.dedup_key) { return false; }
        self.recent_dedup.push_back(u.dedup_key.clone());
        if self.recent_dedup.len() > 16 { self.recent_dedup.pop_front(); }
        // вставка с учётом приоритета: перед первым элементом меньшего приоритета
        let pos = self.items.iter().position(|x| x.priority < u.priority);
        match pos { Some(i) => self.items.insert(i, u), None => self.items.push_back(u) }
        true
    }

    /// Достать следующую реплику. При заторе Done-реплики одной coalesce_group
    /// сливаются в одну («Пиксела и Рекрю закончили»).
    pub fn next(&mut self) -> Option<Utterance> {
        let first = self.items.pop_front()?;
        if let Some(group) = first.coalesce_group.clone() {
            let mut projects = vec![first_project(&first.text)];
            self.items.retain(|x| {
                if x.coalesce_group.as_deref() == Some(group.as_str()) {
                    projects.push(first_project(&x.text)); false
                } else { true }
            });
            if projects.len() > 1 {
                let joined = join_ru(&projects);
                return Some(Utterance { text: format!("{joined} закончили"), ..first });
            }
        }
        Some(first)
    }

    pub fn is_empty(&self) -> bool { self.items.is_empty() }
    pub fn len(&self) -> usize { self.items.len() }

    /// Очистить очередь (для барж-ина: оборвать речь — недостаточно, иначе воркер
    /// заговорит следующую утту). Дедуп-историю НЕ трогаем (повторы по-прежнему
    /// схлопываются). Возвращает число выброшенных утт.
    pub fn clear(&mut self) -> usize {
        let n = self.items.len();
        self.items.clear();
        n
    }
}

/// «Пиксела: …»/«Пиксела закончил» → «Пиксела» (для коалесцирования).
fn first_project(text: &str) -> String {
    text.split([':', ',']).next().unwrap_or(text)
        .split(" готов").next().unwrap_or(text)
        .split(" закончил").next().unwrap_or(text)
        .trim().to_string()
}

/// «А», «А и Б», «А, Б и В».
fn join_ru(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [a] => a.clone(),
        [rest @ .., last] => format!("{} и {}", rest.join(", "), last),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::composer::Priority;

    fn done(project: &str) -> Utterance {
        Utterance { text: format!("{project} закончил"), priority: Priority::Done,
            dedup_key: format!("stop:{project}"), coalesce_group: Some("stop-done".into()), toast_id: None, done: None }
    }
    fn wait(project: &str) -> Utterance {
        Utterance { text: format!("{project} ждёт — нужно разрешение"), priority: Priority::NeedHuman,
            dedup_key: format!("notif:{project}"), coalesce_group: None, toast_id: None, done: None }
    }

    #[test]
    fn need_human_jumps_ahead_of_done() {
        let mut q = SpeechQueue::new();
        q.enqueue(done("Пиксела"));
        q.enqueue(wait("Рекрю"));
        assert_eq!(q.next().unwrap().priority, Priority::NeedHuman);
    }

    #[test]
    fn dedup_repeated_notification() {
        let mut q = SpeechQueue::new();
        assert!(q.enqueue(wait("Пиксела")));
        assert!(!q.enqueue(wait("Пиксела")), "повтор того же dedup_key не добавляется");
    }

    #[test]
    fn clear_drains_items() {
        let mut q = SpeechQueue::new();
        q.enqueue(done("Пиксела"));
        q.enqueue(done("Рекрю"));
        assert_eq!(q.clear(), 2);
        assert!(q.is_empty());
        assert!(q.next().is_none());
    }

    #[test]
    fn coalesces_done_backlog() {
        let mut q = SpeechQueue::new();
        q.enqueue(done("Пиксела"));
        q.enqueue(done("Рекрю"));
        let u = q.next().unwrap();
        assert_eq!(u.text, "Пиксела и Рекрю закончили");
        assert!(q.is_empty(), "обе done-реплики ушли в одну");
    }

    #[test]
    fn single_done_not_coalesced() {
        let mut q = SpeechQueue::new();
        q.enqueue(done("Пиксела"));
        assert_eq!(q.next().unwrap().text, "Пиксела закончил");
    }
}
