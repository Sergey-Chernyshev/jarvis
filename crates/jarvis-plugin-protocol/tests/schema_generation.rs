use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const SCHEMA_FILES: [&str; 4] = [
    "plugin-broker-v1.schema.json",
    "plugin-contribution-v1.schema.json",
    "plugin-settings-v1.schema.json",
    "plugin-ui-bridge-v1.schema.json",
];

#[test]
fn every_typed_object_schema_rejects_unknown_fields() {
    for filename in SCHEMA_FILES {
        let schema = read_schema(filename);
        let mut open_objects = Vec::new();
        collect_open_typed_objects(&schema, "$", &mut open_objects);

        assert!(
            open_objects.is_empty(),
            "{filename} contains typed objects that accept unknown fields:\n{}",
            open_objects.join("\n")
        );
    }
}

#[test]
fn bridge_request_schema_excludes_caller_identity() {
    let schema = read_schema("plugin-ui-bridge-v1.schema.json");
    let request = schema
        .pointer("/definitions/BridgeClientFrame/oneOf")
        .and_then(Value::as_array)
        .and_then(|variants| {
            variants.iter().find(|variant| {
                variant
                    .pointer("/properties/type/enum/0")
                    .and_then(Value::as_str)
                    == Some("request")
            })
        })
        .expect("request bridge-frame schema");
    let properties = request
        .get("properties")
        .and_then(Value::as_object)
        .expect("request properties");

    assert_eq!(
        request.get("additionalProperties"),
        Some(&Value::Bool(false)),
        "an unknown caller identity must make a request schema-invalid"
    );
    for forbidden in ["pluginId", "instanceId", "owner", "principal"] {
        assert!(
            !properties.contains_key(forbidden),
            "{forbidden} must be supplied by the trusted host, not the UI frame"
        );
    }
}

#[test]
fn enum_variant_fields_keep_the_runtime_camel_case_names() {
    let schema = read_schema("plugin-broker-v1.schema.json");

    assert_tagged_variant_field(
        &schema,
        "EntityMutation",
        "type",
        "put",
        "expectedRevision",
        "expected_revision",
    );
    assert_tagged_variant_field(
        &schema,
        "CommandResult",
        "type",
        "accepted",
        "operationRef",
        "operation_ref",
    );
}

#[test]
fn public_identifier_schemas_encode_the_no_path_rule() {
    let bridge = read_schema("plugin-ui-bridge-v1.schema.json");
    assert_no_path_pattern(
        &bridge,
        "/definitions/BridgeClientFrame/oneOf/1/properties/id/pattern",
    );

    let broker = read_schema("plugin-broker-v1.schema.json");
    assert_no_path_pattern(&broker, "/definitions/EntityEnvelope/properties/id/pattern");
    assert_no_path_pattern(
        &broker,
        "/definitions/RuntimeOperationCancel/properties/operationRef/pattern",
    );

    let contributions = read_schema("plugin-contribution-v1.schema.json");
    assert_no_path_pattern(
        &contributions,
        "/definitions/ContextReference/oneOf/0/properties/id/pattern",
    );

    let settings = read_schema("plugin-settings-v1.schema.json");
    assert_no_path_pattern(
        &settings,
        "/definitions/CredentialReference/properties/credentialId/pattern",
    );
    assert_no_path_pattern(
        &settings,
        "/definitions/SettingRecord/properties/projectId/pattern",
    );
}

#[test]
fn every_validated_identifier_schema_is_bounded_and_patterned() {
    for filename in SCHEMA_FILES {
        let schema = read_schema(filename);
        let mut missing = Vec::new();
        collect_unbounded_identifier_schemas(&schema, &schema, "$", &mut missing);
        assert!(
            missing.is_empty(),
            "{filename} has validated identifier fields without matching schema bounds:\n{}",
            missing.join("\n")
        );
    }
}

