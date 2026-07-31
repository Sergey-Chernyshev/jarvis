#![forbid(unsafe_code)]

use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use jarvis_plugin_protocol::bridge::{BridgeClientFrame, BridgeHostFrame};
use jarvis_plugin_protocol::broker::{
    CommandResult, ContractRef, CursorGap, EntityChange, EntityEnvelope, EntityMutation,
    EntityQuery, EntityQuerySnapshot, EntitySelector, EntityWatchRequest, EventChange,
    EventEnvelope, EventMutation, EventWatchRequest, FieldProjection, OperationSubjectRef,
    OutboxAck, OutboxBatch, OutboxMutation, RuntimeOperationCancel, RuntimeOperationChange,
    RuntimeOperationGap, RuntimeOperationQuery, RuntimeOperationView, RuntimeOperationWatch,
    TypedCommandDeclaration, TypedCommandInvocation,
};
use jarvis_plugin_protocol::contribution::ResolvedContributions;
use jarvis_plugin_protocol::settings::{SettingChange, SettingRecord, SettingValue, SettingWrite};
use schemars::{schema_for, JsonSchema};
use serde_json::{Map, Value};

#[derive(JsonSchema)]
#[schemars(rename_all = "camelCase")]
#[allow(dead_code)]
struct BridgeContractV1 {
    client_frame: BridgeClientFrame,
    host_frame: BridgeHostFrame,
}

#[derive(JsonSchema)]
#[schemars(rename_all = "camelCase")]
#[allow(dead_code)]
struct BrokerContractV1 {
    contract_ref: ContractRef,
    entity_envelope: EntityEnvelope,
    event_envelope: EventEnvelope,
    entity_selector: EntitySelector,
    field_projection: FieldProjection,
    entity_mutation: EntityMutation,
    entity_query: EntityQuery,
    entity_query_snapshot: EntityQuerySnapshot,
    entity_watch_request: EntityWatchRequest,
    entity_change: EntityChange,
    cursor_gap: CursorGap,
    event_watch_request: EventWatchRequest,
    event_change: EventChange,
    operation_subject_ref: OperationSubjectRef,
    runtime_operation_view: RuntimeOperationView,
    runtime_operation_query: RuntimeOperationQuery,
    runtime_operation_watch: RuntimeOperationWatch,
    runtime_operation_change: RuntimeOperationChange,
    runtime_operation_gap: RuntimeOperationGap,
    runtime_operation_cancel: RuntimeOperationCancel,
    typed_command_declaration: TypedCommandDeclaration,
    typed_command_invocation: TypedCommandInvocation,
    command_result: CommandResult,
    event_mutation: EventMutation,
    outbox_mutation: OutboxMutation,
    outbox_batch: OutboxBatch,
    outbox_ack: OutboxAck,
}

#[derive(JsonSchema)]
#[schemars(rename_all = "camelCase")]
#[allow(dead_code)]
struct SettingsContractV1 {
    value: SettingValue,
    record: SettingRecord,
    write: SettingWrite,
    change: SettingChange,
}

fn main() -> Result<(), Box<dyn Error>> {
    let output_dir = output_dir(env::args().skip(1))?;
    fs::create_dir_all(&output_dir)?;

    write_schema::<BrokerContractV1>(&output_dir, "plugin-broker-v1.schema.json")?;
    write_schema::<BridgeContractV1>(&output_dir, "plugin-ui-bridge-v1.schema.json")?;
    write_schema::<ResolvedContributions>(&output_dir, "plugin-contribution-v1.schema.json")?;
    write_schema::<SettingsContractV1>(&output_dir, "plugin-settings-v1.schema.json")?;
    Ok(())
}

fn output_dir(arguments: impl Iterator<Item = String>) -> Result<PathBuf, Box<dyn Error>> {
    let mut output_dir = PathBuf::from("schemas");
    let mut arguments = arguments.peekable();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--out-dir" => {
                output_dir = arguments.next().ok_or("--out-dir requires a path")?.into();
            }
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }
    Ok(output_dir)
}

fn write_schema<T: JsonSchema>(output_dir: &Path, filename: &str) -> Result<(), Box<dyn Error>> {
    let schema = serde_json::to_value(schema_for!(T))?;
    let mut bytes = serde_json::to_vec_pretty(&sort_json(close_typed_objects(schema)))?;
    bytes.push(b'\n');
    fs::write(output_dir.join(filename), bytes)?;
    Ok(())
}

fn close_typed_objects(value: Value) -> Value {
    match value {
        Value::Object(mut object) => {
            for child in object.values_mut() {
                *child = close_typed_objects(child.take());
            }

            if object.contains_key("properties") && !object.contains_key("additionalProperties") {
                object.insert("additionalProperties".to_owned(), Value::Bool(false));
            }

            Value::Object(object)
        }
        Value::Array(values) => Value::Array(values.into_iter().map(close_typed_objects).collect()),
        primitive => primitive,
    }
}

fn sort_json(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries: Vec<_> = object.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, sort_json(value)))
                    .collect::<Map<_, _>>(),
            )
        }
        Value::Array(values) => Value::Array(values.into_iter().map(sort_json).collect()),
        primitive => primitive,
    }
}
