use jarvis_plugin_protocol::{
    operation::{Operation, OperationState},
    process::{
        CommandRequest, HostHello, PluginFrame, PluginHello, RequestId, PLUGIN_PROCESS_PROTOCOL,
    },
};
use serde_json::json;

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn process_hello_is_versioned_and_camel_case() {
    let hello: PluginHello = serde_json::from_value(json!({
        "protocolVersion": 2,
        "pluginId": "dev.example.echo",
        "pid": 42,
        "packageDigest": DIGEST,
        "activationGeneration": 7
    }))
    .unwrap();
    assert_eq!(hello.protocol_version, PLUGIN_PROCESS_PROTOCOL);
    assert_eq!(hello.activation_generation, 7);

    let host = HostHello::accepted("dev.example.echo", DIGEST, 7);
    let serialized = serde_json::to_value(host).unwrap();
    assert_eq!(serialized["protocolVersion"], 2);
    assert_eq!(serialized["pluginId"], "dev.example.echo");
    assert_eq!(serialized["packageDigest"], DIGEST);
    assert_eq!(serialized["activationGeneration"], 7);
}

#[test]
fn process_hello_rejects_unknown_fields() {
    let error = serde_json::from_value::<PluginHello>(json!({
        "protocolVersion": 2,
        "pluginId": "dev.example.echo",
        "pid": 42,
        "packageDigest": DIGEST,
        "activationGeneration": 7,
        "trusted": true
    }))
    .unwrap_err();

    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn operation_state_names_are_stable() {
    let op = Operation::new_fixture("op-1", "install", "dev.example.echo");
    assert_eq!(op.state, OperationState::Queued);
    assert_eq!(serde_json::to_value(op).unwrap()["state"], "queued");
}

#[test]
fn request_id_is_non_empty_and_bounded() {
    assert!(RequestId::new("").is_err());
    assert!(RequestId::new("x".repeat(129)).is_err());
    assert_eq!(RequestId::new("request-42").unwrap().as_str(), "request-42");
}

#[test]
fn plugin_frame_uses_a_stable_tag() {
    let request = CommandRequest::new(
        "dev.example.echo",
        DIGEST,
        7,
        "request-42",
        "echo",
        json!({"message": "hello"}),
    )
    .unwrap();
    let value = serde_json::to_value(PluginFrame::CommandRequest(request)).unwrap();

    assert_eq!(value["type"], "commandRequest");
    assert_eq!(value["payload"]["requestId"], "request-42");
}

#[test]
fn plugin_frame_rejects_unknown_envelope_fields() {
    let request = CommandRequest::new(
        "dev.example.echo",
        DIGEST,
        7,
        "request-42",
        "echo",
        json!({}),
    )
    .unwrap();
    let mut value = serde_json::to_value(PluginFrame::CommandRequest(request)).unwrap();
    value["trusted"] = json!(true);

    assert!(serde_json::from_value::<PluginFrame>(value).is_err());
}
