use jarvis_plugin_protocol::bridge::{
    BridgeHostFrame, MAX_BRIDGE_MESSAGE_BYTES, MAX_BRIDGE_SUBSCRIPTIONS,
};
use jarvis_plugin_test_host::ui::{BoundPage, UiTestHost};

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn payload_identity_never_changes_bound_principal() {
    let host = UiTestHost::new(BoundPage::fixture("dev.example.owner", DIGEST, 9));
    let error = host
        .request_fixture(r#"{"pluginId":"dev.victim","method":"entities.query"}"#)
        .unwrap_err();
    assert_eq!(error.code(), "bridge_unknown_field");
    assert_eq!(host.bound_plugin_id(), "dev.example.owner");
}

#[test]
fn request_is_recorded_and_gets_a_deterministic_fixture_response() {
    let host = UiTestHost::new(BoundPage::fixture("dev.example.owner", DIGEST, 9));
    let request = request_json("request/01", "entities.query", 9);

    let first = host.request_fixture(&request).unwrap();
    let second = host.request_fixture(&request).unwrap();

    assert_eq!(first, second);
    assert_eq!(host.recorded_requests().len(), 2);
    let BridgeHostFrame::Response(response) = first else {
        panic!("response frame expected");
    };
    assert_eq!(response.id, "request/01");
    assert_eq!(response.generation, 9);
}

#[test]
fn message_size_and_bound_generation_are_enforced() {
    let host = UiTestHost::new(BoundPage::fixture("dev.example.owner", DIGEST, 9));
    let oversized = "x".repeat(MAX_BRIDGE_MESSAGE_BYTES + 1);
    assert_eq!(
        host.request_fixture(&oversized).unwrap_err().code(),
        "bridge_message_too_large"
    );
    assert_eq!(
        host.request_fixture(&request_json("request/01", "entities.query", 8))
            .unwrap_err()
            .code(),
        "page_generation_stale"
    );
    assert_eq!(
        host.request_fixture("{").unwrap_err().code(),
        "schema_invalid"
    );
}

#[test]
fn subscription_limit_is_enforced() {
    let host = UiTestHost::new(BoundPage::fixture("dev.example.owner", DIGEST, 9));

    for index in 0..MAX_BRIDGE_SUBSCRIPTIONS {
        let response = host
            .request_fixture(&request_json(
                &format!("request/{index}"),
                "entities.watch",
                9,
            ))
            .unwrap();
        assert!(matches!(response, BridgeHostFrame::SubscribeResult(_)));
    }

    assert_eq!(
        host.request_fixture(&request_json("request/overflow", "entities.watch", 9))
            .unwrap_err()
            .code(),
        "bridge_subscription_limit"
    );
}

#[test]
fn every_fake_host_output_round_trips_at_the_maximum_request_id_boundary() {
    let host = UiTestHost::new(BoundPage::fixture("dev.example.owner", DIGEST, 9));
    assert_host_frame_round_trips(host.welcome_fixture());

    let maximum_id = "a".repeat(128);
    let response = host
        .request_fixture(&request_json(&maximum_id, "entities.query", 9))
        .unwrap();
    assert_host_frame_round_trips(response);

    let subscription = host
        .request_fixture(&request_json(&maximum_id, "entities.watch", 9))
        .unwrap();
    let BridgeHostFrame::SubscribeResult(result) = &subscription else {
        panic!("subscribe-result frame expected");
    };
    assert!(
        result.subscription_id.len() <= 128,
        "fake-host subscription id exceeded the public 128-byte identifier bound"
    );
    assert_host_frame_round_trips(subscription);
}

fn assert_host_frame_round_trips(frame: BridgeHostFrame) {
    let value = serde_json::to_value(frame).unwrap();
    serde_json::from_value::<BridgeHostFrame>(value)
        .expect("fake-host output must be valid public wire");
}

fn request_json(id: &str, method: &str, generation: u64) -> String {
    format!(
        r#"{{"v":1,"type":"request","id":"{id}","generation":{generation},"namespace":"broker","method":"{method}","params":{{}},"deadlineMs":10000}}"#
    )
}
