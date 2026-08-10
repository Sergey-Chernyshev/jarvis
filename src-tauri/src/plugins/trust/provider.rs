use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use jarvis_package::{PackageTrustError, UntrustedPackageObservation};
use jarvis_plugin_protocol::manifest::{Digest, ManifestV2, PluginId};
use jarvis_plugin_protocol::package::{
    PackageFile, PackageMetadataV1, PackageSignatureV1, SignatureAlgorithm, PACKAGE_SCHEMA_VERSION,
};
use jarvis_plugin_protocol::receipt::{InstallReceipt, InstallSource};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::catalog::{
    verify_catalog_bytes, CatalogCompatibility, CatalogState, RootTrustConfig, VerifiedCatalog,
    VerifiedCatalogRelease,
};
use super::package::{
    recording_catalog_package_verifier, CatalogEvidenceRecorder, CatalogPackageVerifier,
};
use crate::plugins::package_manager::manager::{
    CatalogItem, CatalogProvider, CatalogQuery, Clock, InstallSourceRef, ManagerError,
    ManagerResult, PluginDetails, SelectedRelease,
};
use crate::plugins::package_manager::paths::PluginPaths;
use crate::plugins::package_manager::random_storage_id;
use crate::plugins::resolver::{
    verify_catalog_receipt, CurrentReceiptTrust, ReceiptTrustError, ReceiptTrustFacts,
};

const MAX_CATALOG_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CATALOG_STATE_BYTES: u64 = 128 * 1024;
const MAX_EVIDENCE_BYTES: u64 = 20 * 1024 * 1024;
const CATALOG_STATE_SCHEMA: u32 = 1;
const CATALOG_STATE_FILE: &str = "catalog-state.json";
const CATALOG_STATE_LOCK: &str = ".catalog-state.lock";
const RECEIPT_EVIDENCE_SCHEMA: u32 = 1;
const DEVELOPER_KEY_ID: &str = "jarvis.developer-unverified";
const DEVELOPER_LINEAGE_PREFIX: &str = "developer:";
const DEVELOPER_SIGNATURE_BYTES: [u8; 64] = [0; 64];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredCatalogState {
    schema_version: u32,
    sequence: u64,
    digest: Digest,
    accepted_roots: RootTrustConfig,
}

struct CatalogStateLock(File);

impl Drop for CatalogStateLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

pub struct ProductionCatalogProvider {
    paths: PluginPaths,
    catalog_path: PathBuf,
    compatibility: CatalogCompatibility,
    clock: Arc<dyn Clock>,
    state: Mutex<Option<CatalogState>>,
    evidence_lock: Arc<Mutex<()>>,
}

impl ProductionCatalogProvider {
    pub fn for_profile(
        paths: PluginPaths,
        compatibility: CatalogCompatibility,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let catalog_path = paths.plugins_root().join("catalog.json");
        Self::new(
            paths,
            catalog_path,
            include_bytes!("../../../resources/plugin-trust-roots.json"),
            compatibility,
            clock,
        )
    }

    pub fn new(
        paths: PluginPaths,
        catalog_path: PathBuf,
        roots: &[u8],
        compatibility: CatalogCompatibility,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let state = RootTrustConfig::parse(roots)
            .ok()
            .filter(RootTrustConfig::is_provisioned)
            .map(CatalogState::new);
        Self {
            paths,
            catalog_path,
            compatibility,
            clock,
            state: Mutex::new(state),
            evidence_lock: Arc::new(Mutex::new(())),
        }
    }

    fn snapshot(&self) -> ManagerResult<(VerifiedCatalog, DateTime<Utc>)> {
        if self.catalog_path != self.paths.plugins_root().join("catalog.json") {
            return Err(ManagerError::new(
                "catalog_path_unsafe",
                "trusted catalog path is outside the fixed profile location",
            ));
        }
        let bytes = read_catalog_file(&self.paths, &self.catalog_path)?;
        let now = DateTime::<Utc>::from_timestamp_millis(self.clock.now_ms()).ok_or_else(|| {
            ManagerError::new(
                "catalog_time",
                "catalog clock is outside the supported range",
            )
        })?;
        let mut state = self.state.lock().map_err(|_| {
            ManagerError::new("catalog_state_unavailable", "catalog state poisoned")
        })?;
        let state = state.as_mut().ok_or_else(|| {
            ManagerError::new(
                "catalog_unconfigured",
                "no trusted plugin catalog root is configured",
            )
        })?;
        let _state_lock = acquire_catalog_state_lock(&self.paths)?;
        synchronize_catalog_state(&self.paths, state)?;
        let previous_state = state.clone();
        let catalog = verify_catalog_bytes(&bytes, now, &self.compatibility, state)
            .map_err(|error| ManagerError::new(error.code(), error.to_string()))?;
        if *state != previous_state {
            if let Err(error) = write_catalog_state(&self.paths, state) {
                *state = previous_state;
                return Err(error);
            }
        }
        Ok((catalog, now))
    }

    fn available_releases(
        &self,
    ) -> ManagerResult<(VerifiedCatalog, DateTime<Utc>, Vec<VerifiedCatalogRelease>)> {
        let (catalog, now) = self.snapshot()?;
        let mut releases = catalog
            .releases()
            .iter()
            .filter_map(|candidate| {
                let record = candidate.release_record();
                catalog
                    .release(
                        record.plugin_id.as_str(),
                        &record.version.to_string(),
                        record.target,
                    )
                    .ok()
                    .cloned()
            })
            .collect::<Vec<_>>();
        releases.sort_by(|left, right| {
            let left = left.release_record();
            let right = right.release_record();
            left.plugin_id
                .as_str()
                .cmp(right.plugin_id.as_str())
                .then_with(|| right.version.cmp(&left.version))
        });
        Ok((catalog, now, releases))
    }

    fn select_release(
        &self,
        id: &str,
        version: Option<&str>,
    ) -> ManagerResult<(VerifiedCatalogRelease, DateTime<Utc>, u64)> {
        let plugin_id = PluginId::new(id.to_owned())
            .map_err(|error| ManagerError::new(error.code(), error.to_string()))?;
        let (catalog, now) = self.snapshot()?;
        let target = self.compatibility.target();
        let release = if let Some(version) = version {
            Version::parse(version)
                .map_err(|_| ManagerError::new("package_version", "invalid package version"))?;
            catalog
                .release(plugin_id.as_str(), version, target)
                .map(Clone::clone)
                .map_err(|error| ManagerError::new(error.code(), error.to_string()))?
        } else {
            let mut releases = catalog
                .releases()
                .iter()
                .filter(|candidate| {
                    let record = candidate.release_record();
                    record.plugin_id == plugin_id && record.target == target
                })
                .filter_map(|candidate| {
                    let record = candidate.release_record();
                    catalog
                        .release(plugin_id.as_str(), &record.version.to_string(), target)
                        .ok()
                        .cloned()
                })
                .collect::<Vec<_>>();
            releases.sort_by(|left, right| {
                right
                    .release_record()
                    .version
                    .cmp(&left.release_record().version)
            });
            releases.into_iter().next().ok_or_else(|| {
                ManagerError::new("package_not_found", "no compatible catalog release found")
            })?
        };
        Ok((release, now, catalog.sequence()))
    }

    fn selected(
        &self,
        release: VerifiedCatalogRelease,
        _now: DateTime<Utc>,
        catalog_sequence: u64,
    ) -> SelectedRelease {
        let record = release.release_record();
        let plugin_id = record.plugin_id.clone();
        let version = record.version.clone();
        let target = record.target;
        let url = record.url.clone();
        let archive_digest = record.archive_digest.clone();
        let publisher_key_id = record.publisher_key_id.clone();
        let publisher_lineage = record.publisher_lineage.clone();
        let clock = self.clock.clone();
        let verifier = recording_catalog_package_verifier(
            release,
            Arc::new(move || {
                DateTime::<Utc>::from_timestamp_millis(clock.now_ms())
                    .ok_or_else(|| PackageTrustError::new("catalog_time"))
            }),
            Arc::new(ReceiptEvidenceRecorder {
                paths: self.paths.clone(),
                evidence_lock: self.evidence_lock.clone(),
                publisher_lineage: publisher_lineage.clone(),
            }),
        );
        SelectedRelease::new(
            catalog_sequence,
            plugin_id,
            version,
            target,
            url,
            archive_digest,
            publisher_key_id,
            publisher_lineage,
            verifier,
        )
    }
}

