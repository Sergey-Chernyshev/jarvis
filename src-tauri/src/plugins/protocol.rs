use std::collections::VecDeque;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Notify;

pub const MAX_EVENT_BYTES: usize = 256 * 1024;
pub const MAX_QUEUED_EVENTS: usize = 256;
pub const MAX_POLL_EVENTS: usize = 64;
pub const MAX_WAIT_MS: u64 = 25_000;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRequest {
    pub protocol_version: u32,
    pub pid: u32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginEvent {
    pub seq: u64,
    pub kind: String,
    pub payload: Value,
}

fn default_limit() -> usize {
    MAX_POLL_EVENTS
}

fn default_wait_ms() -> u64 {
    MAX_WAIT_MS
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventsQuery {
    #[serde(default)]
    pub after: u64,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default = "default_wait_ms")]
    pub wait_ms: u64,
}

impl EventsQuery {
    pub fn clamped(&self) -> (u64, usize, u64) {
        (
            self.after,
            self.limit.clamp(1, MAX_POLL_EVENTS),
            self.wait_ms.min(MAX_WAIT_MS),
        )
    }
}

pub struct EventQueue {
    events: VecDeque<PluginEvent>,
    notify: Arc<Notify>,
}

impl Default for EventQueue {
    fn default() -> Self {
        Self {
            events: VecDeque::new(),
            notify: Arc::new(Notify::new()),
        }
    }
}

impl EventQueue {
    pub fn push(
        &mut self,
        seq: u64,
        kind: impl Into<String>,
        payload: Value,
    ) -> Result<PluginEvent, String> {
        if self.events.back().is_some_and(|event| seq <= event.seq) {
            return Err("event sequence должен строго возрастать".into());
        }
        let payload_size = serde_json::to_vec(&payload)
            .map_err(|err| format!("event payload не сериализуется: {err}"))?
            .len();
        if payload_size > MAX_EVENT_BYTES {
            return Err(format!(
                "размер event payload превышает лимит {MAX_EVENT_BYTES} байт"
            ));
        }
        let kind = kind.into();
        if kind.trim().is_empty() {
            return Err("event kind обязателен".into());
        }

        let event = PluginEvent { seq, kind, payload };
        self.events.push_back(event.clone());
        while self.events.len() > MAX_QUEUED_EVENTS {
            self.events.pop_front();
        }
        self.notify.notify_waiters();
        Ok(event)
    }

    pub fn read_after(&self, after: u64, limit: usize) -> Vec<PluginEvent> {
        self.events
            .iter()
            .filter(|event| event.seq > after)
            .take(limit.min(MAX_POLL_EVENTS))
            .cloned()
            .collect()
    }

    pub fn notifier(&self) -> Arc<Notify> {
        self.notify.clone()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn register_request_uses_versioned_camel_case_wire() {
        let request: RegisterRequest =
            serde_json::from_value(json!({"protocolVersion": 1, "pid": 41})).unwrap();
        assert_eq!(request.protocol_version, 1);
        assert_eq!(request.pid, 41);
    }

    #[test]
    fn event_queue_is_monotonic_bounded_and_replayable_after_seq() {
        let mut queue = EventQueue::default();
        for seq in 1..=300 {
            queue.push(seq, "command", json!({"number": seq})).unwrap();
        }

        assert_eq!(queue.len(), MAX_QUEUED_EVENTS);
        assert_eq!(queue.read_after(0, MAX_POLL_EVENTS)[0].seq, 45);
        let replay = queue.read_after(290, MAX_POLL_EVENTS);
        assert_eq!(
            replay.iter().map(|event| event.seq).collect::<Vec<_>>(),
            (291..=300).collect::<Vec<_>>()
        );
        let err = queue.push(300, "command", json!({})).unwrap_err();
        assert!(
            err.contains("sequence"),
            "не монотонный seq отклонён: {err}"
        );
    }

    #[test]
    fn event_payload_over_256_kib_is_rejected() {
        let mut queue = EventQueue::default();
        let payload = json!({"blob": "x".repeat(MAX_EVENT_BYTES)});

        let err = queue.push(1, "command", payload).unwrap_err();

        assert!(err.contains("размер"), "понятная ошибка квоты: {err}");
        assert!(queue.is_empty());
    }

    #[test]
    fn poll_query_clamps_caller_limits() {
        let query: EventsQuery = serde_json::from_value(json!({
            "after": 9,
            "limit": 9999,
            "waitMs": 999999
        }))
        .unwrap();

        let (after, limit, wait_ms) = query.clamped();

        assert_eq!(after, 9);
        assert_eq!(limit, MAX_POLL_EVENTS);
        assert_eq!(wait_ms, MAX_WAIT_MS);
    }

    #[test]
    fn poll_query_defaults_are_bounded() {
        let query: EventsQuery = serde_json::from_value(json!({})).unwrap();
        assert_eq!(query.clamped(), (0, MAX_POLL_EVENTS, MAX_WAIT_MS));
    }
}
