#[cfg(test)]
mod tests {
    use jarvis_package::PackageDocumentAdapter;
    use jarvis_plugin_protocol::package::{
        PackageTarget, PACKAGE_METADATA_SCHEMA_JSON, PACKAGE_SIGNATURE_SCHEMA_JSON,
    };
    use serde_json::json;

    use super::HostPackageDocumentAdapter;
    use crate::plugins::manifest_v2::HostCompatibility;

    const VALID_NATIVE: &[u8] =
        include_bytes!("../../tests/fixtures/plugin-packages/valid-native/plugin.json");

    fn adapter() -> HostPackageDocumentAdapter {
        HostPackageDocumentAdapter::new(
            HostCompatibility::parse("0.4.0", 2).unwrap(),
        )
    }

    #[test]
    fn real_adapter_resolves_source_target_and_revalidates_packaged_manifest() {
        let adapter = adapter();
        let manifest = adapter
            .resolve_source_manifest(VALID_NATIVE, PackageTarget::DarwinArm64)
            .unwrap();
        assert_eq!(
            manifest.runtime.bridge_entry.as_ref().unwrap().as_str(),
            "bin/darwin-arm64/example-bridge"
        );

        let concrete = serde_json::to_vec(&manifest).unwrap();
        let reparsed = adapter
            .validate_packaged_manifest(&concrete, PackageTarget::DarwinArm64)
            .unwrap();
        assert_eq!(reparsed, manifest);
    }

    #[test]
    fn real_adapter_validates_closed_metadata_and_signature_schemas() {
        let adapter = adapter();
        let metadata = json!({
            "schemaVersion": 1,
            "pluginId": "dev.example.echo",
            "publisher": "example",
            "version": "1.0.0",
            "manifestDigest":
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "target": "darwin-arm64",
            "minimumMacos": "14.0.0",
            "jarvisRange": ">=0.4.0, <0.5.0",
            "pluginApi": 2,
            "state": {
                "schemaVersion": 1,
                "migrations": [],
                "rollbackCompatibleThrough": 1
            },
            "files": [{
                "path": "plugin.json",
                "kind": "regular",
                "mode": "0444",
                "size": 1,
                "digest":
                    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            }],
            "payloadRoot":
                "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
        });
        adapter
            .validate_package_metadata_schema(&serde_json::to_vec(&metadata).unwrap())
            .unwrap();

        let signature = json!({
            "algorithm": "ed25519",
            "keyId": "example.release:1",
            "value":
                "paWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpQ=="
        });
        adapter
            .validate_package_signature_schema(
                &serde_json::to_vec(&signature).unwrap(),
            )
            .unwrap();

        let mut unknown = metadata;
        unknown["escapeSandbox"] = json!(true);
        assert!(adapter
            .validate_package_metadata_schema(
                &serde_json::to_vec(&unknown).unwrap()
            )
            .is_err());
    }

    #[test]
    fn bundled_package_schemas_have_no_remote_references() {
        for schema in [
            PACKAGE_METADATA_SCHEMA_JSON,
            PACKAGE_SIGNATURE_SCHEMA_JSON,
        ] {
            let value: serde_json::Value = serde_json::from_slice(schema).unwrap();
            let mut stack = vec![&value];
            while let Some(current) = stack.pop() {
                match current {
                    serde_json::Value::Array(values) => stack.extend(values),
                    serde_json::Value::Object(values) => {
                        if let Some(reference) = values.get("$ref") {
                            assert!(reference
                                .as_str()
                                .unwrap()
                                .starts_with("#/$defs/"));
                        }
                        stack.extend(values.values());
                    }
                    _ => {}
                }
            }
        }
    }
}
