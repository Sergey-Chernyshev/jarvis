//! Граница hook-протокола: проверка конверта и запоздавших событий до
//! мутации сессии/уведомлений. Старые CLI без turn_id остаются совместимыми.

use crate::model::{Session, Status};
use serde_json::{Map, Value};

pub struct HookEvent<'a> {
    pub name: &'a str,
    pub session_id: &'a str,
    pub payload: &'a Map<String, Value>,
}

impl<'a> HookEvent<'a> {
    pub fn parse(envelope: &'a Value) -> Option<Self> {
        let name = envelope.get("event")?.as_str()?;
        if !matches!(
            name,
            "session-start"
                | "session-end"
                | "prompt"
                | "pre-tool"
                | "post-tool"
                | "notification"
                | "permission"
                | "stop"
                | "stop-failure"
                | "subagent-start"
                | "subagent-stop"
        ) {
            return None;
        }
        let payload = envelope.get("payload")?.as_object()?;
        let session_id = payload.get("session_id")?.as_str()?.trim();
        if session_id.is_empty() {
            return None;
        }
        Some(Self {
            name,
            session_id,
            payload,
        })
    }

    pub fn turn_id(&self) -> Option<&str> {
        self.payload
            .get("turn_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
    }

    pub fn accepts(&self, session: &Session, envelope: &Value) -> bool {
        if self.name == "session-end" {
            // Хук старого процесса может закончить resumed-сессию с тем же id.
            if let (Some(old), Some(current)) =
                (envelope.get("pid").and_then(Value::as_i64), session.pid)
            {
                if old > 0 && current > 0 && old != current {
                    return false;
                }
            }
        }
        if matches!(self.name, "prompt" | "session-start" | "session-end") {
            return true;
        }
        if let (Some(incoming), Some(current)) =
            (self.turn_id(), session.provider_turn_id.as_deref())
        {
            if incoming != current {
                // Только prompt/session-start устанавливает следующий ход.
                // Даже после Done старый pre-tool может приехать с задержкой;
                // по непрозрачному turn_id нельзя доказать, что ход новее.
                return false;
            }
            if session.status == Status::Done
                && matches!(self.name, "pre-tool" | "subagent-start" | "permission" | "notification")
            {
                return false;
            }
        }
        // Повторный Stop не должен пересоздавать тост/озвучку/саммари.
        !(self.name == "stop" && session.status == Status::Done)
    }
}

/// Завершение инструмента подтверждает жизнь инструмента, а не нового хода.
pub fn after_tool(status: Status) -> Status {
    if status == Status::Done {
        Status::Done
    } else {
        Status::Working
    }
}

pub fn completion_is_current(session: &Session, stop_at: i64, revision: u64) -> bool {
    session.status == Status::Done
        && session.done_at == Some(stop_at)
        && session.lifecycle_revision == revision
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn malformed_hooks_do_not_create_unknown_sessions() {
        for value in [
            json!({}),
            json!({"event":"stop","payload":{}}),
            json!({"event":"stop","payload":{"session_id":"  "}}),
            json!({"event":"future","payload":{"session_id":"s"}}),
        ] {
            assert!(HookEvent::parse(&value).is_none());
        }
    }

    #[test]
    fn previous_turn_cannot_complete_or_interrupt_a_new_turn() {
        let mut session = Session::new("s".into(), 0);
        session.status = Status::Working;
        session.provider_turn_id = Some("new".into());
        for event in [
            "stop",
            "stop-failure",
            "post-tool",
            "pre-tool",
            "permission",
            "notification",
        ] {
            let envelope = json!({"event":event,"payload":{"session_id":"s","turn_id":"old"}});
            assert!(
                !HookEvent::parse(&envelope)
                    .unwrap()
                    .accepts(&session, &envelope),
                "{event}"
            );
        }
        let current = json!({"event":"stop","payload":{"session_id":"s","turn_id":"new"}});
        assert!(HookEvent::parse(&current)
            .unwrap()
            .accepts(&session, &current));
    }

    #[test]
    fn completion_survives_late_tool_and_duplicate_stop() {
        let mut session = Session::new("s".into(), 0);
        session.status = after_tool(Status::Done);
        let stop = json!({"event":"stop","payload":{"session_id":"s"}});
        assert_eq!(session.status, Status::Done);
        assert!(!HookEvent::parse(&stop).unwrap().accepts(&session, &stop));
        assert_eq!(after_tool(Status::Waiting), Status::Working);
    }

    #[test]
    fn completed_turn_requires_an_explicit_boundary_before_more_work() {
        let mut session = Session::new("s".into(), 0);
        session.provider_turn_id = Some("current".into());
        for status in [Status::Done, Status::Idle] {
            session.status = status;
            for event in ["pre-tool", "subagent-start", "stop", "post-tool"] {
                let old = json!({"event":event,"payload":{"session_id":"s","turn_id":"old"}});
                assert!(!HookEvent::parse(&old).unwrap().accepts(&session, &old), "{status:?} {event}");
            }
            for event in ["prompt", "session-start"] {
                let new = json!({"event":event,"payload":{"session_id":"s","turn_id":"next"}});
                assert!(HookEvent::parse(&new).unwrap().accepts(&session, &new));
            }
        }
        session.status = Status::Done;
        let late = json!({"event":"subagent-start","payload":{"session_id":"s","turn_id":"current"}});
        assert!(!HookEvent::parse(&late).unwrap().accepts(&session, &late));
    }

    #[test]
    fn old_process_exit_does_not_remove_resumed_session() {
        let mut session = Session::new("s".into(), 0);
        session.pid = Some(200);
        let old = json!({"event":"session-end","pid":100,"payload":{"session_id":"s"}});
        assert!(!HookEvent::parse(&old).unwrap().accepts(&session, &old));
    }

    #[test]
    fn delayed_summary_cannot_publish_after_another_turn_even_in_same_millisecond() {
        let mut session = Session::new("s".into(), 10);
        session.status = Status::Done;
        session.done_at = Some(10);
        session.lifecycle_revision = 2;
        assert!(completion_is_current(&session, 10, 2));
        session.status = Status::Working;
        session.done_at = None;
        session.lifecycle_revision = 3;
        assert!(!completion_is_current(&session, 10, 2));
        // Два Stop из одной remote-партии могут иметь одинаковый now_ms.
        session.status = Status::Done;
        session.done_at = Some(10);
        session.lifecycle_revision = 4;
        assert!(!completion_is_current(&session, 10, 2));
        assert!(completion_is_current(&session, 10, 4));
    }
}
