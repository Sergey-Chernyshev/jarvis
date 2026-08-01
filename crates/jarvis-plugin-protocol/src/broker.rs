use schemars::JsonSchema;
use semver::Version;
use serde::de;
use serde::ser;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::error::PublicErrorCode;
use crate::manifest::Risk;
use crate::operation::OperationRef;
use crate::validation::{is_canonical_contract_name, is_safe_opaque_identifier};

pub const MAX_ENTITY_BYTES: usize = 256 * 1024;
pub const MAX_EVENT_BYTES: usize = 128 * 1024;
pub const MAX_BROKER_BATCH_ITEMS: usize = 128;
pub const MAX_PROJECTION_FIELDS: usize = 64;

const MAX_CONTRACT_ID_BYTES: usize = 256;
const MAX_ID_BYTES: usize = 256;
const MAX_PHASE_BYTES: usize = 128;
#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContractRef {
    #[serde(
        deserialize_with = "deserialize_contract_id",
        serialize_with = "serialize_contract_id"
    )]
    #[schemars(schema_with = "crate::validation::contract_id_256_schema")]
    pub id: String,
    pub version: Version,
    #[serde(
        deserialize_with = "deserialize_digest",
        serialize_with = "serialize_digest"
    )]
    #[schemars(schema_with = "crate::validation::sha256_digest_schema")]
    pub schema_digest: String,
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntityEnvelope {
    pub contract: ContractRef,
    #[serde(deserialize_with = "deserialize_id", serialize_with = "serialize_id")]
    #[schemars(schema_with = "crate::validation::opaque_id_256_schema")]
    pub id: String,
    pub revision: u64,
    pub broker_revision: u64,
    #[serde(
        deserialize_with = "deserialize_state",
        serialize_with = "serialize_state"
    )]
    #[schemars(schema_with = "crate::validation::opaque_id_128_schema")]
    pub state: String,
    #[serde(
        deserialize_with = "deserialize_entity_value",
        serialize_with = "serialize_entity_value"
    )]
    #[schemars(schema_with = "crate::validation::entity_value_schema")]
    pub data: Value,
    pub updated_at_ms: i64,
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventEnvelope {
    pub contract: ContractRef,
    #[serde(deserialize_with = "deserialize_id", serialize_with = "serialize_id")]
    #[schemars(schema_with = "crate::validation::opaque_id_256_schema")]
    pub stream_id: String,
    #[serde(deserialize_with = "deserialize_id", serialize_with = "serialize_id")]
    #[schemars(schema_with = "crate::validation::opaque_id_256_schema")]
    pub event_id: String,
    pub seq: u64,
    #[serde(deserialize_with = "deserialize_id", serialize_with = "serialize_id")]
    #[schemars(schema_with = "crate::validation::opaque_id_256_schema")]
    pub subject: String,
    #[serde(
        deserialize_with = "deserialize_state",
        serialize_with = "serialize_state"
    )]
    #[schemars(schema_with = "crate::validation::opaque_id_128_schema")]
    pub kind: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_id",
        serialize_with = "serialize_optional_id"
    )]
    #[schemars(schema_with = "crate::validation::optional_opaque_id_256_schema")]
    pub correlation_id: Option<String>,
    #[serde(
        deserialize_with = "deserialize_event_value",
        serialize_with = "serialize_event_value"
    )]
    #[schemars(schema_with = "crate::validation::event_value_schema")]
    pub data: Value,
    pub at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntitySelector {
    pub contract: ContractRef,
    #[serde(
        default,
        deserialize_with = "deserialize_ids",
        serialize_with = "serialize_ids"
    )]
    #[schemars(schema_with = "crate::validation::opaque_ids_256_schema")]
    pub ids: Vec<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_states",
        serialize_with = "serialize_states"
    )]
    #[schemars(schema_with = "crate::validation::opaque_states_128_schema")]
    pub states: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FieldProjection {
    #[serde(
        deserialize_with = "deserialize_projection_fields",
        serialize_with = "serialize_projection_fields"
    )]
    #[schemars(schema_with = "crate::validation::projection_fields_256_schema")]
    pub fields: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EntityMutation {
    Put {
        contract: ContractRef,
        #[serde(deserialize_with = "deserialize_id", serialize_with = "serialize_id")]
        #[schemars(schema_with = "crate::validation::opaque_id_256_schema")]
        id: String,
        #[schemars(rename = "expectedRevision")]
        expected_revision: u64,
        #[serde(
            deserialize_with = "deserialize_entity_value",
            serialize_with = "serialize_entity_value"
        )]
        #[schemars(schema_with = "crate::validation::entity_value_schema")]
        data: Value,
    },
    Delete {
        contract: ContractRef,
        #[serde(deserialize_with = "deserialize_id", serialize_with = "serialize_id")]
        #[schemars(schema_with = "crate::validation::opaque_id_256_schema")]
        id: String,
        #[schemars(rename = "expectedRevision")]
        expected_revision: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntityQuery {
    #[serde(
        deserialize_with = "deserialize_selectors",
        serialize_with = "serialize_selectors"
    )]
    #[schemars(length(min = 1, max = 128))]
    pub selectors: Vec<EntitySelector>,
    pub projection: Option<FieldProjection>,
    #[serde(
        deserialize_with = "deserialize_limit",
        serialize_with = "serialize_limit"
    )]
    #[schemars(schema_with = "crate::validation::broker_limit_schema")]
    pub limit: u32,
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntityQuerySnapshot {
    pub snapshot_revision: u64,
    #[serde(
        deserialize_with = "deserialize_entities",
        serialize_with = "serialize_entities"
    )]
    #[schemars(length(max = 128))]
    pub entities: Vec<EntityEnvelope>,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntityWatchRequest {
    pub cursor: u64,
    #[serde(
        deserialize_with = "deserialize_selectors",
        serialize_with = "serialize_selectors"
    )]
    #[schemars(length(min = 1, max = 128))]
    pub selectors: Vec<EntitySelector>,
    pub projection: Option<FieldProjection>,
    #[serde(
        deserialize_with = "deserialize_limit",
        serialize_with = "serialize_limit"
    )]
    #[schemars(schema_with = "crate::validation::broker_limit_schema")]
    pub limit: u32,
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntityChange {
    pub cursor: u64,
    pub entity: EntityEnvelope,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CursorGap {
    pub requested_cursor: u64,
    pub earliest_cursor: u64,
    pub latest_cursor: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventWatchRequest {
    pub cursor: u64,
    pub contract: ContractRef,
    #[serde(
        default,
        deserialize_with = "deserialize_ids",
        serialize_with = "serialize_ids"
    )]
    #[schemars(schema_with = "crate::validation::opaque_ids_256_schema")]
    pub subjects: Vec<String>,
    #[serde(
        deserialize_with = "deserialize_limit",
        serialize_with = "serialize_limit"
    )]
    #[schemars(schema_with = "crate::validation::broker_limit_schema")]
    pub limit: u32,
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventChange {
    pub cursor: u64,
    pub event: EventEnvelope,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationSubjectRef {
    pub contract: ContractRef,
    #[serde(deserialize_with = "deserialize_id", serialize_with = "serialize_id")]
    #[schemars(schema_with = "crate::validation::opaque_id_256_schema")]
    pub subject_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeOperationError {
    pub code: PublicErrorCode,
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeOperationView {
    pub operation_ref: OperationRef,
    pub subject: OperationSubjectRef,
    pub exact_command: ContractRef,
    pub state: RuntimeOperationState,
    #[serde(
        deserialize_with = "deserialize_phase",
        serialize_with = "serialize_state"
    )]
    #[schemars(schema_with = "crate::validation::opaque_id_128_schema")]
    pub phase: String,
    pub provider_generation: u64,
    pub created_at: i64,
    pub updated_at: i64,
    pub deadline_at: i64,
    pub error: Option<RuntimeOperationError>,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeOperationQuery {
    #[serde(
        deserialize_with = "deserialize_subjects",
        serialize_with = "serialize_subjects"
    )]
    #[schemars(length(min = 1, max = 128))]
    pub subjects: Vec<OperationSubjectRef>,
    pub include_terminal_since: Option<i64>,
    #[serde(
        deserialize_with = "deserialize_limit",
        serialize_with = "serialize_limit"
    )]
    #[schemars(schema_with = "crate::validation::broker_limit_schema")]
    pub limit: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeOperationWatch {
    pub cursor: u64,
    #[serde(
        deserialize_with = "deserialize_subjects",
        serialize_with = "serialize_subjects"
    )]
    #[schemars(length(min = 1, max = 128))]
    pub subjects: Vec<OperationSubjectRef>,
    #[serde(
        deserialize_with = "deserialize_limit",
        serialize_with = "serialize_limit"
    )]
    #[schemars(schema_with = "crate::validation::broker_limit_schema")]
    pub limit: u32,
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeOperationChange {
    pub cursor: u64,
    pub operation: RuntimeOperationView,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeOperationGap {
    pub requested_cursor: u64,
    pub earliest_cursor: u64,
    pub latest_cursor: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeOperationCancel {
    pub operation_ref: OperationRef,
    pub expected_state_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypedCommandDeclaration {
    pub command: ContractRef,
    pub risk_floor: Risk,
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypedCommandInvocation {
    pub command: ContractRef,
    pub subject: OperationSubjectRef,
    #[serde(
        deserialize_with = "deserialize_command_value",
        serialize_with = "serialize_entity_value"
    )]
    #[schemars(schema_with = "crate::validation::entity_value_schema")]
    pub args: Value,
    #[serde(
        deserialize_with = "deserialize_deadline",
        serialize_with = "serialize_deadline"
    )]
    #[schemars(schema_with = "crate::validation::command_deadline_schema")]
    pub deadline_ms: u64,
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum CommandResult {
    Completed {
        #[serde(
            deserialize_with = "deserialize_command_value",
            serialize_with = "serialize_entity_value"
        )]
        #[schemars(schema_with = "crate::validation::entity_value_schema")]
        result: Value,
    },
    Accepted {
        #[schemars(rename = "operationRef")]
        operation_ref: OperationRef,
    },
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventMutation {
    pub contract: ContractRef,
    #[serde(deserialize_with = "deserialize_id", serialize_with = "serialize_id")]
    #[schemars(schema_with = "crate::validation::opaque_id_256_schema")]
    pub stream_id: String,
    #[serde(deserialize_with = "deserialize_id", serialize_with = "serialize_id")]
    #[schemars(schema_with = "crate::validation::opaque_id_256_schema")]
    pub event_id: String,
    #[serde(deserialize_with = "deserialize_id", serialize_with = "serialize_id")]
    #[schemars(schema_with = "crate::validation::opaque_id_256_schema")]
    pub subject: String,
    #[serde(
        deserialize_with = "deserialize_state",
        serialize_with = "serialize_state"
    )]
    #[schemars(schema_with = "crate::validation::opaque_id_128_schema")]
    pub kind: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_id",
        serialize_with = "serialize_optional_id"
    )]
    #[schemars(schema_with = "crate::validation::optional_opaque_id_256_schema")]
    pub correlation_id: Option<String>,
    #[serde(
        deserialize_with = "deserialize_event_value",
        serialize_with = "serialize_event_value"
    )]
    #[schemars(schema_with = "crate::validation::event_value_schema")]
    pub data: Value,
    pub at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum OutboxMutation {
    Entity { mutation: EntityMutation },
    Event { event: EventMutation },
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutboxBatch {
    #[serde(deserialize_with = "deserialize_id", serialize_with = "serialize_id")]
    #[schemars(schema_with = "crate::validation::opaque_id_256_schema")]
    pub source_instance_id: String,
    #[serde(deserialize_with = "deserialize_id", serialize_with = "serialize_id")]
    #[schemars(schema_with = "crate::validation::opaque_id_256_schema")]
    pub outbox_id: String,
    #[serde(
        deserialize_with = "deserialize_outbox_mutations",
        serialize_with = "serialize_outbox_mutations"
    )]
    #[schemars(length(min = 1, max = 128))]
    pub mutations: Vec<OutboxMutation>,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutboxAck {
    #[serde(deserialize_with = "deserialize_id", serialize_with = "serialize_id")]
    #[schemars(schema_with = "crate::validation::opaque_id_256_schema")]
    pub source_instance_id: String,
    #[serde(deserialize_with = "deserialize_id", serialize_with = "serialize_id")]
    #[schemars(schema_with = "crate::validation::opaque_id_256_schema")]
    pub outbox_id: String,
    #[serde(
        deserialize_with = "deserialize_digest",
        serialize_with = "serialize_digest"
    )]
    #[schemars(schema_with = "crate::validation::sha256_digest_schema")]
    pub payload_digest: String,
    pub applied_broker_revision: u64,
    #[serde(
        deserialize_with = "deserialize_operation_refs",
        serialize_with = "serialize_operation_refs"
    )]
    #[schemars(length(max = 128))]
    pub accepted_operation_refs: Vec<OperationRef>,
}

