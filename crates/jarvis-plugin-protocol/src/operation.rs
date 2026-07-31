use serde::de;
use serde::{Deserialize, Deserializer, Serialize};

const MAX_OPERATION_REF_BYTES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct OperationRef(String);

impl OperationRef {
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_OPERATION_REF_BYTES
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-')
            })
        {
            return Err("invalid operation ref");
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for OperationRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

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
