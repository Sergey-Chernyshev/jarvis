use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use jsonschema::{Draft, JSONSchema};
use serde::Deserialize;
use serde_json::{json, Value};

const CORPUS: &str =
    include_str!("../../jarvis-plugin-protocol/tests/fixtures/public-contract-parity-v1.json");

#[derive(Debug, Deserialize)]
struct Corpus {
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    name: String,
    schema: String,
    target: String,
    valid: bool,
    value: Value,
    repeat: Option<Repeat>,
    repeat_string: Option<RepeatString>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repeat {
    pointer: String,
    count: usize,
    index_pointer: Option<String>,
    prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RepeatString {
    pointer: String,
    value: String,
    count: usize,
}

#[test]
fn shared_public_contract_corpus_matches_real_json_schema_validation() {
    let corpus: Corpus = serde_json::from_str(CORPUS).expect("parse parity corpus");
    let roots = load_schema_roots();
    let mut mismatches = Vec::new();

    for case in corpus.cases {
        let target_schema = target_schema(
            roots
                .get(&case.schema)
                .unwrap_or_else(|| panic!("unknown schema {}", case.schema)),
            &case,
        );
        let mut options = JSONSchema::options();
        options.with_draft(Draft::Draft7);
        let compiled = options
            .compile(&target_schema)
            .unwrap_or_else(|error| panic!("compile {}: {error}", case.name));
        let value = expanded_value(&case);
        let accepted = compiled.is_valid(&value);
        if accepted != case.valid {
            mismatches.push(format!(
                "{} [{}:{}] expected valid={}, schema accepted={accepted}",
                case.name, case.schema, case.target, case.valid
            ));
        }
    }

    assert!(
        mismatches.is_empty(),
        "JSON Schema diverged from the shared public contract corpus:\n{}",
        mismatches.join("\n")
    );
}

fn load_schema_roots() -> BTreeMap<String, Value> {
    [
        ("bridge", "plugin-ui-bridge-v1.schema.json"),
        ("broker", "plugin-broker-v1.schema.json"),
        ("contributions", "plugin-contribution-v1.schema.json"),
        ("settings", "plugin-settings-v1.schema.json"),
    ]
    .into_iter()
    .map(|(name, filename)| {
        let path = schema_dir().join(filename);
        let bytes =
            fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let schema = serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
        (name.to_owned(), schema)
    })
    .collect()
}

fn schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas")
}

fn target_schema(root: &Value, case: &Case) -> Value {
    if case.schema == "contributions" && case.target == "ResolvedContributions" {
        return root.clone();
    }
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "definitions": root.get("definitions").cloned().unwrap_or_else(|| json!({})),
        "$ref": format!("#/definitions/{}", case.target)
    })
}

fn expanded_value(case: &Case) -> Value {
    let mut value = case.value.clone();
    if let Some(repeat) = &case.repeat {
        let template = value
            .pointer(&repeat.pointer)
            .and_then(Value::as_array)
            .and_then(|values| values.first())
            .cloned()
            .unwrap_or_else(|| panic!("{} repeat template at {}", case.name, repeat.pointer));
        let values = (0..repeat.count)
            .map(|index| {
                let mut item = template.clone();
                if let (Some(pointer), Some(prefix)) = (&repeat.index_pointer, &repeat.prefix) {
                    *item.pointer_mut(pointer).unwrap_or_else(|| {
                        panic!("{} repeat index pointer {pointer}", case.name)
                    }) = Value::String(format!("{prefix}{index}"));
                }
                item
            })
            .collect();
        *value
            .pointer_mut(&repeat.pointer)
            .unwrap_or_else(|| panic!("{} repeat pointer {}", case.name, repeat.pointer)) =
            Value::Array(values);
    }
    if let Some(repeat) = &case.repeat_string {
        *value
            .pointer_mut(&repeat.pointer)
            .unwrap_or_else(|| panic!("{} repeat string pointer {}", case.name, repeat.pointer)) =
            Value::String(repeat.value.repeat(repeat.count));
    }
    value
}
