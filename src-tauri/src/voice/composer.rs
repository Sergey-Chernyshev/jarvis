//! Композитор фраз: превращает структурные сигналы в короткое русское предложение для TTS.
//!
//! `TemplateComposer` — шаблонная реализация трейта `Composer`.
//! Шов для будущей LLM-реализации оставлен через трейт.

use crate::voice::numerals::{
    count_phrase, duration_words, number_words_genitive, number_words_gender, plural, Gender,
};

// ---------------------------------------------------------------------------
// Типы
// ---------------------------------------------------------------------------

/// Приоритет высказывания: выше = важнее (NeedHuman перед Done).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Done = 0,
    NeedHuman = 1,
}

/// Готовое высказывание для TTS-движка.
#[derive(Debug, Clone)]
pub struct Utterance {
    pub text: String,
    pub priority: Priority,
    pub dedup_key: String,
    /// Some → сливается при заторе очереди.
    pub coalesce_group: Option<String>,
}

/// Событие, вызвавшее речь.
#[derive(Debug, Clone, Copy)]
pub enum Event {
    Stop,
    Notification,
    StopFailure,
}

/// Сигналы, из которых compositor строит фразу.
#[derive(Debug, Clone, Default)]
pub struct SpeechSignals {
    pub event: Option<Event>,
    pub sid: String,
    pub project: String,
    pub board_done: Option<i64>,
    pub board_total: Option<i64>,
    pub board_active: Option<String>,
    pub diff_files: Option<i64>,
    pub notification_text: Option<String>,
    pub limit_reset_min: Option<i64>,
}

// ---------------------------------------------------------------------------
// Трейт
// ---------------------------------------------------------------------------

/// Трейт-шов: шаблонная и будущая LLM-реализация взаимозаменяемы.
pub trait Composer: Send + Sync {
    fn compose(&self, s: &SpeechSignals) -> Option<Utterance>;
}

// ---------------------------------------------------------------------------
// Шаблонная реализация
// ---------------------------------------------------------------------------

pub struct TemplateComposer;

