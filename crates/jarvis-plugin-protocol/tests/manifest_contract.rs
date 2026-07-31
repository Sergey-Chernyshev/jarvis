use jarvis_plugin_protocol::manifest::{
    ManifestError, ManifestV2, RuntimeKind, MAX_MANIFEST_BYTES,
};
use serde_json::{json, Value};

const VALID_UI: &[u8] = include_bytes!("fixtures/valid-ui/plugin.json");

fn valid_ui() -> Value {
    serde_json::from_slice(VALID_UI).unwrap()
}

fn parse_value(
    value: &Value,
) -> Result<ManifestV2, jarvis_plugin_protocol::manifest::ManifestError> {
    ManifestV2::parse(&serde_json::to_vec(value).unwrap())
}

fn full_runtime_manifest() -> Value {
    let mut raw = valid_ui();
    raw["contributes"]["commands"][0]["argsSchema"] = json!("schemas/open-args.schema.json");
    raw["contributes"]["commands"][0]["invocationUI"] = json!({
        "type": "schemaForm",
        "defaultsFromContext": ["project.id"]
    });
    raw["contributes"]["projectRuntimes"] = json!([{
        "id": "dev.example.hello-page.runtime",
        "title": "Example Runtime",
        "projectKinds": ["local-folder"],
        "page": "home",
        "providerSchema": "dev.jarvis.core/project-runtime-provider@1.0.0",
        "lifecycleCommands": {
            "provision": "dev.example.hello-page/runtime.provision@1.0.0",
            "start": "dev.example.hello-page/runtime.start@1.0.0",
            "stop": "dev.example.hello-page/runtime.stop@1.0.0",
            "destroy": "dev.example.hello-page/runtime.destroy@1.0.0",
            "sessionCreate": "dev.example.hello-page/session.create@1.0.0",
            "sessionStop": "dev.example.hello-page/session.stop@1.0.0"
        },
        "contracts": {
            "runtime": {
                "core": "dev.jarvis.core/runtime@1.0.0",
                "extension": "dev.example.hello-page/runtime@1.0.0"
            },
            "session": {
                "core": "dev.jarvis.core/session@1.0.0",
                "extension": "dev.example.hello-page/session@1.0.0"
            },
            "turn": {
                "core": "dev.jarvis.core/turn@1.0.0",
                "extension": "dev.example.hello-page/turn@1.0.0"
            }
        }
    }]);

    let mut contracts = vec![
        json!({
            "id": "dev.example.hello-page/runtime@1.0.0",
            "kind": "entity",
            "schema": "schemas/runtime.schema.json",
            "visibility": "granted",
            "sensitivity": "internal"
        }),
        json!({
            "id": "dev.example.hello-page/session@1.0.0",
            "kind": "entity",
            "schema": "schemas/session.schema.json",
            "visibility": "granted",
            "sensitivity": "internal"
        }),
        json!({
            "id": "dev.example.hello-page/turn@1.0.0",
            "kind": "entity",
            "schema": "schemas/turn.schema.json",
            "visibility": "granted",
            "sensitivity": "internal"
        }),
    ];
    for name in [
        "runtime.provision",
        "runtime.start",
        "runtime.stop",
        "runtime.destroy",
        "session.create",
        "session.stop",
    ] {
        contracts.push(json!({
            "id": format!("dev.example.hello-page/{name}@1.0.0"),
            "kind": "command",
            "argsSchema": "schemas/command-args.schema.json",
            "resultSchema": "schemas/operation.schema.json",
            "risk": if name.ends_with("destroy") { "destructive" } else { "control" }
        }));
    }
    raw["contributes"]["dataContracts"] = Value::Array(contracts);
    raw
}

fn native_manifest() -> Value {
    let mut raw = valid_ui();
    raw["runtime"] = json!({
        "kind": "verified-native",
        "lifecycle": "service-bridge",
        "bridgeEntry": "bin/darwin-arm64/example-bridge",
        "service": {
            "id": "example-controller",
            "manager": "launchd-user",
            "entry": "bin/darwin-arm64/example-controller",
            "survivesCoreExit": true
        },
        "protocol": 2,
        "activationEvents": [
            "onPage:home",
            "onCommand:dev.example.hello-page.open"
        ]
    });
    raw
}

