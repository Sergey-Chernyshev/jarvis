use schemars::JsonSchema;
use serde::de;
use serde::ser;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::error::PublicErrorCode;
use crate::validation::{is_canonical_dotted_id, is_safe_opaque_identifier};

pub const BRIDGE_PROTOCOL_V1: u32 = 1;
pub const MAX_BRIDGE_MESSAGE_BYTES: usize = 1_048_576;
pub const MAX_BRIDGE_IN_FLIGHT: usize = 64;
pub const MAX_BRIDGE_SUBSCRIPTIONS: usize = 32;
pub const MAX_BRIDGE_BATCH_EVENTS: usize = 128;
pub const DEFAULT_REQUEST_DEADLINE_MS: u64 = 10_000;
pub const MAX_REQUEST_DEADLINE_MS: u64 = 30_000;

const MAX_ID_BYTES: usize = 128;
const MAX_GRANTS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Hello {
    #[serde(
        deserialize_with = "deserialize_protocol_v1",
        serialize_with = "serialize_protocol_v1"
    )]
    #[schemars(schema_with = "crate::validation::protocol_v1_schema")]
    pub v: u32,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Welcome {
    #[serde(
        deserialize_with = "deserialize_protocol_v1",
        serialize_with = "serialize_protocol_v1"
    )]
    #[schemars(schema_with = "crate::validation::protocol_v1_schema")]
    pub v: u32,
    #[serde(
        deserialize_with = "deserialize_plugin_id",
        serialize_with = "serialize_plugin_id"
    )]
    #[schemars(schema_with = "crate::validation::plugin_id_128_schema")]
    pub plugin_id: String,
    #[serde(
        deserialize_with = "deserialize_digest",
        serialize_with = "serialize_digest"
    )]
    #[schemars(schema_with = "crate::validation::sha256_digest_schema")]
    pub package_digest: String,
    #[serde(
        deserialize_with = "deserialize_identifier",
        serialize_with = "serialize_identifier"
    )]
    #[schemars(schema_with = "crate::validation::opaque_id_128_schema")]
    pub page_id: String,
    pub generation: u64,
    #[serde(
        deserialize_with = "deserialize_grants",
        serialize_with = "serialize_grants"
    )]
    #[schemars(schema_with = "crate::validation::bridge_grants_128_schema")]
    pub grants: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BridgeRequest {
    #[serde(
        deserialize_with = "deserialize_protocol_v1",
        serialize_with = "serialize_protocol_v1"
    )]
    #[schemars(schema_with = "crate::validation::protocol_v1_schema")]
    pub v: u32,
    #[serde(
        deserialize_with = "deserialize_identifier",
        serialize_with = "serialize_identifier"
    )]
    #[schemars(schema_with = "crate::validation::opaque_id_128_schema")]
    pub id: String,
    pub generation: u64,
    #[serde(
        deserialize_with = "deserialize_namespace",
        serialize_with = "serialize_namespace"
    )]
    #[schemars(schema_with = "crate::validation::bridge_namespace_128_schema")]
    pub namespace: String,
    #[serde(
        deserialize_with = "deserialize_method",
        serialize_with = "serialize_namespace"
    )]
    #[schemars(schema_with = "crate::validation::bridge_namespace_128_schema")]
    pub method: String,
    pub params: Value,
    #[serde(
        deserialize_with = "deserialize_deadline",
        serialize_with = "serialize_deadline"
    )]
    #[schemars(schema_with = "crate::validation::bridge_deadline_schema")]
    pub deadline_ms: u64,
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BridgeResponse {
    #[serde(
        deserialize_with = "deserialize_protocol_v1",
        serialize_with = "serialize_protocol_v1"
    )]
    #[schemars(schema_with = "crate::validation::protocol_v1_schema")]
    pub v: u32,
    #[serde(
        deserialize_with = "deserialize_identifier",
        serialize_with = "serialize_identifier"
    )]
    #[schemars(schema_with = "crate::validation::opaque_id_128_schema")]
    pub id: String,
    pub generation: u64,
    pub result: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscribeResult {
    #[serde(
        deserialize_with = "deserialize_protocol_v1",
        serialize_with = "serialize_protocol_v1"
    )]
    #[schemars(schema_with = "crate::validation::protocol_v1_schema")]
    pub v: u32,
    #[serde(
        deserialize_with = "deserialize_identifier",
        serialize_with = "serialize_identifier"
    )]
    #[schemars(schema_with = "crate::validation::opaque_id_128_schema")]
    pub id: String,
    pub generation: u64,
    #[serde(
        deserialize_with = "deserialize_identifier",
        serialize_with = "serialize_identifier"
    )]
    #[schemars(schema_with = "crate::validation::opaque_id_128_schema")]
    pub subscription_id: String,
    pub cursor: u64,
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BridgeEvent {
    #[serde(
        deserialize_with = "deserialize_protocol_v1",
        serialize_with = "serialize_protocol_v1"
    )]
    #[schemars(schema_with = "crate::validation::protocol_v1_schema")]
    pub v: u32,
    pub generation: u64,
    #[serde(
        deserialize_with = "deserialize_identifier",
        serialize_with = "serialize_identifier"
    )]
    #[schemars(schema_with = "crate::validation::opaque_id_128_schema")]
    pub subscription_id: String,
    pub cursor: u64,
    pub event: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Poll {
    #[serde(
        deserialize_with = "deserialize_protocol_v1",
        serialize_with = "serialize_protocol_v1"
    )]
    #[schemars(schema_with = "crate::validation::protocol_v1_schema")]
    pub v: u32,
    pub generation: u64,
    pub cursor: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Cancel {
    #[serde(
        deserialize_with = "deserialize_protocol_v1",
        serialize_with = "serialize_protocol_v1"
    )]
    #[schemars(schema_with = "crate::validation::protocol_v1_schema")]
    pub v: u32,
    #[serde(
        deserialize_with = "deserialize_identifier",
        serialize_with = "serialize_identifier"
    )]
    #[schemars(schema_with = "crate::validation::opaque_id_128_schema")]
    pub id: String,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Unsubscribe {
    #[serde(
        deserialize_with = "deserialize_protocol_v1",
        serialize_with = "serialize_protocol_v1"
    )]
    #[schemars(schema_with = "crate::validation::protocol_v1_schema")]
    pub v: u32,
    pub generation: u64,
    #[serde(
        deserialize_with = "deserialize_identifier",
        serialize_with = "serialize_identifier"
    )]
    #[schemars(schema_with = "crate::validation::opaque_id_128_schema")]
    pub subscription_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Gap {
    #[serde(
        deserialize_with = "deserialize_protocol_v1",
        serialize_with = "serialize_protocol_v1"
    )]
    #[schemars(schema_with = "crate::validation::protocol_v1_schema")]
    pub v: u32,
    pub generation: u64,
    #[serde(
        deserialize_with = "deserialize_identifier",
        serialize_with = "serialize_identifier"
    )]
    #[schemars(schema_with = "crate::validation::opaque_id_128_schema")]
    pub subscription_id: String,
    pub requested_cursor: u64,
    pub earliest_cursor: u64,
    pub latest_cursor: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Close {
    #[serde(
        deserialize_with = "deserialize_protocol_v1",
        serialize_with = "serialize_protocol_v1"
    )]
    #[schemars(schema_with = "crate::validation::protocol_v1_schema")]
    pub v: u32,
    pub generation: u64,
    pub code: PublicErrorCode,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BridgeError {
    #[serde(
        deserialize_with = "deserialize_protocol_v1",
        serialize_with = "serialize_protocol_v1"
    )]
    #[schemars(schema_with = "crate::validation::protocol_v1_schema")]
    pub v: u32,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_identifier",
        serialize_with = "serialize_optional_identifier"
    )]
    #[schemars(schema_with = "crate::validation::optional_opaque_id_128_schema")]
    pub id: Option<String>,
    pub generation: u64,
    pub code: PublicErrorCode,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_identifier",
        serialize_with = "serialize_optional_identifier"
    )]
    #[schemars(schema_with = "crate::validation::optional_opaque_id_128_schema")]
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
    validate_namespace(&value).map_err(de::Error::custom)?;
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
    let value = String::deserialize(deserializer)?;
    if value.len() > MAX_ID_BYTES || !is_canonical_dotted_id(&value, true) {
        return Err(de::Error::custom("invalid plugin id"));
    }
    Ok(value)
}