impl CatalogProvider for ProductionCatalogProvider {
    fn catalog(&self, query: &CatalogQuery) -> ManagerResult<Vec<CatalogItem>> {
        let (_catalog, _now, releases) = self.available_releases()?;
        let text = query.text.as_deref().map(str::to_ascii_lowercase);
        let mut latest = BTreeMap::<String, CatalogItem>::new();
        for release in releases {
            let record = release.release_record();
            if query
                .plugin_id
                .as_deref()
                .is_some_and(|id| id != record.plugin_id.as_str())
                || text.as_ref().is_some_and(|query| {
                    !record
                        .plugin_id
                        .as_str()
                        .to_ascii_lowercase()
                        .contains(query)
                })
            {
                continue;
            }
            latest
                .entry(record.plugin_id.as_str().to_owned())
                .or_insert_with(|| CatalogItem {
                    plugin_id: record.plugin_id.clone(),
                    name: record.plugin_id.as_str().to_owned(),
                    version: record.version.clone(),
                    target: record.target,
                    archive_digest: record.archive_digest.clone(),
                });
        }
        Ok(latest.into_values().collect())
    }

    fn info(&self, id: &PluginId) -> ManagerResult<PluginDetails> {
        let (release, _now, _sequence) = self.select_release(id.as_str(), None)?;
        let record = release.release_record();
        Ok(PluginDetails {
            item: CatalogItem {
                plugin_id: record.plugin_id.clone(),
                name: record.plugin_id.as_str().to_owned(),
                version: record.version.clone(),
                target: record.target,
                archive_digest: record.archive_digest.clone(),
            },
            publisher_key_id: record.publisher_key_id.clone(),
            publisher_lineage: record.publisher_lineage.clone(),
            installed: None,
        })
    }

    fn select(&self, source: &InstallSourceRef) -> ManagerResult<SelectedRelease> {
        let InstallSourceRef::Catalog { id, version } = source;
        let (release, now, sequence) = self.select_release(id, version.as_deref())?;
        Ok(self.selected(release, now, sequence))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredReceiptEvidence {
    schema_version: u32,
    archive_digest: Digest,
    package_json: String,
    plugin_json: String,
    package_signature: PackageSignatureV1,
    publisher_lineage: String,
}

pub(crate) fn record_developer_snapshot_evidence(
    paths: &PluginPaths,
    receipt: &InstallReceipt,
    snapshot: &Path,
    metadata: &PackageMetadataV1,
) -> Result<(), ReceiptTrustError> {
    if receipt.source != InstallSource::DeveloperSnapshot
        || receipt.publisher_key_id != DEVELOPER_KEY_ID
        || receipt.publisher_lineage
            != format!("{DEVELOPER_LINEAGE_PREFIX}{}", metadata.publisher.as_str())
        || receipt
            .package_digest
            .as_str()
            .strip_prefix("sha256:")
            .is_none()
        || metadata.plugin_id != receipt.plugin_id
        || metadata.version != receipt.version
        || metadata.target != receipt.target
    {
        return Err(ReceiptTrustError::new("developer_receipt_evidence_invalid"));
    }
    let expected_manifest = metadata
        .files
        .iter()
        .find(|file| file.path.as_str() == "plugin.json")
        .ok_or_else(|| ReceiptTrustError::new("developer_receipt_evidence_invalid"))?;
    let plugin_json = read_snapshot_file(snapshot, expected_manifest)?;
    let manifest = ManifestV2::parse(&plugin_json)
        .map_err(|_| ReceiptTrustError::new("developer_receipt_evidence_invalid"))?;
    if manifest.id != receipt.plugin_id
        || manifest.version != receipt.version
        || manifest.publisher != metadata.publisher
        || digest_bytes(&plugin_json)? != metadata.manifest_digest
    {
        return Err(ReceiptTrustError::new("developer_receipt_evidence_invalid"));
    }
    let package_json = serde_json_canonicalizer::to_vec(metadata)
        .map_err(|_| ReceiptTrustError::new("developer_receipt_evidence_invalid"))?;
    let evidence = StoredReceiptEvidence {
        schema_version: RECEIPT_EVIDENCE_SCHEMA,
        archive_digest: receipt.package_digest.clone(),
        package_json: String::from_utf8(package_json)
            .map_err(|_| ReceiptTrustError::new("developer_receipt_evidence_invalid"))?,
        plugin_json: String::from_utf8(plugin_json)
            .map_err(|_| ReceiptTrustError::new("developer_receipt_evidence_invalid"))?,
        package_signature: PackageSignatureV1::new(
            SignatureAlgorithm::Ed25519,
            DEVELOPER_KEY_ID,
            STANDARD.encode(DEVELOPER_SIGNATURE_BYTES),
        )
        .map_err(|_| ReceiptTrustError::new("developer_receipt_evidence_invalid"))?,
        publisher_lineage: receipt.publisher_lineage.clone(),
    };
    write_evidence(paths, &evidence)
}

struct ReceiptEvidenceRecorder {
    paths: PluginPaths,
    evidence_lock: Arc<Mutex<()>>,
    publisher_lineage: String,
}

impl CatalogEvidenceRecorder for ReceiptEvidenceRecorder {
    fn record(
        &self,
        observation: &UntrustedPackageObservation<'_>,
    ) -> Result<(), PackageTrustError> {
        let package_json = std::str::from_utf8(observation.package_json())
            .map_err(|_| PackageTrustError::new("receipt_trust_evidence_invalid"))?
            .to_owned();
        let plugin_json = std::str::from_utf8(observation.plugin_json())
            .map_err(|_| PackageTrustError::new("receipt_trust_evidence_invalid"))?
            .to_owned();
        let evidence = StoredReceiptEvidence {
            schema_version: RECEIPT_EVIDENCE_SCHEMA,
            archive_digest: observation.archive_digest().clone(),
            package_json,
            plugin_json,
            package_signature: observation.signature().clone(),
            publisher_lineage: self.publisher_lineage.clone(),
        };
        let _guard = self
            .evidence_lock
            .lock()
            .map_err(|_| PackageTrustError::new("receipt_trust_evidence_store"))?;
        write_evidence(&self.paths, &evidence)
            .map_err(|_| PackageTrustError::new("receipt_trust_evidence_store"))
    }
}

impl CurrentReceiptTrust for ProductionCatalogProvider {
    fn verify_current(
        &self,
        receipt: &InstallReceipt,
    ) -> Result<ReceiptTrustFacts, ReceiptTrustError> {
        match receipt.source {
            InstallSource::DeveloperSnapshot => {
                return verify_developer_evidence(&self.paths, receipt);
            }
            InstallSource::Catalog => {}
            InstallSource::LocalPackage | InstallSource::LegacyBundledV1 => {
                return Err(ReceiptTrustError::new("receipt_trust_source_unsupported"));
            }
        }
        let (catalog, now) = self
            .snapshot()
            .map_err(|error| receipt_error(error.code()))?;
        let release = catalog
            .release(
                receipt.plugin_id.as_str(),
                &receipt.version.to_string(),
                receipt.target,
            )
            .map_err(|error| ReceiptTrustError::new(error.code()))?
            .clone();
        let evidence = read_evidence(&self.paths, &receipt.package_digest)?;
        if evidence.schema_version != RECEIPT_EVIDENCE_SCHEMA
            || evidence.archive_digest != receipt.package_digest
            || evidence.publisher_lineage != receipt.publisher_lineage
            || evidence.package_signature.key_id != receipt.publisher_key_id
            || evidence.package_signature.validate().is_err()
            || evidence.package_json.len() as u64 > MAX_EVIDENCE_BYTES
            || evidence.plugin_json.len() as u64 > MAX_EVIDENCE_BYTES
        {
            return Err(ReceiptTrustError::new("receipt_trust_evidence_invalid"));
        }
        let metadata: PackageMetadataV1 = serde_json::from_slice(evidence.package_json.as_bytes())
            .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_invalid"))?;
        if metadata.schema_version != PACKAGE_SCHEMA_VERSION
            || serde_json_canonicalizer::to_vec(&metadata)
                .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_invalid"))?
                != evidence.package_json.as_bytes()
        {
            return Err(ReceiptTrustError::new("receipt_trust_evidence_invalid"));
        }
        let manifest = ManifestV2::parse(evidence.plugin_json.as_bytes())
            .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_invalid"))?;
        if serde_json_canonicalizer::to_vec(&manifest)
            .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_invalid"))?
            != evidence.plugin_json.as_bytes()
            || digest_bytes(evidence.plugin_json.as_bytes())? != metadata.manifest_digest
        {
            return Err(ReceiptTrustError::new("receipt_trust_evidence_invalid"));
        }
        CatalogPackageVerifier::new(release, now)
            .verify_persisted(
                &evidence.archive_digest,
                evidence.package_json.as_bytes(),
                &metadata,
                &evidence.package_signature,
            )
            .map_err(|error| ReceiptTrustError::new(error.code()))?;
        let facts = ReceiptTrustFacts::from_catalog_evidence(
            evidence.archive_digest,
            metadata,
            manifest,
            evidence.package_signature.key_id,
            evidence.publisher_lineage,
        );
        verify_catalog_receipt(&catalog, receipt, &facts)?;
        Ok(facts)
    }
}

fn verify_developer_evidence(
    paths: &PluginPaths,
    receipt: &InstallReceipt,
) -> Result<ReceiptTrustFacts, ReceiptTrustError> {
    let evidence = read_evidence(paths, &receipt.package_digest)?;
    let expected_signature = PackageSignatureV1::new(
        SignatureAlgorithm::Ed25519,
        DEVELOPER_KEY_ID,
        STANDARD.encode(DEVELOPER_SIGNATURE_BYTES),
    )
    .map_err(|_| ReceiptTrustError::new("developer_receipt_evidence_invalid"))?;
    if evidence.schema_version != RECEIPT_EVIDENCE_SCHEMA
        || evidence.archive_digest != receipt.package_digest
        || evidence.package_signature != expected_signature
        || receipt.publisher_key_id != DEVELOPER_KEY_ID
        || evidence.publisher_lineage != receipt.publisher_lineage
        || evidence.package_json.len() as u64 > MAX_EVIDENCE_BYTES
        || evidence.plugin_json.len() as u64 > MAX_EVIDENCE_BYTES
    {
        return Err(ReceiptTrustError::new("developer_receipt_evidence_invalid"));
    }
    let metadata: PackageMetadataV1 = serde_json::from_slice(evidence.package_json.as_bytes())
        .map_err(|_| ReceiptTrustError::new("developer_receipt_evidence_invalid"))?;
    if metadata.schema_version != PACKAGE_SCHEMA_VERSION
        || serde_json_canonicalizer::to_vec(&metadata)
            .map_err(|_| ReceiptTrustError::new("developer_receipt_evidence_invalid"))?
            != evidence.package_json.as_bytes()
    {
        return Err(ReceiptTrustError::new("developer_receipt_evidence_invalid"));
    }
    let manifest = ManifestV2::parse(evidence.plugin_json.as_bytes())
        .map_err(|_| ReceiptTrustError::new("developer_receipt_evidence_invalid"))?;
    if serde_json_canonicalizer::to_vec(&manifest)
        .map_err(|_| ReceiptTrustError::new("developer_receipt_evidence_invalid"))?
        != evidence.plugin_json.as_bytes()
        || digest_bytes(evidence.plugin_json.as_bytes())? != metadata.manifest_digest
        || receipt.publisher_lineage
            != format!("{DEVELOPER_LINEAGE_PREFIX}{}", metadata.publisher.as_str())
    {
        return Err(ReceiptTrustError::new("developer_receipt_evidence_invalid"));
    }
    Ok(ReceiptTrustFacts::from_catalog_evidence(
        evidence.archive_digest,
        metadata,
        manifest,
        evidence.package_signature.key_id,
        evidence.publisher_lineage,
    ))
}

fn catalog_state_path(paths: &PluginPaths) -> PathBuf {
    paths.plugins_root().join(CATALOG_STATE_FILE)
}

fn sync_private_directory(path: &Path) -> std::io::Result<()> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?
        .sync_all()
}

fn validate_catalog_state_root(paths: &PluginPaths) -> ManagerResult<()> {
    let root = fs::symlink_metadata(paths.plugins_root()).map_err(|error| {
        ManagerError::new(
            "catalog_state_unsafe",
            format!("state root unavailable: {error}"),
        )
    })?;
    if !root.file_type().is_dir()
        || root.file_type().is_symlink()
        || root.uid() != unsafe { libc::geteuid() }
        || root.permissions().mode() & 0o777 != 0o700
    {
        return Err(ManagerError::new(
            "catalog_state_unsafe",
            "catalog state root must be an owned 0700 directory",
        ));
    }
    Ok(())
}

fn acquire_catalog_state_lock(paths: &PluginPaths) -> ManagerResult<CatalogStateLock> {
    validate_catalog_state_root(paths)?;
    let path = paths.plugins_root().join(CATALOG_STATE_LOCK);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|error| {
            ManagerError::new(
                "catalog_state_unsafe",
                format!("catalog state lock cannot be opened: {error}"),
            )
        })?;
    let metadata = file.metadata().map_err(|error| {
        ManagerError::new(
            "catalog_state_unsafe",
            format!("catalog state lock cannot be inspected: {error}"),
        )
    })?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(ManagerError::new(
            "catalog_state_unsafe",
            "catalog state lock must be an owned 0600 single-link regular file",
        ));
    }
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(ManagerError::new(
                "catalog_state_unavailable",
                format!("catalog state lock cannot be acquired: {error}"),
            ));
        }
    }
    let entry = fs::symlink_metadata(&path).map_err(|error| {
        ManagerError::new(
            "catalog_state_unsafe",
            format!("catalog state lock path cannot be inspected: {error}"),
        )
    })?;
    if entry.file_type().is_symlink()
        || entry.dev() != metadata.dev()
        || entry.ino() != metadata.ino()
        || entry.nlink() != 1
        || entry.uid() != unsafe { libc::geteuid() }
        || entry.permissions().mode() & 0o777 != 0o600
    {
        return Err(ManagerError::new(
            "catalog_state_unsafe",
            "catalog state lock path changed while acquiring the lock",
        ));
    }
    Ok(CatalogStateLock(file))
}

