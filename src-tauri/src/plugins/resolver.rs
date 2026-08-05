#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use jarvis_plugin_protocol::manifest::{Digest, ManifestV2, PluginId, RuntimeKind};
use jarvis_plugin_protocol::package::{
    PackageFile, PackageMetadataV1, PackageTarget, PACKAGE_SCHEMA_VERSION,
};
use jarvis_plugin_protocol::receipt::{GrantedPermission, InstallReceipt, InstallSource};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::manifest::{self, PluginPackage};
use super::manifest_v2::{validate_packaged_manifest, HostCompatibility, Target as ManifestTarget};
use super::package_manager::paths::PluginPaths;
use super::package_manager::random_storage_id;
use super::package_manager::receipt::{
    ReceiptStore, ReceiptVisibility, VersionStore, VersionVisibility,
};
use super::supervisor::VerifiedExecutable;
use super::trust::catalog::VerifiedCatalog;

const LEGACY_AGENT_VM_ID: &str = "agent-vm";
const LEGACY_MANIFEST: &[u8] = include_bytes!("../../../plugins/agent-vm/manifest.json");
const RECEIPT_HIGH_WATER_SCHEMA: u32 = 1;
const RECEIPT_HIGH_WATER_FILE: &str = "receipt-high-water.json";
const RECEIPT_HIGH_WATER_LOCK: &str = ".receipt-high-water.lock";
const MAX_RECEIPT_HIGH_WATER_BYTES: u64 = 1024 * 1024;
const MAX_RECEIPT_HIGH_WATER_RECORDS: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReceiptHighWaterRecord {
    generation: u64,
    receipt_digest: Digest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReceiptHighWaterState {
    schema_version: u32,
    receipts: BTreeMap<String, ReceiptHighWaterRecord>,
}

impl Default for ReceiptHighWaterState {
    fn default() -> Self {
        Self {
            schema_version: RECEIPT_HIGH_WATER_SCHEMA,
            receipts: BTreeMap::new(),
        }
    }
}

struct ReceiptHighWaterLock(File);

impl Drop for ReceiptHighWaterLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationSource {
    ReceiptV2,
    DeveloperSnapshot,
    LegacyBundledV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifiedReceiptSource {
    ReceiptV2,
    DeveloperSnapshot,
}

impl VerifiedReceiptSource {
    pub const fn activation_source(self) -> ActivationSource {
        match self {
            Self::ReceiptV2 => ActivationSource::ReceiptV2,
            Self::DeveloperSnapshot => ActivationSource::DeveloperSnapshot,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompatibilityStatus {
    pub migration_available: bool,
}

#[derive(Clone, Debug)]
pub struct VerifiedReceiptPlugin {
    pub manifest: ManifestV2,
    pub root: PathBuf,
    pub bridge_executable: Option<VerifiedExecutable>,
    pub source: VerifiedReceiptSource,
    pub package_digest: Digest,
    pub generation: u64,
    pub grants: Vec<GrantedPermission>,
    pub package_files: Vec<PackageFile>,
    pub status: CompatibilityStatus,
}

#[derive(Clone, Debug)]
pub struct LegacyAgentVmPlugin {
    pub package: PluginPackage,
    pub status: CompatibilityStatus,
}

/// The legacy bridge is a separate variant on purpose: v1 has no v2 package
/// digest, receipt generation, grants, or verified executable lease.
#[derive(Clone, Debug)]
pub enum ResolvedPlugin {
    VerifiedReceipt(VerifiedReceiptPlugin),
    LegacyAgentVm(LegacyAgentVmPlugin),
}

impl ResolvedPlugin {
    pub fn source(&self) -> ActivationSource {
        match self {
            Self::VerifiedReceipt(plugin) => plugin.source.activation_source(),
            Self::LegacyAgentVm(_) => ActivationSource::LegacyBundledV1,
        }
    }

    pub fn root(&self) -> &Path {
        match self {
            Self::VerifiedReceipt(plugin) => &plugin.root,
            Self::LegacyAgentVm(plugin) => &plugin.package.root,
        }
    }

    pub fn status(&self) -> CompatibilityStatus {
        match self {
            Self::VerifiedReceipt(plugin) => plugin.status,
            Self::LegacyAgentVm(plugin) => plugin.status,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolutionPolicy {
    pub developer_mode: bool,
    pub legacy_agent_vm_enabled: bool,
}

impl ResolutionPolicy {
    pub const fn production(legacy_agent_vm_enabled: bool) -> Self {
        Self {
            developer_mode: false,
            legacy_agent_vm_enabled,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReceiptTrustFacts {
    archive_digest: Digest,
    metadata: PackageMetadataV1,
    manifest: ManifestV2,
    publisher_key_id: String,
    publisher_lineage: String,
}

impl ReceiptTrustFacts {
    #[cfg(target_os = "macos")]
    pub fn from_verified_package(
        facts: &jarvis_package::VerifiedPackageFacts,
        publisher_lineage: impl Into<String>,
    ) -> Self {
        Self {
            archive_digest: facts.archive_digest().clone(),
            metadata: facts.metadata().clone(),
            manifest: facts.manifest().clone(),
            publisher_key_id: facts.signature().key_id.clone(),
            publisher_lineage: publisher_lineage.into(),
        }
    }

    pub fn archive_digest(&self) -> &Digest {
        &self.archive_digest
    }

    pub fn metadata(&self) -> &PackageMetadataV1 {
        &self.metadata
    }

    pub fn manifest(&self) -> &ManifestV2 {
        &self.manifest
    }

    pub(crate) fn from_catalog_evidence(
        archive_digest: Digest,
        metadata: PackageMetadataV1,
        manifest: ManifestV2,
        publisher_key_id: String,
        publisher_lineage: String,
    ) -> Self {
        Self {
            archive_digest,
            metadata,
            manifest,
            publisher_key_id,
            publisher_lineage,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptTrustError {
    code: &'static str,
}

impl ReceiptTrustError {
    pub const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for ReceiptTrustError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for ReceiptTrustError {}

/// Supplies facts minted from A4 verification and MUST re-check the current
/// catalog/root/publisher/revocation state (or the current Developer authority)
/// on every call. The resolver intentionally calls it both before and after
/// hashing the immutable version tree.
pub trait CurrentReceiptTrust: Send + Sync {
    fn verify_current(
        &self,
        receipt: &InstallReceipt,
    ) -> Result<ReceiptTrustFacts, ReceiptTrustError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableReceiptTrust;

impl CurrentReceiptTrust for UnavailableReceiptTrust {
    fn verify_current(
        &self,
        _receipt: &InstallReceipt,
    ) -> Result<ReceiptTrustFacts, ReceiptTrustError> {
        Err(ReceiptTrustError::new("receipt_trust_provider_unavailable"))
    }
}

pub trait PluginActivationResolver: Send + Sync {
    fn candidate_ids(&self) -> Vec<PluginId>;

    fn resolve(
        &self,
        plugin_id: &PluginId,
        policy: ResolutionPolicy,
    ) -> Result<ResolvedPlugin, ResolverError>;
}

/// Shared A4 adapter check for catalog-backed trust providers.
pub fn verify_catalog_receipt(
    catalog: &VerifiedCatalog,
    receipt: &InstallReceipt,
    facts: &ReceiptTrustFacts,
) -> Result<(), ReceiptTrustError> {
    let version = receipt.version.to_string();
    let release = catalog
        .release(receipt.plugin_id.as_str(), &version, receipt.target)
        .map_err(|error| ReceiptTrustError::new(error.code()))?;
    let record = release.release_record();
    let metadata = &facts.metadata;
    let manifest = &facts.manifest;
    if record.plugin_id != receipt.plugin_id
        || record.version != receipt.version
        || record.target != receipt.target
        || record.archive_digest != receipt.package_digest
        || record.publisher_key_id != receipt.publisher_key_id
        || record.publisher_lineage != receipt.publisher_lineage
        || facts.archive_digest != receipt.package_digest
        || facts.publisher_key_id != receipt.publisher_key_id
        || facts.publisher_lineage != receipt.publisher_lineage
        || metadata.plugin_id != record.plugin_id
        || metadata.publisher != record.publisher
        || metadata.version != record.version
        || metadata.target != record.target
        || metadata.minimum_macos != record.minimum_macos
        || metadata.jarvis_range != record.jarvis_range
        || metadata.plugin_api != record.plugin_api
        || manifest.id != record.plugin_id
        || manifest.publisher != record.publisher
        || manifest.version != record.version
    {
        return Err(ReceiptTrustError::new("package_catalog_mismatch"));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolverError {
    code: &'static str,
    cause: String,
}

impl ResolverError {
    fn new(code: &'static str, cause: impl Into<String>) -> Self {
        Self {
            code,
            cause: cause.into(),
        }
    }

    fn receipt_blocked(cause: impl Into<String>) -> Self {
        Self::new("receipt_activation_blocked", cause)
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn cause(&self) -> &str {
        &self.cause
    }
}

impl fmt::Display for ResolverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.cause)
    }
}

impl std::error::Error for ResolverError {}

pub struct PluginResolver {
    paths: PluginPaths,
    host: HostCompatibility,
    target: PackageTarget,
    trust: Arc<dyn CurrentReceiptTrust>,
    observed_receipts: Mutex<BTreeMap<String, InstallReceipt>>,
}

impl PluginResolver {
    pub fn new(
        paths: PluginPaths,
        host: HostCompatibility,
        target: PackageTarget,
        trust: Arc<dyn CurrentReceiptTrust>,
    ) -> Self {
        Self {
            paths,
            host,
            target,
            trust,
            observed_receipts: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn resolve(
        &self,
        plugin_id: &PluginId,
        policy: ResolutionPolicy,
    ) -> Result<ResolvedPlugin, ResolverError> {
        let receipts = ReceiptStore::new(self.paths.clone());
        match receipts.current(plugin_id) {
            Ok(Some(receipt)) => self.resolve_receipt(&receipts, receipt, policy),
            Ok(None) => self.resolve_legacy(plugin_id, &receipts, policy),
            Err(error) => Err(ResolverError::receipt_blocked(error.code())),
        }
    }

    pub fn candidate_ids(&self) -> Vec<PluginId> {
        let mut candidates = BTreeMap::new();
        let Ok(entries) = fs::read_dir(self.paths.plugins_root()) else {
            return Vec::new();
        };
        for entry in entries.flatten() {
            if !entry
                .file_type()
                .map(|file_type| file_type.is_dir())
                .unwrap_or(false)
            {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(plugin_id) = PluginId::new(name) else {
                continue;
            };
            let root = entry.path();
            let has_receipt = fs::symlink_metadata(root.join("current")).is_ok();
            let is_legacy_agent_vm = plugin_id.as_str() == LEGACY_AGENT_VM_ID
                && fs::symlink_metadata(root.join("manifest.json")).is_ok();
            if has_receipt || is_legacy_agent_vm {
                candidates.insert(plugin_id.as_str().to_owned(), plugin_id);
            }
        }
        candidates.into_values().collect()
    }

    fn resolve_receipt(
        &self,
        receipts: &ReceiptStore,
        receipt: InstallReceipt,
        policy: ResolutionPolicy,
    ) -> Result<ResolvedPlugin, ResolverError> {
        let mut observed = self
            .observed_receipts
            .lock()
            .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_unavailable"))?;
        let _durable_lock = acquire_receipt_high_water_lock(&self.paths)?;
        let mut durable = read_receipt_high_water(&self.paths)?;
        let current_receipt_digest = receipt_digest(&receipt)?;
        check_receipt_high_water(
            &receipt,
            &current_receipt_digest,
            durable.receipts.get(receipt.plugin_id.as_str()),
        )?;
        if let Some(previous) = observed.get(receipt.plugin_id.as_str()) {
            let previous_digest = receipt_digest(previous)?;
            check_receipt_high_water(
                &receipt,
                &current_receipt_digest,
                Some(&ReceiptHighWaterRecord {
                    generation: previous.generation,
                    receipt_digest: previous_digest,
                }),
            )?;
        }
        if receipt.target != self.target {
            return Err(ResolverError::receipt_blocked("receipt_target_mismatch"));
        }
        if !receipt.enabled {
            return Err(ResolverError::receipt_blocked("receipt_disabled"));
        }
        let source = match receipt.source {
            InstallSource::Catalog | InstallSource::LocalPackage => {
                VerifiedReceiptSource::ReceiptV2
            }
            InstallSource::DeveloperSnapshot if policy.developer_mode => {
                VerifiedReceiptSource::DeveloperSnapshot
            }
            InstallSource::DeveloperSnapshot => {
                return Err(ResolverError::receipt_blocked("developer_mode_disabled"));
            }
            InstallSource::LegacyBundledV1 => {
                return Err(ResolverError::receipt_blocked(
                    "legacy_receipt_is_not_a_v2_trust_proof",
                ));
            }
        };

        let facts = self
            .trust
            .verify_current(&receipt)
            .map_err(|error| ResolverError::receipt_blocked(error.code()))?;
        validate_fact_binding(&receipt, &facts).map_err(ResolverError::receipt_blocked)?;

        let versions = VersionStore::new(self.paths.clone());
        let root = match source {
            VerifiedReceiptSource::ReceiptV2 => {
                require_exact_version(&versions, &receipt)?;
                version_root(&self.paths, &receipt)
            }
            VerifiedReceiptSource::DeveloperSnapshot => {
                developer_snapshot_root(&self.paths, &receipt)?
            }
        };
        let manifest = verify_package_tree(&root, &facts.metadata, &self.host, self.target)
            .map_err(ResolverError::receipt_blocked)?;
        if manifest != facts.manifest {
            return Err(ResolverError::receipt_blocked(
                "verified_manifest_facts_mismatch",
            ));
        }

        let bridge_executable = match manifest.runtime.kind {
            RuntimeKind::UiOnly => None,
            RuntimeKind::VerifiedNative => {
                let bridge_path = manifest.runtime.bridge_entry.as_ref().ok_or_else(|| {
                    ResolverError::receipt_blocked("verified_bridge_entry_missing")
                })?;
                let expected = facts
                    .metadata
                    .files
                    .iter()
                    .find(|file| file.path.as_str() == bridge_path.as_str())
                    .ok_or_else(|| {
                        ResolverError::receipt_blocked("verified_bridge_not_in_package")
                    })?;
                let (descriptor, _) = open_verified_package_file(&root, expected)
                    .map_err(ResolverError::receipt_blocked)?;
                Some(
                    VerifiedExecutable::from_descriptor(
                        descriptor,
                        root.clone(),
                        PathBuf::from(bridge_path.as_str()),
                    )
                    .map_err(ResolverError::receipt_blocked)?,
                )
            }
        };

        // Revocation, Developer authority and catalog state are live inputs.
        // Re-check after filesystem hashing and descriptor acquisition so a
        // change during resolution cannot yield a stale activation lease.
        let final_facts = self
            .trust
            .verify_current(&receipt)
            .map_err(|error| ResolverError::receipt_blocked(error.code()))?;
        if final_facts != facts {
            return Err(ResolverError::receipt_blocked(
                "receipt_trust_changed_during_resolution",
            ));
        }
        require_exact_receipt(receipts, &receipt)?;
        match source {
            VerifiedReceiptSource::ReceiptV2 => require_exact_version(&versions, &receipt)?,
            VerifiedReceiptSource::DeveloperSnapshot => {
                let final_manifest =
                    verify_package_tree(&root, &facts.metadata, &self.host, self.target)
                        .map_err(ResolverError::receipt_blocked)?;
                if final_manifest != manifest {
                    return Err(ResolverError::receipt_blocked(
                        "developer_snapshot_changed_during_resolution",
                    ));
                }
            }
        }

        let durable_changed = durable
            .receipts
            .get(receipt.plugin_id.as_str())
            .is_none_or(|previous| receipt.generation > previous.generation);
        if durable_changed {
            durable.receipts.insert(
                receipt.plugin_id.as_str().to_owned(),
                ReceiptHighWaterRecord {
                    generation: receipt.generation,
                    receipt_digest: current_receipt_digest,
                },
            );
            write_receipt_high_water(&self.paths, &durable)?;
        }
        if observed
            .get(receipt.plugin_id.as_str())
            .is_none_or(|previous| receipt.generation > previous.generation)
        {
            observed.insert(receipt.plugin_id.as_str().to_owned(), receipt.clone());
        }

        Ok(ResolvedPlugin::VerifiedReceipt(VerifiedReceiptPlugin {
            manifest,
            root,
            bridge_executable,
            source,
            package_digest: receipt.package_digest,
            generation: receipt.generation,
            grants: receipt.granted_permissions,
            package_files: facts.metadata.files,
            status: CompatibilityStatus {
                migration_available: false,
            },
        }))
    }

    fn resolve_legacy(
        &self,
        plugin_id: &PluginId,
        receipts: &ReceiptStore,
        policy: ResolutionPolicy,
    ) -> Result<ResolvedPlugin, ResolverError> {
        if plugin_id.as_str() != LEGACY_AGENT_VM_ID {
            return Err(ResolverError::new(
                "legacy_manifest_forbidden",
                "only canonical agent-vm may use the Increment A legacy bridge",
            ));
        }
        if !policy.legacy_agent_vm_enabled {
            return Err(ResolverError::new(
                "legacy_activation_disabled",
                "plugins.agent-vm.enabled is false",
            ));
        }

        let package = load_exact_legacy_agent_vm(&self.paths)?;
        match receipts.current(plugin_id) {
            Ok(None) => {}
            Ok(Some(_)) => {
                return Err(ResolverError::receipt_blocked(
                    "receipt_appeared_during_legacy_resolution",
                ));
            }
            Err(error) => return Err(ResolverError::receipt_blocked(error.code())),
        }
        Ok(ResolvedPlugin::LegacyAgentVm(LegacyAgentVmPlugin {
            package,
            status: CompatibilityStatus {
                migration_available: true,
            },
        }))
    }
}

fn receipt_high_water_path(paths: &PluginPaths) -> PathBuf {
    paths.plugins_root().join(RECEIPT_HIGH_WATER_FILE)
}

fn sync_receipt_high_water_directory(paths: &PluginPaths) -> Result<(), ResolverError> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(paths.plugins_root())
        .and_then(|directory| directory.sync_all())
        .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_store"))
}

fn validate_receipt_high_water_root(paths: &PluginPaths) -> Result<(), ResolverError> {
    let metadata = fs::symlink_metadata(paths.plugins_root())
        .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_unsafe"))?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != effective_uid()
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(ResolverError::receipt_blocked(
            "receipt_generation_state_unsafe",
        ));
    }
    Ok(())
}

fn acquire_receipt_high_water_lock(
    paths: &PluginPaths,
) -> Result<ReceiptHighWaterLock, ResolverError> {
    validate_receipt_high_water_root(paths)?;
    let path = paths.plugins_root().join(RECEIPT_HIGH_WATER_LOCK);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_unsafe"))?;
    let metadata = file
        .metadata()
        .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_unsafe"))?;
    if !metadata.is_file()
        || metadata.uid() != effective_uid()
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(ResolverError::receipt_blocked(
            "receipt_generation_state_unsafe",
        ));
    }
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(ResolverError::receipt_blocked(
                "receipt_generation_state_unavailable",
            ));
        }
    }
    let entry = fs::symlink_metadata(&path)
        .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_unsafe"))?;
    if entry.file_type().is_symlink()
        || entry.dev() != metadata.dev()
        || entry.ino() != metadata.ino()
        || entry.uid() != effective_uid()
        || entry.nlink() != 1
        || entry.permissions().mode() & 0o777 != 0o600
    {
        return Err(ResolverError::receipt_blocked(
            "receipt_generation_state_unsafe",
        ));
    }
    Ok(ReceiptHighWaterLock(file))
}

fn read_receipt_high_water(paths: &PluginPaths) -> Result<ReceiptHighWaterState, ResolverError> {
    validate_receipt_high_water_root(paths)?;
    let path = receipt_high_water_path(paths);
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReceiptHighWaterState::default())
        }
        Err(_) => {
            return Err(ResolverError::receipt_blocked(
                "receipt_generation_state_unsafe",
            ))
        }
    };
    let before = file
        .metadata()
        .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_unsafe"))?;
    let entry = fs::symlink_metadata(&path)
        .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_unsafe"))?;
    if !before.is_file()
        || entry.file_type().is_symlink()
        || before.dev() != entry.dev()
        || before.ino() != entry.ino()
        || before.uid() != effective_uid()
        || before.nlink() != 1
        || before.permissions().mode() & 0o777 != 0o600
        || before.len() > MAX_RECEIPT_HIGH_WATER_BYTES
    {
        return Err(ResolverError::receipt_blocked(
            "receipt_generation_state_unsafe",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_RECEIPT_HIGH_WATER_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_unsafe"))?;
    let after = file
        .metadata()
        .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_unsafe"))?;
    let final_entry = fs::symlink_metadata(&path)
        .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_changed"))?;
    if bytes.len() as u64 > MAX_RECEIPT_HIGH_WATER_BYTES
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || after.nlink() != 1
        || after.uid() != effective_uid()
        || after.permissions().mode() & 0o777 != 0o600
        || final_entry.file_type().is_symlink()
        || final_entry.dev() != after.dev()
        || final_entry.ino() != after.ino()
    {
        return Err(ResolverError::receipt_blocked(
            "receipt_generation_state_changed",
        ));
    }
    let state: ReceiptHighWaterState = serde_json::from_slice(&bytes)
        .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_invalid"))?;
    let canonical = serde_json_canonicalizer::to_vec(&state)
        .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_invalid"))?;
    if state.schema_version != RECEIPT_HIGH_WATER_SCHEMA
        || state.receipts.len() > MAX_RECEIPT_HIGH_WATER_RECORDS
        || canonical != bytes
        || state.receipts.iter().any(|(plugin_id, record)| {
            PluginId::new(plugin_id.clone()).is_err() || record.generation == 0
        })
    {
        return Err(ResolverError::receipt_blocked(
            "receipt_generation_state_invalid",
        ));
    }
    Ok(state)
}

fn receipt_digest(receipt: &InstallReceipt) -> Result<Digest, ResolverError> {
    let bytes = serde_json_canonicalizer::to_vec(receipt)
        .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_invalid"))?;
    digest_from_hash(&Sha256::digest(bytes))
        .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_invalid"))
}

fn check_receipt_high_water(
    receipt: &InstallReceipt,
    digest: &Digest,
    previous: Option<&ReceiptHighWaterRecord>,
) -> Result<(), ResolverError> {
    let Some(previous) = previous else {
        return Ok(());
    };
    if receipt.generation < previous.generation {
        return Err(ResolverError::receipt_blocked("receipt_generation_replay"));
    }
    if receipt.generation == previous.generation && digest != &previous.receipt_digest {
        return Err(ResolverError::receipt_blocked(
            "receipt_generation_conflict",
        ));
    }
    Ok(())
}

fn write_receipt_high_water(
    paths: &PluginPaths,
    state: &ReceiptHighWaterState,
) -> Result<(), ResolverError> {
    if state.schema_version != RECEIPT_HIGH_WATER_SCHEMA
        || state.receipts.len() > MAX_RECEIPT_HIGH_WATER_RECORDS
        || state.receipts.iter().any(|(plugin_id, record)| {
            PluginId::new(plugin_id.clone()).is_err() || record.generation == 0
        })
    {
        return Err(ResolverError::receipt_blocked(
            "receipt_generation_state_invalid",
        ));
    }
    let existing = read_receipt_high_water(paths)?;
    for (plugin_id, previous) in &existing.receipts {
        let Some(next) = state.receipts.get(plugin_id) else {
            return Err(ResolverError::receipt_blocked(
                "receipt_generation_state_replayed",
            ));
        };
        if next.generation < previous.generation
            || (next.generation == previous.generation
                && next.receipt_digest != previous.receipt_digest)
        {
            return Err(ResolverError::receipt_blocked(
                "receipt_generation_state_replayed",
            ));
        }
    }
    if existing == *state {
        sync_receipt_high_water_directory(paths)?;
        return Ok(());
    }
    let bytes = serde_json_canonicalizer::to_vec(state)
        .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_store"))?;
    if bytes.len() as u64 > MAX_RECEIPT_HIGH_WATER_BYTES {
        return Err(ResolverError::receipt_blocked(
            "receipt_generation_state_store",
        ));
    }
    let temporary_path = paths.plugins_root().join(format!(
        ".receipt-high-water-{}.tmp",
        random_storage_id()
            .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_store"))?
    ));
    let final_path = receipt_high_water_path(paths);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary_path)
            .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_store"))?;
        let metadata = file
            .metadata()
            .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_store"))?;
        if !metadata.is_file()
            || metadata.uid() != effective_uid()
            || metadata.nlink() != 1
            || metadata.permissions().mode() & 0o777 != 0o600
        {
            return Err(ResolverError::receipt_blocked(
                "receipt_generation_state_store",
            ));
        }
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_store"))?;
        fs::rename(&temporary_path, &final_path)
            .map_err(|_| ResolverError::receipt_blocked("receipt_generation_state_store"))?;
        sync_receipt_high_water_directory(paths)?;
        if read_receipt_high_water(paths)? != *state {
            return Err(ResolverError::receipt_blocked(
                "receipt_generation_state_changed",
            ));
        }
        Ok(())
    })();
    if temporary_path.exists() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

impl PluginActivationResolver for PluginResolver {
    fn candidate_ids(&self) -> Vec<PluginId> {
        Self::candidate_ids(self)
    }

    fn resolve(
        &self,
        plugin_id: &PluginId,
        policy: ResolutionPolicy,
    ) -> Result<ResolvedPlugin, ResolverError> {
        Self::resolve(self, plugin_id, policy)
    }
}

fn validate_fact_binding(
    receipt: &InstallReceipt,
    facts: &ReceiptTrustFacts,
) -> Result<(), &'static str> {
    let metadata = &facts.metadata;
    let manifest = &facts.manifest;
    if facts.archive_digest != receipt.package_digest
        || facts.publisher_key_id != receipt.publisher_key_id
        || facts.publisher_lineage != receipt.publisher_lineage
        || metadata.schema_version != PACKAGE_SCHEMA_VERSION
        || metadata.plugin_id != receipt.plugin_id
        || metadata.version != receipt.version
        || metadata.target != receipt.target
        || metadata.state.schema_version != receipt.state_schema_version
        || metadata.state.rollback_compatible_through != receipt.rollback_compatible_through
        || manifest.id != receipt.plugin_id
        || manifest.version != receipt.version
        || manifest.publisher != metadata.publisher
        || manifest.compatibility.plugin_api != metadata.plugin_api
        || manifest.state != metadata.state
    {
        return Err("receipt_trust_facts_mismatch");
    }

    let Some(plugin_json) = metadata.files.first() else {
        return Err("receipt_package_files_missing");
    };
    if plugin_json.path.as_str() != "plugin.json" || plugin_json.digest != metadata.manifest_digest
    {
        return Err("receipt_manifest_digest_mismatch");
    }
    let mut paths = BTreeSet::new();
    if metadata
        .files
        .iter()
        .any(|file| !paths.insert(file.path.as_str()))
    {
        return Err("receipt_package_files_duplicate");
    }

    let mut grants = BTreeSet::new();
    for grant in &receipt.granted_permissions {
        if !grants.insert(grant.id)
            || !manifest.permissions.iter().any(|requested| {
                requested.id == grant.id
                    && requested.scope == grant.scope
                    && requested.modes == grant.modes
            })
        {
            return Err("receipt_grant_not_requested");
        }
    }

    match manifest.runtime.kind {
        RuntimeKind::VerifiedNative
            if receipt.native_trust_digest.as_ref() != Some(&receipt.package_digest) =>
        {
            Err("native_exact_digest_consent_missing")
        }
        RuntimeKind::UiOnly if receipt.native_trust_digest.is_some() => {
            Err("ui_receipt_has_native_consent")
        }
        RuntimeKind::UiOnly | RuntimeKind::VerifiedNative => Ok(()),
    }
}

fn require_exact_receipt(
    receipts: &ReceiptStore,
    receipt: &InstallReceipt,
) -> Result<(), ResolverError> {
    match receipts.observe(receipt) {
        Ok(ReceiptVisibility::Exact {
            plugin_id,
            generation,
            package_digest,
        }) if plugin_id == receipt.plugin_id
            && generation == receipt.generation
            && package_digest == receipt.package_digest =>
        {
            Ok(())
        }
        Ok(ReceiptVisibility::Absent | ReceiptVisibility::Different { .. }) => Err(
            ResolverError::receipt_blocked("receipt_changed_during_resolution"),
        ),
        Ok(ReceiptVisibility::Exact { .. }) => {
            Err(ResolverError::receipt_blocked("receipt_identity_mismatch"))
        }
        Err(error) => Err(ResolverError::receipt_blocked(error.code())),
    }
}

fn require_exact_version(
    versions: &VersionStore,
    receipt: &InstallReceipt,
) -> Result<(), ResolverError> {
    match versions.observe(
        &receipt.plugin_id,
        &receipt.version,
        &receipt.package_digest,
    ) {
        Ok(VersionVisibility::Exact {
            plugin_id,
            version,
            package_digest,
        }) if plugin_id == receipt.plugin_id
            && version == receipt.version
            && package_digest == receipt.package_digest =>
        {
            Ok(())
        }
        Ok(VersionVisibility::Absent) => Err(ResolverError::receipt_blocked(
            "receipt_version_directory_missing",
        )),
        Ok(VersionVisibility::Conflict { .. }) => Err(ResolverError::receipt_blocked(
            "receipt_version_digest_conflict",
        )),
        Ok(VersionVisibility::Exact { .. }) => Err(ResolverError::receipt_blocked(
            "receipt_version_identity_mismatch",
        )),
        Err(error) => Err(ResolverError::receipt_blocked(error.code())),
    }
}

fn version_root(paths: &PluginPaths, receipt: &InstallReceipt) -> PathBuf {
    paths
        .versions(&receipt.plugin_id)
        .join(receipt.version.to_string())
        .join(receipt.package_digest.as_str())
}

fn developer_snapshot_root(
    paths: &PluginPaths,
    receipt: &InstallReceipt,
) -> Result<PathBuf, ResolverError> {
    let digest = receipt
        .package_digest
        .as_str()
        .strip_prefix("sha256:")
        .filter(|value| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
        .ok_or_else(|| ResolverError::receipt_blocked("developer_snapshot_digest_invalid"))?;
    Ok(paths
        .cache(&receipt.plugin_id)
        .join("developer")
        .join(digest))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    size: u64,
    mode: u32,
    owner: u32,
}

impl FileIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            mode: metadata.permissions().mode() & 0o777,
            owner: metadata.uid(),
        }
    }
}

fn verify_package_tree(
    root: &Path,
    metadata: &PackageMetadataV1,
    host: &HostCompatibility,
    target: PackageTarget,
) -> Result<ManifestV2, String> {
    let before = directory_identity(root)?;
    let mut actual_files = BTreeSet::new();
    let mut actual_directories = BTreeSet::new();
    collect_tree(root, root, &mut actual_files, &mut actual_directories)?;
    if directory_identity(root)? != before {
        return Err("receipt_version_root_changed".into());
    }

    let expected_files = metadata
        .files
        .iter()
        .map(|file| file.path.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    let mut expected_directories = BTreeSet::new();
    for file in &metadata.files {
        let mut parent = Path::new(file.path.as_str()).parent();
        while let Some(directory) = parent {
            if directory.as_os_str().is_empty() {
                break;
            }
            expected_directories.insert(
                directory
                    .to_str()
                    .ok_or_else(|| "receipt_package_path_encoding".to_owned())?
                    .to_owned(),
            );
            parent = directory.parent();
        }
    }
    if actual_files != expected_files || actual_directories != expected_directories {
        return Err("receipt_package_tree_mismatch".into());
    }

    let mut manifest_bytes = None;
    for expected in &metadata.files {
        let captured = verify_package_file(root, expected)?;
        if expected.path.as_str() == "plugin.json" {
            manifest_bytes = Some(captured);
        }
    }
    let manifest_bytes = manifest_bytes.ok_or_else(|| "receipt_manifest_missing".to_owned())?;
    let manifest_target = match target {
        PackageTarget::DarwinArm64 => ManifestTarget::darwin_arm64(),
        PackageTarget::DarwinAmd64 => ManifestTarget::darwin_amd64(),
    };
    validate_packaged_manifest(&manifest_bytes, &manifest_target, host)
        .map_err(|error| error.code().to_owned())
}

fn directory_identity(path: &Path) -> Result<FileIdentity, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "receipt_package_directory_missing".to_owned())?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != effective_uid()
        || metadata.permissions().mode() & 0o777 != 0o555
    {
        return Err("receipt_package_directory_unsafe".into());
    }
    Ok(FileIdentity::from_metadata(&metadata))
}

fn collect_tree(
    root: &Path,
    directory: &Path,
    files: &mut BTreeSet<String>,
    directories: &mut BTreeSet<String>,
) -> Result<(), String> {
    let before = directory_identity(directory)?;
    let entries =
        fs::read_dir(directory).map_err(|_| "receipt_package_directory_read".to_owned())?;
    for entry in entries {
        let entry = entry.map_err(|_| "receipt_package_directory_read".to_owned())?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "receipt_package_path_escape".to_owned())?;
        let relative = relative
            .to_str()
            .ok_or_else(|| "receipt_package_path_encoding".to_owned())?
            .to_owned();
        let file_type = entry
            .file_type()
            .map_err(|_| "receipt_package_entry_type".to_owned())?;
        if file_type.is_symlink() {
            return Err("receipt_package_symlink".into());
        }
        if file_type.is_dir() {
            if !directories.insert(relative) {
                return Err("receipt_package_tree_duplicate".into());
            }
            collect_tree(root, &path, files, directories)?;
        } else if file_type.is_file() {
            if !files.insert(relative) {
                return Err("receipt_package_tree_duplicate".into());
            }
        } else {
            return Err("receipt_package_special_file".into());
        }
    }
    if directory_identity(directory)? != before {
        return Err("receipt_package_directory_changed".into());
    }
    Ok(())
}

fn verify_package_file(root: &Path, expected: &PackageFile) -> Result<Vec<u8>, String> {
    let (_, captured) = open_verified_package_file(root, expected)?;
    Ok(captured)
}

fn open_verified_package_file(
    root: &Path,
    expected: &PackageFile,
) -> Result<(fs::File, Vec<u8>), String> {
    let path = root.join(expected.path.as_str());
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|_| "receipt_package_file_open".to_owned())?;
    let before = input
        .metadata()
        .map_err(|_| "receipt_package_file_metadata".to_owned())?;
    let expected_mode = expected.mode.as_octal();
    if !before.is_file()
        || before.uid() != effective_uid()
        || before.nlink() != 1
        || before.len() != expected.size
        || before.permissions().mode() & 0o777 != expected_mode
    {
        return Err("receipt_package_file_unsafe".into());
    }
    let identity = FileIdentity::from_metadata(&before);
    let capture_manifest = expected.path.as_str() == "plugin.json";
    let mut captured = Vec::new();
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|_| "receipt_package_file_read".to_owned())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        if capture_manifest {
            captured.extend_from_slice(&buffer[..read]);
        }
    }
    let after = input
        .metadata()
        .map_err(|_| "receipt_package_file_metadata".to_owned())?;
    if FileIdentity::from_metadata(&after) != identity {
        return Err("receipt_package_file_changed".into());
    }
    let hash = hasher.finalize();
    let digest = digest_from_hash(&hash)?;
    if digest != expected.digest {
        return Err("receipt_package_file_digest_mismatch".into());
    }
    Ok((input, captured))
}

fn digest_from_hash(bytes: &[u8]) -> Result<Digest, String> {
    let mut value = String::from("sha256:");
    for byte in bytes {
        write!(&mut value, "{byte:02x}")
            .map_err(|_| "receipt_package_digest_encoding".to_owned())?;
    }
    Digest::new(value).map_err(|_| "receipt_package_digest_encoding".to_owned())
}

fn load_exact_legacy_agent_vm(paths: &PluginPaths) -> Result<PluginPackage, ResolverError> {
    let plugin_id = PluginId::new(LEGACY_AGENT_VM_ID).expect("canonical Agent VM ID is valid");
    let root = paths.plugin(&plugin_id);
    match fs::symlink_metadata(&root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ResolverError::new(
                "legacy_activation_unavailable",
                "bundled Agent VM layout is absent",
            ));
        }
        Err(_) => {
            return Err(ResolverError::new(
                "legacy_activation_blocked",
                "bundled Agent VM root cannot be inspected",
            ));
        }
        Ok(_) => {}
    }
    verify_legacy_directory(&root)?;
    verify_legacy_directory(&root.join("bin"))?;
    let manifest_path = root.join("manifest.json");
    let manifest_bytes = read_legacy_file(&manifest_path, 0o600)?;
    if manifest_bytes != LEGACY_MANIFEST {
        return Err(ResolverError::new(
            "legacy_activation_blocked",
            "bundled Agent VM manifest differs from the exact v1 bridge",
        ));
    }
    let executable_path = root.join("bin/agent-vm-plugin");
    let _ = read_legacy_file(&executable_path, 0o700)?;

    let package = manifest::load_package(&manifest_path)
        .map_err(|error| ResolverError::new("legacy_activation_blocked", error.message))?;
    let canonical_root = root.canonicalize().map_err(|_| {
        ResolverError::new(
            "legacy_activation_blocked",
            "bundled Agent VM root cannot be canonicalized",
        )
    })?;
    let canonical_executable = executable_path.canonicalize().map_err(|_| {
        ResolverError::new(
            "legacy_activation_blocked",
            "bundled Agent VM executable cannot be canonicalized",
        )
    })?;
    if package.root != canonical_root
        || package.executable != canonical_executable
        || package.manifest.id != LEGACY_AGENT_VM_ID
        || package.manifest.protocol_version != 1
        || package.manifest.entry.kind != "binary"
        || package.manifest.entry.path != "bin/agent-vm-plugin"
        || !package.manifest.entry.args.is_empty()
    {
        return Err(ResolverError::new(
            "legacy_activation_blocked",
            "bundled Agent VM layout is not the exact v1 bridge",
        ));
    }
    Ok(package)
}

fn verify_legacy_directory(path: &Path) -> Result<(), ResolverError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        ResolverError::new(
            "legacy_activation_blocked",
            format!("legacy directory {} is missing", path.display()),
        )
    })?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != effective_uid()
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(ResolverError::new(
            "legacy_activation_blocked",
            format!("legacy directory {} is unsafe", path.display()),
        ));
    }
    Ok(())
}