#[test]
fn parses_namespaced_ui_manifest() {
    let manifest = ManifestV2::parse(VALID_UI).unwrap();
    assert_eq!(manifest.id.as_str(), "dev.example.hello-page");
    assert_eq!(manifest.compatibility.plugin_api, 2);
    assert_eq!(manifest.runtime.kind, RuntimeKind::UiOnly);
}

#[test]
fn unknown_nested_security_fields_are_rejected() {
    for pointer in ["/runtime", "/permissions/0", "/contributes/pages/0/handler"] {
        let mut raw = valid_ui();
        if pointer == "/permissions/0" {
            raw["permissions"] = json!([{"id": "projects.read", "escapeSandbox": true}]);
        } else if pointer.ends_with("/handler") {
            raw["contributes"]["commands"][0]["handler"]["escapeSandbox"] = json!(true);
        } else {
            raw["runtime"]["escapeSandbox"] = json!(true);
        }
        assert_eq!(
            parse_value(&raw).unwrap_err().code(),
            "manifest_schema",
            "accepted unknown field at {pointer}"
        );
    }
}

#[test]
fn invalid_version_and_compatibility_range_use_semver_error() {
    for (pointer, invalid) in [
        ("/version", "1.0"),
        ("/compatibility/jarvis", ">=0.4.0 <0.5.0"),
    ] {
        let mut raw = valid_ui();
        *raw.pointer_mut(pointer).unwrap() = json!(invalid);
        assert_eq!(
            parse_value(&raw).unwrap_err().code(),
            "manifest_semver",
            "accepted invalid semantic value at {pointer}"
        );
    }

    let mut long_version = valid_ui();
    long_version["version"] = json!(format!("1.0.0+{}", "a".repeat(123)));
    assert_eq!(
        parse_value(&long_version).unwrap_err().code(),
        "manifest_semver"
    );
}

#[test]
fn plugin_and_contract_ids_are_strict() {
    let mut community_short = valid_ui();
    community_short["id"] = json!("hello-page");
    assert_eq!(
        parse_value(&community_short).unwrap_err().code(),
        "manifest_schema"
    );

    let mut uppercase = valid_ui();
    uppercase["id"] = json!("Dev.Example.Hello");
    assert_eq!(
        parse_value(&uppercase).unwrap_err().code(),
        "manifest_schema"
    );

    let mut owner_short = valid_ui();
    owner_short["id"] = json!("agent-vm");
    owner_short["publisher"] = json!("jarvis-owner");
    owner_short["contributes"]["commands"][0]["id"] = json!("agent-vm.open");
    owner_short["runtime"]["activationEvents"][1] = json!("onCommand:agent-vm.open");
    assert!(parse_value(&owner_short).is_ok());

    let mut contract_range = valid_ui();
    contract_range["contributes"]["dataContracts"] = json!([{
        "id": "dev.example/message@>=1.0.0",
        "kind": "entity",
        "schema": "schemas/message.schema.json",
        "visibility": "granted",
        "sensitivity": "internal"
    }]);
    assert_eq!(
        parse_value(&contract_range).unwrap_err().code(),
        "manifest_schema"
    );
}

#[test]
fn duplicate_contribution_ids_are_rejected_across_kinds() {
    let mut raw = valid_ui();
    raw["contributes"]["actions"] = json!([{
        "id": "dev.example.hello-page.open",
        "title": "Duplicate",
        "icon": "server-play",
        "locations": ["project.actions"],
        "command": "dev.example.hello-page.open",
        "when": "plugin.enabled",
        "context": ["project.id"]
    }]);
    assert_eq!(parse_value(&raw).unwrap_err().code(), "manifest_schema");
}

