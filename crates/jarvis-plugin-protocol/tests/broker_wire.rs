use jarvis_plugin_protocol::broker::{
    CommandResult, ContractRef, CursorGap, EntityEnvelope, EntityMutation, EntityQuerySnapshot,
    EventEnvelope, EventMutation, FieldProjection, OperationSubjectRef, OutboxAck, OutboxBatch,
    OutboxMutation, RuntimeOperationCancel, RuntimeOperationChange, RuntimeOperationGap,
    RuntimeOperationQuery, RuntimeOperationState, RuntimeOperationView, RuntimeOperationWatch,
    TypedCommandInvocation,
};
use jarvis_plugin_protocol::operation::OperationRef;
use serde_json::{json, Value};

const GOLDEN: &str = include_str!("fixtures/ui-contracts-v1.json");
const DIGEST_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DIGEST_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn golden_contract() -> Value {
    serde_json::from_str::<Value>(GOLDEN).unwrap()["contractRef"].clone()
}

fn contract() -> ContractRef {
    serde_json::from_value(golden_contract()).unwrap()
}

fn subject() -> OperationSubjectRef {
    OperationSubjectRef {
        contract: contract(),
        subject_id: "runtime/01".to_string(),
    }
}

#[test]
fn contract_ref_preserves_full_semver_and_exact_schema_digest() {
    let contract = contract();
    assert_eq!(contract.id, "dev.example/runtime");
    assert_eq!(contract.version.to_string(), "1.2.3-alpha.1+build.7");
    assert_eq!(contract.schema_digest, DIGEST_B);
    assert_eq!(serde_json::to_value(contract).unwrap(), golden_contract());

    for invalid in [
        json!({"id":"runtime","version":"1.2.3","schemaDigest":DIGEST_B}),
        json!({"id":"dev.example/runtime","version":"1.2","schemaDigest":DIGEST_B}),
        json!({"id":"dev.example/runtime","version":"1.2.3","schemaDigest":"sha256:ABC"}),
    ] {
        assert!(serde_json::from_value::<ContractRef>(invalid).is_err());
    }
}