fn read_catalog_state(paths: &PluginPaths) -> ManagerResult<Option<StoredCatalogState>> {
    validate_catalog_state_root(paths)?;
    let path = catalog_state_path(paths);
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ManagerError::new(
                "catalog_state_unsafe",
                format!("catalog state cannot be opened: {error}"),
            ))
        }
    };
    let before = file.metadata().map_err(|error| {
        ManagerError::new(
            "catalog_state_unsafe",
            format!("catalog state cannot be inspected: {error}"),
        )
    })?;
    let entry = fs::symlink_metadata(&path).map_err(|error| {
        ManagerError::new(
            "catalog_state_unsafe",
            format!("catalog state path cannot be inspected: {error}"),
        )
    })?;
    if !before.is_file()
        || entry.file_type().is_symlink()
        || before.dev() != entry.dev()
        || before.ino() != entry.ino()
        || before.uid() != unsafe { libc::geteuid() }
        || before.nlink() != 1
        || before.permissions().mode() & 0o777 != 0o600
        || before.len() > MAX_CATALOG_STATE_BYTES
    {
        return Err(ManagerError::new(
            "catalog_state_unsafe",
            "catalog state must be an owned 0600 single-link regular file",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_CATALOG_STATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            ManagerError::new(
                "catalog_state_unsafe",
                format!("catalog state cannot be read: {error}"),
            )
        })?;
    let after = file.metadata().map_err(|error| {
        ManagerError::new(
            "catalog_state_unsafe",
            format!("catalog state cannot be re-inspected: {error}"),
        )
    })?;
    let final_entry = fs::symlink_metadata(&path).map_err(|error| {
        ManagerError::new(
            "catalog_state_changed",
            format!("catalog state path cannot be re-inspected: {error}"),
        )
    })?;
    if bytes.len() as u64 > MAX_CATALOG_STATE_BYTES
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || after.nlink() != 1
        || after.uid() != unsafe { libc::geteuid() }
        || after.permissions().mode() & 0o777 != 0o600
        || final_entry.file_type().is_symlink()
        || final_entry.dev() != after.dev()
        || final_entry.ino() != after.ino()
    {
        return Err(ManagerError::new(
            "catalog_state_changed",
            "catalog state changed while it was being read",
        ));
    }
    let stored: StoredCatalogState = serde_json::from_slice(&bytes)
        .map_err(|_| ManagerError::new("catalog_state_invalid", "catalog state JSON is invalid"))?;
    let canonical = serde_json_canonicalizer::to_vec(&stored).map_err(|_| {
        ManagerError::new(
            "catalog_state_invalid",
            "catalog state cannot be canonicalized",
        )
    })?;
    if stored.schema_version != CATALOG_STATE_SCHEMA || canonical != bytes {
        return Err(ManagerError::new(
            "catalog_state_invalid",
            "catalog state is not the canonical supported schema",
        ));
    }
    Ok(Some(stored))
}

fn catalog_state_from_stored(stored: StoredCatalogState) -> ManagerResult<CatalogState> {
    CatalogState::from_checkpoint(stored.sequence, stored.digest, stored.accepted_roots)
        .map_err(|error| ManagerError::new(error.code(), error.to_string()))
}

