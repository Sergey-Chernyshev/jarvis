#![cfg_attr(not(test), allow(dead_code))]

use chrono::{DateTime, Utc};
use jarvis_package::{PackageTrustError, PackageTrustVerifier, UntrustedPackageObservation};

use super::catalog::VerifiedCatalogRelease;
use super::signature::verify_package_signature;
use super::TrustError;

pub struct CatalogPackageVerifier {
    release: VerifiedCatalogRelease,
    now: DateTime<Utc>,
}

impl CatalogPackageVerifier {
    pub fn new(release: VerifiedCatalogRelease, now: DateTime<Utc>) -> Self {
        Self { release, now }
    }

    fn verify_observation(
        &self,
        observation: &UntrustedPackageObservation<'_>,
    ) -> Result<(), TrustError> {
        if self.now < self.release.catalog_issued_at() {
            return Err(TrustError::new("catalog_not_yet_valid"));
        }
        if self.now >= self.release.catalog_expires_at() {
            return Err(TrustError::new("catalog_expired"));
        }
        if self.release.is_revoked() {
            return Err(TrustError::new("package_revoked"));
        }

        let expected = self.release.release_record();
        let publisher_key = self.release.publisher_key();
        if publisher_key.key_id != expected.publisher_key_id
            || expected.package_signature.key_id != expected.publisher_key_id
            || publisher_key.algorithm != expected.package_signature.algorithm
        {
            return Err(TrustError::new("publisher_key_not_bound"));
        }
        let valid_from = parse_timestamp(&publisher_key.valid_from)?;
        let valid_until = parse_timestamp(&publisher_key.valid_until)?;
        if valid_from >= valid_until || self.now < valid_from || self.now >= valid_until {
            return Err(TrustError::new("publisher_key_not_valid"));
        }

        let observed = observation.metadata();
        if observed.plugin_id != expected.plugin_id
            || observed.publisher != expected.publisher
            || observed.version != expected.version
            || observed.target != expected.target
            || observed.minimum_macos != expected.minimum_macos
            || observed.jarvis_range != expected.jarvis_range
            || observed.plugin_api != expected.plugin_api
            || observation.archive_digest() != &expected.archive_digest
            || observation.signature() != &expected.package_signature
        {
            return Err(TrustError::new("package_catalog_mismatch"));
        }

        verify_package_signature(
            &publisher_key.public_key,
            observation.signature_message(),
            observation.signature().value(),
        )
    }
}