#[test]
fn bridge_optional_identifier_schema_matches_runtime_at_character() {
    let schema = read_schema("plugin-ui-bridge-v1.schema.json");
    let error = tagged_variant(&schema, "BridgeHostFrame", "type", "error");
    for field in ["id", "correlationId"] {
        let pattern = error
            .pointer(&format!("/properties/{field}/pattern"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("BridgeHostFrame.error.{field} pattern"));
        assert!(
            pattern.contains('@'),
            "BridgeHostFrame.error.{field} schema must accept runtime @ identifiers"
        );
    }
}

#[test]
fn optional_identifier_fields_remain_optional_in_public_schemas() {
    let bridge = read_schema("plugin-ui-bridge-v1.schema.json");
    let error = tagged_variant(&bridge, "BridgeHostFrame", "type", "error");
    assert_fields_not_required(error, &["id", "correlationId"]);

    let settings = read_schema("plugin-settings-v1.schema.json");
    for definition in ["SettingRecord", "SettingWrite"] {
        let object = settings
            .pointer(&format!("/definitions/{definition}"))
            .unwrap_or_else(|| panic!("{definition} schema"));
        assert_fields_not_required(object, &["projectId"]);
    }
}

#[test]
fn public_named_identifiers_have_exact_runtime_schema_contracts() {
    const NAMESPACED_KEY_PATTERN: &str = r"^[a-z][a-z0-9_-]*(?:\.[a-z][a-z0-9_-]*)+$";
    const CONTRACT_ID_PATTERN: &str = r"^(?!/)(?!~/)(?![A-Za-z]:[/\\])(?![Ff][Ii][Ll][Ee]:)(?![A-Za-z][A-Za-z0-9+.-]*:/)(?!.*//)(?!.*(?:^|/)(?:\.|\.\.)(?:/|$))(?=[^/]*\.)[a-z0-9._-]+/[a-z0-9._-]+$";

    let broker = read_schema("plugin-broker-v1.schema.json");
    assert_exact_string_contract(
        &broker,
        "/definitions/ContractRef/properties/id",
        256,
        CONTRACT_ID_PATTERN,
    );

    let contributions = read_schema("plugin-contribution-v1.schema.json");
    for pointer in [
        "/definitions/ResolvedActionContribution/properties/id",
        "/definitions/ResolvedActionContribution/properties/command",
        "/definitions/ResolvedCommandContribution/properties/id",
        "/definitions/ResolvedHotkeyContribution/properties/command",
        "/definitions/ResolvedPageContribution/properties/id",
    ] {
        assert_exact_string_contract(&contributions, pointer, 128, NAMESPACED_KEY_PATTERN);
    }

    let settings = read_schema("plugin-settings-v1.schema.json");
    for pointer in [
        "/definitions/SettingRecord/properties/key",
        "/definitions/SettingWrite/properties/key",
    ] {
        assert_exact_string_contract(&settings, pointer, 128, NAMESPACED_KEY_PATTERN);
    }
}

fn read_schema(filename: &str) -> Value {
    let path = schema_dir().join(filename);
    let bytes = fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas")
}

fn collect_open_typed_objects(value: &Value, pointer: &str, open_objects: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if object.contains_key("properties")
                && object.get("additionalProperties") != Some(&Value::Bool(false))
            {
                open_objects.push(pointer.to_owned());
            }

            for (key, child) in object {
                collect_open_typed_objects(
                    child,
                    &format!("{pointer}/{}", escape_json_pointer(key)),
                    open_objects,
                );
            }
        }
        Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                collect_open_typed_objects(child, &format!("{pointer}/{index}"), open_objects);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn assert_tagged_variant_field(
    schema: &Value,
    definition: &str,
    tag: &str,
    variant_name: &str,
    expected_field: &str,
    forbidden_field: &str,
) {
    let variant = tagged_variant(schema, definition, tag, variant_name);
    let properties = variant
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{definition}.{variant_name} properties"));

    assert!(
        properties.contains_key(expected_field),
        "{definition}.{variant_name} must use runtime field {expected_field}"
    );
    assert!(
        !properties.contains_key(forbidden_field),
        "{definition}.{variant_name} must not expose Rust field {forbidden_field}"
    );
}

fn tagged_variant<'a>(
    schema: &'a Value,
    definition: &str,
    tag: &str,
    variant_name: &str,
) -> &'a Value {
    schema
        .pointer(&format!("/definitions/{definition}/oneOf"))
        .and_then(Value::as_array)
        .and_then(|variants| {
            variants.iter().find(|variant| {
                variant
                    .pointer(&format!("/properties/{tag}/enum/0"))
                    .and_then(Value::as_str)
                    == Some(variant_name)
            })
        })
        .unwrap_or_else(|| panic!("{definition}.{variant_name} schema"))
}

