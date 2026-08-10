use std::error::Error;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

pub const PLUGIN_PROCESS_PROTOCOL: u32 = 2;
pub const MAX_REQUEST_ID_BYTES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct RequestId(String);

impl RequestId {
    pub fn new(value: impl Into<String>) -> Result<Self, RequestIdError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(RequestIdError::Empty);
        }
        if value.len() > MAX_REQUEST_ID_BYTES {
            return Err(RequestIdError::TooLong {
                actual: value.len(),
                maximum: MAX_REQUEST_ID_BYTES,
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestIdError {
    Empty,
    TooLong { actual: usize, maximum: usize },
}

impl fmt::Display for RequestIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("request id must not be empty"),
            Self::TooLong { actual, maximum } => write!(
                formatter,
                "request id is {actual} bytes; maximum is {maximum} bytes"
            ),
        }
    }
}

impl Error for RequestIdError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginHello {
    pub protocol_version: u32,
    pub plugin_id: String,
    pub pid: u32,
    pub package_digest: String,
    pub activation_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostHello {
    pub protocol_version: u32,
    pub plugin_id: String,
    pub package_digest: String,
    pub activation_generation: u64,
    pub accepted: bool,
}

impl HostHello {
    pub fn accepted(
        plugin_id: impl Into<String>,
        package_digest: impl Into<String>,
        activation_generation: u64,
    ) -> Self {
        Self {
            protocol_version: PLUGIN_PROCESS_PROTOCOL,
            plugin_id: plugin_id.into(),
            package_digest: package_digest.into(),
            activation_generation,
            accepted: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivationRequest {
    pub plugin_id: String,
    pub package_digest: String,
    pub activation_generation: u64,
    pub request_id: RequestId,
    pub event: String,
    pub context: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivationResponse {
    pub plugin_id: String,
    pub package_digest: String,
    pub activation_generation: u64,
    pub request_id: RequestId,
    pub ready: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShutdownRequest {
    pub plugin_id: String,
    pub package_digest: String,
    pub activation_generation: u64,
    pub request_id: RequestId,
    pub reason: String,
    pub deadline_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShutdownAck {
    pub plugin_id: String,
    pub package_digest: String,
    pub activation_generation: u64,
    pub request_id: RequestId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Heartbeat {
    pub plugin_id: String,
    pub package_digest: String,
    pub activation_generation: u64,
    pub sequence: u64,
    pub emitted_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandRequest {
    pub plugin_id: String,
    pub package_digest: String,
    pub activation_generation: u64,
    pub request_id: RequestId,
    pub command: String,
    pub args: Value,
}

impl CommandRequest {
    pub fn new(
        plugin_id: impl Into<String>,
        package_digest: impl Into<String>,
        activation_generation: u64,
        request_id: impl Into<String>,
        command: impl Into<String>,
        args: Value,
    ) -> Result<Self, RequestIdError> {
        Ok(Self {
            plugin_id: plugin_id.into(),
            package_digest: package_digest.into(),
            activation_generation,
            request_id: RequestId::new(request_id)?,
            command: command.into(),
            args,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandResponse {
    pub plugin_id: String,
    pub package_digest: String,
    pub activation_generation: u64,
    pub request_id: RequestId,
    pub ok: bool,
    pub result: Option<Value>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum PluginFrame {
    PluginHello(PluginHello),
    HostHello(HostHello),
    ActivationRequest(ActivationRequest),
    ActivationResponse(ActivationResponse),
    Heartbeat(Heartbeat),
    CommandRequest(CommandRequest),
    CommandResponse(CommandResponse),
    ShutdownRequest(ShutdownRequest),
    ShutdownAck(ShutdownAck),
}