fn synchronize_catalog_state(paths: &PluginPaths, state: &mut CatalogState) -> ManagerResult<()> {
    let Some(stored) = read_catalog_state(paths)? else {
        return if state.sequence() == 0 {
            Ok(())
        } else {
            Err(ManagerError::new(
                "catalog_state_replayed",
                "durable catalog state disappeared",
            ))
        };
    };
    let durable = catalog_state_from_stored(stored)?;
    if durable.sequence() < state.sequence() {
        return Err(ManagerError::new(
            "catalog_state_replayed",
            "durable catalog state moved backwards",
        ));
    }
    if durable.sequence() == state.sequence() && durable != *state {
        return Err(ManagerError::new(
            "catalog_state_conflict",
            "durable catalog state conflicts at the same sequence",
        ));
    }
    if durable.sequence() > state.sequence() {
        *state = durable;
    }
    Ok(())
}

fn write_catalog_state(paths: &PluginPaths, state: &CatalogState) -> ManagerResult<()> {
    let digest = state.digest().cloned().ok_or_else(|| {
        ManagerError::new(
            "catalog_state_invalid",
            "accepted catalog state has no digest",
        )
    })?;
    let stored = StoredCatalogState {
        schema_version: CATALOG_STATE_SCHEMA,
        sequence: state.sequence(),
        digest,
        accepted_roots: state.accepted_roots().clone(),
    };
    if let Some(existing) = read_catalog_state(paths)? {
        let existing_state = catalog_state_from_stored(existing)?;
        if existing_state.sequence() > state.sequence() {
            return Err(ManagerError::new(
                "catalog_state_replayed",
                "refusing to overwrite a newer durable catalog state",
            ));
        }
        if existing_state.sequence() == state.sequence() {
            if existing_state != *state {
                return Err(ManagerError::new(
                    "catalog_state_conflict",
                    "durable catalog state conflicts at the same sequence",
                ));
            }
            sync_private_directory(&paths.plugins_root()).map_err(|error| {
                ManagerError::new(
                    "catalog_state_store",
                    format!("catalog state directory cannot be persisted: {error}"),
                )
            })?;
            return Ok(());
        }
    }
    let bytes = serde_json_canonicalizer::to_vec(&stored)
        .map_err(|_| ManagerError::new("catalog_state_store", "catalog state cannot be encoded"))?;
    if bytes.len() as u64 > MAX_CATALOG_STATE_BYTES {
        return Err(ManagerError::new(
            "catalog_state_store",
            "catalog state exceeds its byte limit",
        ));
    }
    let temporary_path = paths.plugins_root().join(format!(
        ".catalog-state-{}.tmp",
        random_storage_id().map_err(ManagerError::from)?
    ));
    let final_path = catalog_state_path(paths);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary_path)
            .map_err(|error| {
                ManagerError::new(
                    "catalog_state_store",
                    format!("catalog state temp cannot be created: {error}"),
                )
            })?;
        let metadata = file.metadata().map_err(|error| {
            ManagerError::new(
                "catalog_state_store",
                format!("catalog state temp cannot be inspected: {error}"),
            )
        })?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.permissions().mode() & 0o777 != 0o600
        {
            return Err(ManagerError::new(
                "catalog_state_store",
                "catalog state temp is not an owned 0600 single-link file",
            ));
        }
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| {
                ManagerError::new(
                    "catalog_state_store",
                    format!("catalog state temp cannot be persisted: {error}"),
                )
            })?;
        fs::rename(&temporary_path, &final_path).map_err(|error| {
            ManagerError::new(
                "catalog_state_store",
                format!("catalog state cannot be atomically replaced: {error}"),
            )
        })?;
        sync_private_directory(&paths.plugins_root()).map_err(|error| {
            ManagerError::new(
                "catalog_state_store",
                format!("catalog state directory cannot be persisted: {error}"),
            )
        })?;
        if read_catalog_state(paths)?.as_ref() != Some(&stored) {
            return Err(ManagerError::new(
                "catalog_state_changed",
                "persisted catalog state differs from the accepted state",
            ));
        }
        Ok(())
    })();
    if temporary_path.exists() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn receipt_error(code: &str) -> ReceiptTrustError {
    ReceiptTrustError::new(match code {
        "catalog_unconfigured" => "catalog_unconfigured",
        "catalog_path_unsafe" => "catalog_path_unsafe",
        "catalog_file_unsafe" => "catalog_file_unsafe",
        "catalog_file_changed" => "catalog_file_changed",
        "catalog_time" => "catalog_time",
        "catalog_state_unavailable" => "catalog_state_unavailable",
        "catalog_state_unsafe" => "catalog_state_unsafe",
        "catalog_state_invalid" => "catalog_state_invalid",
        "catalog_state_changed" => "catalog_state_changed",
        "catalog_state_replayed" => "catalog_state_replayed",
        "catalog_state_conflict" => "catalog_state_conflict",
        "catalog_state_store" => "catalog_state_store",
        "catalog_json" => "catalog_json",
        "catalog_schema" => "catalog_schema",
        "catalog_cardinality" => "catalog_cardinality",
        "catalog_duplicate" => "catalog_duplicate",
        "catalog_string" => "catalog_string",
        "catalog_key" => "catalog_key",
        "catalog_not_yet_valid" => "catalog_not_yet_valid",
        "catalog_expired" => "catalog_expired",
        "catalog_trust_not_provisioned" => "catalog_trust_not_provisioned",
        "catalog_replayed" => "catalog_replayed",
        "catalog_conflict" => "catalog_conflict",
        "catalog_previous_digest" => "catalog_previous_digest",
        "catalog_root_expiry" => "catalog_root_expiry",
        "catalog_rotation_key_conflict" => "catalog_rotation_key_conflict",
        "catalog_unknown_root" => "catalog_unknown_root",
        "catalog_root_not_valid" => "catalog_root_not_valid",
        "catalog_threshold" => "catalog_threshold",
        "catalog_rotation_threshold" => "catalog_rotation_threshold",
        "publisher_lineage_invalid" => "publisher_lineage_invalid",
        "publisher_key_not_bound" => "publisher_key_not_bound",
        "publisher_key_revoked" => "publisher_key_revoked",
        "publisher_key_not_valid" => "publisher_key_not_valid",
        "package_revoked" => "package_revoked",
        "catalog_release_url" => "catalog_release_url",
        _ => "catalog_unavailable",
    })
}

fn evidence_path(paths: &PluginPaths, digest: &Digest) -> Result<PathBuf, ReceiptTrustError> {
    let digest = digest
        .as_str()
        .strip_prefix("sha256:")
        .filter(|value| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
        .ok_or_else(|| ReceiptTrustError::new("receipt_trust_evidence_invalid"))?;
    Ok(paths.receipt_trust_root().join(format!("{digest}.json")))
}

fn validate_evidence_root(paths: &PluginPaths) -> Result<(), ReceiptTrustError> {
    let root = fs::symlink_metadata(paths.receipt_trust_root())
        .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_unavailable"))?;
    if !root.is_dir()
        || root.file_type().is_symlink()
        || root.uid() != unsafe { libc::geteuid() }
        || root.permissions().mode() & 0o777 != 0o700
    {
        return Err(ReceiptTrustError::new("receipt_trust_evidence_unsafe"));
    }
    Ok(())
}

fn is_evidence_temporary_name(name: &str) -> bool {
    let Some(identifier) = name
        .strip_prefix('.')
        .and_then(|value| value.strip_suffix(".tmp"))
    else {
        return false;
    };
    let bytes = identifier.as_bytes();
    if bytes.len() != 36 || bytes[14] != b'4' || !matches!(bytes[19], b'8' | b'9' | b'a' | b'b') {
        return false;
    }
    bytes.iter().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            *byte == b'-'
        } else {
            byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
        }
    })
}

fn repair_interrupted_evidence_publish(
    paths: &PluginPaths,
    final_path: &Path,
    file: &File,
    before: &fs::Metadata,
) -> Result<fs::Metadata, ReceiptTrustError> {
    if before.nlink() != 2
        || !before.is_file()
        || before.uid() != unsafe { libc::geteuid() }
        || before.permissions().mode() & 0o777 != 0o600
    {
        return Err(ReceiptTrustError::new("receipt_trust_evidence_unsafe"));
    }
    let mut orphan = None;
    for entry in fs::read_dir(paths.receipt_trust_root())
        .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_unsafe"))?
    {
        let entry = entry.map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_unsafe"))?;
        let path = entry.path();
        if path == final_path {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_unsafe"))?;
        if metadata.dev() != before.dev() || metadata.ino() != before.ino() {
            continue;
        }
        let valid_temporary_link = orphan.is_none()
            && metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o777 == 0o600
            && entry
                .file_name()
                .to_str()
                .is_some_and(is_evidence_temporary_name);
        if !valid_temporary_link {
            return Err(ReceiptTrustError::new("receipt_trust_evidence_unsafe"));
        }
        orphan = Some(path);
    }
    let Some(orphan) = orphan else {
        let after = file
            .metadata()
            .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_unsafe"))?;
        return if after.dev() == before.dev() && after.ino() == before.ino() && after.nlink() == 1 {
            Ok(after)
        } else {
            Err(ReceiptTrustError::new("receipt_trust_evidence_unsafe"))
        };
    };
    match fs::remove_file(&orphan) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(ReceiptTrustError::new("receipt_trust_evidence_unsafe")),
    }
    sync_private_directory(&paths.receipt_trust_root())
        .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_unsafe"))?;
    let after = file
        .metadata()
        .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_unsafe"))?;
    let entry = fs::symlink_metadata(final_path)
        .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_unsafe"))?;
    if after.dev() != before.dev()
        || after.ino() != before.ino()
        || after.nlink() != 1
        || entry.file_type().is_symlink()
        || entry.dev() != after.dev()
        || entry.ino() != after.ino()
    {
        return Err(ReceiptTrustError::new("receipt_trust_evidence_unsafe"));
    }
    Ok(after)
}