#[test]
fn absolute_parent_and_non_normalized_paths_are_rejected() {
    for path in [
        "/tmp/index.html",
        "../index.html",
        "ui/../index.html",
        "ui//index.html",
        r"ui\index.html",
    ] {
        let mut raw = valid_ui();
        raw["contributes"]["pages"][0]["entry"] = json!(path);
        assert_eq!(
            parse_value(&raw).unwrap_err().code(),
            "manifest_schema",
            "accepted unsafe path {path}"
        );
    }
}

#[test]
fn required_entries_and_cross_references_cannot_be_missing() {
    let mut missing_entry = valid_ui();
    missing_entry["contributes"]["pages"][0]
        .as_object_mut()
        .unwrap()
        .remove("entry");
    assert_eq!(
        parse_value(&missing_entry).unwrap_err().code(),
        "manifest_schema"
    );

    let mut missing_page = valid_ui();
    missing_page["contributes"]["commands"][0]["handler"]["page"] = json!("missing");
    assert_eq!(
        parse_value(&missing_page).unwrap_err().code(),
        "manifest_schema"
    );

    let mut missing_command = valid_ui();
    missing_command["contributes"]["hotkeys"] = json!([{
        "command": "dev.example.missing",
        "default": "Cmd+Shift+M",
        "scope": "global"
    }]);
    assert_eq!(
        parse_value(&missing_command).unwrap_err().code(),
        "manifest_schema"
    );
}

#[test]
fn admin_and_arbitrary_shell_permissions_do_not_exist() {
    for permission in ["admin", "process.shell"] {
        let mut raw = valid_ui();
        raw["permissions"] = json!([{"id": permission}]);
        assert_eq!(parse_value(&raw).unwrap_err().code(), "manifest_schema");
    }
}

#[test]
fn unresolved_target_is_rejected_by_packaged_contract() {
    let mut raw = valid_ui();
    raw["contributes"]["pages"][0]["entry"] = json!("ui/${target}/index.html");
    assert_eq!(
        parse_value(&raw).unwrap_err().code(),
        "manifest_unresolved_target"
    );
}

#[test]
fn parses_typed_project_runtime_and_command_contracts() {
    let manifest = parse_value(&full_runtime_manifest()).unwrap();
    assert_eq!(manifest.contributes.project_runtimes.len(), 1);
    assert_eq!(manifest.contributes.data_contracts.len(), 9);
}

#[test]
fn project_runtime_lifecycle_references_declared_command_contracts() {
    let mut raw = full_runtime_manifest();
    raw["contributes"]["projectRuntimes"][0]["lifecycleCommands"]["provision"] =
        json!("dev.example.hello-page/missing@1.0.0");
    assert_eq!(parse_value(&raw).unwrap_err().code(), "manifest_schema");
}

#[test]
fn public_parse_rejects_duplicate_json_keys() {
    let raw = String::from_utf8(VALID_UI.to_vec()).unwrap().replacen(
        "\"name\": \"Hello Page\",",
        "\"name\": \"Hello Page\",\n  \"name\": \"Shadowed Name\",",
        1,
    );
    assert_eq!(
        ManifestV2::parse(raw.as_bytes()).unwrap_err().code(),
        "manifest_schema"
    );
}

