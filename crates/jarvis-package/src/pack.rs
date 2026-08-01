use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use jarvis_plugin_protocol::json::JsonLimits;
use jarvis_plugin_protocol::manifest::{ManifestError, ManifestV2, RuntimeKind};
#[cfg(test)]
use jarvis_plugin_protocol::package::SignatureAlgorithm;
use jarvis_plugin_protocol::package::{
    MacOsVersion, PackageFile, PackageFileKind, PackageFileMode, PackageMetadataV1, PackagePath,
    PackageSignatureV1, PackageTarget, PACKAGE_SCHEMA_VERSION,
};

use crate::hash::{merkle_root, sha256_digest};
use crate::jcs::parse_exact_jcs;

pub const SIGNATURE_MESSAGE_DOMAIN: &[u8] = b"jarvis-plugin-package-v1";

const PACKAGE_JSON_LIMITS: JsonLimits = JsonLimits {
    max_bytes: 16 * 1024 * 1024,
    max_depth: 64,
    max_nodes: 250_000,
    max_string_bytes: 64 * 1024,
};
const SIGNATURE_JSON_LIMITS: JsonLimits = JsonLimits {
    max_bytes: 4 * 1024,
    max_depth: 8,
    max_nodes: 16,
    max_string_bytes: 4 * 1024,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageError {
    code: &'static str,
}

impl PackageError {
    pub fn manifest(error: ManifestError) -> Self {
        Self { code: error.code() }
    }

    pub fn package_metadata() -> Self {
        Self {
            code: "package_metadata",
        }
    }

    pub(crate) fn source_invalid() -> Self {
        Self {
            code: "source_invalid",
        }
    }

    pub(crate) fn source_raced() -> Self {
        Self {
            code: "source_raced",
        }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for PackageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for PackageError {}

pub trait PackageDocumentAdapter {
    fn resolve_source_manifest(
        &self,
        spooled_bytes: &[u8],
        target: PackageTarget,
    ) -> Result<ManifestV2, PackageError>;

    fn validate_packaged_manifest(
        &self,
        canonical_bytes: &[u8],
        target: PackageTarget,
    ) -> Result<ManifestV2, PackageError>;

    fn validate_package_metadata_schema(&self, canonical_bytes: &[u8]) -> Result<(), PackageError>;

    fn validate_package_signature_schema(&self, canonical_bytes: &[u8])
        -> Result<(), PackageError>;
}

pub trait PackageSignatureSource {
    fn sign(&self, message: &[u8]) -> Result<PackageSignatureV1, PackageError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackOptions {
    pub target: PackageTarget,
    pub minimum_macos: MacOsVersion,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PayloadObservation {
    path: PackagePath,
    size: u64,
    digest: jarvis_plugin_protocol::manifest::Digest,
}

impl PayloadObservation {
    pub(crate) fn new(
        path: PackagePath,
        size: u64,
        digest: jarvis_plugin_protocol::manifest::Digest,
    ) -> Self {
        Self { path, size, digest }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedPackageDocuments {
    manifest: ManifestV2,
    manifest_bytes: Vec<u8>,
    metadata: PackageMetadataV1,
    metadata_bytes: Vec<u8>,
    signature: PackageSignatureV1,
    signature_bytes: Vec<u8>,
    signature_message: Vec<u8>,
}

impl PreparedPackageDocuments {
    pub(crate) fn manifest(&self) -> &ManifestV2 {
        &self.manifest
    }

    pub(crate) fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }

    pub(crate) fn metadata(&self) -> &PackageMetadataV1 {
        &self.metadata
    }

    pub(crate) fn metadata_bytes(&self) -> &[u8] {
        &self.metadata_bytes
    }

    pub(crate) fn signature(&self) -> &PackageSignatureV1 {
        &self.signature
    }

    pub(crate) fn signature_bytes(&self) -> &[u8] {
        &self.signature_bytes
    }

    pub(crate) fn signature_message(&self) -> &[u8] {
        &self.signature_message
    }
}

pub(crate) fn prepare_package_documents<A, S>(
    source_manifest_bytes: &[u8],
    payload: Vec<PayloadObservation>,
    options: PackOptions,
    adapter: &A,
    signature_source: &S,
) -> Result<PreparedPackageDocuments, PackageError>
where
    A: PackageDocumentAdapter,
    S: PackageSignatureSource,
{
    let manifest = adapter.resolve_source_manifest(source_manifest_bytes, options.target)?;
    let manifest_bytes = serde_json_canonicalizer::to_vec(&manifest)
        .map_err(|_| PackageError::package_metadata())?;
    let packaged_manifest = adapter.validate_packaged_manifest(&manifest_bytes, options.target)?;
    if packaged_manifest != manifest {
        return Err(PackageError::package_metadata());
    }

    let mut by_path = BTreeMap::new();
    for observed in payload {
        let path = observed.path.as_str();
        if path.eq_ignore_ascii_case("package.json")
            || path.eq_ignore_ascii_case("SIGNATURE")
            || by_path.insert(path.to_owned(), observed).is_some()
        {
            return Err(PackageError::package_metadata());
        }
    }
    let Some(_) = by_path.remove("plugin.json") else {
        return Err(PackageError::package_metadata());
    };

    let mut executable_paths = BTreeSet::new();
    match manifest.runtime.kind {
        RuntimeKind::UiOnly => {}
        RuntimeKind::VerifiedNative => {
            let bridge = manifest
                .runtime
                .bridge_entry
                .as_ref()
                .ok_or_else(PackageError::package_metadata)?;
            let service = manifest
                .runtime
                .service
                .as_ref()
                .ok_or_else(PackageError::package_metadata)?;
            executable_paths.insert(bridge.as_str().to_owned());
            executable_paths.insert(service.entry.as_str().to_owned());
            if executable_paths
                .iter()
                .any(|path| !by_path.contains_key(path))
            {
                return Err(PackageError::package_metadata());
            }
        }
    }

    let mut files = Vec::with_capacity(by_path.len() + 1);
    files.push(PackageFile {
        path: PackagePath::new("plugin.json").map_err(|_| PackageError::package_metadata())?,
        kind: PackageFileKind::Regular,
        mode: PackageFileMode::ReadOnly,
        size: u64::try_from(manifest_bytes.len()).map_err(|_| PackageError::package_metadata())?,
        digest: sha256_digest(&manifest_bytes),
    });
    for (_, observed) in by_path {
        let mode = if executable_paths.contains(observed.path.as_str()) {
            PackageFileMode::Executable
        } else {
            PackageFileMode::ReadOnly
        };
        files.push(PackageFile {
            path: observed.path,
            kind: PackageFileKind::Regular,
            mode,
            size: observed.size,
            digest: observed.digest,
        });
    }

    let metadata = PackageMetadataV1 {
        schema_version: PACKAGE_SCHEMA_VERSION,
        plugin_id: manifest.id.clone(),
        publisher: manifest.publisher.clone(),
        version: manifest.version.clone(),
        manifest_digest: sha256_digest(&manifest_bytes),
        target: options.target,
        minimum_macos: options.minimum_macos,
        jarvis_range: manifest.compatibility.jarvis.clone(),
        plugin_api: manifest.compatibility.plugin_api,
        state: manifest.state.clone(),
        payload_root: merkle_root(&files).map_err(|_| PackageError::package_metadata())?,
        files,
    };
    let metadata_bytes = serde_json_canonicalizer::to_vec(&metadata)
        .map_err(|_| PackageError::package_metadata())?;
    adapter.validate_package_metadata_schema(&metadata_bytes)?;
    let metadata_value = parse_exact_jcs(&metadata_bytes, PACKAGE_JSON_LIMITS)
        .map_err(|_| PackageError::package_metadata())?;
    let validated_metadata: PackageMetadataV1 =
        serde_json::from_value(metadata_value).map_err(|_| PackageError::package_metadata())?;
    if validated_metadata != metadata {
        return Err(PackageError::package_metadata());
    }

    let mut signature_message =
        Vec::with_capacity(SIGNATURE_MESSAGE_DOMAIN.len() + 1 + metadata_bytes.len());
    signature_message.extend_from_slice(SIGNATURE_MESSAGE_DOMAIN);
    signature_message.push(0);
    signature_message.extend_from_slice(&metadata_bytes);
    let signature = signature_source.sign(&signature_message)?;
    signature
        .validate()
        .map_err(|_| PackageError::package_metadata())?;
    let signature_bytes = serde_json_canonicalizer::to_vec(&signature)
        .map_err(|_| PackageError::package_metadata())?;
    adapter.validate_package_signature_schema(&signature_bytes)?;
    let signature_value = parse_exact_jcs(&signature_bytes, SIGNATURE_JSON_LIMITS)
        .map_err(|_| PackageError::package_metadata())?;
    let validated_signature: PackageSignatureV1 =
        serde_json::from_value(signature_value).map_err(|_| PackageError::package_metadata())?;
    if validated_signature != signature {
        return Err(PackageError::package_metadata());
    }

    Ok(PreparedPackageDocuments {
        manifest,
        manifest_bytes,
        metadata,
        metadata_bytes,
        signature,
        signature_bytes,
        signature_message,
    })
}

#[cfg(test)]
pub(crate) const FIXED_OPAQUE_SIGNATURE_VALUE: &str =
    "paWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpQ==";

#[cfg(test)]
pub(crate) struct FixedOpaqueSignature;

#[cfg(test)]
impl PackageSignatureSource for FixedOpaqueSignature {
    fn sign(&self, _message: &[u8]) -> Result<PackageSignatureV1, PackageError> {
        PackageSignatureV1::new(
            SignatureAlgorithm::Ed25519,
            "fixture.opaque:1",
            FIXED_OPAQUE_SIGNATURE_VALUE,
        )
        .map_err(|_| PackageError::package_metadata())
    }
}

#[cfg(test)]
pub(crate) fn fixed_opaque_observation_matches(
    observed_message: &[u8],
    observed_signature: &PackageSignatureV1,
    expected_message: &[u8],
) -> bool {
    let expected_signature = FixedOpaqueSignature
        .sign(expected_message)
        .expect("fixed test signature is valid");
    observed_message == expected_message && observed_signature == &expected_signature
}

#[cfg(test)]
mod tests {
    use jarvis_plugin_protocol::manifest::{ManifestV2, RuntimeKind};
    use jarvis_plugin_protocol::package::{
        MacOsVersion, PackageFileMode, PackagePath, PackageSignatureV1, PackageTarget,
    };

    use super::{
        fixed_opaque_observation_matches, prepare_package_documents, FixedOpaqueSignature,
        PackOptions, PackageDocumentAdapter, PackageError, PayloadObservation,
        FIXED_OPAQUE_SIGNATURE_VALUE, SIGNATURE_MESSAGE_DOMAIN,
    };
    use crate::hash::sha256_digest;

    const SOURCE_MANIFEST: &[u8] =
        include_bytes!("../tests/fixtures/plugin-packages/pack-source/plugin.json");
    const UI: &[u8] = include_bytes!("../tests/fixtures/plugin-packages/pack-source/ui/index.html");
    const SCHEMA: &[u8] =
        include_bytes!("../tests/fixtures/plugin-packages/pack-source/schemas/message.schema.json");

    struct FixtureAdapter;

    impl PackageDocumentAdapter for FixtureAdapter {
        fn resolve_source_manifest(
            &self,
            spooled_bytes: &[u8],
            _target: PackageTarget,
        ) -> Result<ManifestV2, PackageError> {
            ManifestV2::parse(spooled_bytes).map_err(PackageError::manifest)
        }

        fn validate_packaged_manifest(
            &self,
            canonical_bytes: &[u8],
            _target: PackageTarget,
        ) -> Result<ManifestV2, PackageError> {
            ManifestV2::parse(canonical_bytes).map_err(PackageError::manifest)
        }

        fn validate_package_metadata_schema(
            &self,
            _canonical_bytes: &[u8],
        ) -> Result<(), PackageError> {
            Ok(())
        }

        fn validate_package_signature_schema(
            &self,
            _canonical_bytes: &[u8],
        ) -> Result<(), PackageError> {
            Ok(())
        }
    }

    fn observation(path: &str, bytes: &[u8]) -> PayloadObservation {
        PayloadObservation::new(
            PackagePath::new(path).unwrap(),
            u64::try_from(bytes.len()).unwrap(),
            sha256_digest(bytes),
        )
    }

    fn ui_payload() -> Vec<PayloadObservation> {
        vec![
            observation("plugin.json", SOURCE_MANIFEST),
            observation("ui/index.html", UI),
            observation("schemas/message.schema.json", SCHEMA),
        ]
    }

    fn options() -> PackOptions {
        PackOptions {
            target: PackageTarget::DarwinArm64,
            minimum_macos: MacOsVersion::parse("14.0.0").unwrap(),
        }
    }

    #[test]
    fn metadata_equals_concrete_manifest() {
        let prepared = prepare_package_documents(
            SOURCE_MANIFEST,
            ui_payload(),
            options(),
            &FixtureAdapter,
            &FixedOpaqueSignature,
        )
        .unwrap();
        let manifest = prepared.manifest();
        let metadata = prepared.metadata();

        assert_eq!(manifest.runtime.kind, RuntimeKind::UiOnly);
        assert_eq!(metadata.plugin_id, manifest.id);
        assert_eq!(metadata.publisher, manifest.publisher);
        assert_eq!(metadata.version, manifest.version);
        assert_eq!(metadata.jarvis_range, manifest.compatibility.jarvis);
        assert_eq!(metadata.plugin_api, manifest.compatibility.plugin_api);
        assert_eq!(metadata.state, manifest.state);
        assert_eq!(
            metadata.manifest_digest,
            sha256_digest(prepared.manifest_bytes())
        );
        assert_eq!(
            metadata
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec![
                "plugin.json",
                "schemas/message.schema.json",
                "ui/index.html"
            ]
        );
        assert!(metadata
            .files
            .iter()
            .all(|file| file.mode == PackageFileMode::ReadOnly));
        assert_eq!(
            serde_json_canonicalizer::to_vec(metadata).unwrap(),
            prepared.metadata_bytes()
        );
    }

    #[test]
    fn verified_native_entries_alone_are_executable() {
        let manifest = native_manifest();
        let payload = vec![
            observation("plugin.json", &manifest),
            observation("bin/bridge", b"bridge"),
            observation("bin/controller", b"controller"),
            observation("bin/not-declared", b"data"),
        ];
        let prepared = prepare_package_documents(
            &manifest,
            payload,
            options(),
            &FixtureAdapter,
            &FixedOpaqueSignature,
        )
        .unwrap();

        let modes = prepared
            .metadata()
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.mode))
            .collect::<Vec<_>>();
        assert_eq!(
            modes,
            vec![
                ("plugin.json", PackageFileMode::ReadOnly),
                ("bin/bridge", PackageFileMode::Executable),
                ("bin/controller", PackageFileMode::Executable),
                ("bin/not-declared", PackageFileMode::ReadOnly),
            ]
        );
    }

    #[test]
    fn missing_declared_native_entry_is_rejected() {
        let manifest = native_manifest();
        let payload = vec![
            observation("plugin.json", &manifest),
            observation("bin/bridge", b"bridge"),
        ];
        assert_eq!(
            prepare_package_documents(
                &manifest,
                payload,
                options(),
                &FixtureAdapter,
                &FixedOpaqueSignature,
            )
            .unwrap_err()
            .code(),
            "package_metadata"
        );
    }

    #[test]
    fn fixed_opaque_signature_matches_golden() {
        let prepared = prepare_package_documents(
            SOURCE_MANIFEST,
            ui_payload(),
            options(),
            &FixtureAdapter,
            &FixedOpaqueSignature,
        )
        .unwrap();
        assert_eq!(prepared.signature().value, FIXED_OPAQUE_SIGNATURE_VALUE);
        assert_eq!(
            prepared.signature_bytes(),
            serde_json_canonicalizer::to_vec(prepared.signature())
                .unwrap()
                .as_slice()
        );

        let mut expected_message = SIGNATURE_MESSAGE_DOMAIN.to_vec();
        expected_message.push(0);
        expected_message.extend_from_slice(prepared.metadata_bytes());
        assert_eq!(prepared.signature_message(), expected_message);
        assert!(fixed_opaque_observation_matches(
            &expected_message,
            prepared.signature(),
            &expected_message
        ));
    }

    #[test]
    fn fixed_opaque_fixture_rejects_one_bit_message_or_signature_changes() {
        let prepared = prepare_package_documents(
            SOURCE_MANIFEST,
            ui_payload(),
            options(),
            &FixtureAdapter,
            &FixedOpaqueSignature,
        )
        .unwrap();
        let expected_message = prepared.signature_message().to_vec();

        let mut changed_message = expected_message.clone();
        changed_message[0] ^= 1;
        assert!(!fixed_opaque_observation_matches(
            &changed_message,
            prepared.signature(),
            &expected_message
        ));

        let mut changed_signature: PackageSignatureV1 = prepared.signature().clone();
        changed_signature.value.replace_range(0..1, "q");
        assert!(!fixed_opaque_observation_matches(
            &expected_message,
            &changed_signature,
            &expected_message
        ));
    }

    fn native_manifest() -> Vec<u8> {
        br#"{
          "schemaVersion":2,
          "id":"dev.example.native",
          "name":"Native",
          "version":"1.0.0",
          "publisher":"example",
          "compatibility":{"jarvis":">=0.4.0, <0.5.0","pluginApi":2},
          "runtime":{
            "kind":"verified-native",
            "lifecycle":"service-bridge",
            "bridgeEntry":"bin/bridge",
            "service":{
              "id":"controller",
              "manager":"launchd-user",
              "entry":"bin/controller",
              "survivesCoreExit":true
            },
            "protocol":2,
            "activationEvents":[]
          },
          "permissions":[],
          "state":{"schemaVersion":1,"migrations":[],"rollbackCompatibleThrough":1},
          "contributes":{
            "pages":[],"commands":[],"actions":[],"hotkeys":[],"settings":[],
            "projectRuntimes":[],"dataContracts":[]
          }
        }"#
        .to_vec()
    }
}
