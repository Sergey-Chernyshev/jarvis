use schemars::r#gen::SchemaGenerator;
use schemars::schema::{
    ArrayValidation, NumberValidation, Schema, SchemaObject, SingleOrVec, StringValidation,
};
use schemars::JsonSchema;
use serde_json::Value;

const OPAQUE_ID_PATTERN: &str = r"^(?!/)(?!~/)(?![A-Za-z]:[/\\])(?![Ff][Ii][Ll][Ee]:)(?![A-Za-z][A-Za-z0-9+.-]*:/)(?!.*//)(?!.*(?:^|/)(?:\.|\.\.)(?:/|$))(?!.*\/$)[A-Za-z0-9._/@-]+$";
const OPAQUE_ID_NO_AT_PATTERN: &str = r"^(?!/)(?!~/)(?![A-Za-z]:[/\\])(?![Ff][Ii][Ll][Ee]:)(?![A-Za-z][A-Za-z0-9+.-]*:/)(?!.*//)(?!.*(?:^|/)(?:\.|\.\.)(?:/|$))(?!.*\/$)[A-Za-z0-9._/-]+$";
const CONTRACT_ID_PATTERN: &str = r"^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)+/[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)*$";
const NAMESPACED_KEY_PATTERN: &str =
    r"^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)+$";
const BRIDGE_NAMESPACE_PATTERN: &str = r"^(?!\.\.?$)[a-z0-9._-]+$";
const PLUGIN_ID_PATTERN: &str =
    r"^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)*$";
const SHA256_DIGEST_PATTERN: &str = r"^sha256:[0-9a-f]{64}$";

pub(crate) fn is_safe_opaque_identifier(value: &str) -> bool {
    if value.starts_with('/')
        || value.starts_with("~/")
        || value.ends_with('/')
        || value.contains(':')
        || value.contains("//")
        || has_windows_drive_prefix(value)
        || has_path_uri_scheme(value)
    {
        return false;
    }

    !value
        .split('/')
        .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
}

pub(crate) fn is_canonical_segment(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit()
        || !bytes[bytes.len() - 1].is_ascii_lowercase() && !bytes[bytes.len() - 1].is_ascii_digit()
    {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

pub(crate) fn is_canonical_dotted_id(value: &str, allow_single: bool) -> bool {
    if value.is_empty() || (!allow_single && !value.contains('.')) {
        return false;
    }
    value.split('.').all(is_canonical_segment)
}

pub(crate) fn is_canonical_contract_name(value: &str) -> bool {
    let Some((namespace, contract)) = value.split_once('/') else {
        return false;
    };
    !contract.contains('/')
        && is_canonical_dotted_id(namespace, false)
        && is_canonical_dotted_id(contract, true)
}

pub(crate) fn opaque_id_128_schema(generator: &mut SchemaGenerator) -> Schema {
    string_schema(generator, 128, OPAQUE_ID_PATTERN)
}

pub(crate) fn opaque_id_128_no_at_schema(generator: &mut SchemaGenerator) -> Schema {
    string_schema(generator, 128, OPAQUE_ID_NO_AT_PATTERN)
}

pub(crate) fn optional_opaque_id_128_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_string_schema(generator, 128, OPAQUE_ID_PATTERN)
}

pub(crate) fn opaque_id_256_schema(generator: &mut SchemaGenerator) -> Schema {
    string_schema(generator, 256, OPAQUE_ID_PATTERN)
}

pub(crate) fn optional_opaque_id_256_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_string_schema(generator, 256, OPAQUE_ID_PATTERN)
}

pub(crate) fn contract_id_256_schema(generator: &mut SchemaGenerator) -> Schema {
    string_schema(generator, 256, CONTRACT_ID_PATTERN)
}

pub(crate) fn namespaced_key_128_schema(generator: &mut SchemaGenerator) -> Schema {
    string_schema(generator, 128, NAMESPACED_KEY_PATTERN)
}