fn read_evidence_bytes(paths: &PluginPaths, digest: &Digest) -> Result<Vec<u8>, ReceiptTrustError> {
    validate_evidence_root(paths)?;
    let path = evidence_path(paths, digest)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|error| {
            ReceiptTrustError::new(if error.kind() == std::io::ErrorKind::NotFound {
                "receipt_trust_evidence_missing"
            } else {
                "receipt_trust_evidence_unsafe"
            })
        })?;
    let before = file
        .metadata()
        .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_unsafe"))?;
    let entry = fs::symlink_metadata(&path)
        .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_unsafe"))?;
    if !before.is_file()
        || entry.file_type().is_symlink()
        || before.dev() != entry.dev()
        || before.ino() != entry.ino()
        || before.uid() != unsafe { libc::geteuid() }
        || before.permissions().mode() & 0o777 != 0o600
        || before.len() > MAX_EVIDENCE_BYTES
    {
        return Err(ReceiptTrustError::new("receipt_trust_evidence_unsafe"));
    }
    let before = if before.nlink() == 1 {
        before
    } else {
        repair_interrupted_evidence_publish(paths, &path, &file, &before)?
    };
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_EVIDENCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_unsafe"))?;
    let after = file
        .metadata()
        .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_unsafe"))?;
    let final_entry = fs::symlink_metadata(&path)
        .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_changed"))?;
    if bytes.len() as u64 > MAX_EVIDENCE_BYTES
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || after.nlink() != 1
        || after.uid() != unsafe { libc::geteuid() }
        || after.permissions().mode() & 0o777 != 0o600
        || final_entry.file_type().is_symlink()
        || final_entry.dev() != after.dev()
        || final_entry.ino() != after.ino()
    {
        return Err(ReceiptTrustError::new("receipt_trust_evidence_changed"));
    }
    Ok(bytes)
}

fn read_evidence(
    paths: &PluginPaths,
    digest: &Digest,
) -> Result<StoredReceiptEvidence, ReceiptTrustError> {
    let bytes = read_evidence_bytes(paths, digest)?;
    let evidence: StoredReceiptEvidence = serde_json::from_slice(&bytes)
        .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_invalid"))?;
    let canonical = serde_json_canonicalizer::to_vec(&evidence)
        .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_invalid"))?;
    if bytes != canonical || evidence.archive_digest != *digest {
        return Err(ReceiptTrustError::new("receipt_trust_evidence_invalid"));
    }
    Ok(evidence)
}

fn write_evidence(
    paths: &PluginPaths,
    evidence: &StoredReceiptEvidence,
) -> Result<(), ReceiptTrustError> {
    validate_evidence_root(paths)?;
    let bytes = serde_json_canonicalizer::to_vec(evidence)
        .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_store"))?;
    if bytes.len() as u64 > MAX_EVIDENCE_BYTES {
        return Err(ReceiptTrustError::new("receipt_trust_evidence_store"));
    }
    let final_path = evidence_path(paths, &evidence.archive_digest)?;
    match read_evidence_bytes(paths, &evidence.archive_digest) {
        Ok(existing) if existing == bytes => {
            sync_private_directory(&paths.receipt_trust_root())
                .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_store"))?;
            return Ok(());
        }
        Ok(_) => {
            return Err(ReceiptTrustError::new("receipt_trust_evidence_conflict"));
        }
        Err(error) if error.code() == "receipt_trust_evidence_missing" => {}
        Err(error) => return Err(error),
    }

    let temporary_path = paths.receipt_trust_root().join(format!(
        ".{}.tmp",
        random_storage_id()
            .map_err(|_| { ReceiptTrustError::new("receipt_trust_evidence_store") })?
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary_path)
            .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_store"))?;
        let metadata = file
            .metadata()
            .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_store"))?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.permissions().mode() & 0o777 != 0o600
        {
            return Err(ReceiptTrustError::new("receipt_trust_evidence_store"));
        }
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_store"))?;
        fs::hard_link(&temporary_path, &final_path).map_err(|error| {
            ReceiptTrustError::new(if error.kind() == std::io::ErrorKind::AlreadyExists {
                "receipt_trust_evidence_conflict"
            } else {
                "receipt_trust_evidence_store"
            })
        })?;
        fs::remove_file(&temporary_path)
            .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_store"))?;
        sync_private_directory(&paths.receipt_trust_root())
            .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_store"))?;
        let persisted = read_evidence_bytes(paths, &evidence.archive_digest)?;
        if persisted != bytes {
            return Err(ReceiptTrustError::new("receipt_trust_evidence_changed"));
        }
        Ok(())
    })();
    if temporary_path.exists() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn digest_bytes(bytes: &[u8]) -> Result<Digest, ReceiptTrustError> {
    Digest::new(format!("sha256:{:x}", Sha256::digest(bytes)))
        .map_err(|_| ReceiptTrustError::new("receipt_trust_evidence_invalid"))
}

fn read_snapshot_file(
    snapshot: &Path,
    expected: &PackageFile,
) -> Result<Vec<u8>, ReceiptTrustError> {
    let root = fs::symlink_metadata(snapshot)
        .map_err(|_| ReceiptTrustError::new("developer_receipt_evidence_invalid"))?;
    if !root.is_dir()
        || root.file_type().is_symlink()
        || root.uid() != unsafe { libc::geteuid() }
        || root.permissions().mode() & 0o777 != 0o555
        || expected.size > MAX_EVIDENCE_BYTES
    {
        return Err(ReceiptTrustError::new("developer_receipt_evidence_invalid"));
    }
    let path = snapshot.join(expected.path.as_str());
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|_| ReceiptTrustError::new("developer_receipt_evidence_invalid"))?;
    let before = file
        .metadata()
        .map_err(|_| ReceiptTrustError::new("developer_receipt_evidence_invalid"))?;
    let entry = fs::symlink_metadata(&path)
        .map_err(|_| ReceiptTrustError::new("developer_receipt_evidence_invalid"))?;
    if !before.is_file()
        || entry.file_type().is_symlink()
        || before.dev() != entry.dev()
        || before.ino() != entry.ino()
        || before.uid() != unsafe { libc::geteuid() }
        || before.nlink() != 1
        || before.permissions().mode() & 0o777 != expected.mode.as_octal()
        || before.len() != expected.size
    {
        return Err(ReceiptTrustError::new("developer_receipt_evidence_invalid"));
    }
    let mut bytes = Vec::with_capacity(expected.size as usize);
    Read::by_ref(&mut file)
        .take(MAX_EVIDENCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ReceiptTrustError::new("developer_receipt_evidence_invalid"))?;
    let after = file
        .metadata()
        .map_err(|_| ReceiptTrustError::new("developer_receipt_evidence_invalid"))?;
    if bytes.len() as u64 != expected.size
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || digest_bytes(&bytes)? != expected.digest
    {
        return Err(ReceiptTrustError::new("developer_receipt_evidence_invalid"));
    }
    Ok(bytes)
}

