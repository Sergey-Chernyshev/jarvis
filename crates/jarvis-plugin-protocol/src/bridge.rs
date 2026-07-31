use schemars::JsonSchema;
use serde::de;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

pub const BRIDGE_PROTOCOL_V1: u32 = 1;
pub const MAX_BRIDGE_MESSAGE_BYTES: usize = 1_048_576;
pub const MAX_BRIDGE_IN_FLIGHT: usize = 64;
pub const MAX_BRIDGE_SUBSCRIPTIONS: usize = 32;
pub const MAX_BRIDGE_BATCH_EVENTS: usize = 128;
pub const DEFAULT_REQUEST_DEADLINE_MS: u64 = 10_000;
pub const MAX_REQUEST_DEADLINE_MS: u64 = 30_000;

const MAX_ID_BYTES: usize = 128;
const MAX_CODE_BYTES: usize = 128;
const MAX_MESSAGE_BYTES: usize = 1024;
const MAX_GRANTS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Hello {
    #[serde(deserialize_with = "deserialize_protocol_v1")]
    pub v: u32,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Welcome {
    #[serde(deserialize_with = "deserialize_protocol_v1")]
    pub v: u32,
    #[serde(deserialize_with = "deserialize_plugin_id")]
    pub plugin_id: String,
    #[serde(deserialize_with = "deserialize_digest")]
    pub package_digest: String,
    #[serde(deserialize_with = "deserialize_identifier")]
    pub page_id: String,
    pub generation: u64,
    #[serde(deserialize_with = "deserialize_grants")]
    pub grants: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BridgeRequest {
    #[serde(deserialize_with = "deserialize_protocol_v1")]
    pub v: u32,
    #[serde(deserialize_with = "deserialize_identifier")]
    pub id: String,
    pub generation: u64,
    #[serde(deserialize_with = "deserialize_namespace")]
    pub namespace: String,
    #[serde(deserialize_with = "deserialize_method")]
    pub method: String,
    pub params: Value,
    #[serde(deserialize_with = "deserialize_deadline")]
    pub deadline_ms: u64,
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BridgeResponse {
    #[serde(deserialize_with = "deserialize_protocol_v1")]
    pub v: u32,
    #[serde(deserialize_with = "deserialize_identifier")]
    pub id: String,
    pub generation: u64,
    pub result: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscribeResult {
    #[serde(deserialize_with = "deserialize_protocol_v1")]
    pub v: u32,
    #[serde(deserialize_with = "deserialize_identifier")]
    pub id: String,
    pub generation: u64,
    #[serde(deserialize_with = "deserialize_identifier")]
    pub subscription_id: String,
    pub cursor: u64,
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BridgeEvent {
    #[serde(deserialize_with = "deserialize_protocol_v1")]
    pub v: u32,
    pub generation: u64,
    #[serde(deserialize_with = "deserialize_identifier")]
    pub subscription_id: String,
    pub cursor: u64,
    pub event: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Poll {
    #[serde(deserialize_with = "deserialize_protocol_v1")]
    pub v: u32,
    pub generation: u64,
    pub cursor: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Cancel {
    #[serde(deserialize_with = "deserialize_protocol_v1")]
    pub v: u32,
    #[serde(deserialize_with = "deserialize_identifier")]
    pub id: String,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Unsubscribe {
    #[serde(deserialize_with = "deserialize_protocol_v1")]
    pub v: u32,
    pub generation: u64,
    #[serde(deserialize_with = "deserialize_identifier")]
    pub subscription_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Gap {
    #[serde(deserialize_with = "deserialize_protocol_v1")]
    pub v: u32,
    pub generation: u64,
    #[serde(deserialize_with = "deserialize_identifier")]
    pub subscription_id: String,
    pub requested_cursor: u64,
    pub earliest_cursor: u64,
    pub latest_cursor: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Close {
    #[serde(deserialize_with = "deserialize_protocol_v1")]
    pub v: u32,
    pub generation: u64,
    #[serde(deserialize_with = "deserialize_code")]
    pub code: String,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BridgeError {
    #[serde(deserialize_with = "deserialize_protocol_v1")]
    pub v: u32,
    #[serde(default, deserialize_with = "deserialize_optional_identifier")]
    pub id: Option<String>,
    pub generation: u64,
    #[serde(deserialize_with = "deserialize_code")]
    pub code: String,
    #[serde(default, deserialize_with = "deserialize_optional_message")]
    pub message: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_identifier")]
    pub correlation_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BridgeClientFrame {
    Hello(Hello),
    Request(BridgeRequest),
    Poll(Poll),
    Cancel(Cancel),
    Unsubscribe(Unsubscribe),
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BridgeHostFrame {
    Welcome(Welcome),
    Response(BridgeResponse),
    SubscribeResult(SubscribeResult),
    Event(BridgeEvent),
    Gap(Gap),
    Close(Close),
    Error(BridgeError),
}

fn deserialize_protocol_v1<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u32::deserialize(deserializer)?;
    if value != BRIDGE_PROTOCOL_V1 {
        return Err(de::Error::custom("bridge protocol incompatible"));
    }
    Ok(value)
}

fn deserialize_deadline<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u64::deserialize(deserializer)?;
    if value == 0 || value > MAX_REQUEST_DEADLINE_MS {
        return Err(de::Error::custom("invalid bridge deadline"));
    }
    Ok(value)
}

fn deserialize_identifier<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    validate_ascii_token(value, MAX_ID_BYTES).map_err(de::Error::custom)
}

fn deserialize_optional_identifier<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    value
        .map(|item| validate_ascii_token(item, MAX_ID_BYTES))
        .transpose()
        .map_err(de::Error::custom)
}

fn deserialize_namespace<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = deserialize_identifier(deserializer)?;
    if !value.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
    }) {
        return Err(de::Error::custom("invalid bridge namespace"));
    }
    Ok(value)
}

fn deserialize_method<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_namespace(deserializer)
}

fn deserialize_plugin_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = deserialize_namespace(deserializer)?;
    if !value.contains('.') {
        return Err(de::Error::custom("invalid plugin id"));
    }
    Ok(value)
}

fn deserialize_digest<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    let valid = value
        .strip_prefix("sha256:")
        .map(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
        .unwrap_or(false);
    if !valid {
        return Err(de::Error::custom("invalid sha256 digest"));
    }
    Ok(value)
}

fn deserialize_grants<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<String>::deserialize(deserializer)?;
    if values.len() > MAX_GRANTS {
        return Err(de::Error::custom("too many grants"));
    }
    values
        .into_iter()
        .map(|value| {
            let value = validate_ascii_token(value, MAX_ID_BYTES)?;
            if value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'-' | b'_')
            }) {
                Ok(value)
            } else {
                Err("invalid grant")
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(de::Error::custom)
}

fn deserialize_code<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    validate_ascii_token(value, MAX_CODE_BYTES).map_err(de::Error::custom)
}

fn deserialize_optional_message<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    value
        .map(|item| {
            if item.len() <= MAX_MESSAGE_BYTES && !item.chars().any(char::is_control) {
                Ok(item)
            } else {
                Err("invalid bridge message")
            }
        })
        .transpose()
        .map_err(de::Error::custom)
}

fn validate_ascii_token(value: String, max_bytes: usize) -> Result<String, &'static str> {
    if value.is_empty()
        || value.len() > max_bytes
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-' | b'@')
        })
    {
        return Err("invalid bridge token");
    }
    Ok(value)
}