pub(crate) fn bridge_namespace_128_schema(generator: &mut SchemaGenerator) -> Schema {
    string_schema(generator, 128, BRIDGE_NAMESPACE_PATTERN)
}

pub(crate) fn plugin_id_128_schema(generator: &mut SchemaGenerator) -> Schema {
    string_schema(generator, 128, PLUGIN_ID_PATTERN)
}

pub(crate) fn sha256_digest_schema(generator: &mut SchemaGenerator) -> Schema {
    string_schema(generator, 71, SHA256_DIGEST_PATTERN)
}

pub(crate) fn protocol_v1_schema(generator: &mut SchemaGenerator) -> Schema {
    exact_u32_schema(generator, 1)
}

pub(crate) fn bridge_deadline_schema(generator: &mut SchemaGenerator) -> Schema {
    bounded_u64_schema(generator, 1, 30_000)
}

pub(crate) fn broker_limit_schema(generator: &mut SchemaGenerator) -> Schema {
    bounded_u32_schema(generator, 1, 128)
}

pub(crate) fn command_deadline_schema(generator: &mut SchemaGenerator) -> Schema {
    bounded_u64_schema(generator, 1, 30_000)
}

pub(crate) fn contribution_title_256_schema(generator: &mut SchemaGenerator) -> Schema {
    utf8_string_schema(generator, 1, 256, true)
}

pub(crate) fn contribution_shortcut_128_schema(generator: &mut SchemaGenerator) -> Schema {
    utf8_string_schema(generator, 1, 128, true)
}

pub(crate) fn setting_string_65536_schema(generator: &mut SchemaGenerator) -> Schema {
    utf8_string_schema(generator, 0, 65_536, false)
}

pub(crate) fn entity_value_schema(generator: &mut SchemaGenerator) -> Schema {
    bounded_json_value_schema(generator, 256 * 1024)
}

pub(crate) fn event_value_schema(generator: &mut SchemaGenerator) -> Schema {
    bounded_json_value_schema(generator, 128 * 1024)
}

fn exact_u32_schema(generator: &mut SchemaGenerator, value: u32) -> Schema {
    let mut schema: SchemaObject = <u32>::json_schema(generator).into();
    schema.metadata().description = Some(format!("Must equal {value}."));
    schema.enum_values = Some(vec![serde_json::json!(value)]);
    let validation = schema
        .number
        .get_or_insert_with(Box::<NumberValidation>::default);
    validation.minimum = Some(f64::from(value));
    validation.maximum = Some(f64::from(value));
    schema.into()
}

fn bounded_u32_schema(generator: &mut SchemaGenerator, minimum: u32, maximum: u32) -> Schema {
    let mut schema: SchemaObject = <u32>::json_schema(generator).into();
    schema.metadata().description = Some(format!("Inclusive range: {minimum}..={maximum}."));
    let validation = schema
        .number
        .get_or_insert_with(Box::<NumberValidation>::default);
    validation.minimum = Some(f64::from(minimum));
    validation.maximum = Some(f64::from(maximum));
    schema.into()
}

fn bounded_u64_schema(generator: &mut SchemaGenerator, minimum: u64, maximum: u64) -> Schema {
    let mut schema: SchemaObject = <u64>::json_schema(generator).into();
    schema.metadata().description = Some(format!("Inclusive range: {minimum}..={maximum}."));
    let validation = schema
        .number
        .get_or_insert_with(Box::<NumberValidation>::default);
    validation.minimum = Some(minimum as f64);
    validation.maximum = Some(maximum as f64);
    schema.into()
}

