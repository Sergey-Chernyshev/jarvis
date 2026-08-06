use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;

use jarvis_plugin_protocol::bridge::{BridgeClientFrame, BridgeHostFrame};
use jarvis_plugin_protocol::broker::{
    CommandResult, ContractRef, EntityEnvelope, EntityMutation, EntityQuery, EntityQuerySnapshot,
    EntitySelector, EntityWatchRequest, EventEnvelope, EventMutation, EventWatchRequest,
    FieldProjection, OutboxAck, OutboxBatch, RuntimeOperationQuery, RuntimeOperationView,
    RuntimeOperationWatch, TypedCommandInvocation,
};
use jarvis_plugin_protocol::contribution::{
    ResolvedCommandContribution, ResolvedContributions, ResolvedHotkeyContribution,
    ResolvedPageContribution,
};
use jarvis_plugin_protocol::settings::SettingRecord;

const CORPUS: &str = include_str!("fixtures/public-contract-parity-v1.json");

#[derive(Debug, Deserialize)]
struct Corpus {
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    name: String,
    schema: String,
    target: String,
    valid: bool,
    value: Value,
    repeat: Option<Repeat>,
    repeat_string: Option<RepeatString>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repeat {
    pointer: String,
    count: usize,
    index_pointer: Option<String>,
    prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RepeatString {
    pointer: String,
    value: String,
    count: usize,
}

#[test]
fn shared_public_contract_corpus_matches_rust_serde() {
    let corpus: Corpus = serde_json::from_str(CORPUS).expect("parse parity corpus");
    let mut mismatches = Vec::new();

    for case in corpus.cases {
        let value = expanded_value(&case);
        let accepted = accepts_target(&case.target, value);
        if accepted != case.valid {
            mismatches.push(format!(
                "{} [{}:{}] expected valid={}, serde accepted={accepted}",
                case.name, case.schema, case.target, case.valid
            ));
        }
    }

    assert!(
        mismatches.is_empty(),
        "Rust serde diverged from the shared public contract corpus:\n{}",
        mismatches.join("\n")
    );
}

fn accepts_target(target: &str, value: Value) -> bool {
    match target {
        "BridgeClientFrame" => accepts::<BridgeClientFrame>(value),
        "BridgeHostFrame" => accepts::<BridgeHostFrame>(value),
        "CommandResult" => accepts::<CommandResult>(value),
        "ContractRef" => accepts::<ContractRef>(value),
        "EntityEnvelope" => accepts::<EntityEnvelope>(value),
        "EntityMutation" => accepts::<EntityMutation>(value),
        "EntityQuery" => accepts::<EntityQuery>(value),
        "EntityQuerySnapshot" => accepts::<EntityQuerySnapshot>(value),
        "EntitySelector" => accepts::<EntitySelector>(value),
        "EntityWatchRequest" => accepts::<EntityWatchRequest>(value),
        "EventEnvelope" => accepts::<EventEnvelope>(value),
        "EventMutation" => accepts::<EventMutation>(value),
        "EventWatchRequest" => accepts::<EventWatchRequest>(value),
        "FieldProjection" => accepts::<FieldProjection>(value),
        "OutboxAck" => accepts::<OutboxAck>(value),
        "OutboxBatch" => accepts::<OutboxBatch>(value),
        "RuntimeOperationQuery" => accepts::<RuntimeOperationQuery>(value),
        "RuntimeOperationView" => accepts::<RuntimeOperationView>(value),
        "RuntimeOperationWatch" => accepts::<RuntimeOperationWatch>(value),
        "TypedCommandInvocation" => accepts::<TypedCommandInvocation>(value),
        "ResolvedCommandContribution" => accepts::<ResolvedCommandContribution>(value),
        "ResolvedContributions" => accepts::<ResolvedContributions>(value),
        "ResolvedHotkeyContribution" => accepts::<ResolvedHotkeyContribution>(value),
        "ResolvedPageContribution" => accepts::<ResolvedPageContribution>(value),
        "SettingRecord" => accepts::<SettingRecord>(value),
        unknown => panic!("unknown Rust parity target {unknown}"),
    }
}

fn accepts<T: DeserializeOwned>(value: Value) -> bool {
    serde_json::from_value::<T>(value).is_ok()
}

fn expanded_value(case: &Case) -> Value {
    let mut value = case.value.clone();
    if let Some(repeat) = &case.repeat {
        let template = value
            .pointer(&repeat.pointer)
            .and_then(Value::as_array)
            .and_then(|values| values.first())
            .cloned()
            .unwrap_or_else(|| panic!("{} repeat template at {}", case.name, repeat.pointer));
        let values = (0..repeat.count)
            .map(|index| {
                let mut item = template.clone();
                if let (Some(pointer), Some(prefix)) = (&repeat.index_pointer, &repeat.prefix) {
                    *item.pointer_mut(pointer).unwrap_or_else(|| {
                        panic!("{} repeat index pointer {pointer}", case.name)
                    }) = Value::String(format!("{prefix}{index}"));
                }
                item
            })
            .collect();
        *value
            .pointer_mut(&repeat.pointer)
            .unwrap_or_else(|| panic!("{} repeat pointer {}", case.name, repeat.pointer)) =
            Value::Array(values);
    }
    if let Some(repeat) = &case.repeat_string {
        *value
            .pointer_mut(&repeat.pointer)
            .unwrap_or_else(|| panic!("{} repeat string pointer {}", case.name, repeat.pointer)) =
            Value::String(repeat.value.repeat(repeat.count));
    }
    value
}
