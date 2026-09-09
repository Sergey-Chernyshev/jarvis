use jarvis_plugin_protocol::catalog::{
    CatalogPayload, CatalogRelease, CatalogSignatureV1, PublisherKey, PublisherKeyLineage, RootKey,
    RootRotationProposal, SignedCatalog, CATALOG_SCHEMA_JSON, CATALOG_SCHEMA_VERSION,
};
use jarvis_plugin_protocol::package::{PackageTarget, SignatureAlgorithm};
use serde_json::{json, Value};

const DIGEST_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DIGEST_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SIGNATURE: &str =
    "gDDYgr16HoixPzQjmuL8+CTds3bPmnZlxOHqex3+FifEyJqpD8PHzZT5HUWX4tQrUrijxOGqKbQu/ZaPOSAjCQ==";
const PUBLIC_KEY: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";

fn catalog_value() -> Value {
    json!({
        "schemaVersion": 1,
        "sequence": 1,
        "issuedAt": "2026-08-01T00:00:00Z",
        "expiresAt": "2026-08-02T00:00:00Z",
        "previousDigest": null,
        "payload": {
            "publisherLineages": [{
                "id": "example.release",
                "publisher": "example",
                "pluginIds": ["dev.example.echo"],
                "keys": [{
                    "keyId": "example.release:1",
                    "algorithm": "ed25519",
                    "publicKey": PUBLIC_KEY,
                    "validFrom": "2026-01-01T00:00:00Z",
                    "validUntil": "2027-01-01T00:00:00Z"
                }]
            }],
            "releases": [{
                "pluginId": "dev.example.echo",
                "publisher": "example",
                "version": "1.0.0",
                "publisherKeyId": "example.release:1",
                "publisherLineage": "example.release",
                "jarvisRange": ">=0.4.0, <0.5.0",
                "pluginApi": 2,
                "target": "darwin-arm64",
                "minimumMacos": "14.0.0",
                "url": "https://plugins.jarvis.example/dev.example.echo/1.0.0/darwin-arm64.jarvis-plugin",
                "archiveDigest": DIGEST_A,
                "packageSignature": {
                    "algorithm": "ed25519",
                    "keyId": "example.release:1",
                    "value": SIGNATURE
                },
                "revoked": false
            }],
            "rootRotation": {
                "threshold": 1,
                "keys": [{
                    "keyId": "jarvis.root:2",
                    "algorithm": "ed25519",
                    "publicKey": PUBLIC_KEY,
                    "validFrom": "2026-08-01T00:00:00Z",
                    "validUntil": "2027-08-01T00:00:00Z"
                }]
            },
            "revokedPackageDigests": [DIGEST_B],
            "revokedPublisherKeys": ["example.release:0"]
        },
        "signatures": [{
            "algorithm": "ed25519",
            "keyId": "jarvis.root:1",
            "value": SIGNATURE
        }]
    })
}

#[test]
fn catalog_schema_copies_are_byte_identical() {
    assert_eq!(
        CATALOG_SCHEMA_JSON,
        include_bytes!("../../../schemas/plugin-catalog-v1.schema.json")
    );
}

#[test]
fn catalog_round_trips_every_release_equality_field() {
    let catalog = SignedCatalog::parse(&serde_json::to_vec(&catalog_value()).unwrap()).unwrap();
    assert_eq!(catalog.schema_version, CATALOG_SCHEMA_VERSION);
    assert_eq!(catalog.sequence, 1);
    assert_eq!(catalog.payload.releases.len(), 1);

    let CatalogRelease {
        plugin_id,
        publisher,
        version,
        publisher_key_id,
        publisher_lineage,
        jarvis_range,
        plugin_api,
        target,
        minimum_macos,
        url,
        archive_digest,
        package_signature,
        revoked,
    } = &catalog.payload.releases[0];
    assert_eq!(plugin_id.as_str(), "dev.example.echo");
    assert_eq!(publisher.as_str(), "example");
    assert_eq!(version.to_string(), "1.0.0");
    assert_eq!(publisher_key_id, "example.release:1");
    assert_eq!(publisher_lineage, "example.release");
    assert_eq!(jarvis_range.as_str(), ">=0.4.0, <0.5.0");
    assert_eq!(*plugin_api, 2);
    assert_eq!(*target, PackageTarget::DarwinArm64);
    assert_eq!(minimum_macos.as_str(), "14.0.0");
    assert_eq!(
        url,
        "https://plugins.jarvis.example/dev.example.echo/1.0.0/darwin-arm64.jarvis-plugin"
    );
    assert_eq!(archive_digest.as_str(), DIGEST_A);
    assert_eq!(package_signature.algorithm, SignatureAlgorithm::Ed25519);
    assert_eq!(package_signature.key_id, "example.release:1");
    assert_eq!(package_signature.value(), SIGNATURE);
    assert!(!revoked);
    assert_eq!(serde_json::to_value(catalog).unwrap(), catalog_value());
}

#[test]
fn catalog_structs_are_closed_and_reject_duplicate_json_keys() {
    let mut unknown = catalog_value();
    unknown["payload"]["releases"][0]["escapeSandbox"] = json!(true);
    assert!(SignedCatalog::parse(&serde_json::to_vec(&unknown).unwrap()).is_err());

    let duplicate = br#"{
      "schemaVersion":1,
      "schemaVersion":1,
      "sequence":1,
      "issuedAt":"2026-08-01T00:00:00Z",
      "expiresAt":"2026-08-02T00:00:00Z",
      "previousDigest":null,
      "payload":{"publisherLineages":[],"releases":[],"rootRotation":null,"revokedPackageDigests":[],"revokedPublisherKeys":[]},
      "signatures":[{"algorithm":"ed25519","keyId":"jarvis.root:1","value":"gDDYgr16HoixPzQjmuL8+CTds3bPmnZlxOHqex3+FifEyJqpD8PHzZT5HUWX4tQrUrijxOGqKbQu/ZaPOSAjCQ=="}]
    }"#;
    assert_eq!(
        SignedCatalog::parse(duplicate).unwrap_err().code(),
        "catalog_json"
    );
}

