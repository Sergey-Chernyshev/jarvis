#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use chrono::{DateTime, Utc};
    use ed25519_dalek::{Signer, SigningKey};
    use jarvis_plugin_protocol::package::PackageTarget;
    use serde_json::{json, Value};

    use super::{
        verify_catalog_bytes, CatalogCompatibility, CatalogState, RootTrustConfig,
    };
    use crate::plugins::trust::signature::catalog_signature_message;

    const ROOTS: &[u8] =
        include_bytes!("../../../tests/fixtures/plugin-trust/root-public.json");
    const CATALOG_1: &[u8] =
        include_bytes!("../../../tests/fixtures/plugin-trust/catalog-seq-1.json");
    const CATALOG_2: &[u8] =
        include_bytes!("../../../tests/fixtures/plugin-trust/catalog-seq-2-rotated.json");
    const PRODUCTION_ROOTS: &[u8] =
        include_bytes!("../../../resources/plugin-trust-roots.json");
    const SEED_HEX: &str =
        include_str!("../../../tests/fixtures/plugin-trust/package-test-signing-seed.hex");
    const PUBLIC_KEY: &str =
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn compatibility() -> CatalogCompatibility {
        CatalogCompatibility::parse(
            "0.4.0",
            2,
            PackageTarget::DarwinArm64,
            "14.0.0",
        )
        .unwrap()
    }

    fn fixture_state() -> CatalogState {
        CatalogState::new(RootTrustConfig::parse(ROOTS).unwrap())
    }

    fn catalog_value(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).unwrap()
    }

    fn signing_key() -> SigningKey {
        let raw = SEED_HEX.trim().as_bytes();
        let mut seed = [0u8; 32];
        for (index, chunk) in raw.chunks_exact(2).enumerate() {
            seed[index] = (nibble(chunk[0]) << 4) | nibble(chunk[1]);
        }
        SigningKey::from_bytes(&seed)
    }

    fn nibble(value: u8) -> u8 {
        match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            _ => panic!("fixture seed is lowercase hex"),
        }
    }

    fn signed_catalog(mut value: Value, key_ids: &[&str]) -> Vec<u8> {
        value["signatures"] = json!([{
            "algorithm": "ed25519",
            "keyId": "placeholder",
            "value": "gDDYgr16HoixPzQjmuL8+CTds3bPmnZlxOHqex3+FifEyJqpD8PHzZT5HUWX4tQrUrijxOGqKbQu/ZaPOSAjCQ=="
        }]);
        let parsed = jarvis_plugin_protocol::catalog::SignedCatalog::parse(
            &serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let signature = signing_key().sign(&catalog_signature_message(&parsed).unwrap());
        let signature = STANDARD.encode(signature.to_bytes());
        value["signatures"] = Value::Array(
            key_ids
                .iter()
                .map(|key_id| {
                    json!({
                        "algorithm": "ed25519",
                        "keyId": key_id,
                        "value": signature
                    })
                })
                .collect(),
        );
        serde_json::to_vec(&value).unwrap()
    }

    fn verify(
        bytes: &[u8],
        now: &str,
        state: &mut CatalogState,
    ) -> Result<super::VerifiedCatalog, crate::plugins::trust::TrustError> {
        verify_catalog_bytes(bytes, at(now), &compatibility(), state)
    }

    #[test]
    fn accepts_fresh_monotonic_catalog_and_binds_release_digest() {
        let mut state = fixture_state();
        let verified = verify(CATALOG_1, "2026-08-01T00:30:00Z", &mut state).unwrap();
        let release = verified
            .release(
                "dev.example.echo",
                "1.0.0",
                PackageTarget::DarwinArm64,
            )
            .unwrap();
        assert_eq!(
            release.archive_digest().as_str(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(state.sequence(), 1);
    }

    #[test]
    fn same_sequence_and_digest_are_idempotent() {
        let mut state = fixture_state();
        verify(CATALOG_1, "2026-08-01T00:30:00Z", &mut state).unwrap();
        let accepted = state.clone();
        verify(CATALOG_1, "2026-08-01T00:45:00Z", &mut state).unwrap();
        assert_eq!(state, accepted);
    }

    #[test]
    fn rejects_expired_replayed_conflicting_and_wrong_previous_catalogs_atomically() {
        let mut expired = catalog_value(CATALOG_1);
        expired["expiresAt"] = json!("2026-08-01T00:10:00Z");
        let expired = signed_catalog(expired, &["jarvis.root:1"]);
        let mut state = fixture_state();
        let initial = state.clone();
        assert_eq!(
            verify(&expired, "2026-08-01T00:30:00Z", &mut state)
                .unwrap_err()
                .code(),
            "catalog_expired"
        );
        assert_eq!(state, initial);

        verify(CATALOG_1, "2026-08-01T00:30:00Z", &mut state).unwrap();
        verify(CATALOG_2, "2026-08-01T01:30:00Z", &mut state).unwrap();
        let sequence_two = state.clone();
        assert_eq!(
            verify(CATALOG_1, "2026-08-01T01:30:00Z", &mut state)
                .unwrap_err()
                .code(),
            "catalog_replayed"
        );
        assert_eq!(state, sequence_two);

        let mut conflict_state = fixture_state();
        verify(
            CATALOG_1,
            "2026-08-01T00:30:00Z",
            &mut conflict_state,
        )
        .unwrap();
        let accepted = conflict_state.clone();
        let mut conflict = catalog_value(CATALOG_1);
        conflict["payload"]["releases"][0]["url"] =
            json!("https://plugins.jarvis.example/conflict.jarvis-plugin");
        let conflict = signed_catalog(conflict, &["jarvis.root:1"]);
        assert_eq!(
            verify(&conflict, "2026-08-01T00:30:00Z", &mut conflict_state)
                .unwrap_err()
                .code(),
            "catalog_conflict"
        );
        assert_eq!(conflict_state, accepted);

        let mut wrong_previous = catalog_value(CATALOG_2);
        wrong_previous["previousDigest"] =
            json!("sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let wrong_previous =
            signed_catalog(wrong_previous, &["jarvis.root:1", "jarvis.root:2"]);
        assert_eq!(
            verify(
                &wrong_previous,
                "2026-08-01T01:30:00Z",
                &mut conflict_state,
            )
            .unwrap_err()
            .code(),
            "catalog_previous_digest"
        );
        assert_eq!(conflict_state, accepted);
    }

    #[test]
    fn rejects_unknown_and_insufficient_root_thresholds() {
        let mut unknown = catalog_value(CATALOG_1);
        let unknown = signed_catalog(unknown.take(), &["unknown.root:1"]);
        let mut state = fixture_state();
        assert_eq!(
            verify(&unknown, "2026-08-01T00:30:00Z", &mut state)
                .unwrap_err()
                .code(),
            "catalog_unknown_root"
        );

        let roots = RootTrustConfig::parse(
            &serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "threshold": 2,
                "keys": [
                    {
                        "keyId": "jarvis.root:1",
                        "algorithm": "ed25519",
                        "publicKey": PUBLIC_KEY,
                        "validFrom": "2026-01-01T00:00:00Z",
                        "validUntil": "2027-08-01T00:00:00Z"
                    },
                    {
                        "keyId": "jarvis.root:2",
                        "algorithm": "ed25519",
                        "publicKey": PUBLIC_KEY,
                        "validFrom": "2026-01-01T00:00:00Z",
                        "validUntil": "2027-08-01T00:00:00Z"
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let mut threshold_state = CatalogState::new(roots);
        assert_eq!(
            verify(
                CATALOG_1,
                "2026-08-01T00:30:00Z",
                &mut threshold_state,
            )
            .unwrap_err()
            .code(),
            "catalog_threshold"
        );
    }

    #[test]
    fn rotation_requires_old_and_new_thresholds_before_replacing_roots() {
        let mut state = fixture_state();
        verify(CATALOG_1, "2026-08-01T00:30:00Z", &mut state).unwrap();
        let accepted = state.clone();

        let old_only = signed_catalog(catalog_value(CATALOG_2), &["jarvis.root:1"]);
        assert_eq!(
            verify(&old_only, "2026-08-01T01:30:00Z", &mut state)
                .unwrap_err()
                .code(),
            "catalog_rotation_threshold"
        );
        assert_eq!(state, accepted);

        let new_only = signed_catalog(catalog_value(CATALOG_2), &["jarvis.root:2"]);
        assert_eq!(
            verify(&new_only, "2026-08-01T01:30:00Z", &mut state)
                .unwrap_err()
                .code(),
            "catalog_threshold"
        );
        assert_eq!(state, accepted);

        verify(CATALOG_2, "2026-08-01T01:30:00Z", &mut state).unwrap();
        assert_eq!(state.sequence(), 2);
        assert_eq!(state.accepted_root_ids(), vec!["jarvis.root:2"]);
    }

    #[test]
    fn publisher_key_must_be_bound_to_plugin_and_revocation_blocks_release() {
        let mut unbound = catalog_value(CATALOG_1);
        unbound["payload"]["publisherLineages"][0]["pluginIds"] =
            json!(["dev.example.other"]);
        let unbound = signed_catalog(unbound, &["jarvis.root:1"]);
        let mut state = fixture_state();
        let initial = state.clone();
        assert_eq!(
            verify(&unbound, "2026-08-01T00:30:00Z", &mut state)
                .unwrap_err()
                .code(),
            "publisher_key_not_bound"
        );
        assert_eq!(state, initial);

        let mut revoked = catalog_value(CATALOG_1);
        revoked["payload"]["releases"][0]["revoked"] = json!(true);
        let revoked = signed_catalog(revoked, &["jarvis.root:1"]);
        let verified = verify(&revoked, "2026-08-01T00:30:00Z", &mut state).unwrap();
        assert_eq!(
            verified
                .release(
                    "dev.example.echo",
                    "1.0.0",
                    PackageTarget::DarwinArm64,
                )
                .unwrap_err()
                .code(),
            "package_revoked"
        );
    }

    #[test]
    fn empty_production_roots_fail_closed_without_test_material() {
        let roots = RootTrustConfig::parse(PRODUCTION_ROOTS).unwrap();
        assert!(!roots.is_provisioned());
        let mut state = CatalogState::new(roots);
        assert_eq!(
            verify(CATALOG_1, "2026-08-01T00:30:00Z", &mut state)
                .unwrap_err()
                .code(),
            "catalog_trust_not_provisioned"
        );
        assert!(!std::str::from_utf8(PRODUCTION_ROOTS)
            .unwrap()
            .contains(SEED_HEX.trim()));
    }
}