#[test]
fn entity_mutation_carries_revision_but_no_owner_identity() {
    let mutation: EntityMutation = serde_json::from_value(json!({
        "type": "put",
        "contract": golden_contract(),
        "id": "runtime/01",
        "expectedRevision": 4,
        "data": {"status": "running"}
    }))
    .unwrap();
    let EntityMutation::Put {
        expected_revision, ..
    } = mutation
    else {
        panic!("put mutation expected");
    };
    assert_eq!(expected_revision, 4);

    let error = serde_json::from_value::<EntityMutation>(json!({
        "type": "put",
        "contract": golden_contract(),
        "id": "runtime/01",
        "expectedRevision": 4,
        "ownerPluginId": "dev.victim",
        "data": {}
    }))
    .unwrap_err();
    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn entity_event_snapshot_gap_and_projection_fields_are_stable() {
    let entity: EntityEnvelope = serde_json::from_value(json!({
        "contract": golden_contract(),
        "id": "runtime/01",
        "revision": 5,
        "brokerRevision": 12,
        "state": "ready",
        "data": {"status": "running"},
        "updatedAtMs": 1700000000000_i64,
        "stale": false
    }))
    .unwrap();
    assert_eq!(entity.revision, 5);
    assert_eq!(entity.broker_revision, 12);

    let event: EventEnvelope = serde_json::from_value(json!({
        "contract": golden_contract(),
        "streamId": "runtime/01",
        "eventId": "event/09",
        "seq": 9,
        "subject": "runtime/01",
        "kind": "status.changed",
        "correlationId": "correlation/01",
        "data": {"status": "running"},
        "atMs": 1700000000000_i64
    }))
    .unwrap();
    assert_eq!(event.seq, 9);

    let snapshot = EntityQuerySnapshot {
        snapshot_revision: 12,
        entities: vec![entity],
    };
    assert_eq!(
        serde_json::to_value(snapshot).unwrap()["snapshotRevision"],
        12
    );

    let gap: CursorGap = serde_json::from_value(json!({
        "requestedCursor": 2,
        "earliestCursor": 5,
        "latestCursor": 12
    }))
    .unwrap();
    assert_eq!(gap.requested_cursor, 2);
    assert_eq!(gap.earliest_cursor, 5);
    assert_eq!(gap.latest_cursor, 12);

    let projection: FieldProjection =
        serde_json::from_value(json!({"fields":["status","phase"]})).unwrap();
    assert_eq!(projection.fields, ["status", "phase"]);
}

#[test]
fn runtime_operation_shapes_use_only_opaque_operation_ref_and_exact_subject() {
    let view: RuntimeOperationView = serde_json::from_value(json!({
        "operationRef": "runtime-operation/01",
        "subject": {
            "contract": golden_contract(),
            "subjectId": "runtime/01"
        },
        "exactCommand": golden_contract(),
        "state": "waiting_for_provider",
        "phase": "provider-ack",
        "providerGeneration": 11,
        "createdAt": 1700000000000_i64,
        "updatedAt": 1700000000100_i64,
        "deadlineAt": 1700000030000_i64,
        "error": null
    }))
    .unwrap();
    assert_eq!(
        view.operation_ref,
        OperationRef::new("runtime-operation/01").unwrap()
    );
    assert_eq!(view.state, RuntimeOperationState::WaitingForProvider);

    let query: RuntimeOperationQuery = serde_json::from_value(json!({
        "subjects": [{
            "contract": golden_contract(),
            "subjectId": "runtime/01"
        }],
        "includeTerminalSince": 1699999999000_i64,
        "limit": 64
    }))
    .unwrap();
    assert_eq!(query.subjects, [subject()]);

    let watch: RuntimeOperationWatch = serde_json::from_value(json!({
        "cursor": 41,
        "subjects": [{
            "contract": golden_contract(),
            "subjectId": "runtime/01"
        }],
        "limit": 64
    }))
    .unwrap();
    assert_eq!(watch.cursor, 41);

    let change = RuntimeOperationChange {
        cursor: 42,
        operation: view,
    };
    assert_eq!(serde_json::to_value(change).unwrap()["cursor"], 42);

    let gap: RuntimeOperationGap = serde_json::from_value(json!({
        "requestedCursor": 4,
        "earliestCursor": 8,
        "latestCursor": 42
    }))
    .unwrap();
    assert_eq!(gap.latest_cursor, 42);

    let cancel: RuntimeOperationCancel = serde_json::from_value(json!({
        "operationRef": "runtime-operation/01",
        "expectedStateRevision": 3
    }))
    .unwrap();
    assert_eq!(cancel.expected_state_revision, 3);
}

#[test]
fn command_shape_rejects_caller_provider_and_risk_spoofing() {
    let invocation: TypedCommandInvocation = serde_json::from_value(json!({
        "command": golden_contract(),
        "subject": {
            "contract": golden_contract(),
            "subjectId": "runtime/01"
        },
        "args": {"mode": "safe"},
        "deadlineMs": 10000
    }))
    .unwrap();

    for forbidden in ["callerPluginId", "providerId", "principal", "risk"] {
        let mut value = serde_json::to_value(&invocation).unwrap();
        value[forbidden] = json!("forged");
        assert!(
            serde_json::from_value::<TypedCommandInvocation>(value).is_err(),
            "accepted forbidden field {forbidden}"
        );
    }

    let operation_ref = OperationRef::new("runtime-operation/01").unwrap();
    let result = CommandResult::Accepted {
        operation_ref: operation_ref.clone(),
    };
    assert_eq!(serde_json::to_value(result).unwrap()["type"], "accepted");
}

#[test]
fn provider_outbox_replays_entity_and_event_mutations_with_stable_source_identity() {
    let batch = OutboxBatch {
        source_instance_id: "source-instance/01".to_string(),
        outbox_id: "outbox/0001".to_string(),
        mutations: vec![
            OutboxMutation::Entity {
                mutation: EntityMutation::Put {
                    contract: contract(),
                    id: "runtime/01".to_string(),
                    expected_revision: 4,
                    data: json!({"status":"running"}),
                },
            },
            OutboxMutation::Event {
                event: EventMutation {
                    contract: contract(),
                    stream_id: "runtime/01".to_string(),
                    event_id: "event/09".to_string(),
                    subject: "runtime/01".to_string(),
                    kind: "status.changed".to_string(),
                    correlation_id: Some("correlation/01".to_string()),
                    data: json!({"status":"running"}),
                    at_ms: 1700000000000_i64,
                },
            },
        ],
    };
    assert_eq!(
        serde_json::to_value(&batch).unwrap()["sourceInstanceId"],
        "source-instance/01"
    );

    for forbidden in ["ownerPluginId", "providerId", "principal"] {
        let mut value = serde_json::to_value(&batch).unwrap();
        value[forbidden] = json!("forged");
        assert!(
            serde_json::from_value::<OutboxBatch>(value).is_err(),
            "accepted forbidden field {forbidden}"
        );
    }

    let operation_ref = OperationRef::new("runtime-operation/01").unwrap();
    let ack = OutboxAck {
        source_instance_id: batch.source_instance_id,
        outbox_id: batch.outbox_id,
        payload_digest: DIGEST_A.to_string(),
        applied_broker_revision: 12,
        accepted_operation_refs: vec![operation_ref],
    };
    let ack = serde_json::to_value(ack).unwrap();
    assert_eq!(ack["appliedBrokerRevision"], 12);
    assert_eq!(ack["acceptedOperationRefs"][0], "runtime-operation/01");
}

#[test]
fn public_envelopes_reject_unknown_identity_and_provenance_fields() {
    let mut value = json!({
        "contract": golden_contract(),
        "id": "runtime/01",
        "revision": 5,
        "brokerRevision": 12,
        "state": "ready",
        "data": {},
        "updatedAtMs": 1700000000000_i64,
        "stale": false
    });
    for forbidden in [
        "ownerPluginId",
        "packageDigest",
        "rawPath",
        "providerPrivate",
    ] {
        value[forbidden] = json!(if forbidden == "packageDigest" {
            DIGEST_A
        } else {
            "forged"
        });
        assert!(
            serde_json::from_value::<EntityEnvelope>(value.clone()).is_err(),
            "accepted forbidden field {forbidden}"
        );
        value.as_object_mut().unwrap().remove(forbidden);
    }
}
