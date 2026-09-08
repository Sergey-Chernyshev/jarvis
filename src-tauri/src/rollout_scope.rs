//! Immutable Codex rollout ownership and fork history boundaries. Copied parent
//! records are context, not new work performed by the child session.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RolloutScope {
    pub owner_id: Option<String>,
    pub inherited_before: Option<u64>,
    pub next_ordinal: u64,
    pub skipped_inherited: u64,
    pub forked: bool,
}

impl RolloutScope {
    /// Call for every complete JSONL record, including records the caller does
    /// not otherwise interpret. Ordinals are zero based; the first header owns
    /// the file and is always accepted. Later metadata cannot change ownership.
    pub fn accept(&mut self, record: &Value) -> bool {
        let ordinal = record
            .get("ordinal")
            .and_then(Value::as_u64)
            .unwrap_or(self.next_ordinal);
        self.next_ordinal = self.next_ordinal.max(ordinal.saturating_add(1));
        if record["type"] == "session_meta" {
            if self.owner_id.is_some() {
                self.skipped_inherited += 1;
                return false;
            }
            let payload = &record["payload"];
            let id = payload["id"]
                .as_str()
                .or_else(|| payload["session_id"].as_str())
                .filter(|id| !id.is_empty());
            let Some(id) = id else {
                return false;
            };
            self.owner_id = Some(id.into());
            self.forked = payload["forked_from_id"].as_str().is_some()
                || payload["parent_thread_id"].as_str().is_some()
                || payload
                    .pointer("/source/subagent/thread_spawn/parent_thread_id")
                    .and_then(Value::as_str)
                    .is_some();
            self.inherited_before = payload["subagent_history_start_ordinal"].as_u64();
            return true;
        }
        if self.inherited_before.is_some_and(|start| ordinal < start) {
            self.skipped_inherited += 1;
            return false;
        }
        true
    }

    /// A reader that skips a large record without parsing it must still advance
    /// the ordinal once, otherwise a subsequent fork boundary would shift.
    pub fn skip_record(&mut self) {
        self.next_ordinal = self.next_ordinal.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn child_header_owns_identity_and_copied_records_do_not_belong_to_child() {
        let mut scope = RolloutScope::default();
        assert!(scope.accept(&json!({"type":"session_meta","payload":{"id":"child","forked_from_id":"parent","subagent_history_start_ordinal":4}})));
        assert!(!scope.accept(&json!({"type":"session_meta","payload":{"id":"parent"}})));
        assert!(!scope.accept(&json!({"type":"event_msg","payload":{"type":"token_count"}})));
        assert!(!scope.accept(&json!({"type":"world_state"})));
        let mut restored: RolloutScope =
            serde_json::from_str(&serde_json::to_string(&scope).unwrap()).unwrap();
        assert!(restored.accept(&json!({"type":"event_msg","payload":{"type":"task_started"}})));
        assert_eq!(restored.owner_id.as_deref(), Some("child"));
        assert_eq!(restored.skipped_inherited, 3);
    }
    #[test]
    fn explicit_ordinals_and_skipped_large_records_preserve_boundary() {
        let mut scope = RolloutScope::default();
        scope.accept(&json!({"type":"session_meta","payload":{"id":"child","subagent_history_start_ordinal":10}}));
        scope.skip_record();
        assert!(!scope.accept(&json!({"ordinal":9,"type":"response_item"})));
        assert!(scope.accept(&json!({"ordinal":10,"type":"response_item"})));
        assert!(!scope.accept(&json!({"type":"session_meta","payload":{"id":"other"}})));
        assert_eq!(scope.owner_id.as_deref(), Some("child"));
    }
}
