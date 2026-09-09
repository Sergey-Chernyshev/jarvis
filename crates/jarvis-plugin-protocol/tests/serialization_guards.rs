use jarvis_plugin_protocol::bridge::{BridgeRequest, Hello, Welcome, BRIDGE_PROTOCOL_V1};
use jarvis_plugin_protocol::broker::{ContractRef, EntityQuery};
use jarvis_plugin_protocol::contribution::{
    ContributionId, ResolvedContributions, ResolvedPageContribution,
};
use jarvis_plugin_protocol::manifest::{InstancePolicy, PagePlacement};
use semver::Version;
use serde::Serialize;
use serde_json::json;

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn public_dtos_refuse_to_serialize_values_that_the_wire_rejects() {
    assert_serialize_rejected(&Hello {
        v: BRIDGE_PROTOCOL_V1 + 1,
        generation: 1,
    });
    assert_serialize_rejected(&BridgeRequest {
        v: BRIDGE_PROTOCOL_V1,
        id: "request/01".to_owned(),
        generation: 1,
        namespace: "broker".to_owned(),
        method: "entities.query".to_owned(),
        params: json!({}),
        deadline_ms: 0,
    });
    assert_serialize_rejected(&Welcome {
        v: BRIDGE_PROTOCOL_V1,
        plugin_id: "dev_example.plugin".to_owned(),
        package_digest: DIGEST.to_owned(),
        page_id: "home".to_owned(),
        generation: 1,
        grants: Vec::new(),
    });
    assert_serialize_rejected(&ContractRef {
        id: "dev_example.plugin/runtime".to_owned(),
        version: Version::parse("1.0.0").unwrap(),
        schema_digest: DIGEST.to_owned(),
    });
    assert_serialize_rejected(&EntityQuery {
        selectors: Vec::new(),
        projection: None,
        limit: 0,
    });
    assert_serialize_rejected(&ResolvedPageContribution {
        id: ContributionId::new("dev.example.page").unwrap(),
        title: "\u{0000}".to_owned(),
        placements: vec![PagePlacement::Sidebar],
        instance_policy: InstancePolicy::Singleton,
    });
    assert_serialize_rejected(&ResolvedContributions {
        pages: vec![
            ResolvedPageContribution {
                id: ContributionId::new("dev.example.page").unwrap(),
                title: "Page".to_owned(),
                placements: vec![PagePlacement::Sidebar],
                instance_policy: InstancePolicy::Singleton,
            };
            513
        ],
        ..ResolvedContributions::default()
    });
    assert!(jarvis_plugin_protocol::settings::SettingValue::string("x".repeat(65_537)).is_err());
}

fn assert_serialize_rejected(value: &impl Serialize) {
    assert!(
        serde_json::to_value(value).is_err(),
        "invalid public DTO serialized successfully"
    );
}
