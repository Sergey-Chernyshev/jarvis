// The real Draft 7 validator lives in tools/plugin-schema-parity. Keeping this
// source as a disabled compatibility shim avoids duplicating the corpus while
// ensuring the public MSRV test-host never resolves jsonschema's stable-only
// dependency graph.
#[cfg(feature = "schema-parity")]
#[allow(clippy::result_large_err)]
mod schema_parity {

use std::collections::BTreeMap;
use std::fs;
use std::iter::{empty, once};
use std::path::{Path, PathBuf};

use jsonschema::paths::{JSONPointer, JsonPointerNode};
use jsonschema::{Draft, ErrorIterator, JSONSchema, Keyword, ValidationError};
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
    let mut validators = BTreeMap::new();
    let mut mismatches = Vec::new();

    for case in &corpus.cases {
        let key = (case.schema.clone(), case.target.clone());
        validators.entry(key).or_insert_with(|| {
            let target_schema = target_schema(
                roots
                    .get(&case.schema)
                    .unwrap_or_else(|| panic!("unknown schema {}", case.schema)),
                case,
            );
            compile_keyword_aware_schema(&target_schema, &case.name)
        });
    }

    for case in corpus.cases {
        let compiled = validators
            .get(&(case.schema.clone(), case.target.clone()))
            .expect("compiled corpus target schema");
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

fn compile_keyword_aware_schema(schema: &Value, label: &str) -> JSONSchema {
    let mut options = JSONSchema::options();
    options.with_draft(Draft::Draft7);
    options.with_keyword("x-maxUtf8Bytes", max_utf8_bytes_factory);
    options.with_keyword("x-maxJsonBytes", max_json_bytes_factory);
    options
        .compile(schema)
        .unwrap_or_else(|error| panic!("compile {label}: {error}"))
}

#[test]
fn draft7_byte_extensions_require_keyword_aware_validation() {
    let cases = [
        (json!({"type":"string","x-maxUtf8Bytes":1}), json!("é")),
        (json!({"x-maxJsonBytes":3}), json!({"value":"too large"})),
    ];

    for (schema, instance) in cases {
        let mut generic_options = JSONSchema::options();
        generic_options.with_draft(Draft::Draft7);
        let generic = generic_options
            .compile(&schema)
            .expect("generic Draft 7 schema");
        assert!(
            generic.is_valid(&instance),
            "generic Draft 7 must be treated as keyword-unaware, not byte-limit enforcement"
        );

        let mut aware_options = JSONSchema::options();
        aware_options.with_draft(Draft::Draft7);
        aware_options.with_keyword("x-maxUtf8Bytes", max_utf8_bytes_factory);
        aware_options.with_keyword("x-maxJsonBytes", max_json_bytes_factory);
        let aware = aware_options
            .compile(&schema)
            .expect("keyword-aware Draft 7 schema");
        assert!(
            !aware.is_valid(&instance),
            "registered byte-limit keyword must reject the oversized instance"
        );
    }
}

#[derive(Debug)]
struct MaxUtf8Bytes {
    maximum: usize,
}

impl Keyword for MaxUtf8Bytes {
    fn validate<'instance>(
        &self,
        instance: &'instance Value,
        instance_path: &JsonPointerNode,
    ) -> ErrorIterator<'instance> {
        if self.is_valid(instance) {
            Box::new(empty())
        } else {
            Box::new(once(ValidationError::custom(
                JSONPointer::default(),
                instance_path.into(),
                instance,
                format!("string exceeds {} UTF-8 bytes", self.maximum),
            )))
        }
    }

    fn is_valid(&self, instance: &Value) -> bool {
        instance
            .as_str()
            .map(|value| value.len() <= self.maximum)
            .unwrap_or(true)
    }
}

#[derive(Debug)]
struct MaxJsonBytes {
    maximum: usize,
}

impl Keyword for MaxJsonBytes {
    fn validate<'instance>(
        &self,
        instance: &'instance Value,
        instance_path: &JsonPointerNode,
    ) -> ErrorIterator<'instance> {
        if self.is_valid(instance) {
            Box::new(empty())
        } else {
            Box::new(once(ValidationError::custom(
                JSONPointer::default(),
                instance_path.into(),
                instance,
                format!("value exceeds {} serialized JSON bytes", self.maximum),
            )))
        }
    }

    fn is_valid(&self, instance: &Value) -> bool {
        serde_json::to_vec(instance)
            .map(|bytes| bytes.len() <= self.maximum)
            .unwrap_or(false)
    }
}

fn max_utf8_bytes_factory<'a>(
    _: &'a serde_json::Map<String, Value>,
    schema: &'a Value,
    path: JSONPointer,
) -> Result<Box<dyn Keyword>, ValidationError<'a>> {
    Ok(Box::new(MaxUtf8Bytes {
        maximum: keyword_limit(schema, path)?,
    }))
}

fn max_json_bytes_factory<'a>(
    _: &'a serde_json::Map<String, Value>,
    schema: &'a Value,
    path: JSONPointer,
) -> Result<Box<dyn Keyword>, ValidationError<'a>> {
    Ok(Box::new(MaxJsonBytes {
        maximum: keyword_limit(schema, path)?,
    }))
}

fn keyword_limit<'a>(schema: &'a Value, path: JSONPointer) -> Result<usize, ValidationError<'a>> {
    schema
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            ValidationError::custom(
                JSONPointer::default(),
                path,
                schema,
                "byte-limit keyword must be an unsigned integer",
            )
        })
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
}