fn read_legacy_file(path: &Path, mode: u32) -> Result<Vec<u8>, ResolverError> {
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| {
            ResolverError::new(
                "legacy_activation_blocked",
                format!("legacy file {} cannot be opened", path.display()),
            )
        })?;
    let before = input.metadata().map_err(|_| {
        ResolverError::new(
            "legacy_activation_blocked",
            format!("legacy file {} cannot be inspected", path.display()),
        )
    })?;
    let identity = FileIdentity::from_metadata(&before);
    if !before.is_file()
        || before.uid() != effective_uid()
        || before.nlink() != 1
        || before.permissions().mode() & 0o777 != mode
    {
        return Err(ResolverError::new(
            "legacy_activation_blocked",
            format!("legacy file {} is unsafe", path.display()),
        ));
    }
    let mut bytes = Vec::new();
    input.read_to_end(&mut bytes).map_err(|_| {
        ResolverError::new(
            "legacy_activation_blocked",
            format!("legacy file {} cannot be read", path.display()),
        )
    })?;
    let after = input.metadata().map_err(|_| {
        ResolverError::new(
            "legacy_activation_blocked",
            format!("legacy file {} cannot be re-inspected", path.display()),
        )
    })?;
    if FileIdentity::from_metadata(&after) != identity {
        return Err(ResolverError::new(
            "legacy_activation_blocked",
            format!("legacy file {} changed while resolving", path.display()),
        ));
    }
    Ok(bytes)
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and does not dereference memory.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    use jarvis_plugin_protocol::manifest::{Digest, ManifestV2, PluginId, VersionRange};
    use jarvis_plugin_protocol::package::{
        MacOsVersion, PackageFile, PackageFileKind, PackageFileMode, PackageMetadataV1,
        PackagePath, PackageTarget,
    };
    use jarvis_plugin_protocol::receipt::{
        InstallReceipt, InstallSource, INSTALL_RECEIPT_SCHEMA_VERSION,
    };
    use semver::Version;

    use super::{
        developer_snapshot_root, ActivationSource, CurrentReceiptTrust, PluginResolver,
        ReceiptTrustError, ReceiptTrustFacts, ResolutionPolicy, ResolvedPlugin, LEGACY_AGENT_VM_ID,
        RECEIPT_HIGH_WATER_FILE,
    };
    use crate::plugins::manifest_v2::HostCompatibility;
    use crate::plugins::package_manager::paths::PluginPaths;
    use crate::plugins::package_manager::receipt::{ReceiptStore, VersionStore};

    const VALID_UI: &[u8] =
        include_bytes!("../../tests/fixtures/plugin-packages/valid-ui/plugin.json");
    const LEGACY_MANIFEST: &[u8] = include_bytes!("../../../plugins/agent-vm/manifest.json");
    const PAGE: &[u8] = b"<!doctype html><title>Hello</title>";
    static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

    fn digest(fill: char) -> Digest {
        Digest::new(format!("sha256:{}", fill.to_string().repeat(64))).unwrap()
    }

    fn file_digest(bytes: &[u8]) -> Digest {
        use sha2::{Digest as _, Sha256};

        let value = Sha256::digest(bytes);
        let mut encoded = String::from("sha256:");
        for byte in value {
            use std::fmt::Write as _;
            write!(&mut encoded, "{byte:02x}").unwrap();
        }
        Digest::new(encoded).unwrap()
    }

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new(label: &str) -> Self {
            let root = fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!(
                    "jarvis-resolver-{label}-{}-{}",
                    std::process::id(),
                    NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
                ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("profile")).unwrap();
            fs::set_permissions(root.join("profile"), fs::Permissions::from_mode(0o700)).unwrap();
            Self(root)
        }

        fn profile(&self) -> PathBuf {
            self.0.join("profile")
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            fn make_writable(path: &Path) {
                let Ok(metadata) = fs::symlink_metadata(path) else {
                    return;
                };
                if metadata.is_dir() {
                    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
                    if let Ok(entries) = fs::read_dir(path) {
                        for entry in entries.flatten() {
                            make_writable(&entry.path());
                        }
                    }
                } else {
                    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
                }
            }

            make_writable(&self.0);
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct FakeTrust {
        facts: Mutex<BTreeMap<String, ReceiptTrustFacts>>,
        revoked: AtomicBool,
    }

    impl FakeTrust {
        fn insert(&self, facts: ReceiptTrustFacts) {
            self.facts
                .lock()
                .unwrap()
                .insert(facts.archive_digest.as_str().to_owned(), facts);
        }
    }

    impl CurrentReceiptTrust for FakeTrust {
        fn verify_current(
            &self,
            receipt: &InstallReceipt,
        ) -> Result<ReceiptTrustFacts, ReceiptTrustError> {
            if self.revoked.load(Ordering::SeqCst) {
                return Err(ReceiptTrustError::new("package_revoked"));
            }
            self.facts
                .lock()
                .unwrap()
                .get(receipt.package_digest.as_str())
                .cloned()
                .ok_or_else(|| ReceiptTrustError::new("package_catalog_mismatch"))
        }
    }

    struct Fixture {
        _root: TestRoot,
        paths: PluginPaths,
        trust: Arc<FakeTrust>,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let root = TestRoot::new(label);
            Self {
                paths: PluginPaths::new(root.profile()),
                trust: Arc::new(FakeTrust::default()),
                _root: root,
            }
        }

        fn resolver(&self) -> PluginResolver {
            PluginResolver::new(
                self.paths.clone(),
                HostCompatibility::parse("0.4.0", 2).unwrap(),
                PackageTarget::DarwinArm64,
                self.trust.clone(),
            )
        }

        fn facts(&self, package_digest: Digest) -> ReceiptTrustFacts {
            let manifest = ManifestV2::parse(VALID_UI).unwrap();
            let files = vec![
                PackageFile {
                    path: PackagePath::new("plugin.json").unwrap(),
                    kind: PackageFileKind::Regular,
                    mode: PackageFileMode::ReadOnly,
                    size: VALID_UI.len() as u64,
                    digest: file_digest(VALID_UI),
                },
                PackageFile {
                    path: PackagePath::new("ui/pages/home/index.html").unwrap(),
                    kind: PackageFileKind::Regular,
                    mode: PackageFileMode::ReadOnly,
                    size: PAGE.len() as u64,
                    digest: file_digest(PAGE),
                },
            ];
            ReceiptTrustFacts {
                archive_digest: package_digest,
                metadata: PackageMetadataV1 {
                    schema_version: 1,
                    plugin_id: manifest.id.clone(),
                    publisher: manifest.publisher.clone(),
                    version: manifest.version.clone(),
                    manifest_digest: file_digest(VALID_UI),
                    target: PackageTarget::DarwinArm64,
                    minimum_macos: MacOsVersion::parse("14.0.0").unwrap(),
                    jarvis_range: VersionRange::parse(">=0.4.0, <0.5.0").unwrap(),
                    plugin_api: 2,
                    state: manifest.state.clone(),
                    files,
                    payload_root: digest('f'),
                },
                manifest,
                publisher_key_id: "example.release:1".into(),
                publisher_lineage: "example.release".into(),
            }
        }

        fn receipt(
            &self,
            facts: &ReceiptTrustFacts,
            source: InstallSource,
            generation: u64,
            previous: Option<jarvis_plugin_protocol::receipt::ReceiptSummary>,
        ) -> InstallReceipt {
            InstallReceipt {
                schema_version: INSTALL_RECEIPT_SCHEMA_VERSION,
                plugin_id: facts.metadata.plugin_id.clone(),
                version: facts.metadata.version.clone(),
                package_digest: facts.archive_digest.clone(),
                publisher_key_id: facts.publisher_key_id.clone(),
                publisher_lineage: facts.publisher_lineage.clone(),
                target: facts.metadata.target,
                source,
                enabled: true,
                granted_permissions: Vec::new(),
                native_trust_digest: None,
                installed_at_ms: generation as i64,
                generation,
                state_schema_version: facts.metadata.state.schema_version,
                rollback_compatible_through: facts.metadata.state.rollback_compatible_through,
                previous,
            }
        }

        fn stage(&self, facts: &ReceiptTrustFacts) {
            let extracted = self
                .paths
                .quarantine_root()
                .join(format!("extract-{}", facts.archive_digest.as_str()));
            fs::create_dir_all(extracted.join("ui/pages/home")).unwrap();
            fs::write(extracted.join("plugin.json"), VALID_UI).unwrap();
            fs::write(extracted.join("ui/pages/home/index.html"), PAGE).unwrap();
            VersionStore::new(self.paths.clone())
                .finalize_extracted(
                    &extracted,
                    &facts.metadata.plugin_id,
                    &facts.metadata.version,
                    &facts.archive_digest,
                )
                .unwrap();
        }

        fn stage_developer(&self, facts: &ReceiptTrustFacts) {
            let receipt = self.receipt(facts, InstallSource::DeveloperSnapshot, 1, None);
            let snapshot = developer_snapshot_root(&self.paths, &receipt).unwrap();
            fs::create_dir_all(snapshot.join("ui/pages/home")).unwrap();
            fs::write(snapshot.join("plugin.json"), VALID_UI).unwrap();
            fs::write(snapshot.join("ui/pages/home/index.html"), PAGE).unwrap();
            for file in [
                snapshot.join("plugin.json"),
                snapshot.join("ui/pages/home/index.html"),
            ] {
                fs::set_permissions(file, fs::Permissions::from_mode(0o444)).unwrap();
            }
            for directory in [
                snapshot.join("ui/pages/home"),
                snapshot.join("ui/pages"),
                snapshot.join("ui"),
                snapshot,
            ] {
                fs::set_permissions(directory, fs::Permissions::from_mode(0o555)).unwrap();
            }
        }

        fn install(
            &self,
            fill: char,
            source: InstallSource,
            generation: u64,
            previous: Option<jarvis_plugin_protocol::receipt::ReceiptSummary>,
        ) -> InstallReceipt {
            let facts = self.facts(digest(fill));
            if source == InstallSource::DeveloperSnapshot {
                self.stage_developer(&facts);
            } else {
                self.stage(&facts);
            }
            self.trust.insert(facts.clone());
            let receipt = self.receipt(&facts, source, generation, previous);
            ReceiptStore::new(self.paths.clone())
                .commit(&receipt)
                .unwrap();
            receipt
        }

        fn write_legacy_agent_vm(&self) {
            let root = self
                .paths
                .plugin(&PluginId::new(LEGACY_AGENT_VM_ID).unwrap());
            let bin = root.join("bin");
            fs::create_dir_all(&bin).unwrap();
            for directory in [&root, &bin] {
                fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
            }
            fs::write(root.join("manifest.json"), LEGACY_MANIFEST).unwrap();
            fs::set_permissions(
                root.join("manifest.json"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            fs::write(root.join("bin/agent-vm-plugin"), b"legacy executable").unwrap();
            fs::set_permissions(
                root.join("bin/agent-vm-plugin"),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
    }

    fn production() -> ResolutionPolicy {
        ResolutionPolicy::production(true)
    }

    #[test]
    fn exact_receipt_resolves_only_its_immutable_version_and_digest() {
        let fixture = Fixture::new("exact");
        let receipt = fixture.install('a', InstallSource::Catalog, 1, None);

        let resolved = fixture
            .resolver()
            .resolve(&receipt.plugin_id, production())
            .unwrap();

        assert_eq!(resolved.source(), ActivationSource::ReceiptV2);
        assert!(resolved.root().ends_with(
            Path::new(receipt.version.to_string().as_str()).join(receipt.package_digest.as_str())
        ));
        let ResolvedPlugin::VerifiedReceipt(resolved) = resolved else {
            panic!("v2 receipt must remain a typed v2 result");
        };
        assert_eq!(resolved.generation, 1);
        assert_eq!(resolved.package_files.len(), 2);
    }

    #[test]
    fn missing_version_digest_mismatch_and_revocation_all_block() {
        let missing = Fixture::new("missing");
        let missing_facts = missing.facts(digest('a'));
        missing.trust.insert(missing_facts.clone());
        let missing_receipt = missing.receipt(&missing_facts, InstallSource::Catalog, 1, None);
        ReceiptStore::new(missing.paths.clone())
            .commit(&missing_receipt)
            .unwrap();
        assert_eq!(
            missing
                .resolver()
                .resolve(&missing_receipt.plugin_id, production())
                .unwrap_err()
                .code(),
            "receipt_activation_blocked"
        );

        let mismatch = Fixture::new("mismatch");
        let mismatch_receipt = mismatch.install('b', InstallSource::Catalog, 1, None);
        let changed = mismatch
            .paths
            .versions(&mismatch_receipt.plugin_id)
            .join(mismatch_receipt.version.to_string())
            .join(mismatch_receipt.package_digest.as_str())
            .join("ui/pages/home/index.html");
        fs::set_permissions(&changed, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(&changed, b"changed").unwrap();
        fs::set_permissions(&changed, fs::Permissions::from_mode(0o444)).unwrap();
        assert_eq!(
            mismatch
                .resolver()
                .resolve(&mismatch_receipt.plugin_id, production())
                .unwrap_err()
                .code(),
            "receipt_activation_blocked"
        );

        let revoked = Fixture::new("revoked");
        let revoked_receipt = revoked.install('c', InstallSource::Catalog, 1, None);
        revoked.trust.revoked.store(true, Ordering::SeqCst);
        assert_eq!(
            revoked
                .resolver()
                .resolve(&revoked_receipt.plugin_id, production())
                .unwrap_err()
                .code(),
            "receipt_activation_blocked"
        );
    }

    #[test]
    fn developer_receipt_requires_developer_mode() {
        let fixture = Fixture::new("developer");
        let receipt = fixture.install('d', InstallSource::DeveloperSnapshot, 1, None);
        let resolver = fixture.resolver();

        assert_eq!(
            resolver
                .resolve(&receipt.plugin_id, production())
                .unwrap_err()
                .code(),
            "receipt_activation_blocked"
        );
        let resolved = resolver
            .resolve(
                &receipt.plugin_id,
                ResolutionPolicy {
                    developer_mode: true,
                    legacy_agent_vm_enabled: true,
                },
            )
            .unwrap();
        assert_eq!(resolved.source(), ActivationSource::DeveloperSnapshot);
        assert!(resolved
            .root()
            .starts_with(fixture.paths.cache(&receipt.plugin_id).join("developer")));
        assert!(!resolved
            .root()
            .starts_with(fixture.paths.versions(&receipt.plugin_id)));
    }

    #[test]
    fn fresh_profile_keeps_typed_legacy_agent_vm_bridge() {
        let fixture = Fixture::new("legacy");
        fixture.write_legacy_agent_vm();

        let resolved = fixture
            .resolver()
            .resolve(&PluginId::new(LEGACY_AGENT_VM_ID).unwrap(), production())
            .unwrap();

        assert_eq!(resolved.source(), ActivationSource::LegacyBundledV1);
        assert!(resolved.status().migration_available);
        assert!(matches!(resolved, ResolvedPlugin::LegacyAgentVm(_)));
    }

    #[test]
    fn valid_receipt_wins_without_deleting_legacy_files() {
        let fixture = Fixture::new("legacy-plus-receipt");
        fixture.write_legacy_agent_vm();
        let receipt = fixture.install('e', InstallSource::Catalog, 1, None);

        let resolved = fixture
            .resolver()
            .resolve(&receipt.plugin_id, production())
            .unwrap();

        assert_eq!(resolved.source(), ActivationSource::ReceiptV2);
        assert!(fixture
            .paths
            .plugin(&PluginId::new(LEGACY_AGENT_VM_ID).unwrap())
            .join("bin/agent-vm-plugin")
            .exists());
    }

    #[test]
    fn bad_receipt_never_downgrades_to_working_legacy_agent_vm() {
        let fixture = Fixture::new("legacy-blocked");
        fixture.write_legacy_agent_vm();
        let plugin_id = PluginId::new(LEGACY_AGENT_VM_ID).unwrap();
        fs::write(fixture.paths.current(&plugin_id), b"{broken receipt").unwrap();
        fs::set_permissions(
            fixture.paths.current(&plugin_id),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();

        let error = fixture
            .resolver()
            .resolve(&plugin_id, production())
            .unwrap_err();

        assert_eq!(error.code(), "receipt_activation_blocked");
    }

    #[test]
    fn schema_valid_legacy_receipt_is_not_a_v2_trust_proof() {
        let fixture = Fixture::new("legacy-receipt-forbidden");
        fixture.write_legacy_agent_vm();
        let plugin_id = PluginId::new(LEGACY_AGENT_VM_ID).unwrap();
        let receipt = InstallReceipt {
            schema_version: INSTALL_RECEIPT_SCHEMA_VERSION,
            plugin_id: plugin_id.clone(),
            version: Version::parse("0.1.0").unwrap(),
            package_digest: digest('a'),
            publisher_key_id: "jarvis.legacy:1".into(),
            publisher_lineage: "jarvis.legacy".into(),
            target: PackageTarget::DarwinArm64,
            source: InstallSource::LegacyBundledV1,
            enabled: true,
            granted_permissions: Vec::new(),
            native_trust_digest: None,
            installed_at_ms: 1,
            generation: 1,
            state_schema_version: 1,
            rollback_compatible_through: 1,
            previous: None,
        };
        ReceiptStore::new(fixture.paths.clone())
            .commit(&receipt)
            .unwrap();

        let error = fixture
            .resolver()
            .resolve(&plugin_id, production())
            .unwrap_err();

        assert_eq!(error.code(), "receipt_activation_blocked");
        assert_eq!(error.cause(), "legacy_receipt_is_not_a_v2_trust_proof");
    }

    #[test]
    fn arbitrary_v1_manifest_is_not_a_legacy_trust_escape() {
        let fixture = Fixture::new("arbitrary-v1");
        let error = fixture
            .resolver()
            .resolve(&PluginId::new("dev.example.evil").unwrap(), production())
            .unwrap_err();
        assert_eq!(error.code(), "legacy_manifest_forbidden");
    }

    #[test]
    fn explicit_false_legacy_setting_blocks_bridge() {
        let fixture = Fixture::new("legacy-disabled");
        fixture.write_legacy_agent_vm();
        let error = fixture
            .resolver()
            .resolve(
                &PluginId::new(LEGACY_AGENT_VM_ID).unwrap(),
                ResolutionPolicy::production(false),
            )
            .unwrap_err();
        assert_eq!(error.code(), "legacy_activation_disabled");
    }

    #[test]
    fn observed_receipt_generation_cannot_be_replayed() {
        let fixture = Fixture::new("generation-replay");
        let first = fixture.install('1', InstallSource::Catalog, 1, None);
        let resolver = fixture.resolver();
        resolver.resolve(&first.plugin_id, production()).unwrap();

        let second = InstallReceipt {
            generation: 2,
            installed_at_ms: 2,
            previous: Some(first.summary()),
            ..first.clone()
        };
        ReceiptStore::new(fixture.paths.clone())
            .commit(&second)
            .unwrap();
        resolver.resolve(&second.plugin_id, production()).unwrap();
        ReceiptStore::new(fixture.paths.clone())
            .commit(&first)
            .unwrap();

        assert_eq!(
            resolver
                .resolve(&first.plugin_id, production())
                .unwrap_err()
                .code(),
            "receipt_activation_blocked"
        );
    }

    #[test]
    fn receipt_generation_cannot_be_replayed_after_resolver_recreation() {
        let fixture = Fixture::new("generation-restart-replay");
        let first = fixture.install('5', InstallSource::Catalog, 1, None);
        let resolver = fixture.resolver();
        resolver.resolve(&first.plugin_id, production()).unwrap();

        let second = InstallReceipt {
            generation: 2,
            installed_at_ms: 2,
            previous: Some(first.summary()),
            ..first.clone()
        };
        ReceiptStore::new(fixture.paths.clone())
            .commit(&second)
            .unwrap();
        resolver.resolve(&second.plugin_id, production()).unwrap();
        drop(resolver);
        ReceiptStore::new(fixture.paths.clone())
            .commit(&first)
            .unwrap();

        let error = fixture
            .resolver()
            .resolve(&first.plugin_id, production())
            .unwrap_err();
        assert_eq!(error.code(), "receipt_activation_blocked");
        assert_eq!(error.cause(), "receipt_generation_replay");
    }

    #[test]
    fn rejected_newer_receipt_does_not_advance_durable_generation() {
        let fixture = Fixture::new("generation-rejected-newer");
        let first = fixture.install('6', InstallSource::Catalog, 1, None);
        fixture
            .resolver()
            .resolve(&first.plugin_id, production())
            .unwrap();
        let rejected = InstallReceipt {
            generation: 2,
            installed_at_ms: 2,
            enabled: false,
            previous: Some(first.summary()),
            ..first.clone()
        };
        ReceiptStore::new(fixture.paths.clone())
            .commit(&rejected)
            .unwrap();
        let error = fixture
            .resolver()
            .resolve(&rejected.plugin_id, production())
            .unwrap_err();
        assert_eq!(error.cause(), "receipt_disabled");

        ReceiptStore::new(fixture.paths.clone())
            .commit(&first)
            .unwrap();
        let resolved = fixture
            .resolver()
            .resolve(&first.plugin_id, production())
            .unwrap();
        let ResolvedPlugin::VerifiedReceipt(resolved) = resolved else {
            panic!("the last successfully activated generation must remain usable");
        };
        assert_eq!(resolved.generation, 1);
    }

    #[test]
    fn orphan_high_water_temporary_file_cannot_rollback_committed_state() {
        let fixture = Fixture::new("generation-crash-temp");
        let first = fixture.install('7', InstallSource::Catalog, 1, None);
        let resolver = fixture.resolver();
        resolver.resolve(&first.plugin_id, production()).unwrap();
        let second = InstallReceipt {
            generation: 2,
            installed_at_ms: 2,
            previous: Some(first.summary()),
            ..first.clone()
        };
        ReceiptStore::new(fixture.paths.clone())
            .commit(&second)
            .unwrap();
        resolver.resolve(&second.plugin_id, production()).unwrap();
        drop(resolver);

        let orphan = fixture
            .paths
            .plugins_root()
            .join(".receipt-high-water-00000000-0000-4000-8000-000000000000.tmp");
        fs::write(&orphan, b"interrupted stale write").unwrap();
        fs::set_permissions(&orphan, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(fixture
            .paths
            .plugins_root()
            .join(RECEIPT_HIGH_WATER_FILE)
            .exists());
        ReceiptStore::new(fixture.paths.clone())
            .commit(&first)
            .unwrap();

        let error = fixture
            .resolver()
            .resolve(&first.plugin_id, production())
            .unwrap_err();
        assert_eq!(error.cause(), "receipt_generation_replay");
    }

    #[test]
    fn profiles_keep_independent_generation_high_water_marks() {
        let first_profile = Fixture::new("profile-one");
        let first = first_profile.install('3', InstallSource::Catalog, 3, None);
        first_profile
            .resolver()
            .resolve(&first.plugin_id, production())
            .unwrap();

        let second_profile = Fixture::new("profile-two");
        let second = second_profile.install('4', InstallSource::Catalog, 1, None);
        let resolved = second_profile
            .resolver()
            .resolve(&second.plugin_id, production())
            .unwrap();
        let ResolvedPlugin::VerifiedReceipt(resolved) = resolved else {
            panic!("second profile must resolve independently");
        };
        assert_eq!(resolved.generation, 1);
    }
}
