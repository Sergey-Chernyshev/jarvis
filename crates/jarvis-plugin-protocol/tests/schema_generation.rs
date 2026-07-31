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

fn escape_json_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}
