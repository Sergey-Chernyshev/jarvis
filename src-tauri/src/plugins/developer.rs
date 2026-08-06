#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use jarvis_package::{
    extract_verified_package, inspect_and_verify_package, pack_plugin, PackOptions,
    PackageDocumentAdapter, PackageError, PackageSignatureSource,
};
use jarvis_plugin_protocol::manifest::{Digest, ManifestV2, RuntimeKind};
use jarvis_plugin_protocol::package::{
    MacOsVersion, PackageFile, PackageMetadataV1, PackageSignatureV1, PackageTarget,
    SignatureAlgorithm,
};
use jarvis_plugin_protocol::receipt::{
    GrantedPermission, InstallReceipt, InstallSource, ReceiptSummary,
    INSTALL_RECEIPT_SCHEMA_VERSION,
};
use sha2::{Digest as _, Sha256};

use crate::plugins::trust::package::DeveloperPackageVerifier;

const DEVELOPER_KEY_ID: &str = "jarvis.developer-unverified";
const DEVELOPER_LINEAGE_PREFIX: &str = "developer:";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeveloperPackageOptions {
    pub target: PackageTarget,
    pub minimum_macos: MacOsVersion,
}

#[derive(Debug)]
pub(crate) struct DeveloperError {
    code: &'static str,
    message: String,
}

impl DeveloperError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for DeveloperError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for DeveloperError {}