fn assert_no_path_pattern(schema: &Value, pointer: &str) {
    let pattern = schema
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing identifier pattern at {pointer}"));
    for required_guard in [
        "(?!/)",
        "(?!~/)",
        "(?![A-Za-z]:[/\\\\])",
        "(?![Ff][Ii][Ll][Ee]:)",
        "(?!.*//)",
        "(?!.*(?:^|/)(?:\\.|\\.\\.)(?:/|$))",
    ] {
        assert!(
            pattern.contains(required_guard),
            "{pointer} is missing no-path guard {required_guard}"
        );
    }
}

fn collect_unbounded_identifier_schemas(
    root: &Value,
    value: &Value,
    pointer: &str,
    missing: &mut Vec<String>,
) {
    match value {
        Value::Object(object) => {
            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                for (field, property) in properties {
                    if validated_identifier_field(field) {
                        assert_bounded_identifier_node(
                            root,
                            property,
                            &format!("{pointer}/properties/{field}"),
                            missing,
                        );
                    }
                }
            }
            for (key, child) in object {
                collect_unbounded_identifier_schemas(
                    root,
                    child,
                    &format!("{pointer}/{}", escape_json_pointer(key)),
                    missing,
                );
            }
        }
        Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                collect_unbounded_identifier_schemas(
                    root,
                    child,
                    &format!("{pointer}/{index}"),
                    missing,
                );
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn assert_bounded_identifier_node(
    root: &Value,
    node: &Value,
    pointer: &str,
    missing: &mut Vec<String>,
) {
    let node = resolve_local_ref(root, node);
    if node.get("enum").is_some() {
        return;
    }
    if node.get("type").and_then(Value::as_str) == Some("array") {
        if let Some(items) = node.get("items") {
            let items = resolve_local_ref(root, items);
            if schema_allows_string(items) {
                assert_string_bounds(items, &format!("{pointer}/items"), missing);
            }
        }
    } else if schema_allows_string(node) {
        assert_string_bounds(node, pointer, missing);
    }
}

fn assert_string_bounds(node: &Value, pointer: &str, missing: &mut Vec<String>) {
    if node.get("minLength").and_then(Value::as_u64) != Some(1)
        || node.get("maxLength").and_then(Value::as_u64).is_none()
        || node.get("pattern").and_then(Value::as_str).is_none()
    {
        missing.push(pointer.to_owned());
    }
}

fn assert_exact_string_contract(schema: &Value, pointer: &str, max_length: u64, pattern: &str) {
    let node = schema
        .pointer(pointer)
        .unwrap_or_else(|| panic!("missing identifier schema at {pointer}"));
    assert_eq!(
        node.get("type").and_then(Value::as_str),
        Some("string"),
        "{pointer} type"
    );
    assert_eq!(
        node.get("minLength").and_then(Value::as_u64),
        Some(1),
        "{pointer} minLength"
    );
    assert_eq!(
        node.get("maxLength").and_then(Value::as_u64),
        Some(max_length),
        "{pointer} maxLength"
    );
    assert_eq!(
        node.get("pattern").and_then(Value::as_str),
        Some(pattern),
        "{pointer} pattern"
    );
}

fn assert_fields_not_required(object: &Value, fields: &[&str]) {
    let required = object
        .get("required")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("object required fields"));
    for field in fields {
        assert!(
            !required
                .iter()
                .any(|required| required.as_str() == Some(field)),
            "{field} must remain optional"
        );
    }
}

fn resolve_local_ref<'a>(root: &'a Value, node: &'a Value) -> &'a Value {
    let Some(reference) = node.get("$ref").and_then(Value::as_str) else {
        return node;
    };
    let Some(pointer) = reference.strip_prefix('#') else {
        return node;
    };
    root.pointer(pointer).unwrap_or(node)
}

fn schema_allows_string(node: &Value) -> bool {
    match node.get("type") {
        Some(Value::String(kind)) => kind == "string",
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind.as_str() == Some("string")),
        _ => false,
    }
}

fn validated_identifier_field(field: &str) -> bool {
    matches!(
        field,
        "acceptedOperationRefs"
            | "code"
            | "command"
            | "correlationId"
            | "credentialId"
            | "eventId"
            | "fields"
            | "grants"
            | "id"
            | "ids"
            | "key"
            | "kind"
            | "method"
            | "namespace"
            | "operationRef"
            | "outboxId"
            | "packageDigest"
            | "pageId"
            | "payloadDigest"
            | "phase"
            | "pluginId"
            | "projectId"
            | "schemaDigest"
            | "sourceInstanceId"
            | "state"
            | "states"
            | "streamId"
            | "subject"
            | "subjectId"
            | "subjects"
            | "subscriptionId"
    )
}

fn escape_json_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}