fn read_catalog_file(paths: &PluginPaths, path: &PathBuf) -> ManagerResult<Vec<u8>> {
    let parent = fs::symlink_metadata(paths.plugins_root()).map_err(|error| {
        ManagerError::new(
            if error.kind() == std::io::ErrorKind::NotFound {
                "catalog_unconfigured"
            } else {
                "catalog_path_unsafe"
            },
            error.to_string(),
        )
    })?;
    if !parent.is_dir()
        || parent.file_type().is_symlink()
        || parent.uid() != unsafe { libc::geteuid() }
        || parent.permissions().mode() & 0o777 != 0o700
    {
        return Err(ManagerError::new(
            "catalog_path_unsafe",
            "catalog parent must be an owned 0700 directory",
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| {
            ManagerError::new(
                if error.kind() == std::io::ErrorKind::NotFound {
                    "catalog_unconfigured"
                } else {
                    "catalog_file_unsafe"
                },
                error.to_string(),
            )
        })?;
    let before = file
        .metadata()
        .map_err(|error| ManagerError::new("catalog_file_unsafe", error.to_string()))?;
    let entry = fs::symlink_metadata(path)
        .map_err(|error| ManagerError::new("catalog_file_unsafe", error.to_string()))?;
    if !before.is_file()
        || entry.file_type().is_symlink()
        || before.dev() != entry.dev()
        || before.ino() != entry.ino()
        || before.uid() != unsafe { libc::geteuid() }
        || before.nlink() != 1
        || before.permissions().mode() & 0o777 != 0o600
        || before.len() > MAX_CATALOG_BYTES
    {
        return Err(ManagerError::new(
            "catalog_file_unsafe",
            "catalog must be an owned 0600 single-link regular file",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_CATALOG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ManagerError::new("catalog_file_unsafe", error.to_string()))?;
    let after = file
        .metadata()
        .map_err(|error| ManagerError::new("catalog_file_unsafe", error.to_string()))?;
    if bytes.len() as u64 > MAX_CATALOG_BYTES
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
    {
        return Err(ManagerError::new(
            "catalog_file_changed",
            "catalog changed while it was being read",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File, OpenOptions};
    use std::os::unix::fs::symlink;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    use chrono::DateTime;
    use ed25519_dalek::{Signer, SigningKey};
    use jarvis_package::{
        inspect_and_verify_package, pack_plugin, PackOptions, PackageError, PackageSignatureSource,
    };
    use jarvis_plugin_protocol::catalog::SignedCatalog;
    use jarvis_plugin_protocol::package::{
        MacOsVersion, PackageMetadataV1, PackageSignatureV1, PackageTarget, SignatureAlgorithm,
    };
    use jarvis_plugin_protocol::receipt::{
        InstallReceipt, InstallSource, INSTALL_RECEIPT_SCHEMA_VERSION,
    };
    use serde_json::json;

    use super::{evidence_path, read_evidence, ProductionCatalogProvider, CATALOG_STATE_FILE};
    use crate::plugins::manifest_v2::HostCompatibility;
    use crate::plugins::package::HostPackageDocumentAdapter;
    use crate::plugins::package_manager::manager::{CatalogProvider, Clock, InstallSourceRef};
    use crate::plugins::package_manager::paths::PluginPaths;
    use crate::plugins::package_manager::receipt::{ReceiptStore, VersionStore};
    use crate::plugins::resolver::{PluginResolver, ResolutionPolicy, ResolvedPlugin};
    use crate::plugins::trust::catalog::CatalogCompatibility;
    use crate::plugins::trust::package::{AllowPackageVerifier, SharedPackageVerifier};
    use crate::plugins::trust::signature::{catalog_digest, catalog_signature_message};

    const ROOTS: &[u8] = include_bytes!("../../../tests/fixtures/plugin-trust/root-public.json");
    const CATALOG: &[u8] =
        include_bytes!("../../../tests/fixtures/plugin-trust/catalog-seq-1.json");
    const ROTATED_CATALOG: &[u8] =
        include_bytes!("../../../tests/fixtures/plugin-trust/catalog-seq-2-rotated.json");
    const SIGNING_SEED: &str =
        include_str!("../../../tests/fixtures/plugin-trust/package-test-signing-seed.hex");
    const ROOT_2_SIGNING_SEED: &str =
        include_str!("../../../tests/fixtures/plugin-trust/root-2-test-signing-seed.hex");
    const PUBLIC_KEY: &str =
        include_str!("../../../tests/fixtures/plugin-trust/package-test-public-key.hex");

    struct FixedClock;

    impl Clock for FixedClock {
        fn now_ms(&self) -> i64 {
            DateTime::parse_from_rfc3339("2026-08-01T00:30:00Z")
                .unwrap()
                .timestamp_millis()
        }
    }

    struct RotatedClock;

    impl Clock for RotatedClock {
        fn now_ms(&self) -> i64 {
            DateTime::parse_from_rfc3339("2026-08-01T01:30:00Z")
                .unwrap()
                .timestamp_millis()
        }
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!(
                    "jarvis-production-catalog-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct TestSignatureSource;

    impl PackageSignatureSource for TestSignatureSource {
        fn sign(&self, message: &[u8]) -> Result<PackageSignatureV1, PackageError> {
            let signature = signing_key().sign(message);
            PackageSignatureV1::new(
                SignatureAlgorithm::Ed25519,
                "example.release:1",
                STANDARD.encode(signature.to_bytes()),
            )
            .map_err(|_| PackageError::package_metadata())
        }
    }

    fn signing_key() -> SigningKey {
        signing_key_from_seed(SIGNING_SEED)
    }

    fn signing_key_from_seed(raw: &str) -> SigningKey {
        let raw = raw.trim().as_bytes();
        let mut seed = [0_u8; 32];
        for (index, chunk) in raw.chunks_exact(2).enumerate() {
            seed[index] = (nibble(chunk[0]) << 4) | nibble(chunk[1]);
        }
        SigningKey::from_bytes(&seed)
    }

    fn nibble(value: u8) -> u8 {
        match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            _ => panic!("test seed is lowercase hex"),
        }
    }

    fn package_source() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../crates/jarvis-package/tests/fixtures/plugin-packages/pack-source")
    }

    fn adapter() -> HostPackageDocumentAdapter {
        HostPackageDocumentAdapter::new(HostCompatibility::parse("0.4.0", 2).unwrap())
    }

    fn build_archive(root: &TestDirectory) -> (PathBuf, PackageMetadataV1) {
        let archive_path = root.0.join("fixture.jarvis-plugin");
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&archive_path)
            .unwrap();
        pack_plugin(
            &package_source(),
            PackOptions {
                target: PackageTarget::DarwinArm64,
                minimum_macos: MacOsVersion::parse("14.0.0").unwrap(),
            },
            &adapter(),
            &TestSignatureSource,
            &mut output,
        )
        .unwrap();
        output.sync_all().unwrap();
        let evidence = inspect_and_verify_package(
            File::open(&archive_path).unwrap(),
            &adapter(),
            &AllowPackageVerifier,
        )
        .unwrap();
        (archive_path, evidence.facts().metadata().clone())
    }

    fn write_catalog(
        path: &PathBuf,
        metadata: &PackageMetadataV1,
        archive_digest: &jarvis_plugin_protocol::manifest::Digest,
        package_signature: &PackageSignatureV1,
    ) {
        write_catalog_sequence(path, metadata, archive_digest, package_signature, 1, None);
    }

    fn write_catalog_sequence(
        path: &PathBuf,
        metadata: &PackageMetadataV1,
        archive_digest: &jarvis_plugin_protocol::manifest::Digest,
        package_signature: &PackageSignatureV1,
        sequence: u64,
        previous_digest: Option<&jarvis_plugin_protocol::manifest::Digest>,
    ) -> jarvis_plugin_protocol::manifest::Digest {
        let mut catalog = json!({
            "schemaVersion": 1,
            "sequence": sequence,
            "issuedAt": "2026-08-01T00:00:00Z",
            "expiresAt": "2026-08-02T00:00:00Z",
            "previousDigest": previous_digest,
            "payload": {
                "publisherLineages": [{
                    "id": "example.release",
                    "publisher": metadata.publisher,
                    "pluginIds": [metadata.plugin_id],
                    "keys": [{
                        "keyId": "example.release:1",
                        "algorithm": "ed25519",
                        "publicKey": PUBLIC_KEY.trim(),
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
                    "archiveDigest": archive_digest,
                    "packageSignature": package_signature,
                    "revoked": false
                }],
                "rootRotation": null,
                "revokedPackageDigests": [],
                "revokedPublisherKeys": []
            },
            "signatures": [{
                "algorithm": "ed25519",
                "keyId": "jarvis.root:1",
                "value": "gDDYgr16HoixPzQjmuL8+CTds3bPmnZlxOHqex3+FifEyJqpD8PHzZT5HUWX4tQrUrijxOGqKbQu/ZaPOSAjCQ=="
            }]
        });
        let parsed = SignedCatalog::parse(&serde_json::to_vec(&catalog).unwrap()).unwrap();
        let signature = signing_key()
            .sign(&catalog_signature_message(&parsed).unwrap())
            .to_bytes();
        catalog["signatures"][0]["value"] = json!(STANDARD.encode(signature));
        let bytes = serde_json::to_vec(&catalog).unwrap();
        let digest = catalog_digest(&SignedCatalog::parse(&bytes).unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        digest
    }

    fn write_rotated_catalog_successor(path: &PathBuf) {
        let rotated = SignedCatalog::parse(ROTATED_CATALOG).unwrap();
        let previous_digest = catalog_digest(&rotated).unwrap();
        let mut catalog: serde_json::Value = serde_json::from_slice(ROTATED_CATALOG).unwrap();
        let root_2_signature = catalog["signatures"]
            .as_array()
            .unwrap()
            .iter()
            .find(|signature| signature["keyId"] == "jarvis.root:2")
            .unwrap()
            .clone();
        catalog["sequence"] = json!(3);
        catalog["previousDigest"] = json!(previous_digest);
        catalog["payload"]["rootRotation"] = serde_json::Value::Null;
        catalog["signatures"] = json!([root_2_signature]);
        let parsed = SignedCatalog::parse(&serde_json::to_vec(&catalog).unwrap()).unwrap();
        let signature = signing_key_from_seed(ROOT_2_SIGNING_SEED)
            .sign(&catalog_signature_message(&parsed).unwrap())
            .to_bytes();
        catalog["signatures"][0]["value"] = json!(STANDARD.encode(signature));
        fs::write(path, serde_json::to_vec(&catalog).unwrap()).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn stage_version(
        paths: &PluginPaths,
        metadata: &PackageMetadataV1,
        manifest: &jarvis_plugin_protocol::manifest::ManifestV2,
        archive_digest: &jarvis_plugin_protocol::manifest::Digest,
    ) {
        let extracted = paths.quarantine_root().join("resolver-extracted");
        fs::create_dir(&extracted).unwrap();
        for file in &metadata.files {
            let destination = extracted.join(file.path.as_str());
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            let bytes = if file.path.as_str() == "plugin.json" {
                serde_json_canonicalizer::to_vec(manifest).unwrap()
            } else {
                fs::read(package_source().join(file.path.as_str())).unwrap()
            };
            fs::write(&destination, bytes).unwrap();
            fs::set_permissions(
                &destination,
                fs::Permissions::from_mode(file.mode.as_octal()),
            )
            .unwrap();
        }
        VersionStore::new(paths.clone())
            .finalize_extracted(
                &extracted,
                &metadata.plugin_id,
                &metadata.version,
                archive_digest,
            )
            .unwrap();
    }

    #[test]
    fn production_provider_selects_install_from_verified_catalog() {
        let root = TestDirectory::new();
        let profile = root.0.join("profile");
        let paths = PluginPaths::new(profile);
        paths.prepare().unwrap();
        let catalog_path = paths.plugins_root().join("catalog.json");
        fs::write(&catalog_path, CATALOG).unwrap();
        fs::set_permissions(&catalog_path, fs::Permissions::from_mode(0o600)).unwrap();
        let provider = ProductionCatalogProvider::new(
            paths,
            catalog_path,
            ROOTS,
            CatalogCompatibility::parse("0.4.0", 2, PackageTarget::DarwinArm64, "14.0.0").unwrap(),
            Arc::new(FixedClock),
        );

        let selected = provider
            .select(&InstallSourceRef::Catalog {
                id: "dev.example.echo".into(),
                version: Some("1.0.0".into()),
            })
            .unwrap();

        assert_eq!(selected.plugin_id.as_str(), "dev.example.echo");
        assert_eq!(selected.version.to_string(), "1.0.0");
        assert_eq!(selected.catalog_sequence, 1);
        assert_eq!(
            selected.archive_digest.as_str(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(selected.publisher_key_id, "example.release:1");
        assert_eq!(selected.publisher_lineage, "example.release");
    }

    #[test]
    fn provider_recreation_rejects_symlinked_catalog_state() {
        let root = TestDirectory::new();
        let paths = PluginPaths::new(root.0.join("profile"));
        paths.prepare().unwrap();
        let catalog_path = paths.plugins_root().join("catalog.json");
        fs::write(&catalog_path, CATALOG).unwrap();
        fs::set_permissions(&catalog_path, fs::Permissions::from_mode(0o600)).unwrap();
        let compatibility =
            CatalogCompatibility::parse("0.4.0", 2, PackageTarget::DarwinArm64, "14.0.0").unwrap();
        let provider = ProductionCatalogProvider::new(
            paths.clone(),
            catalog_path.clone(),
            ROOTS,
            compatibility.clone(),
            Arc::new(FixedClock),
        );
        provider
            .select(&InstallSourceRef::Catalog {
                id: "dev.example.echo".into(),
                version: Some("1.0.0".into()),
            })
            .unwrap();
        drop(provider);
        let state_path = paths.plugins_root().join(CATALOG_STATE_FILE);
        let backing_path = paths.plugins_root().join("catalog-state.backing");
        fs::rename(&state_path, &backing_path).unwrap();
        symlink(&backing_path, &state_path).unwrap();

        let error = ProductionCatalogProvider::new(
            paths,
            catalog_path,
            ROOTS,
            compatibility,
            Arc::new(FixedClock),
        )
        .select(&InstallSourceRef::Catalog {
            id: "dev.example.echo".into(),
            version: Some("1.0.0".into()),
        })
        .unwrap_err();

        assert_eq!(error.code(), "catalog_state_unsafe");
    }

    #[test]
    fn provider_recreation_rejects_hardlinked_catalog_state() {
        let root = TestDirectory::new();
        let paths = PluginPaths::new(root.0.join("profile"));
        paths.prepare().unwrap();
        let catalog_path = paths.plugins_root().join("catalog.json");
        fs::write(&catalog_path, CATALOG).unwrap();
        fs::set_permissions(&catalog_path, fs::Permissions::from_mode(0o600)).unwrap();
        let compatibility =
            CatalogCompatibility::parse("0.4.0", 2, PackageTarget::DarwinArm64, "14.0.0").unwrap();
        let provider = ProductionCatalogProvider::new(
            paths.clone(),
            catalog_path.clone(),
            ROOTS,
            compatibility.clone(),
            Arc::new(FixedClock),
        );
        provider
            .select(&InstallSourceRef::Catalog {
                id: "dev.example.echo".into(),
                version: Some("1.0.0".into()),
            })
            .unwrap();
        drop(provider);
        fs::hard_link(
            paths.plugins_root().join(CATALOG_STATE_FILE),
            paths.plugins_root().join("catalog-state.alias"),
        )
        .unwrap();

        let error = ProductionCatalogProvider::new(
            paths,
            catalog_path,
            ROOTS,
            compatibility,
            Arc::new(FixedClock),
        )
        .select(&InstallSourceRef::Catalog {
            id: "dev.example.echo".into(),
            version: Some("1.0.0".into()),
        })
        .unwrap_err();

        assert_eq!(error.code(), "catalog_state_unsafe");
    }

    #[test]
    fn provider_recreation_accepts_the_next_catalog_sequence() {
        let root = TestDirectory::new();
        let paths = PluginPaths::new(root.0.join("profile"));
        paths.prepare().unwrap();
        let (archive_path, metadata) = build_archive(&root);
        let package = inspect_and_verify_package(
            File::open(&archive_path).unwrap(),
            &adapter(),
            &AllowPackageVerifier,
        )
        .unwrap();
        let archive_digest = package.facts().archive_digest().clone();
        let package_signature = package.facts().signature().clone();
        let catalog_path = paths.plugins_root().join("catalog.json");
        let first_digest = write_catalog_sequence(
            &catalog_path,
            &metadata,
            &archive_digest,
            &package_signature,
            1,
            None,
        );
        let compatibility =
            CatalogCompatibility::parse("0.4.0", 2, PackageTarget::DarwinArm64, "14.0.0").unwrap();
        let provider = ProductionCatalogProvider::new(
            paths.clone(),
            catalog_path.clone(),
            ROOTS,
            compatibility.clone(),
            Arc::new(FixedClock),
        );
        provider
            .select(&InstallSourceRef::Catalog {
                id: metadata.plugin_id.as_str().into(),
                version: Some(metadata.version.to_string()),
            })
            .unwrap();
        drop(provider);

        write_catalog_sequence(
            &catalog_path,
            &metadata,
            &archive_digest,
            &package_signature,
            2,
            Some(&first_digest),
        );
        let recreated = ProductionCatalogProvider::new(
            paths,
            catalog_path,
            ROOTS,
            compatibility,
            Arc::new(FixedClock),
        );

        let selected = recreated
            .select(&InstallSourceRef::Catalog {
                id: metadata.plugin_id.as_str().into(),
                version: Some(metadata.version.to_string()),
            })
            .unwrap();
        assert_eq!(selected.catalog_sequence, 2);
    }

    #[test]
    fn provider_recreation_rejects_a_previous_catalog_sequence() {
        let root = TestDirectory::new();
        let paths = PluginPaths::new(root.0.join("profile"));
        paths.prepare().unwrap();
        let (archive_path, metadata) = build_archive(&root);
        let package = inspect_and_verify_package(
            File::open(&archive_path).unwrap(),
            &adapter(),
            &AllowPackageVerifier,
        )
        .unwrap();
        let archive_digest = package.facts().archive_digest().clone();
        let package_signature = package.facts().signature().clone();
        let catalog_path = paths.plugins_root().join("catalog.json");
        let first_digest = write_catalog_sequence(
            &catalog_path,
            &metadata,
            &archive_digest,
            &package_signature,
            1,
            None,
        );
        let compatibility =
            CatalogCompatibility::parse("0.4.0", 2, PackageTarget::DarwinArm64, "14.0.0").unwrap();
        let provider = ProductionCatalogProvider::new(
            paths.clone(),
            catalog_path.clone(),
            ROOTS,
            compatibility.clone(),
            Arc::new(FixedClock),
        );
        provider
            .select(&InstallSourceRef::Catalog {
                id: metadata.plugin_id.as_str().into(),
                version: Some(metadata.version.to_string()),
            })
            .unwrap();
        write_catalog_sequence(
            &catalog_path,
            &metadata,
            &archive_digest,
            &package_signature,
            2,
            Some(&first_digest),
        );
        provider
            .select(&InstallSourceRef::Catalog {
                id: metadata.plugin_id.as_str().into(),
                version: Some(metadata.version.to_string()),
            })
            .unwrap();
        drop(provider);

        write_catalog_sequence(
            &catalog_path,
            &metadata,
            &archive_digest,
            &package_signature,
            1,
            None,
        );
        let recreated = ProductionCatalogProvider::new(
            paths,
            catalog_path,
            ROOTS,
            compatibility,
            Arc::new(FixedClock),
        );

        let error = recreated
            .select(&InstallSourceRef::Catalog {
                id: metadata.plugin_id.as_str().into(),
                version: Some(metadata.version.to_string()),
            })
            .unwrap_err();
        assert_eq!(error.code(), "catalog_replayed");
    }

    #[test]
    fn provider_recreation_uses_persisted_rotated_roots() {
        let root = TestDirectory::new();
        let paths = PluginPaths::new(root.0.join("profile"));
        paths.prepare().unwrap();
        let catalog_path = paths.plugins_root().join("catalog.json");
        fs::write(&catalog_path, CATALOG).unwrap();
        fs::set_permissions(&catalog_path, fs::Permissions::from_mode(0o600)).unwrap();
        let compatibility =
            CatalogCompatibility::parse("0.4.0", 2, PackageTarget::DarwinArm64, "14.0.0").unwrap();
        let provider = ProductionCatalogProvider::new(
            paths.clone(),
            catalog_path.clone(),
            ROOTS,
            compatibility.clone(),
            Arc::new(RotatedClock),
        );
        provider
            .select(&InstallSourceRef::Catalog {
                id: "dev.example.echo".into(),
                version: Some("1.0.0".into()),
            })
            .unwrap();
        fs::write(&catalog_path, ROTATED_CATALOG).unwrap();
        fs::set_permissions(&catalog_path, fs::Permissions::from_mode(0o600)).unwrap();
        let rotated = provider
            .select(&InstallSourceRef::Catalog {
                id: "dev.example.echo".into(),
                version: Some("1.0.0".into()),
            })
            .unwrap();
        assert_eq!(rotated.catalog_sequence, 2);
        drop(provider);
        write_rotated_catalog_successor(&catalog_path);

        let successor = ProductionCatalogProvider::new(
            paths,
            catalog_path,
            ROOTS,
            compatibility,
            Arc::new(RotatedClock),
        )
        .select(&InstallSourceRef::Catalog {
            id: "dev.example.echo".into(),
            version: Some("1.0.0".into()),
        })
        .unwrap();

        assert_eq!(successor.catalog_sequence, 3);
    }

    #[test]
    fn interrupted_evidence_publish_is_repaired_on_read() {
        let root = TestDirectory::new();
        let paths = PluginPaths::new(root.0.join("profile"));
        paths.prepare().unwrap();
        let (archive_path, metadata) = build_archive(&root);
        let package = inspect_and_verify_package(
            File::open(&archive_path).unwrap(),
            &adapter(),
            &AllowPackageVerifier,
        )
        .unwrap();
        let archive_digest = package.facts().archive_digest().clone();
        let package_signature = package.facts().signature().clone();
        let catalog_path = paths.plugins_root().join("catalog.json");
        write_catalog(
            &catalog_path,
            &metadata,
            &archive_digest,
            &package_signature,
        );
        let provider = ProductionCatalogProvider::new(
            paths.clone(),
            catalog_path,
            ROOTS,
            CatalogCompatibility::parse("0.4.0", 2, PackageTarget::DarwinArm64, "14.0.0").unwrap(),
            Arc::new(FixedClock),
        );
        let selected = provider
            .select(&InstallSourceRef::Catalog {
                id: metadata.plugin_id.as_str().into(),
                version: Some(metadata.version.to_string()),
            })
            .unwrap();
        let verifier = SharedPackageVerifier::new(selected.verifier_for_test());
        inspect_and_verify_package(File::open(&archive_path).unwrap(), &adapter(), &verifier)
            .unwrap();

        let final_path = evidence_path(&paths, &archive_digest).unwrap();
        let orphan_path = paths
            .receipt_trust_root()
            .join(".00000000-0000-4000-8000-000000000000.tmp");
        fs::hard_link(&final_path, &orphan_path).unwrap();
        assert_eq!(fs::metadata(&final_path).unwrap().nlink(), 2);

        let evidence = read_evidence(&paths, &archive_digest).unwrap();

        assert_eq!(evidence.archive_digest, archive_digest);
        assert!(!orphan_path.exists());
        assert_eq!(fs::metadata(final_path).unwrap().nlink(), 1);
    }

    #[test]
    fn receipt_selected_by_production_provider_passes_resolver() {
        let root = TestDirectory::new();
        let profile = root.0.join("profile");
        let paths = PluginPaths::new(profile);
        paths.prepare().unwrap();
        let (archive_path, metadata) = build_archive(&root);
        let unsigned_evidence = inspect_and_verify_package(
            File::open(&archive_path).unwrap(),
            &adapter(),
            &AllowPackageVerifier,
        )
        .unwrap();
        let archive_digest = unsigned_evidence.facts().archive_digest().clone();
        let package_signature = unsigned_evidence.facts().signature().clone();
        let catalog_path = paths.plugins_root().join("catalog.json");
        write_catalog(
            &catalog_path,
            &metadata,
            &archive_digest,
            &package_signature,
        );
        let provider = Arc::new(ProductionCatalogProvider::new(
            paths.clone(),
            catalog_path,
            ROOTS,
            CatalogCompatibility::parse("0.4.0", 2, PackageTarget::DarwinArm64, "14.0.0").unwrap(),
            Arc::new(FixedClock),
        ));
        let selected = provider
            .select(&InstallSourceRef::Catalog {
                id: metadata.plugin_id.as_str().into(),
                version: Some(metadata.version.to_string()),
            })
            .unwrap();
        let verifier = SharedPackageVerifier::new(selected.verifier_for_test());
        let evidence =
            inspect_and_verify_package(File::open(&archive_path).unwrap(), &adapter(), &verifier)
                .unwrap();
        let manifest = evidence.facts().manifest().clone();
        stage_version(&paths, &metadata, &manifest, &archive_digest);
        let receipt = InstallReceipt {
            schema_version: INSTALL_RECEIPT_SCHEMA_VERSION,
            plugin_id: metadata.plugin_id.clone(),
            version: metadata.version.clone(),
            package_digest: archive_digest,
            publisher_key_id: selected.publisher_key_id,
            publisher_lineage: selected.publisher_lineage,
            target: metadata.target,
            source: InstallSource::Catalog,
            enabled: true,
            granted_permissions: Vec::new(),
            native_trust_digest: None,
            installed_at_ms: FixedClock.now_ms(),
            generation: 1,
            state_schema_version: metadata.state.schema_version,
            rollback_compatible_through: metadata.state.rollback_compatible_through,
            previous: None,
        };
        ReceiptStore::new(paths.clone()).commit(&receipt).unwrap();
        let resolver = PluginResolver::new(
            paths,
            HostCompatibility::parse("0.4.0", 2).unwrap(),
            PackageTarget::DarwinArm64,
            provider,
        );

        let resolved = resolver
            .resolve(&metadata.plugin_id, ResolutionPolicy::production(false))
            .unwrap();

        assert!(matches!(resolved, ResolvedPlugin::VerifiedReceipt(_)));
    }
}
