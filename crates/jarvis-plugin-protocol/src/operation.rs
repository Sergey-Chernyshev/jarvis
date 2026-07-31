use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Operation {
    pub id: String,
    pub kind: String,
    pub plugin_id: String,
    pub state: OperationState,
    pub phase: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

impl Operation {
    #[doc(hidden)]
    pub fn new_fixture(id: &str, kind: &str, plugin_id: &str) -> Self {
        Self {
            id: id.to_string(),
            kind: kind.to_string(),
            plugin_id: plugin_id.to_string(),
            state: OperationState::Queued,
            phase: "queued".to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            error_code: None,
            error_message: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperationState {
    Queued,
    Running,
    WaitingForConsent,
    Succeeded,
    Failed,
    Cancelled,
}