impl PackageTrustVerifier for CatalogPackageVerifier {
    fn verify(
        &self,
        observation: &UntrustedPackageObservation<'_>,
    ) -> Result<(), PackageTrustError> {
        self.verify_observation(observation)
            .map_err(|error| PackageTrustError::new(error.code()))
    }
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, TrustError> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|_| TrustError::new("publisher_key_not_valid"))
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::{Seek, SeekFrom};
    use std::os::fd::OwnedFd;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use chrono::{DateTime, Utc};
    use ed25519_dalek::{Signer, SigningKey};
    use jarvis_package::{
        extract_verified_package, inspect_and_verify_package, pack_plugin, PackOptions,
        PackageError, PackageSignatureSource, PackageTrustError, PackageTrustVerifier,
        UntrustedPackageObservation,
    };
    use jarvis_plugin_protocol::catalog::SignedCatalog;
    use jarvis_plugin_protocol::manifest::Digest;
    use jarvis_plugin_protocol::package::{
        MacOsVersion, PackageMetadataV1, PackageSignatureV1, PackageTarget, SignatureAlgorithm,
    };
    use serde_json::{json, Value};

    use super::CatalogPackageVerifier;
    use crate::plugins::manifest_v2::HostCompatibility;
    use crate::plugins::package::HostPackageDocumentAdapter;
    use crate::plugins::trust::catalog::{
        verify_catalog_bytes, CatalogCompatibility, CatalogState, RootTrustConfig,
        VerifiedCatalogRelease,
    };
    use crate::plugins::trust::signature::catalog_signature_message;

    const ROOTS: &[u8] = include_bytes!("../../../tests/fixtures/plugin-trust/root-public.json");
    const TEST_SEED: &str =
        include_str!("../../../tests/fixtures/plugin-trust/package-test-signing-seed.hex");
    const TEST_PUBLIC_KEY: &str =
        include_str!("../../../tests/fixtures/plugin-trust/package-test-public-key.hex");
    const SOURCE_RELATIVE: &str =
        "../crates/jarvis-package/tests/fixtures/plugin-packages/pack-source";
    const NOW: &str = "2026-08-01T00:30:00Z";

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new() -> Self {
            loop {
                let suffix = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir()
                    .join(format!("jarvis-a4-package-{}-{suffix}", std::process::id()));
                match fs::create_dir(&path) {
                    Ok(()) => return Self { path },
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("create isolated package test directory: {error}"),
                }
            }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[derive(Clone)]
    struct TestSignatureSource {
        seed: [u8; 32],
    }

    impl PackageSignatureSource for TestSignatureSource {
        fn sign(&self, message: &[u8]) -> Result<PackageSignatureV1, PackageError> {
            let signature = SigningKey::from_bytes(&self.seed).sign(message);
            PackageSignatureV1::new(
                SignatureAlgorithm::Ed25519,
                "example.release:1",
                STANDARD.encode(signature.to_bytes()),
            )
            .map_err(|_| PackageError::package_metadata())
        }
    }

    #[derive(Clone)]
    struct ObservationSnapshot {
        metadata: PackageMetadataV1,
        signature: PackageSignatureV1,
        archive_digest: Digest,
    }

    #[derive(Default)]
    struct CaptureVerifier {
        observation: Mutex<Option<ObservationSnapshot>>,
    }

    impl PackageTrustVerifier for CaptureVerifier {
        fn verify(
            &self,
            observation: &UntrustedPackageObservation<'_>,
        ) -> Result<(), PackageTrustError> {
            *self.observation.lock().unwrap() = Some(ObservationSnapshot {
                metadata: observation.metadata().clone(),
                signature: observation.signature().clone(),
                archive_digest: observation.archive_digest().clone(),
            });
            Err(PackageTrustError::new("test_observation_captured"))
        }
    }

    struct RecordingVerifier {
        inner: CatalogPackageVerifier,
        error: Mutex<Option<&'static str>>,
    }

    impl RecordingVerifier {
        fn new(inner: CatalogPackageVerifier) -> Self {
            Self {
                inner,
                error: Mutex::new(None),
            }
        }

        fn error_code(&self) -> Option<&'static str> {
            *self.error.lock().unwrap()
        }
    }

    impl PackageTrustVerifier for RecordingVerifier {
        fn verify(
            &self,
            observation: &UntrustedPackageObservation<'_>,
        ) -> Result<(), PackageTrustError> {
            let result = self.inner.verify(observation);
            *self.error.lock().unwrap() = result.as_ref().err().map(PackageTrustError::code);
            result
        }
    }

    struct PackedFixture {
        _root: TestDirectory,
        archive_path: PathBuf,
        observation: ObservationSnapshot,
    }

    impl PackedFixture {
        fn signed_with(seed: [u8; 32]) -> Self {
            let root = TestDirectory::new();
            let archive_path = root.path().join("package.jarvis-plugin");
            let source = Path::new(env!("CARGO_MANIFEST_DIR")).join(SOURCE_RELATIVE);
            let adapter = adapter();
            let mut archive = File::create(&archive_path).unwrap();
            let digest = pack_plugin(
                &source,
                PackOptions {
                    target: PackageTarget::DarwinArm64,
                    minimum_macos: MacOsVersion::parse("14.0.0").unwrap(),
                },
                &adapter,
                &TestSignatureSource { seed },
                &mut archive,
            )
            .unwrap();
            archive.seek(SeekFrom::Start(0)).unwrap();
            drop(archive);

            let capture = CaptureVerifier::default();
            assert_eq!(
                inspect_and_verify_package(File::open(&archive_path).unwrap(), &adapter, &capture,)
                    .unwrap_err()
                    .code(),
                "package_trust"
            );
            let observation = capture.observation.lock().unwrap().take().unwrap();
            assert_eq!(observation.archive_digest, digest);
            Self {
                _root: root,
                archive_path,
                observation,
            }
        }

        fn valid() -> Self {
            Self::signed_with(fixture_seed())
        }
    }

    #[derive(Clone)]
    struct CatalogSelection {
        plugin_id: String,
        version: String,
        target: PackageTarget,
        jarvis_version: String,
        plugin_api: u32,
        macos_version: String,
    }

    impl Default for CatalogSelection {
        fn default() -> Self {
            Self {
                plugin_id: "dev.example.package-fixture".to_owned(),
                version: "1.2.3".to_owned(),
                target: PackageTarget::DarwinArm64,
                jarvis_version: "0.4.0".to_owned(),
                plugin_api: 2,
                macos_version: "14.0.0".to_owned(),
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Mismatch {
        PluginId,
        Publisher,
        Version,
        Target,
        MinimumMacos,
        JarvisRange,
        PluginApi,
        ArchiveDigest,
        SignatureAlgorithm,
        SignatureKeyId,
        SignatureValue,
        PublisherLineage,
    }

    fn adapter() -> HostPackageDocumentAdapter {
        HostPackageDocumentAdapter::new(HostCompatibility::parse("0.4.0", 2).unwrap())
    }

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn fixture_seed() -> [u8; 32] {
        decode_seed(TEST_SEED.trim())
    }

    fn decode_seed(value: &str) -> [u8; 32] {
        let mut seed = [0u8; 32];
        for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
            seed[index] = (nibble(chunk[0]) << 4) | nibble(chunk[1]);
        }
        seed
    }

    fn nibble(value: u8) -> u8 {
        match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            _ => panic!("test seed is lowercase hex"),
        }
    }

    fn base_catalog(observation: &ObservationSnapshot) -> Value {
        let metadata = &observation.metadata;
        json!({
            "schemaVersion": 1,
            "sequence": 1,
            "issuedAt": "2026-08-01T00:00:00Z",
            "expiresAt": "2026-08-02T00:00:00Z",
            "previousDigest": null,
            "payload": {
                "publisherLineages": [{
                    "id": "example.release",
                    "publisher": metadata.publisher,
                    "pluginIds": [metadata.plugin_id],
                    "keys": [{
                        "keyId": "example.release:1",
                        "algorithm": "ed25519",
                        "publicKey": TEST_PUBLIC_KEY.trim(),
                        "validFrom": "2026-01-01T00:00:00Z",
                        "validUntil": "2027-08-01T00:00:00Z"
                    }]
                }],
                "releases": [{
                    "pluginId": metadata.plugin_id,
                    "publisher": metadata.publisher,
                    "version": metadata.version,
                    "publisherKeyId": "example.release:1",
                    "publisherLineage": "example.release",
                    "jarvisRange": metadata.jarvis_range,
                    "pluginApi": metadata.plugin_api,
                    "target": metadata.target,
                    "minimumMacos": metadata.minimum_macos,
                    "url": "https://plugins.jarvis.example/package.jarvis-plugin",
                    "archiveDigest": observation.archive_digest,
                    "packageSignature": observation.signature,
                    "revoked": false
                }],
                "rootRotation": null,
                "revokedPackageDigests": [],
                "revokedPublisherKeys": []
            },
            "signatures": [{
                "algorithm": "ed25519",
                "keyId": "jarvis.root:1",
                "value":
                    "gDDYgr16HoixPzQjmuL8+CTds3bPmnZlxOHqex3+FifEyJqpD8PHzZT5HUWX4tQrUrijxOGqKbQu/ZaPOSAjCQ=="
            }]
        })
    }

    fn sign_catalog(mut value: Value) -> Vec<u8> {
        let parsed = SignedCatalog::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        let signature = SigningKey::from_bytes(&fixture_seed())
            .sign(&catalog_signature_message(&parsed).unwrap());
        value["signatures"][0]["value"] = json!(STANDARD.encode(signature.to_bytes()));
        serde_json::to_vec(&value).unwrap()
    }

    fn verified_release(
        catalog: Value,
        selection: &CatalogSelection,
    ) -> Result<VerifiedCatalogRelease, crate::plugins::trust::TrustError> {
        let compatibility = CatalogCompatibility::parse(
            &selection.jarvis_version,
            selection.plugin_api,
            selection.target,
            &selection.macos_version,
        )
        .unwrap();
        let mut state = CatalogState::new(RootTrustConfig::parse(ROOTS).unwrap());
        let verified =
            verify_catalog_bytes(&sign_catalog(catalog), at(NOW), &compatibility, &mut state)?;
        verified
            .release(&selection.plugin_id, &selection.version, selection.target)
            .cloned()
    }

    fn flip_signature(value: &str) -> String {
        let mut decoded = STANDARD.decode(value).unwrap();
        decoded[0] ^= 1;
        STANDARD.encode(decoded)
    }

    fn apply_mismatch(mismatch: Mismatch, catalog: &mut Value, selection: &mut CatalogSelection) {
        match mismatch {
            Mismatch::PluginId => {
                catalog["payload"]["publisherLineages"][0]["pluginIds"][0] =
                    json!("dev.example.other");
                catalog["payload"]["releases"][0]["pluginId"] = json!("dev.example.other");
                selection.plugin_id = "dev.example.other".to_owned();
            }
            Mismatch::Publisher => {
                catalog["payload"]["publisherLineages"][0]["publisher"] = json!("other");
                catalog["payload"]["releases"][0]["publisher"] = json!("other");
            }
            Mismatch::Version => {
                catalog["payload"]["releases"][0]["version"] = json!("1.2.4");
                selection.version = "1.2.4".to_owned();
            }
            Mismatch::Target => {
                catalog["payload"]["releases"][0]["target"] = json!("darwin-amd64");
                selection.target = PackageTarget::DarwinAmd64;
            }
            Mismatch::MinimumMacos => {
                catalog["payload"]["releases"][0]["minimumMacos"] = json!("13.0.0");
            }
            Mismatch::JarvisRange => {
                catalog["payload"]["releases"][0]["jarvisRange"] = json!(">=0.3.0, <0.5.0");
            }
            Mismatch::PluginApi => {
                catalog["payload"]["releases"][0]["pluginApi"] = json!(3);
                selection.plugin_api = 3;
            }
            Mismatch::ArchiveDigest => {
                catalog["payload"]["releases"][0]["archiveDigest"] = json!(
                    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                );
            }
            Mismatch::SignatureAlgorithm => {
                catalog["payload"]["releases"][0]["packageSignature"]["algorithm"] =
                    json!("rsa-pss");
            }
            Mismatch::SignatureKeyId => {
                catalog["payload"]["publisherLineages"][0]["keys"][0]["keyId"] =
                    json!("example.release:2");
                catalog["payload"]["releases"][0]["publisherKeyId"] = json!("example.release:2");
                catalog["payload"]["releases"][0]["packageSignature"]["keyId"] =
                    json!("example.release:2");
            }
            Mismatch::SignatureValue => {
                let value = catalog["payload"]["releases"][0]["packageSignature"]["value"]
                    .as_str()
                    .unwrap();
                catalog["payload"]["releases"][0]["packageSignature"]["value"] =
                    json!(flip_signature(value));
            }
            Mismatch::PublisherLineage => {
                catalog["payload"]["releases"][0]["publisherLineage"] = json!("example.missing");
            }
        }
    }

    fn verifier_for(observation: &ObservationSnapshot) -> CatalogPackageVerifier {
        let release =
            verified_release(base_catalog(observation), &CatalogSelection::default()).unwrap();
        CatalogPackageVerifier::new(release, at(NOW))
    }

    #[test]
    fn catalog_package_verifier_accepts_exact_observation_and_signature() {
        let fixture = PackedFixture::valid();
        let evidence = inspect_and_verify_package(
            File::open(&fixture.archive_path).unwrap(),
            &adapter(),
            &verifier_for(&fixture.observation),
        )
        .unwrap();
        assert!(std::mem::size_of_val(&evidence) > 0);
    }

    #[test]
    fn catalog_package_verifier_rejects_each_release_field_mismatch() {
        let fixture = PackedFixture::valid();
        let output = TestDirectory::new();

        for mismatch in [
            Mismatch::PluginId,
            Mismatch::Publisher,
            Mismatch::Version,
            Mismatch::Target,
            Mismatch::MinimumMacos,
            Mismatch::JarvisRange,
            Mismatch::PluginApi,
            Mismatch::ArchiveDigest,
            Mismatch::SignatureKeyId,
            Mismatch::SignatureValue,
        ] {
            let mut catalog = base_catalog(&fixture.observation);
            let mut selection = CatalogSelection::default();
            apply_mismatch(mismatch, &mut catalog, &mut selection);
            let release = verified_release(catalog, &selection).unwrap();
            let recorder = RecordingVerifier::new(CatalogPackageVerifier::new(release, at(NOW)));
            assert_eq!(
                inspect_and_verify_package(
                    File::open(&fixture.archive_path).unwrap(),
                    &adapter(),
                    &recorder,
                )
                .unwrap_err()
                .code(),
                "package_trust",
                "{mismatch:?}"
            );
            assert_eq!(
                recorder.error_code(),
                Some("package_catalog_mismatch"),
                "{mismatch:?}"
            );
            assert!(!output.path().join("quarantine").exists());
        }

        let mut unsupported = base_catalog(&fixture.observation);
        apply_mismatch(
            Mismatch::SignatureAlgorithm,
            &mut unsupported,
            &mut CatalogSelection::default(),
        );
        assert_eq!(
            SignedCatalog::parse(&serde_json::to_vec(&unsupported).unwrap())
                .unwrap_err()
                .code(),
            "catalog_schema"
        );

        let mut unbound = base_catalog(&fixture.observation);
        apply_mismatch(
            Mismatch::PublisherLineage,
            &mut unbound,
            &mut CatalogSelection::default(),
        );
        assert_eq!(
            verified_release(unbound, &CatalogSelection::default())
                .unwrap_err()
                .code(),
            "publisher_lineage_invalid"
        );
        assert!(!output.path().join("quarantine").exists());
    }

    #[test]
    fn catalog_package_verifier_rejects_bad_signature_before_extraction() {
        let fixture = PackedFixture::signed_with([7u8; 32]);
        let output = TestDirectory::new();
        let recorder = RecordingVerifier::new(verifier_for(&fixture.observation));
        assert_eq!(
            inspect_and_verify_package(
                File::open(&fixture.archive_path).unwrap(),
                &adapter(),
                &recorder,
            )
            .unwrap_err()
            .code(),
            "package_trust"
        );
        assert_eq!(recorder.error_code(), Some("package_signature_invalid"));
        assert!(!output.path().join("quarantine").exists());
    }

    #[test]
    fn catalog_package_verifier_rejects_revocation_before_extraction() {
        let fixture = PackedFixture::valid();
        let output = TestDirectory::new();
        let mut catalog = base_catalog(&fixture.observation);
        catalog["payload"]["releases"][0]["revoked"] = json!(true);
        assert_eq!(
            verified_release(catalog, &CatalogSelection::default())
                .unwrap_err()
                .code(),
            "package_revoked"
        );
        assert!(!output.path().join("quarantine").exists());
    }

    #[test]
    fn verified_evidence_keeps_the_pass_one_archive_fd() {
        let fixture = PackedFixture::valid();
        let evidence = inspect_and_verify_package(
            File::open(&fixture.archive_path).unwrap(),
            &adapter(),
            &verifier_for(&fixture.observation),
        )
        .unwrap();
        let original = fixture._root.path().join("verified-original");
        fs::rename(&fixture.archive_path, &original).unwrap();
        fs::write(&fixture.archive_path, b"attacker replacement").unwrap();

        let output = fixture._root.path().join("output");
        fs::create_dir(&output).unwrap();
        let output_fd: OwnedFd = File::open(&output).unwrap().into();
        extract_verified_package(evidence, &output_fd, "quarantine").unwrap();
        assert_eq!(
            fs::read(output.join("quarantine/ui/index.html")).unwrap(),
            include_bytes!(
                "../../../../crates/jarvis-package/tests/fixtures/plugin-packages/pack-source/ui/index.html"
            )
        );
    }
}