impl From<PackageError> for DeveloperError {
    fn from(error: PackageError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceIdentity {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl SourceIdentity {
    fn inspect(path: &Path) -> Result<Self, DeveloperError> {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            DeveloperError::new(
                "developer_source_invalid",
                format!("cannot inspect {}: {error}", path.display()),
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(DeveloperError::new(
                "developer_source_invalid",
                format!("{} is not a real directory", path.display()),
            ));
        }
        if metadata.uid() != effective_user_id() {
            return Err(DeveloperError::new(
                "developer_source_owner",
                format!("{} is not owned by the current user", path.display()),
            ));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            mode: metadata.mode(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }
}

#[derive(Clone, Debug)]
struct SourceDiagnostic {
    canonical_path: PathBuf,
    identity: SourceIdentity,
}

#[derive(Debug)]
struct ModeGate {
    enabled: bool,
    generation: u64,
    disabling: bool,
}

#[derive(Debug)]
struct DeveloperModeState {
    gate: Mutex<ModeGate>,
}

impl DeveloperModeState {
    fn new(enabled: bool) -> Self {
        Self {
            gate: Mutex::new(ModeGate {
                enabled,
                generation: 1,
                disabling: false,
            }),
        }
    }

    fn current_generation(&self) -> Result<u64, DeveloperError> {
        let gate = self.gate.lock().unwrap();
        if !gate.enabled || gate.disabling {
            return Err(DeveloperError::new(
                "developer_mode_disabled",
                "Developer Mode is disabled",
            ));
        }
        Ok(gate.generation)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeDigestConsent {
    digest: Digest,
}

impl NativeDigestConsent {
    pub(crate) fn new(digest: Digest) -> Self {
        Self { digest }
    }
}

#[derive(Debug)]
pub(crate) struct PreparedDeveloperLink {
    snapshot: PathBuf,
    source: SourceDiagnostic,
    package_digest: Digest,
    metadata: PackageMetadataV1,
    manifest: ManifestV2,
    mode_generation: u64,
}

impl PreparedDeveloperLink {
    pub(crate) fn package_digest(&self) -> &Digest {
        &self.package_digest
    }

    pub(crate) fn manifest(&self) -> &ManifestV2 {
        &self.manifest
    }

    pub(crate) fn snapshot(&self) -> &Path {
        &self.snapshot
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DeveloperLink {
    receipt: InstallReceipt,
    snapshot: PathBuf,
    source: SourceDiagnostic,
    metadata: PackageMetadataV1,
    runtime_kind: RuntimeKind,
    runtime_epoch: Option<String>,
    mode_generation: u64,
    admission: Arc<AtomicBool>,
}

impl DeveloperLink {
    pub(crate) fn receipt(&self) -> &InstallReceipt {
        &self.receipt
    }

    pub(crate) fn snapshot(&self) -> &Path {
        &self.snapshot
    }

    pub(crate) fn metadata(&self) -> &PackageMetadataV1 {
        &self.metadata
    }

    pub(crate) fn runtime_kind(&self) -> RuntimeKind {
        self.runtime_kind
    }

    pub(crate) fn diagnostic_source_path(&self) -> &Path {
        &self.source.canonical_path
    }

    pub(crate) fn source_device_inode(&self) -> (u64, u64) {
        (self.source.identity.device, self.source.identity.inode)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedDeveloperSnapshot {
    root: PathBuf,
    package_digest: Digest,
    generation: u64,
}

impl ResolvedDeveloperSnapshot {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn package_digest(&self) -> &Digest {
        &self.package_digest
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PermissionDiff {
    added: Vec<GrantedPermission>,
    removed: Vec<GrantedPermission>,
}

impl PermissionDiff {
    pub(crate) fn added(&self) -> &[GrantedPermission] {
        &self.added
    }

    pub(crate) fn removed(&self) -> &[GrantedPermission] {
        &self.removed
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeveloperReloadApproval {
    previous_digest: Digest,
    next_digest: Digest,
}

#[derive(Debug)]
pub(crate) struct DeveloperReloadPlan {
    prepared: PreparedDeveloperLink,
    previous_digest: Digest,
    permission_diff: PermissionDiff,
}

impl DeveloperReloadPlan {
    pub(crate) fn changed(&self) -> bool {
        self.previous_digest != self.prepared.package_digest
    }

    pub(crate) fn package_digest(&self) -> &Digest {
        &self.prepared.package_digest
    }

    pub(crate) fn permission_diff(&self) -> &PermissionDiff {
        &self.permission_diff
    }

    pub(crate) fn approval(&self) -> DeveloperReloadApproval {
        DeveloperReloadApproval {
            previous_digest: self.previous_digest.clone(),
            next_digest: self.prepared.package_digest.clone(),
        }
    }
}

pub(crate) trait DeveloperTeardownPort: Send + Sync {
    fn teardown_and_revoke(&self, link: &DeveloperLink) -> Result<(), String>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeveloperDisableReport {
    pub revoked_links: usize,
}

pub(crate) struct DeveloperLinker<A> {
    profile: PathBuf,
    adapter: A,
    options: DeveloperPackageOptions,
    mode: Arc<DeveloperModeState>,
    runtime_epoch: String,
}

impl<A> DeveloperLinker<A>
where
    A: PackageDocumentAdapter,
{
    pub(crate) fn new(
        profile: PathBuf,
        adapter: A,
        options: DeveloperPackageOptions,
        mode_enabled: bool,
    ) -> Result<Self, DeveloperError> {
        if !profile.is_absolute() {
            return Err(DeveloperError::new(
                "developer_profile_invalid",
                "plugin profile must be absolute",
            ));
        }
        Ok(Self {
            profile,
            adapter,
            options,
            mode: Arc::new(DeveloperModeState::new(mode_enabled)),
            runtime_epoch: random_identifier()?,
        })
    }

    pub(crate) fn mode_enabled(&self) -> bool {
        let gate = self.mode.gate.lock().unwrap();
        gate.enabled && !gate.disabling
    }

    pub(crate) fn enable_mode(&self) {
        let mut gate = self.mode.gate.lock().unwrap();
        if !gate.enabled {
            gate.generation = gate.generation.saturating_add(1);
            gate.enabled = true;
        }
        gate.disabling = false;
    }

    pub(crate) fn link(
        &self,
        source: &Path,
        consent: Option<NativeDigestConsent>,
    ) -> Result<DeveloperLink, DeveloperError> {
        let prepared = self.prepare_link(source)?;
        self.commit_link(prepared, consent)
    }

    pub(crate) fn prepare_link(
        &self,
        source: &Path,
    ) -> Result<PreparedDeveloperLink, DeveloperError> {
        let mode_generation = self.mode.current_generation()?;
        let source = inspect_source(source, &self.profile)?;
        let cache_root = prepare_cache_root(&self.profile)?;
        let mut archive = OwnerOnlyArchive::create(&cache_root)?;
        let expected_digest = pack_plugin(
            &source.canonical_path,
            PackOptions {
                target: self.options.target,
                minimum_macos: self.options.minimum_macos.clone(),
            },
            &self.adapter,
            &DeveloperSignatureSource,
            archive.file_mut(),
        )?;
        archive.file_mut().flush().map_err(|error| {
            DeveloperError::new(
                "developer_archive_write",
                format!("cannot flush developer package: {error}"),
            )
        })?;
        archive.file_mut().sync_all().map_err(|error| {
            DeveloperError::new(
                "developer_archive_write",
                format!("cannot sync developer package: {error}"),
            )
        })?;
        if SourceIdentity::inspect(&source.canonical_path)? != source.identity {
            return Err(DeveloperError::new(
                "source_raced",
                "developer source root changed while packaging",
            ));
        }

        let archive_file = archive.take_file()?;
        let evidence = inspect_and_verify_package(
            archive_file,
            &self.adapter,
            &DeveloperPackageVerifier::new(
                expected_digest.clone(),
                DEVELOPER_KEY_ID,
                STANDARD.encode([0_u8; 64]),
            ),
        )?;
        let facts = evidence.facts();
        let package_digest = facts.archive_digest().clone();
        let metadata = facts.metadata().clone();
        let manifest = facts.manifest().clone();
        if package_digest != expected_digest {
            return Err(DeveloperError::new(
                "developer_package_digest_mismatch",
                "packed and verified developer digests differ",
            ));
        }
        validate_developer_runtime_policy(&manifest)?;
        let snapshot =
            extract_developer_snapshot(&self.profile, &metadata, &package_digest, evidence)?;
        Ok(PreparedDeveloperLink {
            snapshot,
            source,
            package_digest,
            metadata,
            manifest,
            mode_generation,
        })
    }

    pub(crate) fn commit_link(
        &self,
        prepared: PreparedDeveloperLink,
        consent: Option<NativeDigestConsent>,
    ) -> Result<DeveloperLink, DeveloperError> {
        self.commit_prepared(prepared, 1, None, consent)
    }

    pub(crate) fn commit_replacement(
        &self,
        previous: &InstallReceipt,
        prepared: PreparedDeveloperLink,
        consent: Option<NativeDigestConsent>,
    ) -> Result<DeveloperLink, DeveloperError> {
        if previous.source != InstallSource::DeveloperSnapshot
            || previous.plugin_id != prepared.manifest.id
        {
            return Err(DeveloperError::new(
                "developer_reload_stale",
                "stored developer receipt does not match the prepared source",
            ));
        }
        let generation = previous.generation.checked_add(1).ok_or_else(|| {
            DeveloperError::new(
                "developer_generation_overflow",
                "developer receipt generation overflow",
            )
        })?;
        self.commit_prepared(prepared, generation, Some(previous.summary()), consent)
    }

    pub(crate) fn prepare_reload(
        &self,
        current: &DeveloperLink,
    ) -> Result<DeveloperReloadPlan, DeveloperError> {
        self.resolve(current)?;
        let prepared = self.prepare_link(&current.source.canonical_path)?;
        if prepared.manifest.id != current.receipt.plugin_id {
            return Err(DeveloperError::new(
                "developer_plugin_identity_changed",
                "developer source now declares another plugin id",
            ));
        }
        Ok(DeveloperReloadPlan {
            permission_diff: permission_diff(
                &current.receipt.granted_permissions,
                &prepared.manifest,
            ),
            previous_digest: current.receipt.package_digest.clone(),
            prepared,
        })
    }

    pub(crate) fn reload_without_approval(
        &self,
        current: &DeveloperLink,
    ) -> Result<DeveloperLink, DeveloperError> {
        let plan = self.prepare_reload(current)?;
        if plan.changed() {
            return Err(DeveloperError::new(
                "developer_source_changed",
                "developer source digest changed; explicit reload approval is required",
            ));
        }
        Ok(current.clone())
    }

    pub(crate) fn commit_reload(
        &self,
        current: &DeveloperLink,
        plan: DeveloperReloadPlan,
        approval: Option<DeveloperReloadApproval>,
        consent: Option<NativeDigestConsent>,
    ) -> Result<DeveloperLink, DeveloperError> {
        self.resolve(current)?;
        if plan.previous_digest != current.receipt.package_digest
            || plan.prepared.manifest.id != current.receipt.plugin_id
        {
            return Err(DeveloperError::new(
                "developer_reload_stale",
                "developer reload plan no longer matches the active generation",
            ));
        }
        if plan.changed()
            && approval.as_ref()
                != Some(&DeveloperReloadApproval {
                    previous_digest: current.receipt.package_digest.clone(),
                    next_digest: plan.prepared.package_digest.clone(),
                })
        {
            return Err(DeveloperError::new(
                "developer_source_changed",
                "developer source digest changed; exact reload approval is required",
            ));
        }
        let generation = current.receipt.generation.checked_add(1).ok_or_else(|| {
            DeveloperError::new(
                "developer_generation_overflow",
                "developer receipt generation overflow",
            )
        })?;
        let next = self.commit_prepared(
            plan.prepared,
            generation,
            Some(current.receipt.summary()),
            consent,
        )?;
        current.admission.store(false, Ordering::SeqCst);
        Ok(next)
    }

    pub(crate) fn resolve(
        &self,
        link: &DeveloperLink,
    ) -> Result<ResolvedDeveloperSnapshot, DeveloperError> {
        let mode_generation = self.mode.current_generation()?;
        if mode_generation != link.mode_generation || !link.admission.load(Ordering::SeqCst) {
            return Err(DeveloperError::new(
                "developer_generation_revoked",
                "developer activation generation is no longer admitted",
            ));
        }
        if !link.receipt.enabled || link.receipt.source != InstallSource::DeveloperSnapshot {
            return Err(DeveloperError::new(
                "developer_receipt_inactive",
                "developer receipt is not active",
            ));
        }
        validate_snapshot(&link.snapshot, &link.metadata, true).map_err(|error| {
            DeveloperError::new(
                "developer_snapshot_changed",
                format!("developer snapshot failed revalidation: {error}"),
            )
        })?;
        if link.runtime_kind == RuntimeKind::VerifiedNative {
            if link.receipt.native_trust_digest.as_ref() != Some(&link.receipt.package_digest) {
                return Err(DeveloperError::new(
                    "developer_native_consent_required",
                    "native consent does not match the developer package digest",
                ));
            }
            if link.runtime_epoch.as_deref() != Some(self.runtime_epoch.as_str()) {
                return Err(DeveloperError::new(
                    "developer_native_reconsent",
                    "native developer plugins require consent after every Jarvis restart",
                ));
            }
        }
        Ok(ResolvedDeveloperSnapshot {
            root: link.snapshot.clone(),
            package_digest: link.receipt.package_digest.clone(),
            generation: link.receipt.generation,
        })
    }

    pub(crate) fn reconsent_native(
        &self,
        current: &DeveloperLink,
        consent: NativeDigestConsent,
    ) -> Result<DeveloperLink, DeveloperError> {
        let mode_generation = self.mode.current_generation()?;
        if mode_generation != current.mode_generation || !current.admission.load(Ordering::SeqCst) {
            return Err(DeveloperError::new(
                "developer_generation_revoked",
                "developer activation generation is no longer admitted",
            ));
        }
        if current.runtime_kind != RuntimeKind::VerifiedNative {
            return Err(DeveloperError::new(
                "developer_native_consent_not_applicable",
                "UI-only developer plugins do not use native consent",
            ));
        }
        if consent.digest != current.receipt.package_digest {
            return Err(DeveloperError::new(
                "developer_native_consent_required",
                "exact native package digest consent is required",
            ));
        }
        validate_snapshot(&current.snapshot, &current.metadata, true)?;
        let generation = current.receipt.generation.checked_add(1).ok_or_else(|| {
            DeveloperError::new(
                "developer_generation_overflow",
                "developer receipt generation overflow",
            )
        })?;
        let mut receipt = current.receipt.clone();
        receipt.previous = Some(current.receipt.summary());
        receipt.generation = generation;
        receipt.native_trust_digest = Some(current.receipt.package_digest.clone());
        receipt.installed_at_ms = now_ms()?;
        receipt.validate().map_err(|error| {
            DeveloperError::new(error.code(), "developer receipt contract rejected")
        })?;
        let next = DeveloperLink {
            receipt,
            snapshot: current.snapshot.clone(),
            source: current.source.clone(),
            metadata: current.metadata.clone(),
            runtime_kind: current.runtime_kind,
            runtime_epoch: Some(self.runtime_epoch.clone()),
            mode_generation,
            admission: Arc::new(AtomicBool::new(true)),
        };
        current.admission.store(false, Ordering::SeqCst);
        Ok(next)
    }

    pub(crate) fn disable_mode(
        &self,
        links: &[DeveloperLink],
        teardown: &dyn DeveloperTeardownPort,
    ) -> Result<DeveloperDisableReport, DeveloperError> {
        let current_generation = {
            let mut gate = self.mode.gate.lock().unwrap();
            if !gate.enabled {
                return Ok(DeveloperDisableReport::default());
            }
            gate.disabling = true;
            gate.enabled = false;
            gate.generation
        };
        let active = links
            .iter()
            .filter(|link| {
                link.mode_generation == current_generation && link.admission.load(Ordering::SeqCst)
            })
            .collect::<Vec<_>>();
        for link in &active {
            if let Err(error) = teardown.teardown_and_revoke(link) {
                let mut gate = self.mode.gate.lock().unwrap();
                gate.enabled = true;
                gate.disabling = false;
                return Err(DeveloperError::new(
                    "developer_teardown_failed",
                    format!(
                        "cannot teardown {} generation {}: {error}",
                        link.receipt.plugin_id.as_str(),
                        link.receipt.generation
                    ),
                ));
            }
        }
        for link in &active {
            link.admission.store(false, Ordering::SeqCst);
        }
        let mut gate = self.mode.gate.lock().unwrap();
        gate.generation = gate.generation.saturating_add(1);
        gate.disabling = false;
        Ok(DeveloperDisableReport {
            revoked_links: active.len(),
        })
    }

    fn commit_prepared(
        &self,
        prepared: PreparedDeveloperLink,
        generation: u64,
        previous: Option<ReceiptSummary>,
        consent: Option<NativeDigestConsent>,
    ) -> Result<DeveloperLink, DeveloperError> {
        let current_mode_generation = self.mode.current_generation()?;
        if current_mode_generation != prepared.mode_generation {
            return Err(DeveloperError::new(
                "developer_link_stale",
                "Developer Mode changed while the snapshot was being prepared",
            ));
        }
        validate_snapshot(&prepared.snapshot, &prepared.metadata, true)?;
        let runtime_kind = prepared.manifest.runtime.kind;
        let (native_trust_digest, runtime_epoch) = if runtime_kind == RuntimeKind::VerifiedNative {
            if consent.as_ref().map(|value| &value.digest) != Some(&prepared.package_digest) {
                return Err(DeveloperError::new(
                    "developer_native_consent_required",
                    "exact native package digest consent is required",
                ));
            }
            (
                Some(prepared.package_digest.clone()),
                Some(self.runtime_epoch.clone()),
            )
        } else {
            (None, None)
        };
        let receipt = InstallReceipt {
            schema_version: INSTALL_RECEIPT_SCHEMA_VERSION,
            plugin_id: prepared.manifest.id.clone(),
            version: prepared.manifest.version.clone(),
            package_digest: prepared.package_digest.clone(),
            publisher_key_id: DEVELOPER_KEY_ID.to_owned(),
            publisher_lineage: format!(
                "{DEVELOPER_LINEAGE_PREFIX}{}",
                prepared.manifest.publisher.as_str()
            ),
            target: prepared.metadata.target,
            source: InstallSource::DeveloperSnapshot,
            enabled: true,
            granted_permissions: granted_permissions(&prepared.manifest),
            native_trust_digest,
            installed_at_ms: now_ms()?,
            generation,
            state_schema_version: prepared.manifest.state.schema_version,
            rollback_compatible_through: prepared.manifest.state.rollback_compatible_through,
            previous,
        };
        receipt.validate().map_err(|error| {
            DeveloperError::new(error.code(), "developer receipt contract rejected")
        })?;
        Ok(DeveloperLink {
            receipt,
            snapshot: prepared.snapshot,
            source: prepared.source,
            metadata: prepared.metadata,
            runtime_kind,
            runtime_epoch,
            mode_generation: current_mode_generation,
            admission: Arc::new(AtomicBool::new(true)),
        })
    }
}

struct DeveloperSignatureSource;

impl PackageSignatureSource for DeveloperSignatureSource {
    fn sign(&self, _message: &[u8]) -> Result<PackageSignatureV1, PackageError> {
        PackageSignatureV1::new(
            SignatureAlgorithm::Ed25519,
            DEVELOPER_KEY_ID,
            STANDARD.encode([0_u8; 64]),
        )
        .map_err(|_| PackageError::package_metadata())
    }
}

struct OwnerOnlyArchive {
    path: PathBuf,
    file: Option<File>,
}

impl OwnerOnlyArchive {
    fn create(parent: &Path) -> Result<Self, DeveloperError> {
        for _ in 0..16 {
            let path = parent.join(format!(".developer-package-{}", random_identifier()?));
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(file) => {
                    validate_owner_file(&file, 0o600)?;
                    return Ok(Self {
                        path,
                        file: Some(file),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(DeveloperError::new(
                        "developer_archive_create",
                        format!("cannot create developer package archive: {error}"),
                    ))
                }
            }
        }
        Err(DeveloperError::new(
            "developer_archive_create",
            "cannot allocate a unique developer package archive",
        ))
    }

    fn file_mut(&mut self) -> &mut File {
        self.file
            .as_mut()
            .expect("developer archive file is available before verification")
    }

    fn take_file(&mut self) -> Result<File, DeveloperError> {
        let mut file = self.file.take().ok_or_else(|| {
            DeveloperError::new(
                "developer_archive_state",
                "developer package archive was already consumed",
            )
        })?;
        file.seek(SeekFrom::Start(0)).map_err(|error| {
            DeveloperError::new(
                "developer_archive_read",
                format!("cannot rewind developer package archive: {error}"),
            )
        })?;
        Ok(file)
    }
}

impl Drop for OwnerOnlyArchive {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn inspect_source(source: &Path, profile: &Path) -> Result<SourceDiagnostic, DeveloperError> {
    let direct = SourceIdentity::inspect(source)?;
    let canonical_path = fs::canonicalize(source).map_err(|error| {
        DeveloperError::new(
            "developer_source_invalid",
            format!("cannot canonicalize {}: {error}", source.display()),
        )
    })?;
    let canonical = SourceIdentity::inspect(&canonical_path)?;
    if direct.device != canonical.device || direct.inode != canonical.inode {
        return Err(DeveloperError::new(
            "developer_source_invalid",
            "developer source root may not be a symbolic link",
        ));
    }
    if canonical_path.starts_with(profile) {
        return Err(DeveloperError::new(
            "developer_source_inside_profile",
            "developer source cannot live inside Jarvis managed storage",
        ));
    }
    Ok(SourceDiagnostic {
        canonical_path,
        identity: canonical,
    })
}

fn prepare_cache_root(profile: &Path) -> Result<PathBuf, DeveloperError> {
    ensure_owner_directory(profile, 0o700)?;
    let cache = profile.join("plugin-cache");
    ensure_owner_directory(&cache, 0o700)?;
    Ok(cache)
}

fn extract_developer_snapshot(
    profile: &Path,
    metadata: &PackageMetadataV1,
    digest: &Digest,
    evidence: jarvis_package::VerifiedPackageEvidence,
) -> Result<PathBuf, DeveloperError> {
    let cache = prepare_cache_root(profile)?;
    let plugin_cache = cache.join(metadata.plugin_id.as_str());
    ensure_owner_directory(&plugin_cache, 0o700)?;
    let developer_root = plugin_cache.join("developer");
    ensure_owner_directory(&developer_root, 0o700)?;
    let digest_component = digest.as_str().strip_prefix("sha256:").ok_or_else(|| {
        DeveloperError::new(
            "developer_digest_invalid",
            "developer package digest has no sha256 prefix",
        )
    })?;
    let snapshot = developer_root.join(digest_component);
    let parent = open_owner_directory(&developer_root)?;
    let parent: OwnedFd = parent.into();
    match extract_verified_package(evidence, &parent, digest_component) {
        Ok(extracted) => {
            let metadata_on_disk = fs::symlink_metadata(&snapshot).map_err(|error| {
                DeveloperError::new(
                    "developer_snapshot_extract",
                    format!("cannot inspect extracted snapshot: {error}"),
                )
            })?;
            let extracted_device = u64::try_from(extracted.device()).map_err(|_| {
                DeveloperError::new(
                    "developer_snapshot_extract",
                    "extracted snapshot has an invalid device identity",
                )
            })?;
            if metadata_on_disk.dev() != extracted_device
                || metadata_on_disk.ino() != extracted.inode()
            {
                return Err(DeveloperError::new(
                    "developer_snapshot_extract",
                    "extracted snapshot identity changed",
                ));
            }
        }
        Err(error) if snapshot.exists() => {
            validate_snapshot(&snapshot, metadata, true).map_err(|validation| {
                DeveloperError::new(
                    "developer_snapshot_conflict",
                    format!(
                        "digest snapshot already exists but is invalid ({error}; {validation})"
                    ),
                )
            })?;
            return Ok(snapshot);
        }
        Err(error) => return Err(error.into()),
    }
    validate_snapshot(&snapshot, metadata, false)?;
    harden_snapshot(&snapshot, metadata)?;
    validate_snapshot(&snapshot, metadata, true)?;
    Ok(snapshot)
}

fn validate_developer_runtime_policy(manifest: &ManifestV2) -> Result<(), DeveloperError> {
    if manifest.runtime.kind != RuntimeKind::VerifiedNative {
        return Ok(());
    }
    if manifest
        .runtime
        .activation_events
        .iter()
        .any(|event| event == "onStartup" || event.starts_with("onStartup:"))
    {
        return Err(DeveloperError::new(
            "developer_unattended_activation_forbidden",
            "unverified native developer plugins cannot activate on startup",
        ));
    }
    if manifest
        .runtime
        .service
        .as_ref()
        .map(|service| service.survives_core_exit)
        .unwrap_or(false)
    {
        return Err(DeveloperError::new(
            "developer_persistent_service_forbidden",
            "unverified native developer plugins cannot install persistent services",
        ));
    }
    Ok(())
}

fn granted_permissions(manifest: &ManifestV2) -> Vec<GrantedPermission> {
    manifest
        .permissions
        .iter()
        .map(|permission| GrantedPermission {
            id: permission.id,
            scope: permission.scope.clone(),
            modes: permission.modes.clone(),
        })
        .collect()
}

fn permission_diff(previous: &[GrantedPermission], next: &ManifestV2) -> PermissionDiff {
    let next = granted_permissions(next);
    PermissionDiff {
        added: next
            .iter()
            .filter(|permission| !previous.contains(permission))
            .cloned()
            .collect(),
        removed: previous
            .iter()
            .filter(|permission| !next.contains(permission))
            .cloned()
            .collect(),
    }
}

fn validate_snapshot(
    root: &Path,
    metadata: &PackageMetadataV1,
    require_immutable_modes: bool,
) -> Result<(), DeveloperError> {
    let root_metadata = fs::symlink_metadata(root).map_err(|error| {
        DeveloperError::new(
            "developer_snapshot_invalid",
            format!("cannot inspect {}: {error}", root.display()),
        )
    })?;
    if root_metadata.file_type().is_symlink()
        || !root_metadata.is_dir()
        || root_metadata.uid() != effective_user_id()
        || (require_immutable_modes && root_metadata.mode() & 0o777 != 0o555)
    {
        return Err(DeveloperError::new(
            "developer_snapshot_invalid",
            format!("{} is not an immutable owned directory", root.display()),
        ));
    }
    let expected_files = metadata
        .files
        .iter()
        .map(|file| (file.path.as_str().to_owned(), file))
        .collect::<BTreeMap<_, _>>();
    let expected_directories = expected_directory_set(expected_files.keys().map(String::as_str));
    let mut observed_files = BTreeSet::new();
    let mut observed_directories = BTreeSet::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|error| {
            DeveloperError::new(
                "developer_snapshot_invalid",
                format!("cannot read {}: {error}", directory.display()),
            )
        })? {
            let entry = entry.map_err(|error| {
                DeveloperError::new(
                    "developer_snapshot_invalid",
                    format!("cannot read snapshot entry: {error}"),
                )
            })?;
            let path = entry.path();
            let entry_metadata = fs::symlink_metadata(&path).map_err(|error| {
                DeveloperError::new(
                    "developer_snapshot_invalid",
                    format!("cannot inspect {}: {error}", path.display()),
                )
            })?;
            let relative = relative_snapshot_path(root, &path)?;
            if entry_metadata.file_type().is_symlink()
                || entry_metadata.uid() != effective_user_id()
            {
                return Err(DeveloperError::new(
                    "developer_snapshot_invalid",
                    format!("unsafe snapshot entry {relative}"),
                ));
            }
            if entry_metadata.is_dir() {
                if require_immutable_modes && entry_metadata.mode() & 0o777 != 0o555 {
                    return Err(DeveloperError::new(
                        "developer_snapshot_invalid",
                        format!("snapshot directory {relative} is mutable"),
                    ));
                }
                observed_directories.insert(relative);
                pending.push(path);
                continue;
            }
            if !entry_metadata.is_file() || entry_metadata.nlink() != 1 {
                return Err(DeveloperError::new(
                    "developer_snapshot_invalid",
                    format!("snapshot entry {relative} is not a private regular file"),
                ));
            }
            let expected = expected_files.get(&relative).ok_or_else(|| {
                DeveloperError::new(
                    "developer_snapshot_invalid",
                    format!("unexpected snapshot file {relative}"),
                )
            })?;
            validate_snapshot_file(&path, &entry_metadata, expected, require_immutable_modes)?;
            observed_files.insert(relative);
        }
    }
    if observed_files != expected_files.keys().cloned().collect()
        || observed_directories != expected_directories
    {
        return Err(DeveloperError::new(
            "developer_snapshot_invalid",
            "snapshot tree does not exactly match verified package metadata",
        ));
    }
    Ok(())
}

fn validate_snapshot_file(
    path: &Path,
    path_metadata: &fs::Metadata,
    expected: &PackageFile,
    require_immutable_modes: bool,
) -> Result<(), DeveloperError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| {
            DeveloperError::new(
                "developer_snapshot_invalid",
                format!("cannot open {}: {error}", path.display()),
            )
        })?;
    let opened = file.metadata().map_err(|error| {
        DeveloperError::new(
            "developer_snapshot_invalid",
            format!("cannot inspect opened {}: {error}", path.display()),
        )
    })?;
    if opened.dev() != path_metadata.dev()
        || opened.ino() != path_metadata.ino()
        || opened.nlink() != 1
        || opened.uid() != effective_user_id()
        || opened.len() != expected.size
        || (require_immutable_modes && opened.mode() & 0o777 != expected.mode.as_octal())
    {
        return Err(DeveloperError::new(
            "developer_snapshot_invalid",
            format!("snapshot file identity changed for {}", path.display()),
        ));
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            DeveloperError::new(
                "developer_snapshot_invalid",
                format!("cannot hash {}: {error}", path.display()),
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let observed = Digest::new(format!("sha256:{:x}", hasher.finalize())).map_err(|_| {
        DeveloperError::new(
            "developer_snapshot_invalid",
            "cannot represent snapshot digest",
        )
    })?;
    if observed != expected.digest {
        return Err(DeveloperError::new(
            "developer_snapshot_invalid",
            format!("snapshot digest mismatch for {}", path.display()),
        ));
    }
    Ok(())
}

fn harden_snapshot(root: &Path, metadata: &PackageMetadataV1) -> Result<(), DeveloperError> {
    for expected in &metadata.files {
        let path = root.join(expected.path.as_str());
        chmod_owned_regular_file(&path, expected.mode.as_octal())?;
    }
    let mut directories =
        expected_directory_set(metadata.files.iter().map(|file| file.path.as_str()))
            .into_iter()
            .map(|relative| root.join(relative))
            .collect::<Vec<_>>();
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        chmod_owned_directory(&directory, 0o555)?;
    }
    chmod_owned_directory(root, 0o555)
}

fn expected_directory_set<'a>(paths: impl Iterator<Item = &'a str>) -> BTreeSet<String> {
    let mut directories = BTreeSet::new();
    for path in paths {
        let mut current = Path::new(path).parent();
        while let Some(parent) = current {
            if parent.as_os_str().is_empty() {
                break;
            }
            directories.insert(parent.to_string_lossy().into_owned());
            current = parent.parent();
        }
    }
    directories
}

fn relative_snapshot_path(root: &Path, path: &Path) -> Result<String, DeveloperError> {
    let relative = path.strip_prefix(root).map_err(|_| {
        DeveloperError::new(
            "developer_snapshot_invalid",
            "snapshot path escaped its digest root",
        )
    })?;
    relative.to_str().map(str::to_owned).ok_or_else(|| {
        DeveloperError::new(
            "developer_snapshot_invalid",
            "snapshot path is not valid UTF-8",
        )
    })
}

fn ensure_owner_directory(path: &Path, mode: u32) -> Result<(), DeveloperError> {
    let path_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.uid() != effective_user_id()
            {
                return Err(DeveloperError::new(
                    "developer_path_unsafe",
                    format!("{} is not an owned real directory", path.display()),
                ));
            }
            metadata
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|error| {
                DeveloperError::new(
                    "developer_path_create",
                    format!("cannot create {}: {error}", path.display()),
                )
            })?;
            fs::symlink_metadata(path).map_err(|error| {
                DeveloperError::new(
                    "developer_path_invalid",
                    format!("cannot inspect created {}: {error}", path.display()),
                )
            })?
        }
        Err(error) => {
            return Err(DeveloperError::new(
                "developer_path_invalid",
                format!("cannot inspect {}: {error}", path.display()),
            ))
        }
    };
    let directory = open_owner_directory(path)?;
    let opened_metadata = directory.metadata().map_err(|error| {
        DeveloperError::new(
            "developer_path_invalid",
            format!("cannot inspect opened {}: {error}", path.display()),
        )
    })?;
    if path_metadata.dev() != opened_metadata.dev() || path_metadata.ino() != opened_metadata.ino()
    {
        return Err(DeveloperError::new(
            "developer_path_unsafe",
            format!("{} changed before it could be protected", path.display()),
        ));
    }
    directory
        .set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|error| {
            DeveloperError::new(
                "developer_path_permissions",
                format!("cannot protect {}: {error}", path.display()),
            )
        })?;
    let protected = directory.metadata().map_err(|error| {
        DeveloperError::new(
            "developer_path_invalid",
            format!("cannot inspect protected {}: {error}", path.display()),
        )
    })?;
    if protected.mode() & 0o777 != mode {
        return Err(DeveloperError::new(
            "developer_path_permissions",
            format!("{} did not retain owner-only permissions", path.display()),
        ));
    }
    Ok(())
}

fn open_owner_directory(path: &Path) -> Result<File, DeveloperError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| {
            DeveloperError::new(
                "developer_path_invalid",
                format!("cannot open {}: {error}", path.display()),
            )
        })?;
    let metadata = file.metadata().map_err(|error| {
        DeveloperError::new(
            "developer_path_invalid",
            format!("cannot inspect opened {}: {error}", path.display()),
        )
    })?;
    if !metadata.is_dir() || metadata.uid() != effective_user_id() {
        return Err(DeveloperError::new(
            "developer_path_unsafe",
            format!("{} is not an owned directory", path.display()),
        ));
    }
    Ok(file)
}

fn chmod_owned_regular_file(path: &Path, mode: u32) -> Result<(), DeveloperError> {
    let path_metadata = fs::symlink_metadata(path).map_err(|error| {
        DeveloperError::new(
            "developer_snapshot_permissions",
            format!("cannot inspect {}: {error}", path.display()),
        )
    })?;
    if path_metadata.file_type().is_symlink()
        || !path_metadata.is_file()
        || path_metadata.uid() != effective_user_id()
        || path_metadata.nlink() != 1
    {
        return Err(DeveloperError::new(
            "developer_snapshot_permissions",
            format!("{} is not an owned single-link file", path.display()),
        ));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| {
            DeveloperError::new(
                "developer_snapshot_permissions",
                format!("cannot open {}: {error}", path.display()),
            )
        })?;
    let opened = file.metadata().map_err(|error| {
        DeveloperError::new(
            "developer_snapshot_permissions",
            format!("cannot inspect opened {}: {error}", path.display()),
        )
    })?;
    if opened.dev() != path_metadata.dev()
        || opened.ino() != path_metadata.ino()
        || opened.nlink() != 1
    {
        return Err(DeveloperError::new(
            "developer_snapshot_permissions",
            format!("{} changed before it could be protected", path.display()),
        ));
    }
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|error| {
            DeveloperError::new(
                "developer_snapshot_permissions",
                format!("cannot protect {}: {error}", path.display()),
            )
        })
}

fn chmod_owned_directory(path: &Path, mode: u32) -> Result<(), DeveloperError> {
    let path_metadata = fs::symlink_metadata(path).map_err(|error| {
        DeveloperError::new(
            "developer_snapshot_permissions",
            format!("cannot inspect {}: {error}", path.display()),
        )
    })?;
    let directory = open_owner_directory(path)?;
    let opened = directory.metadata().map_err(|error| {
        DeveloperError::new(
            "developer_snapshot_permissions",
            format!("cannot inspect opened {}: {error}", path.display()),
        )
    })?;
    if path_metadata.file_type().is_symlink()
        || !path_metadata.is_dir()
        || path_metadata.uid() != effective_user_id()
        || opened.dev() != path_metadata.dev()
        || opened.ino() != path_metadata.ino()
    {
        return Err(DeveloperError::new(
            "developer_snapshot_permissions",
            format!("{} changed before it could be protected", path.display()),
        ));
    }
    directory
        .set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|error| {
            DeveloperError::new(
                "developer_snapshot_permissions",
                format!("cannot protect {}: {error}", path.display()),
            )
        })
}

fn validate_owner_file(file: &File, mode: u32) -> Result<(), DeveloperError> {
    let metadata = file.metadata().map_err(|error| {
        DeveloperError::new(
            "developer_archive_create",
            format!("cannot inspect developer package archive: {error}"),
        )
    })?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != effective_user_id()
        || metadata.mode() & 0o777 != mode
    {
        return Err(DeveloperError::new(
            "developer_archive_create",
            "developer package archive is not an owner-only regular file",
        ));
    }
    Ok(())
}

fn random_identifier() -> Result<String, DeveloperError> {
    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|error| {
        DeveloperError::new(
            "developer_random",
            format!("cannot create developer identifier: {error}"),
        )
    })?;
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use fmt::Write as _;
        write!(&mut result, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(result)
}

fn now_ms() -> Result<i64, DeveloperError> {
    let duration = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| {
        DeveloperError::new("developer_clock", "system clock is before the Unix epoch")
    })?;
    i64::try_from(duration.as_millis()).map_err(|_| {
        DeveloperError::new(
            "developer_clock",
            "system clock cannot be represented in milliseconds",
        )
    })
}

fn effective_user_id() -> u32 {
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    use jarvis_package::{PackageDocumentAdapter, PackageError};
    use jarvis_plugin_protocol::manifest::{ManifestV2, PermissionId, RuntimeKind};
    use jarvis_plugin_protocol::package::{
        MacOsVersion, PackageMetadataV1, PackageSignatureV1, PackageTarget, PACKAGE_SCHEMA_VERSION,
    };

    use super::{
        DeveloperLink, DeveloperLinker, DeveloperPackageOptions, DeveloperTeardownPort,
        NativeDigestConsent,
    };

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            loop {
                let suffix = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "jarvis-developer-{label}-{}-{suffix}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self { path },
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("create isolated Developer Mode fixture: {error}"),
                }
            }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = make_tree_owner_writable(&self.path);
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn make_tree_owner_writable(path: &Path) -> std::io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            for entry in fs::read_dir(path)? {
                make_tree_owner_writable(&entry?.path())?;
            }
        } else {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    #[derive(Clone, Copy)]
    struct TestDocumentAdapter;

    impl PackageDocumentAdapter for TestDocumentAdapter {
        fn resolve_source_manifest(
            &self,
            bytes: &[u8],
            _target: PackageTarget,
        ) -> Result<ManifestV2, PackageError> {
            ManifestV2::parse(bytes).map_err(PackageError::manifest)
        }

        fn validate_packaged_manifest(
            &self,
            bytes: &[u8],
            _target: PackageTarget,
        ) -> Result<ManifestV2, PackageError> {
            ManifestV2::parse(bytes).map_err(PackageError::manifest)
        }

        fn validate_package_metadata_schema(&self, bytes: &[u8]) -> Result<(), PackageError> {
            let metadata: PackageMetadataV1 =
                serde_json::from_slice(bytes).map_err(|_| PackageError::package_metadata())?;
            if metadata.schema_version != PACKAGE_SCHEMA_VERSION {
                return Err(PackageError::package_metadata());
            }
            Ok(())
        }

        fn validate_package_signature_schema(&self, bytes: &[u8]) -> Result<(), PackageError> {
            let signature: PackageSignatureV1 =
                serde_json::from_slice(bytes).map_err(|_| PackageError::package_metadata())?;
            signature
                .validate()
                .map_err(|_| PackageError::package_metadata())
        }
    }

    struct DevFixture {
        _root: TestDirectory,
        source: PathBuf,
        profile: PathBuf,
        linker: DeveloperLinker<TestDocumentAdapter>,
    }

    impl DevFixture {
        fn enabled() -> Self {
            Self::with_native(false, false)
        }

        fn disabled() -> Self {
            Self::with_native_and_mode(false, false, false)
        }

        fn enabled_native() -> Self {
            Self::with_native(true, false)
        }

        fn persistent_native() -> Self {
            Self::with_native(true, true)
        }

        fn with_native(native: bool, survives_core_exit: bool) -> Self {
            Self::with_native_and_mode(native, survives_core_exit, true)
        }

        fn with_native_and_mode(
            native: bool,
            survives_core_exit: bool,
            mode_enabled: bool,
        ) -> Self {
            let root = TestDirectory::new(if native { "native" } else { "ui" });
            let source = root.path().join("source");
            let profile = root.path().join("profile");
            fs::create_dir(&source).unwrap();
            fs::create_dir(&profile).unwrap();
            if native {
                write_native_source(&source, survives_core_exit, &[]);
            } else {
                write_ui_source(&source, "version-one", &[]);
            }
            let linker = DeveloperLinker::new(
                profile.clone(),
                TestDocumentAdapter,
                DeveloperPackageOptions {
                    target: PackageTarget::DarwinArm64,
                    minimum_macos: MacOsVersion::parse("14.0.0").unwrap(),
                },
                mode_enabled,
            )
            .unwrap();
            Self {
                _root: root,
                source,
                profile,
                linker,
            }
        }

        fn link(&self) -> Result<DeveloperLink, super::DeveloperError> {
            self.linker.link(&self.source, None)
        }

        fn link_with_consent(&self) -> Result<DeveloperLink, super::DeveloperError> {
            let prepared = self.linker.prepare_link(&self.source)?;
            let consent = NativeDigestConsent::new(prepared.package_digest().clone());
            self.linker.commit_link(prepared, Some(consent))
        }

        fn write_ui(&self, contents: &str, permissions: &[&str]) {
            write_ui_source(&self.source, contents, permissions);
        }
    }

    fn write_ui_source(root: &Path, contents: &str, permissions: &[&str]) {
        fs::create_dir_all(root.join("ui")).unwrap();
        fs::write(root.join("ui/index.html"), contents).unwrap();
        let permissions = permissions
            .iter()
            .map(|id| serde_json::json!({ "id": id }))
            .collect::<Vec<_>>();
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "id": "dev.example.snapshot",
            "name": "Developer Snapshot",
            "version": "1.0.0",
            "publisher": "example",
            "compatibility": {
                "jarvis": ">=0.3.3, <0.5.0",
                "pluginApi": 2
            },
            "runtime": {
                "kind": "ui-only",
                "protocol": 2,
                "activationEvents": []
            },
            "permissions": permissions,
            "state": {
                "schemaVersion": 1,
                "migrations": [],
                "rollbackCompatibleThrough": 1
            },
            "contributes": {
                "pages": [],
                "commands": [],
                "actions": [],
                "hotkeys": [],
                "settings": [],
                "projectRuntimes": [],
                "dataContracts": []
            }
        });
        fs::write(
            root.join("plugin.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    fn write_native_source(root: &Path, survives_core_exit: bool, activation_events: &[&str]) {
        fs::create_dir_all(root.join("bin/darwin-arm64")).unwrap();
        for name in ["bridge", "controller"] {
            let path = root.join("bin/darwin-arm64").join(name);
            fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "id": "dev.example.native-snapshot",
            "name": "Native Developer Snapshot",
            "version": "1.0.0",
            "publisher": "example",
            "compatibility": {
                "jarvis": ">=0.3.3, <0.5.0",
                "pluginApi": 2
            },
            "runtime": {
                "kind": "verified-native",
                "lifecycle": "service-bridge",
                "bridgeEntry": "bin/darwin-arm64/bridge",
                "service": {
                    "id": "controller",
                    "manager": "launchd-user",
                    "entry": "bin/darwin-arm64/controller",
                    "survivesCoreExit": survives_core_exit
                },
                "protocol": 2,
                "activationEvents": activation_events
            },
            "permissions": [],
            "state": {
                "schemaVersion": 1,
                "migrations": [],
                "rollbackCompatibleThrough": 1
            },
            "contributes": {
                "pages": [],
                "commands": [],
                "actions": [],
                "hotkeys": [],
                "settings": [],
                "projectRuntimes": [],
                "dataContracts": []
            }
        });
        fs::write(
            root.join("plugin.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn link_runs_from_digest_snapshot_not_mutable_source() {
        let fixture = DevFixture::enabled();
        let linked = fixture.link().unwrap();
        fixture.write_ui("version-two", &[]);

        assert_eq!(
            fs::read_to_string(linked.snapshot().join("ui/index.html")).unwrap(),
            "version-one"
        );
        assert_eq!(
            fixture
                .linker
                .reload_without_approval(&linked)
                .unwrap_err()
                .code(),
            "developer_source_changed"
        );
    }

    #[test]
    fn developer_mode_off_rejects_link_without_creating_cache() {
        let fixture = DevFixture::disabled();

        assert_eq!(
            fixture.link().unwrap_err().code(),
            "developer_mode_disabled"
        );
        assert!(!fixture.profile.join("plugin-cache").exists());
    }

    #[test]
    fn schema_and_package_quotas_still_apply_in_developer_mode() {
        let fixture = DevFixture::enabled();
        fs::write(fixture.source.join("plugin.json"), b"{}").unwrap();
        assert_eq!(fixture.link().unwrap_err().code(), "manifest_schema");

        fs::write(
            fixture.source.join("plugin.json"),
            vec![b' '; 256 * 1024 + 1],
        )
        .unwrap();
        assert_eq!(fixture.link().unwrap_err().code(), "archive_quota");
    }

    #[cfg(unix)]
    #[test]
    fn source_symlinks_are_rejected_before_snapshot_admission() {
        use std::os::unix::fs::symlink;

        let fixture = DevFixture::enabled();
        symlink(
            fixture.source.join("ui/index.html"),
            fixture.source.join("ui/alias.html"),
        )
        .unwrap();

        assert_eq!(fixture.link().unwrap_err().code(), "source_invalid");
    }

    #[test]
    fn snapshot_cache_is_owner_only_and_payload_is_immutable() {
        let fixture = DevFixture::enabled();
        let linked = fixture.link().unwrap();
        let developer_parent = linked.snapshot().parent().unwrap();

        assert_eq!(
            fs::metadata(developer_parent).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(linked.snapshot())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o555
        );
        assert_eq!(
            fs::metadata(linked.snapshot().join("ui/index.html"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o444
        );
        assert_eq!(fs::metadata(developer_parent).unwrap().uid(), unsafe {
            libc::geteuid()
        });
    }

    #[test]
    fn unverified_native_link_requires_exact_digest_consent() {
        let fixture = DevFixture::enabled_native();
        let prepared = fixture.linker.prepare_link(&fixture.source).unwrap();
        assert_eq!(
            prepared.manifest().runtime.kind,
            RuntimeKind::VerifiedNative
        );
        assert!(prepared.snapshot().is_dir());
        let wrong = NativeDigestConsent::new(
            jarvis_plugin_protocol::manifest::Digest::new(format!("sha256:{}", "f".repeat(64)))
                .unwrap(),
        );

        assert_eq!(
            fixture
                .linker
                .commit_link(prepared, Some(wrong))
                .unwrap_err()
                .code(),
            "developer_native_consent_required"
        );
    }

    #[test]
    fn unverified_native_link_requires_new_consent_after_restart() {
        let fixture = DevFixture::enabled_native();
        let linked = fixture.link_with_consent().unwrap();
        assert!(linked.receipt().native_trust_digest.is_some());
        let restarted = DeveloperLinker::new(
            fixture.profile.clone(),
            TestDocumentAdapter,
            DeveloperPackageOptions {
                target: PackageTarget::DarwinArm64,
                minimum_macos: MacOsVersion::parse("14.0.0").unwrap(),
            },
            true,
        )
        .unwrap();

        assert_eq!(
            restarted.resolve(&linked).unwrap_err().code(),
            "developer_native_reconsent"
        );

        let consent = NativeDigestConsent::new(linked.receipt().package_digest.clone());
        let reconsented = restarted.reconsent_native(&linked, consent).unwrap();
        assert_eq!(reconsented.receipt().generation, 2);
        assert_eq!(restarted.resolve(&reconsented).unwrap().generation(), 2);
        assert_eq!(
            restarted.resolve(&linked).unwrap_err().code(),
            "developer_generation_revoked"
        );
    }

    #[test]
    fn linked_persistent_services_and_unattended_startup_are_forbidden() {
        let persistent = DevFixture::persistent_native();
        assert_eq!(
            persistent
                .linker
                .prepare_link(&persistent.source)
                .unwrap_err()
                .code(),
            "developer_persistent_service_forbidden"
        );

        let startup = DevFixture::enabled_native();
        write_native_source(&startup.source, false, &["onStartup"]);
        assert_eq!(
            startup
                .linker
                .prepare_link(&startup.source)
                .unwrap_err()
                .code(),
            "developer_unattended_activation_forbidden"
        );
    }

    #[derive(Default)]
    struct RecordingTeardown {
        generations: Mutex<Vec<u64>>,
    }

    impl DeveloperTeardownPort for RecordingTeardown {
        fn teardown_and_revoke(&self, link: &DeveloperLink) -> Result<(), String> {
            self.generations
                .lock()
                .unwrap()
                .push(link.receipt().generation);
            Ok(())
        }
    }

    #[test]
    fn disabling_mode_tears_down_before_revoking_all_activation_generations() {
        let fixture = DevFixture::enabled();
        let linked = fixture.link().unwrap();
        fixture.linker.resolve(&linked).unwrap();
        let teardown = RecordingTeardown::default();

        fixture
            .linker
            .disable_mode(std::slice::from_ref(&linked), &teardown)
            .unwrap();

        assert!(!fixture.linker.mode_enabled());
        assert_eq!(
            teardown.generations.lock().unwrap().as_slice(),
            [linked.receipt().generation]
        );
        assert_eq!(
            fixture.linker.resolve(&linked).unwrap_err().code(),
            "developer_mode_disabled"
        );
        fixture.linker.enable_mode();
        assert_eq!(
            fixture.linker.resolve(&linked).unwrap_err().code(),
            "developer_generation_revoked"
        );
    }

    #[test]
    fn changed_digest_exposes_permission_diff_and_switches_generation_only_after_approval() {
        let fixture = DevFixture::enabled();
        let linked = fixture.link().unwrap();
        fixture.write_ui("version-two", &["notifications.publish"]);
        let plan = fixture.linker.prepare_reload(&linked).unwrap();

        assert!(plan.changed());
        assert_ne!(plan.package_digest(), &linked.receipt().package_digest);
        assert_eq!(
            plan.permission_diff()
                .added()
                .iter()
                .map(|permission| permission.id)
                .collect::<Vec<_>>(),
            vec![PermissionId::NotificationsPublish]
        );
        assert!(plan.permission_diff().removed().is_empty());
        assert_eq!(fixture.linker.resolve(&linked).unwrap().generation(), 1);

        let approval = plan.approval();
        let reloaded = fixture
            .linker
            .commit_reload(&linked, plan, Some(approval), None)
            .unwrap();
        assert_eq!(reloaded.receipt().generation, 2);
        assert_eq!(
            fs::read_to_string(reloaded.snapshot().join("ui/index.html")).unwrap(),
            "version-two"
        );
        assert_eq!(
            fixture.linker.resolve(&linked).unwrap_err().code(),
            "developer_generation_revoked"
        );
    }

    #[test]
    fn developer_receipt_never_serializes_disclosed_source_as_activation_root() {
        let fixture = DevFixture::enabled();
        let linked = fixture.link().unwrap();
        let receipt = serde_json::to_string(linked.receipt()).unwrap();
        let resolved = fixture.linker.resolve(&linked).unwrap();

        assert!(!receipt.contains(fixture.source.to_string_lossy().as_ref()));
        assert_ne!(linked.snapshot(), fixture.source);
        assert_eq!(
            linked.diagnostic_source_path(),
            fs::canonicalize(&fixture.source).unwrap()
        );
        assert_ne!(linked.source_device_inode(), (0, 0));
        assert_eq!(resolved.root(), linked.snapshot());
        assert_eq!(resolved.package_digest(), &linked.receipt().package_digest);
        assert_eq!(linked.runtime_kind(), RuntimeKind::UiOnly);
    }
}