fn deserialize_contract_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.len() > MAX_CONTRACT_ID_BYTES || !is_canonical_contract_name(&value) {
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
    validate_bounded_value(&value, maximum_bytes).map_err(de::Error::custom)?;
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

fn deserialize_outbox_mutations<'de, D>(deserializer: D) -> Result<Vec<OutboxMutation>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<OutboxMutation>::deserialize(deserializer)?;
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

fn serialize_id<S>(value: &String, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    validate_token(value.clone(), MAX_ID_BYTES).map_err(ser::Error::custom)?;
    value.serialize(serializer)
}

fn serialize_optional_id<S>(value: &Option<String>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if let Some(value) = value {
        validate_token(value.clone(), MAX_ID_BYTES).map_err(ser::Error::custom)?;
    }
    value.serialize(serializer)
}

fn serialize_state<S>(value: &String, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    validate_token(value.clone(), MAX_PHASE_BYTES).map_err(ser::Error::custom)?;
    value.serialize(serializer)
}

fn serialize_entity_value<S>(value: &Value, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serialize_bounded_value(value, serializer, MAX_ENTITY_BYTES)
}

fn serialize_event_value<S>(value: &Value, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serialize_bounded_value(value, serializer, MAX_EVENT_BYTES)
}

fn serialize_bounded_value<S>(
    value: &Value,
    serializer: S,
    maximum_bytes: usize,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    validate_bounded_value(value, maximum_bytes).map_err(ser::Error::custom)?;
    value.serialize(serializer)
}

fn serialize_projection_fields<S>(fields: &Vec<String>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if fields.is_empty() || fields.len() > MAX_PROJECTION_FIELDS {
        return Err(ser::Error::custom("invalid field projection"));
    }
    for field in fields {
        validate_token(field.clone(), MAX_ID_BYTES).map_err(ser::Error::custom)?;
    }
    fields.serialize(serializer)
}

fn serialize_ids<S>(values: &Vec<String>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if values.len() > MAX_BROKER_BATCH_ITEMS {
        return Err(ser::Error::custom("too many ids"));
    }
    for value in values {
        validate_token(value.clone(), MAX_ID_BYTES).map_err(ser::Error::custom)?;
    }
    values.serialize(serializer)
}

fn serialize_states<S>(values: &Vec<String>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if values.len() > MAX_BROKER_BATCH_ITEMS {
        return Err(ser::Error::custom("too many states"));
    }
    for value in values {
        validate_token(value.clone(), MAX_PHASE_BYTES).map_err(ser::Error::custom)?;
    }
    values.serialize(serializer)
}

fn serialize_selectors<S>(values: &Vec<EntitySelector>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serialize_bounded_nonempty_broker_vec(values, serializer, "selectors")
}

fn serialize_entities<S>(values: &Vec<EntityEnvelope>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if values.len() > MAX_BROKER_BATCH_ITEMS {
        return Err(ser::Error::custom("too many entities"));
    }
    values.serialize(serializer)
}

fn serialize_subjects<S>(
    values: &Vec<OperationSubjectRef>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serialize_bounded_nonempty_broker_vec(values, serializer, "operation subjects")
}

fn serialize_outbox_mutations<S>(
    values: &Vec<OutboxMutation>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serialize_bounded_nonempty_broker_vec(values, serializer, "outbox mutations")
}

fn serialize_operation_refs<S>(values: &Vec<OperationRef>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if values.len() > MAX_BROKER_BATCH_ITEMS {
        return Err(ser::Error::custom("too many operation refs"));
    }
    values.serialize(serializer)
}

fn serialize_bounded_nonempty_broker_vec<S, T>(
    values: &Vec<T>,
    serializer: S,
    label: &str,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: Serialize,
{
    if values.is_empty() || values.len() > MAX_BROKER_BATCH_ITEMS {
        return Err(ser::Error::custom(format!("invalid {label}")));
    }
    values.serialize(serializer)
}

fn serialize_limit<S>(value: &u32, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if *value == 0 || *value as usize > MAX_BROKER_BATCH_ITEMS {
        return Err(ser::Error::custom("invalid broker limit"));
    }
    value.serialize(serializer)
}

fn serialize_deadline<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if *value == 0 || *value > 30_000 {
        return Err(ser::Error::custom("invalid command deadline"));
    }
    value.serialize(serializer)
}

fn serialize_contract_id<S>(value: &String, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if value.len() > MAX_CONTRACT_ID_BYTES || !is_canonical_contract_name(value) {
        return Err(ser::Error::custom("invalid contract id"));
    }
    value.serialize(serializer)
}

fn serialize_digest<S>(value: &String, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
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
        return Err(ser::Error::custom("invalid schema digest"));
    }
    value.serialize(serializer)
}

fn validate_token(value: String, maximum_bytes: usize) -> Result<String, &'static str> {
    if value.is_empty()
        || value.len() > maximum_bytes
        || !is_safe_opaque_identifier(&value)
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'/' | b'-' | b'@')
        })
    {
        return Err("invalid broker token");
    }
    Ok(value)
}

fn validate_bounded_value(value: &Value, maximum_bytes: usize) -> Result<(), &'static str> {
    let Ok(bytes) = serde_json::to_vec(value) else {
        return Err("invalid broker value");
    };
    if bytes.len() > maximum_bytes {
        Err("broker value too large")
    } else {
        Ok(())
    }
}
