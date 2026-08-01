use jarvis_plugin_protocol::manifest::{Digest, PluginId, PublisherId, StateDeclaration};
use jarvis_plugin_protocol::package::{
    MacOsVersion, PackageFile, PackageFileKind, PackageFileMode, PackageMetadataV1, PackagePath,
    PackageSignatureV1, PackageTarget, SignatureAlgorithm, PACKAGE_METADATA_SCHEMA_JSON,
    PACKAGE_SCHEMA_VERSION, PACKAGE_SIGNATURE_SCHEMA_JSON,
};
use semver::Version;
use serde_json::{json, Value};

const DIGEST_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DIGEST_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn metadata_value() -> Value {
    json!({
        "schemaVersion": 1,
        "pluginId": "dev.example.echo",
        "publisher": "example",
        "version": "1.2.3",
        "manifestDigest": DIGEST_A,
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
            "size": 128,
            "digest": DIGEST_A
        }],
        "payloadRoot": DIGEST_B
    })
}

#[test]
fn package_schema_copies_are_byte_identical() {
    assert_eq!(
        PACKAGE_METADATA_SCHEMA_JSON,
        include_bytes!("../../../schemas/plugin-package-v1.schema.json")
    );
}

#[test]
fn package_signature_schema_copies_are_byte_identical() {
    assert_eq!(
        PACKAGE_SIGNATURE_SCHEMA_JSON,
        include_bytes!("../../../schemas/plugin-package-signature-v1.schema.json")
    );
}

#[test]
fn package_schema_rejects_unknown_fields_and_wrong_enum_spellings() {
    let mut unknown = metadata_value();
    unknown["unexpected"] = json!(true);
    assert!(serde_json::from_value::<PackageMetadataV1>(unknown).is_err());

    let mut target = metadata_value();
    target["target"] = json!("darwin-aarch64");
    assert!(serde_json::from_value::<PackageMetadataV1>(target).is_err());

    let mut kind = metadata_value();
    kind["files"][0]["kind"] = json!("file");
    assert!(serde_json::from_value::<PackageMetadataV1>(kind).is_err());

    let mut mode = metadata_value();
    mode["files"][0]["mode"] = json!("444");
    assert!(serde_json::from_value::<PackageMetadataV1>(mode).is_err());
}

#[test]
fn package_path_accepts_exact_1024_bytes_and_rejects_1025() {
    let exact = ["a".repeat(255), "b".repeat(255), "c".repeat(255), "d".repeat(255)]
        .join("/");
    assert_eq!(exact.len(), 1023);
    let exact = format!("x/{exact}");
    assert_eq!(exact.len(), 1025);

    let exact = [
        "a".repeat(254),
        "b".repeat(255),
        "c".repeat(255),
        "d".repeat(255),
    ]
    .join("/");
    assert_eq!(exact.len(), 1022);
    let exact = format!("x/{exact}");
    assert_eq!(exact.len(), 1024);
    assert_eq!(PackagePath::new(&exact).unwrap().as_str(), exact);

    let over = format!("{exact}x");
    assert_eq!(over.len(), 1025);
    assert!(PackagePath::new(over).is_err());
}

#[test]
fn package_path_accepts_exact_255_byte_component_and_rejects_256() {
    let exact = "é".repeat(127) + "a";
    assert_eq!(exact.len(), 255);
    assert!(PackagePath::new(&exact).is_ok());

    let over = exact + "b";
    assert_eq!(over.len(), 256);
    assert!(PackagePath::new(over).is_err());
}

#[test]
fn macos_version_requires_canonical_three_component_form() {
    for valid in ["0.0.0", "14.0.0", "999.999.999"] {
        assert_eq!(MacOsVersion::parse(valid).unwrap().as_str(), valid);
    }
    for invalid in [
        "14",
        "14.0",
        "14.0.0.0",
        "014.0.0",
        "14.00.0",
        "+14.0.0",
        "-1.0.0",
        "14.0.0-beta",
        "14.0.0+build",
        " 14.0.0",
        "14.0.0 ",
    ] {
        assert!(MacOsVersion::parse(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn signature_requires_canonical_padded_base64_of_64_bytes() {
    let canonical = "paWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpQ==";
    let signature: PackageSignatureV1 = serde_json::from_value(json!({
        "algorithm": "ed25519",
        "keyId": "example.release-key:1",
        "value": canonical
    }))
    .unwrap();
    assert_eq!(signature.algorithm, SignatureAlgorithm::Ed25519);
    assert_eq!(signature.value(), canonical);

    for invalid in [
        "",
        "paWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpQ",
        "paWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpQ__",
        "paWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpR==",
    ] {
        let value = json!({
            "algorithm": "ed25519",
            "keyId": "key",
            "value": invalid
        });
        assert!(serde_json::from_value::<PackageSignatureV1>(value).is_err());
    }
}

#[test]
fn package_metadata_round_trips_without_wire_field_drift() {
    let metadata: PackageMetadataV1 = serde_json::from_value(metadata_value()).unwrap();
    assert_eq!(metadata.schema_version, PACKAGE_SCHEMA_VERSION);
    assert_eq!(metadata.plugin_id, PluginId::new("dev.example.echo").unwrap());
    assert_eq!(metadata.publisher, PublisherId::new("example").unwrap());
    assert_eq!(metadata.version, Version::parse("1.2.3").unwrap());
    assert_eq!(metadata.manifest_digest, Digest::new(DIGEST_A).unwrap());
    assert_eq!(metadata.target, PackageTarget::DarwinArm64);
    assert_eq!(metadata.minimum_macos, MacOsVersion::parse("14.0.0").unwrap());
    assert_eq!(metadata.jarvis_range.as_str(), ">=0.4.0, <0.5.0");
    assert_eq!(
        metadata.state,
        StateDeclaration {
            schema_version: 1,
            migrations: Vec::new(),
            rollback_compatible_through: 1,
        }
    );
    assert_eq!(
        metadata.files,
        vec![PackageFile {
            path: PackagePath::new("plugin.json").unwrap(),
            kind: PackageFileKind::Regular,
            mode: PackageFileMode::ReadOnly,
            size: 128,
            digest: Digest::new(DIGEST_A).unwrap(),
        }]
    );
    assert_eq!(
        serde_json::to_value(metadata).unwrap(),
        metadata_value()
    );
}
