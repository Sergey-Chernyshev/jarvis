use semver::Version;
use serde::de;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::manifest::Risk;
use crate::operation::OperationRef;

pub const MAX_ENTITY_BYTES: usize = 256 * 1024;
pub const MAX_EVENT_BYTES: usize = 128 * 1024;
pub const MAX_BROKER_BATCH_ITEMS: usize = 128;
pub const MAX_PROJECTION_FIELDS: usize = 64;

const MAX_CONTRACT_ID_BYTES: usize = 256;
const MAX_ID_BYTES: usize = 256;
const MAX_PHASE_BYTES: usize = 128;
const MAX_ERROR_MESSAGE_BYTES: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContractRef {
    #[serde(deserialize_with = "deserialize_contract_id")]
    pub id: String,
    pub version: Version,
    #[serde(deserialize_with = "deserialize_digest")]
    pub schema_digest: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntityEnvelope {
    pub contract: ContractRef,
    #[serde(deserialize_with = "deserialize_id")]
    pub id: String,
    pub revision: u64,
    pub broker_revision: u64,
    #[serde(deserialize_with = "deserialize_state")]
    pub state: String,
    #[serde(deserialize_with = "deserialize_entity_value")]
    pub data: Value,
    pub updated_at_ms: i64,
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventEnvelope {
    pub contract: ContractRef,
    #[serde(deserialize_with = "deserialize_id")]
    pub stream_id: String,
    #[serde(deserialize_with = "deserialize_id")]
    pub event_id: String,
    pub seq: u64,
    #[serde(deserialize_with = "deserialize_id")]
    pub subject: String,
    #[serde(deserialize_with = "deserialize_state")]
    pub kind: String,
    #[serde(default, deserialize_with = "deserialize_optional_id")]
    pub correlation_id: Option<String>,
    #[serde(deserialize_with = "deserialize_event_value")]
    pub data: Value,
    pub at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntitySelector {
    pub contract: ContractRef,
    #[serde(default, deserialize_with = "deserialize_ids")]
    pub ids: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_states")]
    pub states: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FieldProjection {
    #[serde(deserialize_with = "deserialize_projection_fields")]
    pub fields: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EntityMutation {
    Put {
        contract: ContractRef,
        #[serde(deserialize_with = "deserialize_id")]
        id: String,
        expected_revision: u64,
        #[serde(deserialize_with = "deserialize_entity_value")]
        data: Value,
    },
    Delete {
        contract: ContractRef,
        #[serde(deserialize_with = "deserialize_id")]
        id: String,
        expected_revision: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntityQuery {
    #[serde(deserialize_with = "deserialize_selectors")]
    pub selectors: Vec<EntitySelector>,
    pub projection: Option<FieldProjection>,
    #[serde(deserialize_with = "deserialize_limit")]
    pub limit: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntityQuerySnapshot {
    pub snapshot_revision: u64,
    #[serde(deserialize_with = "deserialize_entities")]
    pub entities: Vec<EntityEnvelope>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntityWatchRequest {
    pub cursor: u64,
    #[serde(deserialize_with = "deserialize_selectors")]
    pub selectors: Vec<EntitySelector>,
    pub projection: Option<FieldProjection>,
    #[serde(deserialize_with = "deserialize_limit")]
    pub limit: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntityChange {
    pub cursor: u64,
    pub entity: EntityEnvelope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CursorGap {
    pub requested_cursor: u64,
    pub earliest_cursor: u64,
    pub latest_cursor: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventWatchRequest {
    pub cursor: u64,
    pub contract: ContractRef,
    #[serde(default, deserialize_with = "deserialize_ids")]
    pub subjects: Vec<String>,
    #[serde(deserialize_with = "deserialize_limit")]
    pub limit: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventChange {
    pub cursor: u64,
    pub event: EventEnvelope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationSubjectRef {
    pub contract: ContractRef,
    #[serde(deserialize_with = "deserialize_id")]
    pub subject_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeOperationState {
    Queued,
    Dispatching,
    Running,
    WaitingForProvider,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeOperationError {
    #[serde(deserialize_with = "deserialize_state")]
    pub code: String,
    #[serde(default, deserialize_with = "deserialize_optional_error_message")]
    pub message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeOperationView {
    pub operation_ref: OperationRef,
    pub subject: OperationSubjectRef,
    pub exact_command: ContractRef,
    pub state: RuntimeOperationState,
    #[serde(deserialize_with = "deserialize_phase")]
    pub phase: String,
    pub provider_generation: u64,
    pub created_at: i64,
    pub updated_at: i64,
    pub deadline_at: i64,
    pub error: Option<RuntimeOperationError>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeOperationQuery {
    #[serde(deserialize_with = "deserialize_subjects")]
    pub subjects: Vec<OperationSubjectRef>,
    pub include_terminal_since: Option<i64>,
    #[serde(deserialize_with = "deserialize_limit")]
    pub limit: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeOperationWatch {
    pub cursor: u64,
    #[serde(deserialize_with = "deserialize_subjects")]
    pub subjects: Vec<OperationSubjectRef>,
    #[serde(deserialize_with = "deserialize_limit")]
    pub limit: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeOperationChange {
    pub cursor: u64,
    pub operation: RuntimeOperationView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeOperationGap {
    pub requested_cursor: u64,
    pub earliest_cursor: u64,
    pub latest_cursor: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeOperationCancel {
    pub operation_ref: OperationRef,
    pub expected_state_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypedCommandDeclaration {
    pub command: ContractRef,
    pub risk_floor: Risk,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypedCommandInvocation {
    pub command: ContractRef,
    pub subject: OperationSubjectRef,
    #[serde(deserialize_with = "deserialize_command_value")]
    pub args: Value,
    #[serde(deserialize_with = "deserialize_deadline")]
    pub deadline_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum CommandResult {
    Completed {
        #[serde(deserialize_with = "deserialize_command_value")]
        result: Value,
    },
    Accepted {
        operation_ref: OperationRef,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutboxItem {
    pub operation_ref: OperationRef,
    pub invocation: TypedCommandInvocation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutboxBatch {
    #[serde(deserialize_with = "deserialize_id")]
    pub batch_id: String,
    pub cursor: u64,
    #[serde(deserialize_with = "deserialize_outbox_items")]
    pub items: Vec<OutboxItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutboxAck {
    #[serde(deserialize_with = "deserialize_id")]
    pub batch_id: String,
    pub cursor: u64,
    #[serde(deserialize_with = "deserialize_operation_refs")]
    pub accepted: Vec<OperationRef>,
}

fn deserialize_contract_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.is_empty() || value.len() > MAX_CONTRACT_ID_BYTES {
        return Err(de::Error::custom("invalid contract id"));
    }
    let Some((namespace, name)) = value.split_once('/') else {
        return Err(de::Error::custom("invalid contract id"));
    };
    if namespace.is_empty()
        || name.is_empty()
        || name.contains('/')
        || !namespace.contains('.')
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b'/' | b'-')
        })
    {
        return Err(de::Error::custom("invalid contract id"));
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
        return Err(de::Error::custom("invalid schema digest"));
    }
    Ok(value)
}

fn deserialize_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    validate_token(value, MAX_ID_BYTES).map_err(de::Error::custom)
}

fn deserialize_optional_id<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(|value| validate_token(value, MAX_ID_BYTES))
        .transpose()
        .map_err(de::Error::custom)
}

fn deserialize_state<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    validate_token(value, MAX_PHASE_BYTES).map_err(de::Error::custom)
}

fn deserialize_phase<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_state(deserializer)
}

fn deserialize_entity_value<'de, D>(deserializer: D) -> Result<Value, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_value(deserializer, MAX_ENTITY_BYTES)
}

fn deserialize_event_value<'de, D>(deserializer: D) -> Result<Value, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_value(deserializer, MAX_EVENT_BYTES)
}

fn deserialize_command_value<'de, D>(deserializer: D) -> Result<Value, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_value(deserializer, MAX_ENTITY_BYTES)
}

fn deserialize_bounded_value<'de, D>(
    deserializer: D,
    maximum_bytes: usize,
) -> Result<Value, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    let bytes = serde_json::to_vec(&value).map_err(de::Error::custom)?;
    if bytes.len() > maximum_bytes {
        return Err(de::Error::custom("broker value too large"));
    }
    Ok(value)
}

fn deserialize_projection_fields<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let fields = Vec::<String>::deserialize(deserializer)?;
    if fields.is_empty() || fields.len() > MAX_PROJECTION_FIELDS {
        return Err(de::Error::custom("invalid field projection"));
    }
    fields
        .into_iter()
        .map(|field| validate_token(field, MAX_ID_BYTES))
        .collect::<Result<Vec<_>, _>>()
        .map_err(de::Error::custom)
}

fn deserialize_ids<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<String>::deserialize(deserializer)?;
    if values.len() > MAX_BROKER_BATCH_ITEMS {
        return Err(de::Error::custom("too many ids"));
    }
    values
        .into_iter()
        .map(|value| validate_token(value, MAX_ID_BYTES))
        .collect::<Result<Vec<_>, _>>()
        .map_err(de::Error::custom)
}

fn deserialize_states<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<String>::deserialize(deserializer)?;
    if values.len() > MAX_BROKER_BATCH_ITEMS {
        return Err(de::Error::custom("too many states"));
    }
    values
        .into_iter()
        .map(|value| validate_token(value, MAX_PHASE_BYTES))
        .collect::<Result<Vec<_>, _>>()
        .map_err(de::Error::custom)
}

fn deserialize_selectors<'de, D>(deserializer: D) -> Result<Vec<EntitySelector>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<EntitySelector>::deserialize(deserializer)?;
    if values.is_empty() || values.len() > MAX_BROKER_BATCH_ITEMS {
        return Err(de::Error::custom("invalid selectors"));
    }
    Ok(values)
}

fn deserialize_entities<'de, D>(deserializer: D) -> Result<Vec<EntityEnvelope>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<EntityEnvelope>::deserialize(deserializer)?;
    if values.len() > MAX_BROKER_BATCH_ITEMS {
        return Err(de::Error::custom("too many entities"));
    }
    Ok(values)
}

fn deserialize_subjects<'de, D>(deserializer: D) -> Result<Vec<OperationSubjectRef>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<OperationSubjectRef>::deserialize(deserializer)?;
    if values.is_empty() || values.len() > MAX_BROKER_BATCH_ITEMS {
        return Err(de::Error::custom("invalid operation subjects"));
    }
    Ok(values)
}

fn deserialize_outbox_items<'de, D>(deserializer: D) -> Result<Vec<OutboxItem>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<OutboxItem>::deserialize(deserializer)?;
    if values.is_empty() || values.len() > MAX_BROKER_BATCH_ITEMS {
        return Err(de::Error::custom("invalid outbox batch"));
    }
    Ok(values)
}

fn deserialize_operation_refs<'de, D>(deserializer: D) -> Result<Vec<OperationRef>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<OperationRef>::deserialize(deserializer)?;
    if values.len() > MAX_BROKER_BATCH_ITEMS {
        return Err(de::Error::custom("too many operation refs"));
    }
    Ok(values)
}

fn deserialize_limit<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u32::deserialize(deserializer)?;
    if value == 0 || value as usize > MAX_BROKER_BATCH_ITEMS {
        return Err(de::Error::custom("invalid broker limit"));
    }
    Ok(value)
}

fn deserialize_deadline<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u64::deserialize(deserializer)?;
    if value == 0 || value > 30_000 {
        return Err(de::Error::custom("invalid command deadline"));
    }
    Ok(value)
}

fn deserialize_optional_error_message<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(|value| {
            if value.len() <= MAX_ERROR_MESSAGE_BYTES && !value.chars().any(char::is_control) {
                Ok(value)
            } else {
                Err("invalid operation error message")
            }
        })
        .transpose()
        .map_err(de::Error::custom)
}

fn validate_token(value: String, maximum_bytes: usize) -> Result<String, &'static str> {
    if value.is_empty()
        || value.len() > maximum_bytes
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-' | b'@')
        })
    {
        return Err("invalid broker token");
    }
    Ok(value)
}
