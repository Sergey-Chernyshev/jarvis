use jarvis_plugin_protocol::contribution::{
    ContextReference, ContributionId, ResolvedActionContribution,
};
use jarvis_plugin_protocol::manifest::{ActionLocation, Risk};
use serde_json::json;

#[test]
fn resolved_action_uses_namespaced_id_declared_locations_and_host_risk_floor() {
    let action: ResolvedActionContribution = serde_json::from_value(json!({
        "id": "dev.example.owner.open-runtime",
        "title": "Open runtime",
        "locations": ["project.actions"],
        "command": "dev.example.owner.open-runtime",
        "riskFloor": "control",
        "context": [
            {"type": "project", "id": "project/01"},
            {"type": "runtime", "id": "runtime/01"}
        ]
    }))
    .unwrap();

    assert_eq!(
        action.id,
        ContributionId::new("dev.example.owner.open-runtime").unwrap()
    );
    assert_eq!(action.locations, [ActionLocation::ProjectActions]);
    assert_eq!(action.risk_floor, Risk::Control);
    assert_eq!(
        action.context,
        [
            ContextReference::Project {
                id: "project/01".to_string()
            },
            ContextReference::Runtime {
                id: "runtime/01".to_string()
            }
        ]
    );
}

#[test]
fn contribution_id_must_be_namespaced_and_bounded() {
    for invalid in ["open-runtime", "Dev.Example.Open", "dev.example/open", ""] {
        assert!(
            serde_json::from_value::<ContributionId>(json!(invalid)).is_err(),
            "accepted invalid contribution id {invalid:?}"
        );
    }
    assert!(serde_json::from_value::<ContributionId>(json!("x".repeat(129))).is_err());
}

#[test]
fn resolved_context_never_accepts_raw_path_text_or_html() {
    for forbidden in ["rawPath", "text", "html"] {
        let mut value = json!({
            "id": "dev.example.owner.open-runtime",
            "title": "Open runtime",
            "locations": ["project.actions"],
            "command": "dev.example.owner.open-runtime",
            "riskFloor": "read",
            "context": [{"type": "project", "id": "project/01"}]
        });
        value[forbidden] = json!("secret");
        let error = serde_json::from_value::<ResolvedActionContribution>(value).unwrap_err();
        assert!(
            error.to_string().contains("unknown field"),
            "wrong error for {forbidden}: {error}"
        );
    }
}

#[test]
fn context_ids_reject_host_path_and_uri_shapes() {
    for invalid in path_like_ids() {
        assert!(
            serde_json::from_value::<ContextReference>(json!({
                "type": "project",
                "id": invalid
            }))
            .is_err(),
            "path-like context id must be rejected: {invalid}"
        );
    }
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