#[test]
fn catalog_parser_enforces_nested_cardinality_uniqueness_and_string_bounds() {
    let mut duplicate_signature = catalog_value();
    let signature = duplicate_signature["signatures"][0].clone();
    duplicate_signature["signatures"]
        .as_array_mut()
        .unwrap()
        .push(signature);
    assert_eq!(
        SignedCatalog::parse(&serde_json::to_vec(&duplicate_signature).unwrap())
            .unwrap_err()
            .code(),
        "catalog_duplicate"
    );

    let mut empty_plugin_binding = catalog_value();
    empty_plugin_binding["payload"]["publisherLineages"][0]["pluginIds"] = json!([]);
    assert_eq!(
        SignedCatalog::parse(&serde_json::to_vec(&empty_plugin_binding).unwrap())
            .unwrap_err()
            .code(),
        "catalog_cardinality"
    );

    let mut long_lineage = catalog_value();
    long_lineage["payload"]["publisherLineages"][0]["id"] = json!("x".repeat(129));
    assert_eq!(
        SignedCatalog::parse(&serde_json::to_vec(&long_lineage).unwrap())
            .unwrap_err()
            .code(),
        "catalog_string"
    );
}

#[test]
fn catalog_signature_and_public_keys_require_canonical_encodings() {
    let mut bad_signature = catalog_value();
    bad_signature["signatures"][0]["value"] = json!(
        "gDDYgr16HoixPzQjmuL8+CTds3bPmnZlxOHqex3+FifEyJqpD8PHzZT5HUWX4tQrUrijxOGqKbQu/ZaPOSAjCR=="
    );
    assert!(SignedCatalog::parse(&serde_json::to_vec(&bad_signature).unwrap()).is_err());

    let mut bad_public_key = catalog_value();
    bad_public_key["payload"]["publisherLineages"][0]["keys"][0]["publicKey"] =
        json!(PUBLIC_KEY.to_uppercase());
    assert!(SignedCatalog::parse(&serde_json::to_vec(&bad_public_key).unwrap()).is_err());
}

#[test]
fn root_threshold_cannot_count_the_same_public_key_under_multiple_ids() {
    let mut duplicate_key = catalog_value();
    duplicate_key["payload"]["rootRotation"]["threshold"] = json!(2);
    let mut second = duplicate_key["payload"]["rootRotation"]["keys"][0].clone();
    second["keyId"] = json!("jarvis.root:3");
    duplicate_key["payload"]["rootRotation"]["keys"]
        .as_array_mut()
        .unwrap()
        .push(second);
    assert_eq!(
        SignedCatalog::parse(&serde_json::to_vec(&duplicate_key).unwrap())
            .unwrap_err()
            .code(),
        "catalog_duplicate"
    );
}

#[test]
fn catalog_sequence_stays_within_the_jcs_exact_integer_range() {
    let mut maximum = catalog_value();
    maximum["sequence"] = json!(9_007_199_254_740_991_u64);
    assert!(SignedCatalog::parse(&serde_json::to_vec(&maximum).unwrap()).is_ok());

    let mut aliased = maximum;
    aliased["sequence"] = json!(9_007_199_254_740_992_u64);
    assert_eq!(
        SignedCatalog::parse(&serde_json::to_vec(&aliased).unwrap())
            .unwrap_err()
            .code(),
        "catalog_schema"
    );
}

#[test]
fn catalog_public_dtos_keep_exact_wire_names() {
    let parsed = SignedCatalog::parse(&serde_json::to_vec(&catalog_value()).unwrap()).unwrap();
    let CatalogPayload {
        publisher_lineages,
        releases,
        root_rotation,
        revoked_package_digests,
        revoked_publisher_keys,
    } = parsed.payload;
    assert_eq!(publisher_lineages.len(), 1);
    assert_eq!(releases.len(), 1);
    assert_eq!(revoked_package_digests[0].as_str(), DIGEST_B);
    assert_eq!(revoked_publisher_keys, ["example.release:0"]);

    let PublisherKeyLineage {
        id,
        publisher,
        plugin_ids,
        keys,
    } = &publisher_lineages[0];
    assert_eq!(id, "example.release");
    assert_eq!(publisher.as_str(), "example");
    assert_eq!(plugin_ids[0].as_str(), "dev.example.echo");
    let PublisherKey {
        key_id, public_key, ..
    } = &keys[0];
    assert_eq!(key_id, "example.release:1");
    assert_eq!(public_key, PUBLIC_KEY);

    let RootRotationProposal { threshold, keys } = root_rotation.unwrap();
    assert_eq!(threshold, 1);
    let RootKey {
        key_id, public_key, ..
    } = &keys[0];
    assert_eq!(key_id, "jarvis.root:2");
    assert_eq!(public_key, PUBLIC_KEY);

    let CatalogSignatureV1 {
        algorithm,
        key_id,
        value,
    } = &parsed.signatures[0];
    assert_eq!(*algorithm, SignatureAlgorithm::Ed25519);
    assert_eq!(key_id, "jarvis.root:1");
    assert_eq!(value, SIGNATURE);
}