fn deserialize_digest<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    validate_digest(&value).map_err(de::Error::custom)?;
    Ok(value)
}

fn deserialize_grants<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<String>::deserialize(deserializer)?;
    validate_grants(&values).map_err(de::Error::custom)?;
    Ok(values)
}

fn serialize_protocol_v1<S>(value: &u32, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if *value != BRIDGE_PROTOCOL_V1 {
        return Err(ser::Error::custom("bridge protocol incompatible"));
    }
    value.serialize(serializer)
}

fn serialize_deadline<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if *value == 0 || *value > MAX_REQUEST_DEADLINE_MS {
        return Err(ser::Error::custom("invalid bridge deadline"));
    }
    value.serialize(serializer)
}

fn serialize_identifier<S>(value: &String, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    validate_ascii_token(value.clone(), MAX_ID_BYTES).map_err(ser::Error::custom)?;
    value.serialize(serializer)
}

fn serialize_optional_identifier<S>(
    value: &Option<String>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if let Some(value) = value {
        validate_ascii_token(value.clone(), MAX_ID_BYTES).map_err(ser::Error::custom)?;
    }
    value.serialize(serializer)
}

fn serialize_namespace<S>(value: &String, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    validate_namespace(value).map_err(ser::Error::custom)?;
    value.serialize(serializer)
}

fn serialize_plugin_id<S>(value: &String, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if value.len() > MAX_ID_BYTES || !is_canonical_dotted_id(value, true) {
        return Err(ser::Error::custom("invalid plugin id"));
    }
    value.serialize(serializer)
}

fn serialize_digest<S>(value: &String, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    validate_digest(value).map_err(ser::Error::custom)?;
    value.serialize(serializer)
}

fn serialize_grants<S>(values: &Vec<String>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    validate_grants(values).map_err(ser::Error::custom)?;
    values.serialize(serializer)
}

fn validate_ascii_token(value: String, max_bytes: usize) -> Result<String, &'static str> {
    if value.is_empty()
        || value.len() > max_bytes
        || !is_safe_opaque_identifier(&value)
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'/' | b'-' | b'@')
        })
    {
        return Err("invalid bridge token");
    }
    Ok(value)
}

fn validate_namespace(value: &str) -> Result<(), &'static str> {
    validate_ascii_token(value.to_owned(), MAX_ID_BYTES)?;
    if !value.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
    }) {
        return Err("invalid bridge namespace");
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), &'static str> {
    let valid = value
        .strip_prefix("sha256:")
        .map(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
        .unwrap_or(false);
    if valid {
        Ok(())
    } else {
        Err("invalid sha256 digest")
    }
}

fn validate_grants(values: &[String]) -> Result<(), &'static str> {
    if values.len() > MAX_GRANTS {
        return Err("too many grants");
    }
    for value in values {
        validate_ascii_token(value.clone(), MAX_ID_BYTES)?;
        if !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        }) {
            return Err("invalid grant");
        }
    }
    Ok(())
}