impl Composer for TemplateComposer {
    fn compose(&self, s: &SpeechSignals) -> Option<Utterance> {
        let project = if s.project.is_empty() { "Сессия" } else { s.project.as_str() };

        // первое предложение, обрезанное до 140 символов
        let trunc = |t: &str| -> String {
            let one = t.split(['.', '!', '?']).next().unwrap_or(t).trim();
            let chars: Vec<char> = one.chars().collect();
            if chars.len() > 140 {
                chars[..140].iter().collect()
            } else {
                one.to_string()
            }
        };

        match s.event? {
            Event::Notification => {
                let gist = s
                    .notification_text
                    .as_deref()
                    .map(trunc)
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "нужен ты".into());
                Some(Utterance {
                    text: format!("{project} ждёт — {gist}"),
                    priority: Priority::NeedHuman,
                    dedup_key: format!("notif:{}:{gist}", s.sid),
                    coalesce_group: None,
                })
            }

            Event::StopFailure => {
                let when = s
                    .limit_reset_min
                    .map(duration_words)
                    .map(|w| format!(", сброс через {w}"))
                    .unwrap_or_default();
                Some(Utterance {
                    text: format!("{project} упёрся в лимит{when}"),
                    priority: Priority::NeedHuman,
                    dedup_key: format!("limit:{}", s.sid),
                    coalesce_group: None,
                })
            }

            Event::Stop => {
                let text = match (s.board_done, s.board_total) {
                    (Some(done), Some(total)) if total > 0 => {
                        // Числитель согласуется с «задача» (ж.р.): «одна», «две».
                        let head = format!(
                            "{project}: {} из {} {}",
                            number_words_gender(done, Gender::F),
                            number_words_genitive(total),
                            plural(total, "задача", "задачи", "задач")
                        );
                        match s.board_active.as_deref().filter(|a| !a.is_empty()) {
                            Some(a) => format!("{head}, сейчас {a}"),
                            None => head,
                        }
                    }
                    _ => match s.diff_files {
                        Some(n) if n > 0 => format!(
                            "{project} готов, изменено {}",
                            count_phrase(n, Gender::M, "файл", "файла", "файлов")
                        ),
                        _ => format!("{project} закончил"),
                    },
                };
                Some(Utterance {
                    text,
                    priority: Priority::Done,
                    dedup_key: format!("stop:{}", s.sid),
                    coalesce_group: Some("stop-done".into()),
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Вспомогательная функция: строит минимальный SpeechSignals с событием и проектом.
    fn sig(event: Event, project: &str) -> SpeechSignals {
        SpeechSignals {
            event: Some(event),
            project: project.to_string(),
            ..Default::default()
        }
    }

    // --- Stop: предпочитает доску ---

    #[test]
    fn stop_prefers_board_over_diff() {
        let c = TemplateComposer;
        let s = SpeechSignals {
            board_done: Some(4),
            board_total: Some(6),
            board_active: Some("docker-compose".into()),
            diff_files: Some(3),
            ..sig(Event::Stop, "Пиксела")
        };
        let u = c.compose(&s).expect("должно быть Some");
        assert!(
            u.text.contains("четыре из шести задач"),
            "текст должен содержать «четыре из шести задач», получено: {:?}",
            u.text
        );
        assert!(
            u.text.contains("docker-compose"),
            "текст должен содержать «docker-compose», получено: {:?}",
            u.text
        );
        assert_eq!(u.priority, Priority::Done);
    }

    #[test]
    fn stop_board_feminine_numerator_one() {
        // done=1 → числитель в женском роде: «одна», а не «один»
        let c = TemplateComposer;
        let s = SpeechSignals {
            board_done: Some(1),
            board_total: Some(6),
            ..sig(Event::Stop, "Пиксела")
        };
        let u = c.compose(&s).expect("должно быть Some");
        assert_eq!(u.text, "Пиксела: одна из шести задач");
    }

    #[test]
    fn stop_board_feminine_numerator_two() {
        // done=2 → «две», а не «два»
        let c = TemplateComposer;
        let s = SpeechSignals {
            board_done: Some(2),
            board_total: Some(6),
            ..sig(Event::Stop, "Пиксела")
        };
        let u = c.compose(&s).expect("должно быть Some");
        assert!(
            u.text.contains("две из шести задач"),
            "текст должен содержать «две из шести задач», получено: {:?}",
            u.text
        );
    }

    // --- Stop: откат к diff, затем голый факт ---

    #[test]
    fn stop_falls_to_diff() {
        let c = TemplateComposer;
        let s = SpeechSignals {
            diff_files: Some(3),
            ..sig(Event::Stop, "Рекрю")
        };
        let u = c.compose(&s).expect("должно быть Some");
        assert_eq!(u.text, "Рекрю готов, изменено три файла");
    }

    #[test]
    fn stop_bare_fact() {
        let c = TemplateComposer;
        let s = sig(Event::Stop, "Тикетинг");
        let u = c.compose(&s).expect("должно быть Some");
        assert_eq!(u.text, "Тикетинг закончил");
    }

    // --- Notification: NeedHuman, не сливается ---

    #[test]
    fn notification_need_human_not_coalesced() {
        let c = TemplateComposer;
        let s = SpeechSignals {
            notification_text: Some("нужно разрешение на Bash".into()),
            ..sig(Event::Notification, "Пиксела")
        };
        let u = c.compose(&s).expect("должно быть Some");
        assert_eq!(u.priority, Priority::NeedHuman);
        assert!(
            u.text.starts_with("Пиксела ждёт"),
            "текст должен начинаться с «Пиксела ждёт», получено: {:?}",
            u.text
        );
        assert!(
            u.text.contains("Bash"),
            "текст должен содержать «Bash», получено: {:?}",
            u.text
        );
        assert_eq!(u.coalesce_group, None);
    }

    // --- StopFailure: называет сброс словами ---

    #[test]
    fn stop_failure_speaks_reset_in_words() {
        let c = TemplateComposer;
        let s = SpeechSignals {
            limit_reset_min: Some(134),
            ..sig(Event::StopFailure, "Пиксела")
        };
        let u = c.compose(&s).expect("должно быть Some");
        assert_eq!(u.priority, Priority::NeedHuman);
        assert!(
            u.text.contains("упёрся в лимит"),
            "текст должен содержать «упёрся в лимит», получено: {:?}",
            u.text
        );
        assert!(
            u.text.contains("два часа четырнадцать минут"),
            "текст должен содержать «два часа четырнадцать минут», получено: {:?}",
            u.text
        );
    }

    // --- Длинное уведомление обрезается ---

    #[test]
    fn long_notification_truncated() {
        let c = TemplateComposer;
        let long_text = "a".repeat(500);
        let s = SpeechSignals {
            notification_text: Some(long_text),
            ..sig(Event::Notification, "Тест")
        };
        let u = c.compose(&s).expect("должно быть Some");
        let len: usize = u.text.chars().count();
        assert!(
            len <= 160,
            "длина текста ({len} символов) должна быть не более 160"
        );
    }

    // --- Stop сливается ---

    #[test]
    fn stop_has_coalesce_group() {
        let c = TemplateComposer;
        let s = sig(Event::Stop, "Любой");
        let u = c.compose(&s).expect("должно быть Some");
        assert_eq!(u.coalesce_group, Some("stop-done".into()));
    }
}