#[test]
fn public_parse_enforces_exact_byte_string_node_and_depth_quotas() {
    const MAX_DEPTH: usize = 64;
    const MAX_NODES: usize = 20_000;
    const MAX_STRING_BYTES: usize = 64 * 1024;

    let exact_bytes = vec![b' '; MAX_MANIFEST_BYTES];
    assert_ne!(
        ManifestV2::parse(&exact_bytes).unwrap_err(),
        ManifestError::TooLarge
    );
    let over_bytes = vec![b' '; MAX_MANIFEST_BYTES + 1];
    assert_eq!(
        ManifestV2::parse(&over_bytes).unwrap_err(),
        ManifestError::TooLarge
    );

    let exact_string = serde_json::to_vec(&"x".repeat(MAX_STRING_BYTES)).unwrap();
    assert_ne!(
        ManifestV2::parse(&exact_string).unwrap_err(),
        ManifestError::TooLarge
    );
    let over_string = serde_json::to_vec(&"x".repeat(MAX_STRING_BYTES + 1)).unwrap();
    assert_eq!(
        ManifestV2::parse(&over_string).unwrap_err(),
        ManifestError::TooLarge
    );

    let exact_nodes = serde_json::to_vec(&vec![Value::Null; MAX_NODES - 1]).unwrap();
    assert_ne!(
        ManifestV2::parse(&exact_nodes).unwrap_err(),
        ManifestError::TooLarge
    );
    let over_nodes = serde_json::to_vec(&vec![Value::Null; MAX_NODES]).unwrap();
    assert_eq!(
        ManifestV2::parse(&over_nodes).unwrap_err(),
        ManifestError::TooLarge
    );

    let exact_depth = format!("{}null{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
    assert_ne!(
        ManifestV2::parse(exact_depth.as_bytes()).unwrap_err(),
        ManifestError::TooDeep
    );
    let over_depth = format!(
        "{}null{}",
        "[".repeat(MAX_DEPTH + 1),
        "]".repeat(MAX_DEPTH + 1)
    );
    assert_eq!(
        ManifestV2::parse(over_depth.as_bytes()).unwrap_err(),
        ManifestError::TooDeep
    );
}

#[test]
fn permission_shapes_are_capability_specific() {
    let mut valid = valid_ui();
    valid["permissions"] = json!([
        {"id": "projects.read", "scope": "selected"},
        {"id": "filesystem.mount", "scope": "selected", "modes": ["read", "write"]},
        {"id": "memory.read", "scope": ["global"]},
        {"id": "memory.propose-write", "scope": ["selected-project"]},
        {"id": "notifications.publish"},
        {"id": "credentials.request", "scope": ["claude", "codex"]},
        {"id": "process.vm-provider"},
        {"id": "chat.compose.contribute"},
        {"id": "chat.composer.text.read", "scope": "invocation"},
        {"id": "projects.contribute"}
    ]);
    assert!(parse_value(&valid).is_ok());

    for permission in [
        json!({"id": "projects.read"}),
        json!({"id": "filesystem.mount", "modes": ["read"]}),
        json!({"id": "filesystem.mount", "scope": "selected"}),
        json!({"id": "memory.read"}),
        json!({"id": "credentials.request"}),
        json!({"id": "chat.composer.text.read"}),
        json!({"id": "notifications.publish", "scope": "selected"}),
        json!({"id": "process.vm-provider", "modes": ["read"]}),
        json!({"id": "projects.read", "scope": "selected", "modes": ["read"]}),
        json!({"id": "memory.read", "scope": ["global", "global"]}),
        json!({"id": "credentials.request", "scope": ["claude", "claude"]}),
        json!({
            "id": "filesystem.mount",
            "scope": "selected",
            "modes": ["read", "read"]
        }),
    ] {
        let mut raw = valid_ui();
        raw["permissions"] = json!([permission]);
        assert_eq!(
            parse_value(&raw).unwrap_err().code(),
            "manifest_schema",
            "accepted invalid permission declaration: {}",
            raw["permissions"][0]
        );
    }
}

#[test]
fn scoped_contribution_ids_belong_to_the_declaring_plugin() {
    let mut command = valid_ui();
    command["contributes"]["commands"][0]["id"] = json!("attacker.command");
    command["runtime"]["activationEvents"][1] = json!("onCommand:attacker.command");
    assert_eq!(parse_value(&command).unwrap_err().code(), "manifest_schema");

    let mut action = valid_ui();
    action["contributes"]["actions"] = json!([{
        "id": "attacker.action",
        "title": "Wrong owner",
        "icon": "server-play",
        "locations": ["project.actions"],
        "command": "dev.example.hello-page.open",
        "when": "plugin.enabled",
        "context": ["project.id"]
    }]);
    assert_eq!(parse_value(&action).unwrap_err().code(), "manifest_schema");

    let mut setting = valid_ui();
    setting["contributes"]["settings"] = json!([{
        "id": "attacker.setting",
        "title": "Wrong owner",
        "type": "boolean",
        "default": false
    }]);
    assert_eq!(parse_value(&setting).unwrap_err().code(), "manifest_schema");

    let mut runtime = full_runtime_manifest();
    runtime["contributes"]["projectRuntimes"][0]["id"] = json!("attacker.runtime");
    assert_eq!(parse_value(&runtime).unwrap_err().code(), "manifest_schema");
}

#[test]
fn public_parse_enforces_nested_list_cardinality_and_uniqueness() {
    let mut cases = Vec::new();

    let mut page_empty = valid_ui();
    page_empty["contributes"]["pages"][0]["placements"] = json!([]);
    cases.push(page_empty);

    let mut page_duplicate = valid_ui();
    page_duplicate["contributes"]["pages"][0]["placements"] = json!(["sidebar", "sidebar"]);
    cases.push(page_duplicate);

    let mut command_empty = valid_ui();
    command_empty["contributes"]["commands"][0]["placements"] = json!([]);
    cases.push(command_empty);

    let mut command_duplicate = valid_ui();
    command_duplicate["contributes"]["commands"][0]["placements"] =
        json!(["globalPalette", "globalPalette"]);
    cases.push(command_duplicate);

    let mut action_location_duplicate = valid_ui();
    action_location_duplicate["contributes"]["actions"] = json!([{
        "id": "dev.example.hello-page.action",
        "title": "Open",
        "icon": "server-play",
        "locations": ["project.actions", "project.actions"],
        "command": "dev.example.hello-page.open",
        "when": "plugin.enabled",
        "context": ["project.id"]
    }]);
    cases.push(action_location_duplicate);

    let mut action_context_duplicate = valid_ui();
    action_context_duplicate["contributes"]["actions"] = json!([{
        "id": "dev.example.hello-page.action",
        "title": "Open",
        "icon": "server-play",
        "locations": ["project.actions"],
        "command": "dev.example.hello-page.open",
        "when": "plugin.enabled",
        "context": ["project.id", "project.id"]
    }]);
    cases.push(action_context_duplicate);

    let mut invocation_context_duplicate = full_runtime_manifest();
    invocation_context_duplicate["contributes"]["commands"][0]["invocationUI"]
        ["defaultsFromContext"] = json!(["project.id", "project.id"]);
    cases.push(invocation_context_duplicate);

    let mut project_kind_duplicate = full_runtime_manifest();
    project_kind_duplicate["contributes"]["projectRuntimes"][0]["projectKinds"] =
        json!(["local-folder", "local-folder"]);
    cases.push(project_kind_duplicate);

    let mut setting_enum_duplicate = valid_ui();
    setting_enum_duplicate["contributes"]["settings"] = json!([{
        "id": "dev.example.hello-page.mode",
        "title": "Mode",
        "type": "string",
        "default": "safe",
        "enum": ["safe", "safe"]
    }]);
    cases.push(setting_enum_duplicate);

    let mut duplicate_hotkey = valid_ui();
    duplicate_hotkey["contributes"]["hotkeys"] = json!([
        {
            "command": "dev.example.hello-page.open",
            "default": "Cmd+Shift+H",
            "scope": "global"
        },
        {
            "command": "dev.example.hello-page.open",
            "default": "Cmd+Alt+H",
            "scope": "global"
        }
    ]);
    cases.push(duplicate_hotkey);

    for raw in cases {
        assert_eq!(
            parse_value(&raw).unwrap_err().code(),
            "manifest_schema",
            "accepted invalid nested list: {raw}"
        );
    }
}

#[test]
fn public_parse_bounds_service_and_hotkey_strings() {
    let mut service = native_manifest();
    service["runtime"]["service"]["id"] = json!("");
    assert_eq!(parse_value(&service).unwrap_err().code(), "manifest_schema");

    let long_default = "x".repeat(129);
    for default in ["", long_default.as_str()] {
        let mut hotkey = valid_ui();
        hotkey["contributes"]["hotkeys"] = json!([{
            "command": "dev.example.hello-page.open",
            "default": default,
            "scope": "global"
        }]);
        assert_eq!(parse_value(&hotkey).unwrap_err().code(), "manifest_schema");
    }
}
