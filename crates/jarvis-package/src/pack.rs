use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
#[cfg(target_os = "macos")]
use std::fs::File;
#[cfg(target_os = "macos")]
use std::io::{Cursor, Seek, SeekFrom, Write};
#[cfg(target_os = "macos")]
use std::path::Path;

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
#[cfg(target_os = "macos")]
use crate::source::snapshot_source;

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

    pub(crate) fn archive_write() -> Self {
        Self {
            code: "archive_write",
        }
    }

    pub(crate) fn archive_header() -> Self {
        Self {
            code: "archive_header",
        }
    }

    pub(crate) fn archive_entry_type() -> Self {
        Self {
            code: "archive_entry_type",
        }
    }

    pub(crate) fn archive_path() -> Self {
        Self {
            code: "archive_path",
        }
    }

    pub(crate) fn archive_duplicate() -> Self {
        Self {
            code: "archive_duplicate",
        }
    }

    pub(crate) fn archive_case_collision() -> Self {
        Self {
            code: "archive_case_collision",
        }
    }

    pub(crate) fn archive_order() -> Self {
        Self {
            code: "archive_order",
        }
    }

    pub(crate) fn archive_truncated() -> Self {
        Self {
            code: "archive_truncated",
        }
    }

    pub(crate) fn archive_trailing() -> Self {
        Self {
            code: "archive_trailing",
        }
    }

    pub(crate) fn archive_quota() -> Self {
        Self {
            code: "archive_quota",
        }
    }

    pub(crate) fn package_trust(code: &'static str) -> Self {
        Self { code }
    }

    pub(crate) fn archive_changed_after_verification() -> Self {
        Self {
            code: "archive_changed_after_verification",
        }
    }

    pub(crate) fn extraction_failed() -> Self {
        Self {
            code: "extraction_failed",
        }
    }

    pub(crate) fn quarantine_manual_cleanup() -> Self {
        Self {
            code: "quarantine_manual_cleanup",
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
    source_mode: u32,
}

impl PayloadObservation {
    pub(crate) fn new(
        path: PackagePath,
        size: u64,
        digest: jarvis_plugin_protocol::manifest::Digest,
        source_mode: u32,
    ) -> Self {
        Self {
            path,
            size,
            digest,
            source_mode,
        }
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
    let Some(source_manifest) = by_path.remove("plugin.json") else {
        return Err(PackageError::package_metadata());
    };
    if source_manifest.source_mode & 0o111 != 0 {
        return Err(PackageError::package_metadata());
    }

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
        let declared_executable = executable_paths.contains(observed.path.as_str());
        if observed.source_mode & 0o111 != 0 && !declared_executable {
            return Err(PackageError::package_metadata());
        }
        let mode = if declared_executable {
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

#[cfg(target_os = "macos")]
fn validate_prepared_archive_limits(
    prepared: &PreparedPackageDocuments,
    limits: crate::archive::ArchiveLimits,
) -> Result<u64, PackageError> {
    let payload_files = u64::try_from(prepared.metadata().files.len())
        .map_err(|_| PackageError::archive_quota())?;
    let logical_entries = payload_files
        .checked_add(2)
        .ok_or_else(PackageError::archive_quota)?;
    if payload_files > limits.max_payload_files || logical_entries > limits.max_logical_entries {
        return Err(PackageError::archive_quota());
    }

    let mut raw_records = 2_u64;
    let mut unpacked_payload_bytes = 0_u64;
    let mut physical_bytes = 2 * crate::archive::BLOCK_SIZE as u64;
    for file in &prepared.metadata().files {
        if file.size > limits.max_single_payload_file
            || (file.path.as_str() == "plugin.json" && file.size > limits.max_plugin_json_bytes)
        {
            return Err(PackageError::archive_quota());
        }
        unpacked_payload_bytes = unpacked_payload_bytes
            .checked_add(file.size)
            .ok_or_else(PackageError::archive_quota)?;
        physical_bytes = physical_bytes
            .checked_add(crate::archive::projected_entry_bytes(
                &file.path, file.size,
            )?)
            .ok_or_else(PackageError::archive_quota)?;
        raw_records = raw_records
            .checked_add(if file.path.as_str().len() > 100 { 2 } else { 1 })
            .ok_or_else(PackageError::archive_quota)?;
    }
    if unpacked_payload_bytes > limits.max_unpacked_payload_bytes {
        return Err(PackageError::archive_quota());
    }

    let package_size = u64::try_from(prepared.metadata_bytes().len())
        .map_err(|_| PackageError::archive_quota())?;
    let signature_size = u64::try_from(prepared.signature_bytes().len())
        .map_err(|_| PackageError::archive_quota())?;
    if package_size > limits.max_package_json_bytes || signature_size > limits.max_signature_bytes {
        return Err(PackageError::archive_quota());
    }
    let package_path =
        PackagePath::new("package.json").map_err(|_| PackageError::archive_quota())?;
    let signature_path =
        PackagePath::new("SIGNATURE").map_err(|_| PackageError::archive_quota())?;
    physical_bytes = physical_bytes
        .checked_add(crate::archive::projected_entry_bytes(
            &package_path,
            package_size,
        )?)
        .and_then(|size| {
            crate::archive::projected_entry_bytes(&signature_path, signature_size)
                .ok()
                .and_then(|entry| size.checked_add(entry))
        })
        .ok_or_else(PackageError::archive_quota)?;
    if raw_records > limits.max_raw_records || physical_bytes > limits.max_physical_bytes {
        return Err(PackageError::archive_quota());
    }
    Ok(physical_bytes)
}

#[cfg(target_os = "macos")]
pub fn pack_plugin<A, S, W>(
    source_root: &Path,
    options: PackOptions,
    adapter: &A,
    signature_source: &S,
    mut output: W,
) -> Result<jarvis_plugin_protocol::manifest::Digest, PackageError>
where
    A: PackageDocumentAdapter,
    S: PackageSignatureSource,
    W: Write,
{
    let snapshot = snapshot_source(source_root)?;
    let source_manifest_bytes = snapshot.read_file("plugin.json")?;
    let payload = snapshot
        .files()
        .iter()
        .map(|file| {
            Ok(PayloadObservation::new(
                file.path().clone(),
                file.length(),
                file.digest().clone(),
                file.source_mode(),
            ))
        })
        .collect::<Result<Vec<_>, PackageError>>()?;
    let prepared = prepare_package_documents(
        &source_manifest_bytes,
        payload,
        options,
        adapter,
        signature_source,
    )?;
    let projected_physical_bytes =
        validate_prepared_archive_limits(&prepared, crate::archive::ArchiveLimits::production())?;

    let mut archive_file = owner_only_archive_tempfile()?;
    {
        let mut builder = tar::Builder::new(&mut archive_file);
        for file in &prepared.metadata().files {
            if file.path.as_str() == "plugin.json" {
                crate::archive::append_profile_entry(
                    &mut builder,
                    &file.path,
                    file.mode.as_octal(),
                    file.size,
                    Cursor::new(prepared.manifest_bytes()),
                )?;
            } else {
                let spooled = snapshot
                    .files()
                    .iter()
                    .find(|spooled| spooled.path() == &file.path)
                    .ok_or_else(PackageError::package_metadata)?;
                crate::archive::append_profile_entry(
                    &mut builder,
                    &file.path,
                    file.mode.as_octal(),
                    file.size,
                    snapshot.reader(spooled),
                )?;
            }
        }
        let package_path =
            PackagePath::new("package.json").map_err(|_| PackageError::package_metadata())?;
        crate::archive::append_profile_entry(
            &mut builder,
            &package_path,
            PackageFileMode::ReadOnly.as_octal(),
            u64::try_from(prepared.metadata_bytes().len())
                .map_err(|_| PackageError::package_metadata())?,
            Cursor::new(prepared.metadata_bytes()),
        )?;
        let signature_path =
            PackagePath::new("SIGNATURE").map_err(|_| PackageError::package_metadata())?;
        crate::archive::append_profile_entry(
            &mut builder,
            &signature_path,
            PackageFileMode::ReadOnly.as_octal(),
            u64::try_from(prepared.signature_bytes().len())
                .map_err(|_| PackageError::package_metadata())?,
            Cursor::new(prepared.signature_bytes()),
        )?;
        builder
            .finish()
            .map_err(|_| PackageError::archive_write())?;
    }
    archive_file
        .flush()
        .map_err(|_| PackageError::archive_write())?;
    archive_file
        .seek(SeekFrom::Start(0))
        .map_err(|_| PackageError::archive_write())?;
    let inspection = crate::archive::inspect_reader_with_limits(
        &mut archive_file,
        crate::archive::ArchiveLimits::production(),
    )?;
    validate_packed_archive(
        &archive_file,
        &inspection,
        &prepared,
        projected_physical_bytes,
    )?;
    archive_file
        .seek(SeekFrom::Start(0))
        .map_err(|_| PackageError::archive_write())?;
    std::io::copy(&mut archive_file, &mut output).map_err(|_| PackageError::archive_write())?;
    output.flush().map_err(|_| PackageError::archive_write())?;
    Ok(inspection.physical_digest().clone())
}

#[cfg(target_os = "macos")]
fn validate_packed_archive(
    archive_file: &File,
    inspection: &crate::archive::ArchiveInspection,
    prepared: &PreparedPackageDocuments,
    projected_physical_bytes: u64,
) -> Result<(), PackageError> {
    let stat = rustix::fs::fstat(archive_file).map_err(|_| PackageError::archive_write())?;
    if stat.st_size < 0
        || u64::try_from(stat.st_size).map_err(|_| PackageError::archive_write())?
            != inspection.physical_bytes()
        || inspection.physical_bytes() != projected_physical_bytes
        || inspection.plugin_json() != prepared.manifest_bytes()
        || inspection.package_json() != prepared.metadata_bytes()
        || inspection.signature() != prepared.signature_bytes()
        || inspection.payload_entries().len() != prepared.metadata().files.len()
        || !inspection
            .entries()
            .windows(2)
            .all(|entries| entries[0].body_offset() < entries[1].body_offset())
    {
        return Err(PackageError::package_metadata());
    }
    for (observed, expected) in inspection
        .payload_entries()
        .iter()
        .zip(&prepared.metadata().files)
    {
        if observed.path() != &expected.path
            || observed.mode() != expected.mode
            || observed.size() != expected.size
            || observed.digest() != &expected.digest
        {
            return Err(PackageError::package_metadata());
        }
    }
    let generated = &inspection.entries()[inspection.payload_entries().len()..];
    if generated.len() != 2
        || generated[0].path().as_str() != "package.json"
        || generated[0].mode() != PackageFileMode::ReadOnly
        || generated[0].digest() != &sha256_digest(prepared.metadata_bytes())
        || generated[1].path().as_str() != "SIGNATURE"
        || generated[1].mode() != PackageFileMode::ReadOnly
        || generated[1].digest() != &sha256_digest(prepared.signature_bytes())
    {
        return Err(PackageError::package_metadata());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn owner_only_archive_tempfile() -> Result<File, PackageError> {
    let file = tempfile::tempfile().map_err(|_| PackageError::archive_write())?;
    rustix::fs::fchmod(&file, rustix::fs::Mode::from_raw_mode(0o600))
        .map_err(|_| PackageError::archive_write())?;
    let stat = rustix::fs::fstat(&file).map_err(|_| PackageError::archive_write())?;
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile
        || stat.st_nlink != 0
        || stat.st_mode & 0o777 != 0o600
    {
        return Err(PackageError::archive_write());
    }
    Ok(file)
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
    use std::fs;

    use jarvis_plugin_protocol::manifest::{ManifestV2, RuntimeKind};
    use jarvis_plugin_protocol::package::{
        MacOsVersion, PackageFileMode, PackagePath, PackageSignatureV1, PackageTarget,
    };

    use super::{
        fixed_opaque_observation_matches, prepare_package_documents, FixedOpaqueSignature,
        PackOptions, PackageDocumentAdapter, PackageError, PayloadObservation,
        FIXED_OPAQUE_SIGNATURE_VALUE, SIGNATURE_MESSAGE_DOMAIN,
    };
    use crate::archive::{encode_checksum_for_test, encode_number_for_test, entry_bytes_for_test};
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
            0o644,
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

    #[cfg(target_os = "macos")]
    #[test]
    fn prepared_archive_projection_accepts_exact_physical_limit_only() {
        let prepared = prepare_package_documents(
            SOURCE_MANIFEST,
            ui_payload(),
            options(),
            &FixtureAdapter,
            &FixedOpaqueSignature,
        )
        .unwrap();
        let production = crate::archive::ArchiveLimits::production();
        let projected = super::validate_prepared_archive_limits(&prepared, production).unwrap();
        let archive = pack_fixture_archive().unwrap();
        assert_eq!(u64::try_from(archive.len()).unwrap(), projected);
        let mut exact = production;
        exact.max_physical_bytes = projected;
        assert_eq!(
            super::validate_prepared_archive_limits(&prepared, exact).unwrap(),
            projected
        );
        exact.max_physical_bytes = projected - 1;
        assert_eq!(
            super::validate_prepared_archive_limits(&prepared, exact)
                .unwrap_err()
                .code(),
            "archive_quota"
        );
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

    fn long_path(component_before_unicode: usize) -> String {
        [
            "a".repeat(255),
            "b".repeat(255),
            "c".repeat(255),
            "d".repeat(component_before_unicode),
            "é".to_owned(),
        ]
        .join("/")
    }

    #[cfg(target_os = "macos")]
    fn pack_fixture_archive() -> Result<Vec<u8>, PackageError> {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/plugin-packages/pack-source");
        let mut archive = Vec::new();
        super::pack_plugin(
            &source,
            options(),
            &FixtureAdapter,
            &FixedOpaqueSignature,
            &mut archive,
        )?;
        Ok(archive)
    }

    #[test]
    fn executable_source_not_declared_by_manifest_is_rejected() {
        let mut payload = ui_payload();
        payload[1].source_mode = 0o755;
        assert_eq!(
            prepare_package_documents(
                SOURCE_MANIFEST,
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
    fn gnu_header_profiles_are_byte_exact() {
        let tar_header = tar::Header::new_gnu();
        assert_eq!(&tar_header.as_bytes()[257..263], b"ustar ");
        assert_eq!(&tar_header.as_bytes()[263..265], b" \0");

        let short_path = "a".repeat(100);
        let short = entry_bytes_for_test(&short_path, 0o444, b"x").unwrap();
        assert_eq!(&short[0..100], short_path.as_bytes());
        assert_eq!(&short[100..108], b"0000444\0");
        assert_eq!(&short[108..116], b"0000000\0");
        assert_eq!(&short[116..124], b"0000000\0");
        assert_eq!(&short[124..136], b"00000000001\0");
        assert_eq!(&short[136..148], b"00000000000\0");
        assert_eq!(short[156], b'0');
        assert_eq!(&short[257..263], b"ustar ");
        assert_eq!(&short[263..265], b" \0");
        assert_eq!(&short[329..337], b"0000000\0");
        assert_eq!(&short[337..345], b"0000000\0");
        assert!(short[157..257].iter().all(|byte| *byte == 0));
        assert!(short[265..329].iter().all(|byte| *byte == 0));
        assert!(short[345..512].iter().all(|byte| *byte == 0));
        assert_eq!(short[154], 0);
        assert_eq!(short[155], b' ');
        let stored_checksum =
            u64::from_str_radix(std::str::from_utf8(&short[148..154]).unwrap(), 8).unwrap();
        let mut checksum_header = short[..512].to_vec();
        checksum_header[148..156].fill(b' ');
        let recomputed_checksum = checksum_header
            .iter()
            .map(|byte| u64::from(*byte))
            .sum::<u64>();
        assert_eq!(stored_checksum, recomputed_checksum);
        assert_eq!(short.len(), 512 + 512 + 1024);
        assert!(short[513..1024].iter().all(|byte| *byte == 0));
        assert!(short[1024..].iter().all(|byte| *byte == 0));

        let path_101 = "b".repeat(101);
        let long = entry_bytes_for_test(&path_101, 0o555, b"xy").unwrap();
        assert_eq!(&long[0..13], b"././@LongLink");
        assert_eq!(&long[100..108], b"0000644\0");
        assert_eq!(&long[124..136], b"00000000146\0");
        assert_eq!(long[156], b'L');
        assert_eq!(&long[512..613], path_101.as_bytes());
        assert_eq!(long[613], 0);
        assert_eq!(&long[1024..1037], b"././@LongFile");
        assert_eq!(&long[1124..1132], b"0000555\0");
        assert_eq!(long[1180], b'0');

        let path_1024 = long_path(253);
        assert_eq!(path_1024.len(), 1_024);
        let longest = entry_bytes_for_test(&path_1024, 0o444, b"").unwrap();
        assert_eq!(longest[156], b'L');
        assert_eq!(&longest[124..136], b"00000002001\0");
        assert_eq!(&longest[512..1536], path_1024.as_bytes());
        assert_eq!(longest[1536], 0);
        assert_eq!(&longest[2048..2061], b"././@LongFile");
        assert_eq!(longest[2204], b'0');

        assert!(entry_bytes_for_test(&long_path(254), 0o444, b"").is_err());
        assert!(entry_bytes_for_test(&"z".repeat(256), 0o444, b"").is_err());

        assert_eq!(encode_number_for_test(8, 0).unwrap(), b"0000000\0".to_vec());
        assert_eq!(
            encode_number_for_test(12, 0).unwrap(),
            b"00000000000\0".to_vec()
        );
        assert_eq!(encode_checksum_for_test(0).unwrap(), b"000000\0 ".to_vec());
        assert!(encode_number_for_test(8, 0o10_000_000).is_err());
        assert!(encode_number_for_test(12, 0o1_000_000_000_000).is_err());
        assert!(encode_checksum_for_test(0o1_000_000).is_err());
    }

    #[test]
    fn identical_input_matches_committed_archive_golden() {
        let actual = pack_fixture_archive().unwrap();
        let fixture_root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugin-packages");
        let expected = fs::read(fixture_root.join("golden/darwin-arm64.jarvis-plugin")).unwrap();
        let expected_digest =
            fs::read_to_string(fixture_root.join("golden/darwin-arm64.sha256")).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(
            crate::hash::sha256_digest(&actual).as_str(),
            expected_digest.trim()
        );
    }

    #[test]
    #[ignore = "rewrites the committed deterministic archive golden"]
    fn regenerate_package_golden() {
        let archive = pack_fixture_archive().unwrap();
        let fixture_root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugin-packages");
        let golden = fixture_root.join("golden");
        fs::create_dir_all(&golden).unwrap();
        fs::write(golden.join("darwin-arm64.jarvis-plugin"), &archive).unwrap();
        fs::write(
            golden.join("darwin-arm64.sha256"),
            format!("{}\n", crate::hash::sha256_digest(&archive).as_str()),
        )
        .unwrap();
    }
}
