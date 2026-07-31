use std::sync::Arc;

use jarvis_plugin_protocol::manifest::{
    parse_bounded_json, ManifestError, ManifestV2, MANIFEST_SCHEMA_JSON, PLUGIN_API_VERSION,
};
pub use jarvis_plugin_protocol::manifest::{
    MAX_JSON_NODES, MAX_JSON_STRING_BYTES, MAX_MANIFEST_BYTES,
};
use jsonschema::{Draft, JSONSchema, SchemaResolver, SchemaResolverError};
use semver::Version;
use serde_json::Value;
use url::Url;

#[derive(Clone, Copy, Debug)]
struct DenyExternalSchemaResolver;

impl SchemaResolver for DenyExternalSchemaResolver {
    fn resolve(
        &self,
        _root_schema: &Value,
        _url: &Url,
        _original_reference: &str,
    ) -> Result<Arc<Value>, SchemaResolverError> {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "external JSON Schema resolution is disabled",
        )
        .into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    DarwinArm64,
    DarwinAmd64,
}

impl Target {
    pub const fn darwin_arm64() -> Self {
        Self::DarwinArm64
    }

    pub const fn darwin_amd64() -> Self {
        Self::DarwinAmd64
    }

    pub const fn package_token(self) -> &'static str {
        match self {
            Self::DarwinArm64 => "darwin-arm64",
            Self::DarwinAmd64 => "darwin-amd64",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostCompatibility {
    pub jarvis_version: Version,
    pub plugin_api: u32,
}

impl HostCompatibility {
    pub fn new(jarvis_version: Version, plugin_api: u32) -> Self {
        Self {
            jarvis_version,
            plugin_api,
        }
    }

    pub fn parse(jarvis_version: &str, plugin_api: u32) -> Result<Self, ManifestError> {
        let jarvis_version = Version::parse(jarvis_version).map_err(|_| ManifestError::Semver)?;
        Ok(Self::new(jarvis_version, plugin_api))
    }
}

pub fn validate_source_manifest(
    bytes: &[u8],
    target: &Target,
    host: &HostCompatibility,
) -> Result<ManifestV2, ManifestError> {
    validate_manifest(bytes, target, host, true)
}

pub fn validate_packaged_manifest(
    bytes: &[u8],
    target: &Target,
    host: &HostCompatibility,
) -> Result<ManifestV2, ManifestError> {
    validate_manifest(bytes, target, host, false)
}

fn validate_manifest(
    bytes: &[u8],
    target: &Target,
    host: &HostCompatibility,
    source: bool,
) -> Result<ManifestV2, ManifestError> {
    let mut value = parse_bounded_json(bytes)?;
    if source {
        substitute_target_entry(&mut value, "/runtime/bridgeEntry", *target);
        substitute_target_entry(&mut value, "/runtime/service/entry", *target);
    }
    if value_contains_template(&value) {
        return Err(ManifestError::UnresolvedTarget);
    }

    // Typed public semantics run first so every consumer sees the same stable
    // error code. The bundled schema remains a second, defense-in-depth gate.
    let concrete = serde_json::to_vec(&value).map_err(|_| ManifestError::Schema)?;
    let manifest = ManifestV2::parse(&concrete)?;

    let schema: Value =
        serde_json::from_slice(MANIFEST_SCHEMA_JSON).map_err(|_| ManifestError::Schema)?;
    validate_schema_references(&schema)?;
    let mut options = JSONSchema::options();
    options
        .with_draft(Draft::Draft202012)
        .with_resolver(DenyExternalSchemaResolver);
    let compiled = options
        .compile(&schema)
        .map_err(|_| ManifestError::Schema)?;
    if !compiled.is_valid(&value) {
        return Err(ManifestError::Schema);
    }

    if host.plugin_api != PLUGIN_API_VERSION
        || manifest.compatibility.plugin_api != host.plugin_api
        || !manifest.compatibility.jarvis.matches(&host.jarvis_version)
    {
        return Err(ManifestError::Incompatible);
    }
    Ok(manifest)
}

fn substitute_target_entry(value: &mut Value, pointer: &str, target: Target) {
    let Some(Value::String(entry)) = value.pointer_mut(pointer) else {
        return;
    };
    if entry.contains("${target}") {
        *entry = entry.replace("${target}", target.package_token());
    }
}

fn value_contains_template(value: &Value) -> bool {
    let mut stack = vec![value];
    while let Some(current) = stack.pop() {
        match current {
            Value::String(value) => {
                if value.contains("${") {
                    return true;
                }
            }
            Value::Array(values) => stack.extend(values),
            Value::Object(values) => {
                for (key, value) in values {
                    if key.contains("${") {
                        return true;
                    }
                    stack.push(value);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }
    false
}

fn validate_schema_references(schema: &Value) -> Result<(), ManifestError> {
    let mut stack = vec![schema];
    while let Some(current) = stack.pop() {
        match current {
            Value::Array(values) => stack.extend(values),
            Value::Object(values) => {
                if let Some(reference) = values.get("$ref") {
                    let Some(reference) = reference.as_str() else {
                        return Err(ManifestError::Schema);
                    };
                    if !reference.starts_with("#/$defs/") {
                        return Err(ManifestError::Schema);
                    }
                }
                stack.extend(values.values());
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use jarvis_plugin_protocol::manifest::{ManifestV2, RuntimeKind, MANIFEST_SCHEMA_JSON};
    use jsonschema::{Draft, JSONSchema, SchemaResolver};
    use serde_json::{json, Value};
    use url::Url;

    use super::{
        parse_bounded_json, validate_packaged_manifest, validate_schema_references,
        validate_source_manifest, DenyExternalSchemaResolver, HostCompatibility, Target,
        MAX_JSON_NODES, MAX_JSON_STRING_BYTES, MAX_MANIFEST_BYTES,
    };

    const VALID_UI: &[u8] =
        include_bytes!("../../tests/fixtures/plugin-packages/valid-ui/plugin.json");
    const PUBLIC_VALID_UI: &[u8] = include_bytes!(
        "../../../crates/jarvis-plugin-protocol/tests/fixtures/valid-ui/plugin.json"
    );
    const VALID_NATIVE: &[u8] =
        include_bytes!("../../tests/fixtures/plugin-packages/valid-native/plugin.json");
    const APPROVED_DESIGN: &str = include_str!(
        "../../../docs/superpowers/specs/2026-07-31-plugin-platform-agent-vm-v2-design.md"
    );

    fn ui_value() -> Value {
        serde_json::from_slice(VALID_UI).unwrap()
    }

    fn future_host() -> HostCompatibility {
        HostCompatibility::parse("0.4.0", 2).unwrap()
    }

    #[test]
    fn packaged_ui_and_source_native_manifests_validate() {
        let target = Target::darwin_arm64();
        let host = future_host();
        let ui = validate_packaged_manifest(VALID_UI, &target, &host).unwrap();
        assert_eq!(ui.id.as_str(), "dev.example.hello-page");

        let native = validate_source_manifest(VALID_NATIVE, &target, &host).unwrap();
        assert_eq!(
            native.runtime.bridge_entry.unwrap().as_str(),
            "bin/darwin-arm64/example-bridge"
        );
        assert_eq!(
            native.runtime.service.unwrap().entry.as_str(),
            "bin/darwin-arm64/example-controller"
        );
    }

    #[test]
    fn public_and_host_ui_fixtures_are_byte_identical() {
        assert_eq!(PUBLIC_VALID_UI, VALID_UI);
    }

    #[test]
    fn public_and_host_error_codes_are_identical() {
        let mut cases = Vec::new();

        let mut invalid_version = ui_value();
        invalid_version["version"] = json!("1.0");
        cases.push((
            serde_json::to_vec(&invalid_version).unwrap(),
            "manifest_semver",
        ));

        let mut incompatible_api = ui_value();
        incompatible_api["compatibility"]["pluginApi"] = json!(3);
        cases.push((
            serde_json::to_vec(&incompatible_api).unwrap(),
            "manifest_incompatible",
        ));

        let mut incompatible_protocol = ui_value();
        incompatible_protocol["runtime"]["protocol"] = json!(3);
        cases.push((
            serde_json::to_vec(&incompatible_protocol).unwrap(),
            "manifest_incompatible",
        ));

        let mut encoded_path = ui_value();
        encoded_path["contributes"]["pages"][0]["entry"] = json!("ui/%2e%2e/index.html");
        cases.push((
            serde_json::to_vec(&encoded_path).unwrap(),
            "manifest_schema",
        ));

        let mut unknown_field = ui_value();
        unknown_field["escapeSandbox"] = json!(true);
        cases.push((
            serde_json::to_vec(&unknown_field).unwrap(),
            "manifest_schema",
        ));

        let mut invalid_permission = ui_value();
        invalid_permission["permissions"] = json!([{"id": "projects.read"}]);
        cases.push((
            serde_json::to_vec(&invalid_permission).unwrap(),
            "manifest_schema",
        ));

        let mut zero_migration = ui_value();
        zero_migration["state"]["migrations"] = json!([{
            "from": 0,
            "to": 1,
            "entry": "migrations/upgrade"
        }]);
        cases.push((
            serde_json::to_vec(&zero_migration).unwrap(),
            "manifest_schema",
        ));

        cases.push((vec![b' '; MAX_MANIFEST_BYTES + 1], "manifest_too_large"));

        for (bytes, expected) in cases {
            assert_eq!(
                ManifestV2::parse(&bytes).unwrap_err().code(),
                expected,
                "public parser returned a different error for {expected}"
            );
            assert_eq!(
                validate_packaged_manifest(&bytes, &Target::darwin_arm64(), &future_host())
                    .unwrap_err()
                    .code(),
                expected,
                "host parser returned a different error for {expected}"
            );
        }
    }

    #[test]
    fn approved_agent_vm_manifest_remains_contract_valid() {
        let marker = "```json\n{\n  \"schemaVersion\": 2,\n  \"id\": \"agent-vm\",";
        let block = APPROVED_DESIGN
            .split_once(marker)
            .map(|(_, suffix)| suffix)
            .expect("approved Agent VM manifest block");
        let body = block
            .split_once("\n```")
            .map(|(body, _)| body)
            .expect("approved Agent VM manifest terminator");
        let manifest = format!("{{\n  \"schemaVersion\": 2,\n  \"id\": \"agent-vm\",{body}");

        let parsed = validate_packaged_manifest(
            manifest.as_bytes(),
            &Target::darwin_arm64(),
            &future_host(),
        )
        .unwrap();
        assert_eq!(parsed.id.as_str(), "agent-vm");
        assert_eq!(parsed.contributes.project_runtimes.len(), 1);
    }

    #[test]
    fn packaged_manifest_rejects_every_unresolved_template() {
        let error =
            validate_packaged_manifest(VALID_NATIVE, &Target::darwin_arm64(), &future_host())
                .unwrap_err();
        assert_eq!(error.code(), "manifest_unresolved_target");

        let mut raw = ui_value();
        raw["name"] = json!("Hello ${unknown}");
        let error = validate_source_manifest(
            &serde_json::to_vec(&raw).unwrap(),
            &Target::darwin_arm64(),
            &future_host(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "manifest_unresolved_target");
    }

    #[test]
    fn unknown_nested_security_fields_fail_schema_validation() {
        let mut raw: Value = serde_json::from_slice(VALID_NATIVE).unwrap();
        raw["runtime"]["service"]["escapeSandbox"] = json!(true);
        let error = validate_source_manifest(
            &serde_json::to_vec(&raw).unwrap(),
            &Target::darwin_arm64(),
            &future_host(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "manifest_schema");
    }

    #[test]
    fn remote_schema_refs_are_rejected_before_compilation() {
        for reference in [
            "https://example.invalid/schema.json",
            "file:///tmp/schema.json",
            "#/definitions/not-allowed",
        ] {
            let schema = json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "$ref": reference
            });
            assert_eq!(
                validate_schema_references(&schema).unwrap_err().code(),
                "manifest_schema",
                "accepted remote or non-definitions ref {reference}"
            );
        }
        assert!(validate_schema_references(&json!({
            "$ref": "#/$defs/page"
        }))
        .is_ok());
    }

    #[test]
    fn schema_compiler_has_a_fail_closed_external_resolver() {
        let resolver = DenyExternalSchemaResolver;
        for reference in [
            "https://example.invalid/schema.json",
            "file:///tmp/schema.json",
        ] {
            let url = Url::parse(reference).unwrap();
            assert!(
                resolver.resolve(&json!({}), &url, reference).is_err(),
                "external resolver accepted {reference}"
            );
        }
    }

    #[test]
    fn bundled_schema_encodes_permission_specific_shapes() {
        let schema: Value = serde_json::from_slice(MANIFEST_SCHEMA_JSON).unwrap();
        let mut options = JSONSchema::options();
        options.with_draft(Draft::Draft202012);
        let compiled = options.compile(&schema).unwrap();

        for permission in [
            json!({"id": "projects.read"}),
            json!({"id": "filesystem.mount", "scope": "selected"}),
            json!({"id": "memory.read", "scope": "global"}),
            json!({"id": "notifications.publish", "scope": "selected"}),
            json!({"id": "credentials.request", "scope": ["claude", "claude"]}),
        ] {
            let mut raw = ui_value();
            raw["permissions"] = json!([permission]);
            assert!(
                !compiled.is_valid(&raw),
                "schema accepted invalid permission: {}",
                raw["permissions"][0]
            );
        }
    }

    #[test]
    fn bundled_schema_rejects_expressible_noncanonical_paths() {
        let schema: Value = serde_json::from_slice(MANIFEST_SCHEMA_JSON).unwrap();
        let mut options = JSONSchema::options();
        options.with_draft(Draft::Draft202012);
        let compiled = options.compile(&schema).unwrap();

        for path in [
            "/ui/index.html",
            r"ui\index.html",
            "ui/../index.html",
            "ui//index.html",
            "ui/%2e%2e/index.html",
            "ui/index.html?mode=full",
            "ui/index.html#main",
            "ui/index:alternate.html",
            "ui/index\n.html",
            "ui/index\u{85}.html",
        ] {
            let mut raw = ui_value();
            raw["contributes"]["pages"][0]["entry"] = json!(path);
            assert!(
                !compiled.is_valid(&raw),
                "schema accepted noncanonical path {path:?}"
            );
        }

        let mut nfc = ui_value();
        nfc["contributes"]["pages"][0]["entry"] = json!("ui/café.html");
        assert!(compiled.is_valid(&nfc));
    }

    #[test]
    fn bundled_schema_copy_is_equal_closed_and_local_only() {
        let repository_schema: Value = serde_json::from_slice(include_bytes!(
            "../../../schemas/plugin-manifest-v2.schema.json"
        ))
        .unwrap();
        let public_schema: Value = serde_json::from_slice(MANIFEST_SCHEMA_JSON).unwrap();
        assert_eq!(public_schema, repository_schema);
        validate_schema_references(&public_schema).unwrap();

        let mut stack = vec![&public_schema];
        while let Some(current) = stack.pop() {
            match current {
                Value::Array(values) => stack.extend(values),
                Value::Object(values) => {
                    if values.get("type") == Some(&json!("object")) {
                        assert_eq!(
                            values.get("additionalProperties"),
                            Some(&json!(false)),
                            "open object schema: {current}"
                        );
                    }
                    stack.extend(values.values());
                }
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
            }
        }
    }

    #[test]
    fn duplicate_json_keys_are_rejected_instead_of_last_write_wins() {
        let raw = br#"{
          "schemaVersion": 2,
          "schemaVersion": 1
        }"#;
        assert_eq!(
            validate_packaged_manifest(raw, &Target::darwin_arm64(), &future_host(),)
                .unwrap_err()
                .code(),
            "manifest_schema"
        );
    }

    #[test]
    fn byte_string_node_and_depth_quotas_are_stable_and_fast() {
        let cases = [
            (vec![b' '; MAX_MANIFEST_BYTES + 1], "manifest_too_large"),
            (
                serde_json::to_vec(&"x".repeat(MAX_JSON_STRING_BYTES + 1)).unwrap(),
                "manifest_too_large",
            ),
            (
                serde_json::to_vec(&vec![Value::Null; MAX_JSON_NODES + 1]).unwrap(),
                "manifest_too_large",
            ),
            (
                format!("{}null{}", "[".repeat(65), "]".repeat(65)).into_bytes(),
                "manifest_too_deep",
            ),
        ];
        for (bytes, expected) in cases {
            let started = Instant::now();
            let error = validate_packaged_manifest(&bytes, &Target::darwin_arm64(), &future_host())
                .unwrap_err();
            assert_eq!(error.code(), expected);
            assert!(started.elapsed() < Duration::from_secs(1));
        }
    }

    #[test]
    fn exact_quota_boundaries_are_not_rejected_as_overflow() {
        let exact_string = serde_json::to_vec(&"x".repeat(MAX_JSON_STRING_BYTES)).unwrap();
        assert!(parse_bounded_json(&exact_string).is_ok());

        let exact_nodes = serde_json::to_vec(&vec![Value::Null; MAX_JSON_NODES - 1]).unwrap();
        assert!(parse_bounded_json(&exact_nodes).is_ok());

        let exact_depth = format!("{}null{}", "[".repeat(64), "]".repeat(64)).into_bytes();
        assert!(parse_bounded_json(&exact_depth).is_ok());

        let exact_bytes = vec![b' '; MAX_MANIFEST_BYTES];
        assert_ne!(
            parse_bounded_json(&exact_bytes).unwrap_err().code(),
            "manifest_too_large"
        );
    }

    #[test]
    fn absolute_escape_and_missing_declared_entries_are_rejected() {
        for path in ["/tmp/index.html", "../index.html", "ui/../index.html"] {
            let mut raw = ui_value();
            raw["contributes"]["pages"][0]["entry"] = json!(path);
            let error = validate_packaged_manifest(
                &serde_json::to_vec(&raw).unwrap(),
                &Target::darwin_arm64(),
                &future_host(),
            )
            .unwrap_err();
            assert_eq!(error.code(), "manifest_schema");
        }

        let mut missing_page_entry = ui_value();
        missing_page_entry["contributes"]["pages"][0]
            .as_object_mut()
            .unwrap()
            .remove("entry");
        assert_eq!(
            validate_packaged_manifest(
                &serde_json::to_vec(&missing_page_entry).unwrap(),
                &Target::darwin_arm64(),
                &future_host(),
            )
            .unwrap_err()
            .code(),
            "manifest_schema"
        );

        let mut missing_native_entry: Value = serde_json::from_slice(VALID_NATIVE).unwrap();
        missing_native_entry["runtime"]
            .as_object_mut()
            .unwrap()
            .remove("bridgeEntry");
        assert_eq!(
            validate_source_manifest(
                &serde_json::to_vec(&missing_native_entry).unwrap(),
                &Target::darwin_arm64(),
                &future_host(),
            )
            .unwrap_err()
            .code(),
            "manifest_schema"
        );
    }

    #[test]
    fn validation_does_not_require_or_execute_native_entries() {
        let started = Instant::now();
        let manifest =
            validate_source_manifest(VALID_NATIVE, &Target::darwin_arm64(), &future_host())
                .unwrap();
        assert_eq!(manifest.runtime.kind, RuntimeKind::VerifiedNative);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn current_jarvis_version_is_explicitly_incompatible() {
        let current = HostCompatibility::parse("0.3.3", 2).unwrap();
        assert_eq!(
            validate_packaged_manifest(VALID_UI, &Target::darwin_arm64(), &current)
                .unwrap_err()
                .code(),
            "manifest_incompatible"
        );
    }

    #[test]
    fn both_supported_targets_have_canonical_package_tokens() {
        assert_eq!(Target::darwin_arm64().package_token(), "darwin-arm64");
        assert_eq!(Target::darwin_amd64().package_token(), "darwin-amd64");
    }
}