fn utf8_string_schema(
    generator: &mut SchemaGenerator,
    min_length: u32,
    max_bytes: u32,
    forbid_controls: bool,
) -> Schema {
    let mut schema: SchemaObject = <String>::json_schema(generator).into();
    schema.metadata().description = Some(format!(
        "UTF-8 byte length: {min_length}..={max_bytes}. Validators must enforce x-maxUtf8Bytes; standard maxLength counts Unicode scalars."
    ));
    let validation = schema
        .string
        .get_or_insert_with(Box::<StringValidation>::default);
    validation.min_length = Some(min_length);
    validation.max_length = Some(max_bytes);
    if forbid_controls {
        validation.pattern = Some(r"^[^\u0000-\u001F\u007F-\u009F]*$".to_owned());
    }
    schema
        .extensions
        .insert("x-maxUtf8Bytes".to_owned(), serde_json::json!(max_bytes));
    schema.into()
}

fn bounded_json_value_schema(generator: &mut SchemaGenerator, max_bytes: usize) -> Schema {
    let mut schema: SchemaObject = <Value>::json_schema(generator).into();
    schema.metadata().description = Some(format!(
        "Serialized JSON size must not exceed {max_bytes} bytes. Validators must enforce x-maxJsonBytes; generic Draft 7 validators ignore extension keywords."
    ));
    schema
        .extensions
        .insert("tsType".to_owned(), serde_json::json!("unknown"));
    schema
        .extensions
        .insert("x-maxJsonBytes".to_owned(), serde_json::json!(max_bytes));
    schema.into()
}

pub(crate) fn opaque_ids_256_schema(generator: &mut SchemaGenerator) -> Schema {
    string_array_schema(generator, 0, 128, 256, OPAQUE_ID_PATTERN)
}

pub(crate) fn opaque_states_128_schema(generator: &mut SchemaGenerator) -> Schema {
    string_array_schema(generator, 0, 128, 128, OPAQUE_ID_PATTERN)
}

pub(crate) fn projection_fields_256_schema(generator: &mut SchemaGenerator) -> Schema {
    string_array_schema(generator, 1, 64, 256, OPAQUE_ID_PATTERN)
}

pub(crate) fn bridge_grants_128_schema(generator: &mut SchemaGenerator) -> Schema {
    string_array_schema(generator, 0, 256, 128, BRIDGE_NAMESPACE_PATTERN)
}

fn string_schema(generator: &mut SchemaGenerator, max_length: u32, pattern: &str) -> Schema {
    let mut schema: SchemaObject = <String>::json_schema(generator).into();
    apply_string_validation(&mut schema, max_length, pattern);
    schema.into()
}

fn optional_string_schema(
    generator: &mut SchemaGenerator,
    max_length: u32,
    pattern: &str,
) -> Schema {
    let mut schema: SchemaObject = <Option<String>>::json_schema(generator).into();
    apply_string_validation(&mut schema, max_length, pattern);
    schema.into()
}

fn string_array_schema(
    generator: &mut SchemaGenerator,
    min_items: u32,
    max_items: u32,
    item_max_length: u32,
    item_pattern: &str,
) -> Schema {
    let mut schema: SchemaObject = <Vec<String>>::json_schema(generator).into();
    let validation = schema
        .array
        .get_or_insert_with(Box::<ArrayValidation>::default);
    validation.min_items = Some(min_items);
    validation.max_items = Some(max_items);
    validation.items = Some(SingleOrVec::Single(Box::new(string_schema(
        generator,
        item_max_length,
        item_pattern,
    ))));
    schema.into()
}

fn apply_string_validation(schema: &mut SchemaObject, max_length: u32, pattern: &str) {
    let validation = schema
        .string
        .get_or_insert_with(Box::<StringValidation>::default);
    validation.min_length = Some(1);
    validation.max_length = Some(max_length);
    validation.pattern = Some(pattern.to_owned());
}

fn has_windows_drive_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn has_path_uri_scheme(value: &str) -> bool {
    let Some((scheme, remainder)) = value.split_once(':') else {
        return false;
    };
    let mut bytes = scheme.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    let valid_scheme = first.is_ascii_alphabetic()
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'));
    valid_scheme && (scheme.eq_ignore_ascii_case("file") || remainder.starts_with('/'))
}
