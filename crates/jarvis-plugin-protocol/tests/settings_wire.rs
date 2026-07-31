use jarvis_plugin_protocol::settings::{
    CredentialReference, SettingChange, SettingKey, SettingRecord, SettingScope, SettingValue,
    SettingWrite,
};
use serde_json::json;

#[test]
fn user_and_project_settings_bind_scope_revision_and_expected_revision() {
    let user: SettingRecord = serde_json::from_value(json!({
        "key": "dev.example.owner.theme",
        "scope": "user",
        "projectId": null,
        "value": {"type": "string", "value": "dark"},
        "revision": 3
    }))
    .unwrap();
    assert_eq!(user.scope, SettingScope::User);
    assert_eq!(user.revision, 3);

    let project: SettingWrite = serde_json::from_value(json!({
        "key": "dev.example.owner.runtime-token",
        "scope": "project",
        "projectId": "project/01",
        "value": {
            "type": "credentialReference",
            "reference": {"credentialId": "credential/01"}
        },
        "expectedRevision": 4
    }))
    .unwrap();
    assert_eq!(project.scope, SettingScope::Project);
    assert_eq!(project.expected_revision, 4);

    let change = SettingChange {
        cursor: 9,
        setting: user,
    };
    assert_eq!(serde_json::to_value(change).unwrap()["cursor"], 9);
}

#[test]
fn sensitive_setting_value_is_only_an_opaque_credential_reference() {
    let value: SettingValue = serde_json::from_value(json!({
        "type": "credentialReference",
        "reference": {"credentialId": "credential/01"}
    }))
    .unwrap();
    assert_eq!(
        value,
        SettingValue::CredentialReference {
            reference: CredentialReference::new("credential/01").unwrap()
        }
    );

    for forbidden in [
        json!({"type":"credentialReference","reference":{"credentialId":"credential/01","value":"secret"}}),
        json!({"type":"secret","value":"secret"}),
        json!({"type":"credentialReference","reference":"secret"}),
    ] {
        assert!(serde_json::from_value::<SettingValue>(forbidden).is_err());
    }
}

#[test]
fn setting_key_is_namespaced_bounded_and_unknown_identity_is_rejected() {
    assert_eq!(
        SettingKey::new("dev.example.owner.theme").unwrap().as_str(),
        "dev.example.owner.theme"
    );
    for invalid in ["theme", "Dev.Example.Theme", "", "x\n"] {
        assert!(serde_json::from_value::<SettingKey>(json!(invalid)).is_err());
    }
    assert!(serde_json::from_value::<SettingKey>(json!("x".repeat(129))).is_err());

    let error = serde_json::from_value::<SettingWrite>(json!({
        "key": "dev.example.owner.theme",
        "scope": "user",
        "projectId": null,
        "value": {"type": "string", "value": "dark"},
        "expectedRevision": 4,
        "pluginId": "dev.victim"
    }))
    .unwrap_err();
    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn setting_references_reject_host_path_and_uri_shapes() {
    for invalid in path_like_ids() {
        let project_record = json!({
            "key": "dev.example.owner.timeout",
            "scope": "project",
            "projectId": invalid,
            "value": {"type": "integer", "value": 30},
            "revision": 3
        });
        assert!(
            serde_json::from_value::<SettingRecord>(project_record).is_err(),
            "path-like project reference must be rejected: {invalid}"
        );

        let credential = json!({
            "type": "credentialReference",
            "reference": {"credentialId": invalid}
        });
        assert!(
            serde_json::from_value::<SettingValue>(credential).is_err(),
            "path-like credential reference must be rejected: {invalid}"
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
