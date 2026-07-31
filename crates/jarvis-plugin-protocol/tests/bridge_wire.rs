use jarvis_plugin_protocol::bridge::{
    BridgeClientFrame, BridgeHostFrame, BridgeRequest, BRIDGE_PROTOCOL_V1, MAX_BRIDGE_MESSAGE_BYTES,
};
use serde_json::{json, Value};

const GOLDEN: &str = include_str!("fixtures/ui-contracts-v1.json");

fn golden(name: &str) -> Value {
    serde_json::from_str::<Value>(GOLDEN).unwrap()[name].clone()
}

#[test]
fn request_has_no_caller_identity_field() {
    let request: BridgeClientFrame = serde_json::from_value(golden("bridgeRequest")).unwrap();
    let BridgeClientFrame::Request(BridgeRequest { generation, .. }) = request else {
        panic!("request frame expected");
    };
    assert_eq!(generation, 7);
    assert_eq!(BRIDGE_PROTOCOL_V1, 1);
    assert_eq!(MAX_BRIDGE_MESSAGE_BYTES, 1_048_576);
}

#[test]
fn spoofed_identity_is_rejected_as_unknown_input() {
    let error = serde_json::from_value::<BridgeClientFrame>(json!({
        "v": 1,
        "type": "request",
        "id": "request/01",
        "generation": 7,
        "pluginId": "dev.victim",
        "namespace": "broker",
        "method": "entities.query",
        "params": {},
        "deadlineMs": 10000
    }))
    .unwrap_err();
    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn welcome_identity_is_informational_host_output_only() {
    let welcome: BridgeHostFrame = serde_json::from_value(golden("bridgeWelcome")).unwrap();
    let serialized = serde_json::to_value(welcome).unwrap();
    assert_eq!(serialized, golden("bridgeWelcome"));

    assert!(serde_json::from_value::<BridgeClientFrame>(golden("bridgeWelcome")).is_err());
}

#[test]
fn bridge_error_has_stable_redacted_fields() {
    let error: BridgeHostFrame = serde_json::from_value(golden("bridgeError")).unwrap();
    let serialized = serde_json::to_value(error).unwrap();
    assert_eq!(serialized, golden("bridgeError"));

    let mut unknown = golden("bridgeError");
    unknown["sql"] = json!("select * from broker_entities");
    assert!(serde_json::from_value::<BridgeHostFrame>(unknown).is_err());
}
