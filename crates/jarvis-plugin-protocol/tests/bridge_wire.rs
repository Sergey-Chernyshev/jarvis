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
fn request_ids_reject_host_path_and_uri_shapes() {
    for invalid in path_like_ids() {
        let mut request = golden("bridgeRequest");
        request["id"] = json!(invalid);
        assert!(
            serde_json::from_value::<BridgeClientFrame>(request).is_err(),
            "path-like request id must be rejected: {invalid}"
        );
    }
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

#[test]
fn bridge_error_and_close_codes_use_the_exact_public_vocabulary() {
    const PUBLIC_CODES: [&str; 23] = [
        "bridge_protocol_incompatible",
        "bridge_message_too_large",
        "bridge_rate_limited",
        "bridge_in_flight_limit",
        "bridge_subscription_limit",
        "bridge_deadline",
        "bridge_cancelled",
        "page_binding_missing",
        "page_generation_stale",
        "package_digest_stale",
        "grant_revoked",
        "grant_scope_denied",
        "contract_not_found",
        "contract_incompatible",
        "schema_invalid",
        "revision_conflict",
        "cursor_gap",
        "resource_handle_invalid",
        "resource_handle_expired",
        "resource_handle_exhausted",
        "operation_pending",
        "provider_unavailable",
        "plugin_ui_isolation_unavailable",
    ];

    for code in PUBLIC_CODES {
        let error = json!({
            "v": 1,
            "type": "error",
            "id": "request/01",
            "generation": 7,
            "code": code,
            "correlationId": "correlation/01"
        });
        assert!(
            serde_json::from_value::<BridgeHostFrame>(error).is_ok(),
            "stable public error code must remain accepted: {code}"
        );

        let close = json!({
            "v": 1,
            "type": "close",
            "generation": 7,
            "code": code
        });
        assert!(
            serde_json::from_value::<BridgeHostFrame>(close).is_ok(),
            "stable public close code must remain accepted: {code}"
        );
    }

    for unknown in [
        "sql_error",
        "bridge_unknown_field",
        "provider_private_failure",
    ] {
        let error = json!({
            "v": 1,
            "type": "error",
            "id": "request/01",
            "generation": 7,
            "code": unknown,
            "correlationId": "correlation/01"
        });
        assert!(
            serde_json::from_value::<BridgeHostFrame>(error).is_err(),
            "unknown public error code must be rejected: {unknown}"
        );
    }

    let diagnostic = json!({
        "v": 1,
        "type": "error",
        "id": "request/01",
        "generation": 7,
        "code": "schema_invalid",
        "message": "SELECT * FROM secrets at /Users/alice/private.db",
        "correlationId": "correlation/01"
    });
    assert!(
        serde_json::from_value::<BridgeHostFrame>(diagnostic).is_err(),
        "free-form diagnostics must not be representable on the public bridge wire"
    );
}

#[test]
fn bridge_optional_identifiers_keep_the_same_at_character_contract() {
    let mut error = golden("bridgeError");
    error["id"] = json!("request@01");
    error["correlationId"] = json!("correlation@01");
    let parsed: BridgeHostFrame = serde_json::from_value(error.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), error);
}

fn path_like_ids() -> [&'static str; 10] {
    [
        "/Users/alice/repo",
        "~/repo",
        "C:/repo",
        "c:/repo",
        "file:///Users/alice/repo",
        "https://example.test/repo",
        "project//01",
        "project/./01",
        "project/../01",
        "project/",
    ]
}
