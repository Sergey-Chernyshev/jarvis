use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use jarvis_package::{extract_verified_package, inspect_and_verify_package, PackageTrustVerifier};
use jarvis_plugin_protocol::manifest::{
    Digest, PermissionId, PluginId, RuntimeKind, StateDeclaration,
};
use jarvis_plugin_protocol::operation::{Operation, OperationState};
use jarvis_plugin_protocol::package::PackageTarget;
use jarvis_plugin_protocol::receipt::{
    GrantedPermission, InstallReceipt, InstallSource, INSTALL_RECEIPT_SCHEMA_VERSION,
};
use semver::Version;
use serde::{Deserialize, Serialize};

use super::consent::{validate_approval, Approval, PermissionDiff};
use super::downloader::{stage_download, DownloadLimits, Downloader};
use super::health::{HealthCheck, HealthRunner};
use super::lock::ManagerLock;
use super::migration::{MigrationRequest, MigrationRunner};
use super::operation::{OperationFailure, OperationJournal};
use super::paths::PluginPaths;
use super::quarantine::{open_fixed_parent, QuarantineArchiveRef};
use super::receipt::{ReceiptStore, ReceiptVisibility, VersionStore, VersionVisibility};
use super::recovery::{decide_install_recovery, InstallRecoveryDecision, SavedInstallPhase};
use super::{random_storage_id, DurableObservation, StorageError};
use crate::plugins::manifest_v2::HostCompatibility;
use crate::plugins::package::HostPackageDocumentAdapter;
use crate::plugins::trust::package::SharedPackageVerifier;

pub type ManagerResult<T> = Result<T, ManagerError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagerError {
    code: String,
    message: String,
}

impl ManagerError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }
}

impl fmt::Display for ManagerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ManagerError {}

impl From<StorageError> for ManagerError {
    fn from(error: StorageError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogQuery {
    pub text: Option<String>,
    pub plugin_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogItem {
    pub plugin_id: PluginId,
    pub name: String,
    pub version: Version,
    pub target: PackageTarget,
    pub archive_digest: Digest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginDetails {
    pub item: CatalogItem,
    pub publisher_key_id: String,
    pub publisher_lineage: String,
    pub installed: Option<InstallReceipt>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum InstallSourceRef {
    Catalog { id: String, version: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallPlan {
    pub operation_id: String,
    pub state: OperationState,
    pub source: InstallSourceRef,
    pub plugin_id: PluginId,
    pub version: Version,
    pub target: PackageTarget,
    pub catalog_sequence: u64,
    pub archive_digest: Digest,
    pub package_digest: Digest,
    pub publisher_key_id: String,
    pub publisher_lineage: String,
    pub permission_diff: PermissionDiff,
    pub requested_permissions: Vec<GrantedPermission>,
    pub native_trust_digest: Option<Digest>,
    pub rollback_available: bool,
    pub irreversible_migration: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageFactsSnapshot {
    pub(crate) plugin_id: PluginId,
    pub(crate) version: Version,
    pub(crate) target: PackageTarget,
    pub(crate) archive_digest: Digest,
    pub(crate) runtime_kind: RuntimeKind,
    pub(crate) service_entry: Option<String>,
    pub(crate) requested_permissions: Vec<GrantedPermission>,
    pub(crate) state: StateDeclaration,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreparedInstall {
    plan: InstallPlan,
    archive: QuarantineArchiveRef,
    facts: PackageFactsSnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "stage",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum ManagerOperationPayload {
    Preparing { source: InstallSourceRef },
    Install { prepared: PreparedInstall },
    Lifecycle,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DoctorReport {
    pub plugin_id: Option<PluginId>,
    pub current: Option<InstallReceipt>,
    pub recoverable_operations: Vec<Operation>,
    pub issues: Vec<String>,
}

pub trait PackageManagerApi: Send + Sync {
    fn catalog(&self, query: CatalogQuery) -> ManagerResult<Vec<CatalogItem>>;
    fn info(&self, id: &PluginId) -> ManagerResult<PluginDetails>;
    fn prepare_install(&self, source: InstallSourceRef) -> ManagerResult<InstallPlan>;
    fn prepared_install(&self, operation_id: &str) -> ManagerResult<InstallPlan>;
    fn commit_install(&self, approval: Approval) -> ManagerResult<InstallReceipt>;
    fn update(&self, id: Option<&PluginId>) -> ManagerResult<Vec<Operation>>;
    fn rollback(&self, id: &PluginId, version: Option<&Version>) -> ManagerResult<InstallReceipt>;
    fn set_enabled(&self, id: &PluginId, enabled: bool) -> ManagerResult<Operation>;
    fn uninstall(&self, id: &PluginId) -> ManagerResult<Operation>;
    fn purge(&self, id: &PluginId, confirmation: &str) -> ManagerResult<Operation>;
    fn doctor(&self, id: Option<&PluginId>) -> ManagerResult<DoctorReport>;
}

#[derive(Clone)]
pub struct SelectedRelease {
    pub catalog_sequence: u64,
    pub plugin_id: PluginId,
    pub version: Version,
    pub target: PackageTarget,
    pub url: String,
    pub archive_digest: Digest,
    pub publisher_key_id: String,
    pub publisher_lineage: String,
    verifier: Arc<dyn PackageTrustVerifier + Send + Sync>,
}

impl fmt::Debug for SelectedRelease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SelectedRelease")
            .field("catalog_sequence", &self.catalog_sequence)
            .field("plugin_id", &self.plugin_id)
            .field("version", &self.version)
            .field("target", &self.target)
            .field("archive_digest", &self.archive_digest)
            .field("publisher_key_id", &self.publisher_key_id)
            .field("publisher_lineage", &self.publisher_lineage)
            .finish_non_exhaustive()
    }
}

impl SelectedRelease {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        catalog_sequence: u64,
        plugin_id: PluginId,
        version: Version,
        target: PackageTarget,
        url: String,
        archive_digest: Digest,
        publisher_key_id: String,
        publisher_lineage: String,
        verifier: Arc<dyn PackageTrustVerifier + Send + Sync>,
    ) -> Self {
        Self {
            catalog_sequence,
            plugin_id,
            version,
            target,
            url,
            archive_digest,
            publisher_key_id,
            publisher_lineage,
            verifier,
        }
    }

    #[cfg(test)]
    pub(crate) fn verifier_for_test(&self) -> Arc<dyn PackageTrustVerifier + Send + Sync> {
        self.verifier.clone()
    }
}

pub trait CatalogProvider: Send + Sync {
    fn catalog(&self, query: &CatalogQuery) -> ManagerResult<Vec<CatalogItem>>;
    fn info(&self, id: &PluginId) -> ManagerResult<PluginDetails>;
    fn select(&self, source: &InstallSourceRef) -> ManagerResult<SelectedRelease>;
}

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        crate::util::now_ms()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeardownStatus {
    Complete,
    Busy,
}

pub trait LifecycleHost: Send + Sync {
    fn teardown(&self, plugin_id: &PluginId) -> ManagerResult<TeardownStatus>;
    fn resume_activation(&self, _plugin_id: &PluginId) -> ManagerResult<()> {
        Ok(())
    }
    fn uninstall_activation(&self, paths: &PluginPaths, plugin_id: &PluginId) -> ManagerResult<()>;
    fn has_live_resources(&self, plugin_id: &PluginId) -> ManagerResult<bool>;
    fn purge_owned_data(&self, paths: &PluginPaths, plugin_id: &PluginId) -> ManagerResult<()>;
}

pub trait PackageEngine: Send + Sync {
    fn inspect(
        &self,
        paths: &PluginPaths,
        archive: &QuarantineArchiveRef,
        release: &SelectedRelease,
    ) -> ManagerResult<PackageFactsSnapshot>;

    fn verify_and_extract(
        &self,
        paths: &PluginPaths,
        archive: &QuarantineArchiveRef,
        release: &SelectedRelease,
    ) -> ManagerResult<VerifiedExtraction>;
}

#[derive(Clone, Debug)]
pub struct StrictPackageEngine {
    adapter: HostPackageDocumentAdapter,
}

impl StrictPackageEngine {
    pub fn new(compatibility: HostCompatibility) -> Self {
        Self {
            adapter: HostPackageDocumentAdapter::new(compatibility),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedExtraction {
    pub root: PathBuf,
    pub(crate) facts: PackageFactsSnapshot,
}

impl PackageEngine for StrictPackageEngine {
    fn inspect(
        &self,
        paths: &PluginPaths,
        archive: &QuarantineArchiveRef,
        release: &SelectedRelease,
    ) -> ManagerResult<PackageFactsSnapshot> {
        let parent = open_fixed_parent(paths, archive)?;
        let file = parent.open_archive(archive)?;
        let verifier = SharedPackageVerifier::new(release.verifier.clone());
        let evidence =
            inspect_and_verify_package(file, &self.adapter, &verifier).map_err(package_error)?;
        snapshot_facts(evidence.facts(), release)
    }

    fn verify_and_extract(
        &self,
        paths: &PluginPaths,
        archive: &QuarantineArchiveRef,
        release: &SelectedRelease,
    ) -> ManagerResult<VerifiedExtraction> {
        let parent = open_fixed_parent(paths, archive)?;
        let file = parent.open_archive(archive)?;
        let verifier = SharedPackageVerifier::new(release.verifier.clone());
        let evidence =
            inspect_and_verify_package(file, &self.adapter, &verifier).map_err(package_error)?;
        let facts = snapshot_facts(evidence.facts(), release)?;
        let extraction_name = format!(".extract-{}", random_storage_id()?);
        let extracted = extract_verified_package(evidence, parent.raw_fd(), &extraction_name)
            .map_err(package_error)?;
        Ok(VerifiedExtraction {
            root: paths.quarantine_root().join(extracted.name()),
            facts,
        })
    }
}

pub struct PluginManager {
    paths: PluginPaths,
    journal: OperationJournal,
    version_store: VersionStore,
    receipt_store: ReceiptStore,
    catalog: Arc<dyn CatalogProvider>,
    downloader: Arc<dyn Downloader>,
    package_engine: Arc<dyn PackageEngine>,
    migrations: Arc<dyn MigrationRunner>,
    health: Arc<dyn HealthRunner>,
    lifecycle: Arc<dyn LifecycleHost>,
    clock: Arc<dyn Clock>,
    download_limits: DownloadLimits,
}

impl PluginManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        paths: PluginPaths,
        catalog: Arc<dyn CatalogProvider>,
        downloader: Arc<dyn Downloader>,
        package_engine: Arc<dyn PackageEngine>,
        migrations: Arc<dyn MigrationRunner>,
        health: Arc<dyn HealthRunner>,
        lifecycle: Arc<dyn LifecycleHost>,
        clock: Arc<dyn Clock>,
    ) -> ManagerResult<Self> {
        paths.prepare()?;
        let journal = OperationJournal::open(paths.operations_db())?;
        let manager = Self {
            version_store: VersionStore::new(paths.clone()),
            receipt_store: ReceiptStore::new(paths.clone()),
            paths,
            journal,
            catalog,
            downloader,
            package_engine,
            migrations,
            health,
            lifecycle,
            clock,
            download_limits: DownloadLimits::default(),
        };
        manager.recover()?;
        Ok(manager)
    }

    pub fn with_download_limits(mut self, limits: DownloadLimits) -> Self {
        self.download_limits = limits;
        self
    }

    pub fn recover(&self) -> ManagerResult<Vec<Operation>> {
        let _manager_lock = ManagerLock::acquire(&self.paths)?;
        let recoverable = self
            .journal
            .recoverable_with_payload::<ManagerOperationPayload>()?;
        let mut reconciled = Vec::with_capacity(recoverable.len());
        for stored in recoverable {
            match (&stored.operation.state, &stored.payload) {
                (OperationState::WaitingForConsent, ManagerOperationPayload::Install { .. }) => {}
                (OperationState::Running, ManagerOperationPayload::Lifecycle) => {
                    let plugin_id =
                        PluginId::new(stored.operation.plugin_id.clone()).map_err(|error| {
                            ManagerError::new("operation_plugin_id", error.to_string())
                        })?;
                    let interrupted = match self.lifecycle.teardown(&plugin_id) {
                        Ok(TeardownStatus::Complete) => ManagerError::new(
                            "lifecycle_interrupted",
                            "lifecycle operation was interrupted and the activation was torn down",
                        ),
                        Ok(TeardownStatus::Busy) => ManagerError::new(
                            "lifecycle_teardown_busy",
                            "interrupted lifecycle still owns resources and remains blocked",
                        ),
                        Err(error) => {
                            self.fail_operation(&stored.operation.id, &error);
                            return Err(error);
                        }
                    };
                    self.fail_operation(&stored.operation.id, &interrupted);
                }
                (OperationState::Running, ManagerOperationPayload::Install { prepared }) => {
                    let fresh = self
                        .catalog
                        .select(&prepared.plan.source)
                        .and_then(|selection| ensure_same_release(&selection, &prepared.plan));
                    let version = self.version_store.observe(
                        &prepared.plan.plugin_id,
                        &prepared.plan.version,
                        &prepared.plan.package_digest,
                    )?;
                    let current = self.receipt_store.current(&prepared.plan.plugin_id)?;
                    let receipt_visibility = current
                        .as_ref()
                        .filter(|receipt| receipt_matches_prepared(receipt, prepared))
                        .map(|receipt| ReceiptVisibility::Exact {
                            plugin_id: receipt.plugin_id.clone(),
                            generation: receipt.generation,
                            package_digest: receipt.package_digest.clone(),
                        })
                        .unwrap_or(ReceiptVisibility::Absent);
                    match decide_install_recovery(
                        saved_phase(&stored.operation.phase),
                        fresh,
                        version,
                        receipt_visibility,
                    )? {
                        InstallRecoveryDecision::Succeeded => {
                            self.journal.transition(
                                &stored.operation.id,
                                OperationState::Succeeded,
                                "recovered",
                                None,
                            )?;
                            cleanup_prepared_archive(&self.paths, prepared);
                        }
                        InstallRecoveryDecision::ResumeReceiptCommit => {
                            let receipt = self.build_receipt(prepared, current.as_ref());
                            let visibility = reconcile_receipt_observation(
                                &self.receipt_store,
                                &receipt,
                                self.receipt_store.commit(&receipt)?,
                            )?;
                            let fresh_terminal = self
                                .catalog
                                .select(&prepared.plan.source)
                                .and_then(|selection| {
                                    ensure_same_release(&selection, &prepared.plan)
                                });
                            let version = self.version_store.observe(
                                &prepared.plan.plugin_id,
                                &prepared.plan.version,
                                &prepared.plan.package_digest,
                            )?;
                            if decide_install_recovery(
                                SavedInstallPhase::ReceiptWritten,
                                fresh_terminal,
                                version,
                                visibility,
                            )? == InstallRecoveryDecision::Succeeded
                            {
                                self.journal.transition(
                                    &stored.operation.id,
                                    OperationState::Succeeded,
                                    "recovered",
                                    None,
                                )?;
                                cleanup_prepared_archive(&self.paths, prepared);
                            } else {
                                self.fail_operation(
                                    &stored.operation.id,
                                    &ManagerError::new(
                                        "install_interrupted",
                                        "receipt recovery did not become exact",
                                    ),
                                );
                            }
                        }
                        InstallRecoveryDecision::ResumeFreshVerification
                        | InstallRecoveryDecision::Failed { .. } => self.fail_operation(
                            &stored.operation.id,
                            &ManagerError::new(
                                "install_interrupted",
                                "operation requires an explicit fresh commit after restart",
                            ),
                        ),
                    }
                }
                _ => self.fail_operation(
                    &stored.operation.id,
                    &ManagerError::new(
                        "install_interrupted",
                        "operation stopped before a recoverable install checkpoint",
                    ),
                ),
            }
            reconciled.push(self.operation(&stored.operation.id)?);
        }
        Ok(reconciled)
    }

    fn build_receipt(
        &self,
        prepared: &PreparedInstall,
        previous: Option<&InstallReceipt>,
    ) -> InstallReceipt {
        InstallReceipt {
            schema_version: INSTALL_RECEIPT_SCHEMA_VERSION,
            plugin_id: prepared.plan.plugin_id.clone(),
            version: prepared.plan.version.clone(),
            package_digest: prepared.plan.package_digest.clone(),
            publisher_key_id: prepared.plan.publisher_key_id.clone(),
            publisher_lineage: prepared.plan.publisher_lineage.clone(),
            target: prepared.plan.target,
            source: InstallSource::Catalog,
            enabled: false,
            granted_permissions: prepared.plan.requested_permissions.clone(),
            native_trust_digest: prepared.plan.native_trust_digest.clone(),
            installed_at_ms: self.clock.now_ms(),
            generation: previous
                .map(|receipt| receipt.generation.saturating_add(1))
                .unwrap_or(1),
            state_schema_version: prepared.facts.state.schema_version,
            rollback_compatible_through: prepared.facts.state.rollback_compatible_through,
            previous: previous.map(InstallReceipt::summary),
        }
    }

    fn operation(&self, id: &str) -> ManagerResult<Operation> {
        Ok(self
            .journal
            .load_with_payload::<serde_json::Value>(id)?
            .operation)
    }

    fn fail_operation(&self, id: &str, error: &ManagerError) {
        let _ = self.journal.transition(
            id,
            OperationState::Failed,
            "failed",
            Some(OperationFailure {
                code: error.code.clone(),
                message: error.message.clone(),
            }),
        );
    }

    fn prepare_install_locked(&self, source: InstallSourceRef) -> ManagerResult<InstallPlan> {
        let release = self.catalog.select(&source)?;
        let payload = ManagerOperationPayload::Preparing {
            source: source.clone(),
        };
        let operation_id =
            self.journal
                .begin_with_payload("install", release.plugin_id.as_str(), &payload)?;
        let result = (|| {
            self.journal.transition_with_payload(
                &operation_id,
                OperationState::Running,
                "catalog",
                &payload,
                None,
            )?;
            self.journal
                .checkpoint(&operation_id, "download", &payload)?;
            let staged = stage_download(
                &self.paths,
                &release.url,
                self.downloader.as_ref(),
                self.download_limits,
            )?;
            if staged.archive_digest != release.archive_digest {
                return Err(ManagerError::new(
                    "archive_digest_mismatch",
                    "downloaded archive does not match the signed catalog",
                ));
            }
            self.journal
                .checkpoint(&operation_id, "archive-digest", &payload)?;
            self.journal
                .checkpoint(&operation_id, "package-signature", &payload)?;
            let facts = self
                .package_engine
                .inspect(&self.paths, &staged.archive, &release)?;
            ensure_facts_match_release(&facts, &release)?;
            self.journal
                .checkpoint(&operation_id, "manifest", &payload)?;
            self.journal
                .checkpoint(&operation_id, "compatibility", &payload)?;
            let current = self.receipt_store.current(&release.plugin_id)?;
            let requested_permissions = facts.requested_permissions.clone();
            let permission_diff = permission_diff(
                current.as_ref(),
                &requested_permissions,
                facts.runtime_kind,
                &facts.archive_digest,
            );
            self.journal
                .checkpoint(&operation_id, "permission-diff", &payload)?;
            let current_schema = current
                .as_ref()
                .map(|receipt| receipt.state_schema_version)
                .unwrap_or(facts.state.schema_version);
            let rollback_available = facts.state.migrations.is_empty()
                && facts.state.rollback_compatible_through <= current_schema;
            let plan = InstallPlan {
                operation_id: operation_id.clone(),
                state: OperationState::WaitingForConsent,
                source,
                plugin_id: release.plugin_id,
                version: release.version,
                target: release.target,
                catalog_sequence: release.catalog_sequence,
                archive_digest: staged.archive_digest.clone(),
                package_digest: staged.archive_digest,
                publisher_key_id: release.publisher_key_id,
                publisher_lineage: release.publisher_lineage,
                permission_diff,
                requested_permissions,
                native_trust_digest: (facts.runtime_kind == RuntimeKind::VerifiedNative)
                    .then(|| facts.archive_digest.clone()),
                rollback_available,
                irreversible_migration: !rollback_available,
            };
            let prepared = PreparedInstall {
                plan: plan.clone(),
                archive: staged.archive,
                facts,
            };
            self.journal.transition_with_payload(
                &operation_id,
                OperationState::WaitingForConsent,
                "consent",
                &ManagerOperationPayload::Install { prepared },
                None,
            )?;
            Ok(plan)
        })();
        if let Err(error) = &result {
            self.fail_operation(&operation_id, error);
        }
        result
    }

    fn commit_install_locked(&self, approval: Approval) -> ManagerResult<InstallReceipt> {
        let stored = self
            .journal
            .load_with_payload::<ManagerOperationPayload>(&approval.operation_id)?;
        if stored.operation.state != OperationState::WaitingForConsent {
            return Err(ManagerError::new(
                "install_not_waiting_for_consent",
                "install operation is not waiting for approval",
            ));
        }
        let ManagerOperationPayload::Install { prepared } = stored.payload else {
            return Err(ManagerError::new(
                "operation_payload",
                "operation does not contain a prepared install",
            ));
        };
        validate_approval(&prepared.plan, &approval)?;
        self.journal.transition_with_payload(
            &prepared.plan.operation_id,
            OperationState::Running,
            "reverify-current-catalog",
            &ManagerOperationPayload::Install {
                prepared: prepared.clone(),
            },
            None,
        )?;
        let result = self.finish_install(&prepared);
        if let Err(error) = &result {
            self.fail_operation(&prepared.plan.operation_id, error);
        }
        result
    }

    fn finish_install(&self, prepared: &PreparedInstall) -> ManagerResult<InstallReceipt> {
        let fresh = self.catalog.select(&prepared.plan.source)?;
        ensure_same_release(&fresh, &prepared.plan)?;
        self.journal.checkpoint(
            &prepared.plan.operation_id,
            "extract",
            &ManagerOperationPayload::Install {
                prepared: prepared.clone(),
            },
        )?;
        let extraction =
            self.package_engine
                .verify_and_extract(&self.paths, &prepared.archive, &fresh)?;
        if extraction.facts != prepared.facts {
            return Err(ManagerError::new(
                "package_facts_changed",
                "fresh package inspection differs from the approved plan",
            ));
        }

        self.journal.checkpoint(
            &prepared.plan.operation_id,
            "migration",
            &ManagerOperationPayload::Install {
                prepared: prepared.clone(),
            },
        )?;
        let previous = self.receipt_store.current(&prepared.plan.plugin_id)?;
        if previous.is_some()
            && self.lifecycle.teardown(&prepared.plan.plugin_id)? == TeardownStatus::Busy
        {
            return Err(ManagerError::new(
                "plugin_teardown_busy",
                "current activation still owns live resources",
            ));
        }
        let current_schema = previous
            .as_ref()
            .map(|receipt| receipt.state_schema_version)
            .unwrap_or(extraction.facts.state.schema_version);
        let migration = self.migrations.migrate(&MigrationRequest {
            package_root: extraction.root.clone(),
            state_root: self.paths.data(&prepared.plan.plugin_id),
            current_schema_version: current_schema,
            target: extraction.facts.state.clone(),
        })?;
        if prepared.plan.rollback_available && !migration.rollback_available {
            return Err(ManagerError::new(
                "migration_plan_changed",
                "migration reversibility differs from the approved plan",
            ));
        }

        if extraction.facts.runtime_kind == RuntimeKind::VerifiedNative {
            self.journal.checkpoint(
                &prepared.plan.operation_id,
                "health-running",
                &ManagerOperationPayload::Install {
                    prepared: prepared.clone(),
                },
            )?;
            let program_relative = extraction.facts.service_entry.clone().ok_or_else(|| {
                ManagerError::new(
                    "health_program",
                    "native package has no declared service entry",
                )
            })?;
            self.health.check(&HealthCheck {
                package_root: extraction.root.clone(),
                program_relative,
                args: vec!["--health-check".into()],
                timeout: Duration::from_secs(15),
                package_digest: prepared.plan.package_digest.clone(),
            })?;
            self.journal.checkpoint(
                &prepared.plan.operation_id,
                "health-passed",
                &ManagerOperationPayload::Install {
                    prepared: prepared.clone(),
                },
            )?;
        }

        self.journal.checkpoint(
            &prepared.plan.operation_id,
            "version-commit",
            &ManagerOperationPayload::Install {
                prepared: prepared.clone(),
            },
        )?;
        let version = reconcile_version_observation(
            &self.version_store,
            &prepared.plan.plugin_id,
            &prepared.plan.version,
            &prepared.plan.package_digest,
            self.version_store.finalize_extracted(
                &extraction.root,
                &prepared.plan.plugin_id,
                &prepared.plan.version,
                &prepared.plan.package_digest,
            )?,
        )?;
        match version {
            VersionVisibility::Exact { .. } => {}
            VersionVisibility::Conflict { .. } => {
                return Err(ManagerError::new(
                    "version_digest_conflict",
                    "the same version is already installed with another digest",
                ));
            }
            VersionVisibility::Absent => {
                return Err(ManagerError::new(
                    "install_interrupted",
                    "immutable package version is not visible after commit",
                ));
            }
        }
        self.journal.checkpoint(
            &prepared.plan.operation_id,
            "version-committed",
            &ManagerOperationPayload::Install {
                prepared: prepared.clone(),
            },
        )?;

        let fresh_before_receipt = self.catalog.select(&prepared.plan.source)?;
        ensure_same_release(&fresh_before_receipt, &prepared.plan)?;
        let mut receipt = self.build_receipt(prepared, previous.as_ref());
        receipt.state_schema_version = migration.schema_version;
        self.journal.checkpoint(
            &prepared.plan.operation_id,
            "receipt-write",
            &ManagerOperationPayload::Install {
                prepared: prepared.clone(),
            },
        )?;
        let receipt_visibility = reconcile_receipt_observation(
            &self.receipt_store,
            &receipt,
            self.receipt_store.commit(&receipt)?,
        )?;
        self.journal.checkpoint(
            &prepared.plan.operation_id,
            "receipt-written",
            &ManagerOperationPayload::Install {
                prepared: prepared.clone(),
            },
        )?;
        let fresh_terminal = self
            .catalog
            .select(&prepared.plan.source)
            .and_then(|selection| ensure_same_release(&selection, &prepared.plan));
        let version_visibility = self.version_store.observe(
            &prepared.plan.plugin_id,
            &prepared.plan.version,
            &prepared.plan.package_digest,
        )?;
        match decide_install_recovery(
            SavedInstallPhase::ReceiptWritten,
            fresh_terminal,
            version_visibility,
            receipt_visibility,
        )? {
            InstallRecoveryDecision::Succeeded => {}
            InstallRecoveryDecision::ResumeFreshVerification
            | InstallRecoveryDecision::ResumeReceiptCommit
            | InstallRecoveryDecision::Failed { .. } => {
                return Err(ManagerError::new(
                    "install_interrupted",
                    "install durability could not be reconciled",
                ));
            }
        }
        self.journal.transition(
            &prepared.plan.operation_id,
            OperationState::Succeeded,
            "succeeded",
            None,
        )?;
        cleanup_prepared_archive(&self.paths, prepared);
        Ok(receipt)
    }

    fn fresh_release_for_receipt(
        &self,
        receipt: &InstallReceipt,
    ) -> ManagerResult<SelectedRelease> {
        let source = InstallSourceRef::Catalog {
            id: receipt.plugin_id.as_str().to_owned(),
            version: Some(receipt.version.to_string()),
        };
        let release = self.catalog.select(&source)?;
        if release.archive_digest != receipt.package_digest
            || release.publisher_key_id != receipt.publisher_key_id
            || release.publisher_lineage != receipt.publisher_lineage
            || release.target != receipt.target
        {
            return Err(ManagerError::new(
                "package_catalog_mismatch",
                "installed receipt no longer matches the current catalog lineage",
            ));
        }
        Ok(release)
    }
}

impl PackageManagerApi for PluginManager {
    fn catalog(&self, query: CatalogQuery) -> ManagerResult<Vec<CatalogItem>> {
        self.catalog.catalog(&query)
    }

    fn info(&self, id: &PluginId) -> ManagerResult<PluginDetails> {
        let mut details = self.catalog.info(id)?;
        details.installed = self.receipt_store.current(id)?;
        Ok(details)
    }

    fn prepare_install(&self, source: InstallSourceRef) -> ManagerResult<InstallPlan> {
        let _manager_lock = ManagerLock::acquire(&self.paths)?;
        self.prepare_install_locked(source)
    }

    fn prepared_install(&self, operation_id: &str) -> ManagerResult<InstallPlan> {
        let stored = self
            .journal
            .load_with_payload::<ManagerOperationPayload>(operation_id)?;
        if stored.operation.state != OperationState::WaitingForConsent {
            return Err(ManagerError::new(
                "install_not_waiting_for_consent",
                "install operation is not waiting for approval",
            ));
        }
        let ManagerOperationPayload::Install { prepared } = stored.payload else {
            return Err(ManagerError::new(
                "operation_payload",
                "operation does not contain a prepared install",
            ));
        };
        Ok(prepared.plan)
    }

    fn commit_install(&self, approval: Approval) -> ManagerResult<InstallReceipt> {
        let _manager_lock = ManagerLock::acquire(&self.paths)?;
        self.commit_install_locked(approval)
    }

    fn update(&self, id: Option<&PluginId>) -> ManagerResult<Vec<Operation>> {
        let _manager_lock = ManagerLock::acquire(&self.paths)?;
        let candidates = if let Some(id) = id {
            vec![self.catalog.info(id)?.item]
        } else {
            self.catalog.catalog(&CatalogQuery::default())?
        };
        let mut operations = Vec::new();
        for item in candidates {
            let Some(current) = self.receipt_store.current(&item.plugin_id)? else {
                continue;
            };
            if item.version <= current.version {
                continue;
            }
            let plan = self.prepare_install_locked(InstallSourceRef::Catalog {
                id: item.plugin_id.as_str().to_owned(),
                version: Some(item.version.to_string()),
            })?;
            operations.push(self.operation(&plan.operation_id)?);
        }
        Ok(operations)
    }

    fn rollback(&self, id: &PluginId, version: Option<&Version>) -> ManagerResult<InstallReceipt> {
        let _manager_lock = ManagerLock::acquire(&self.paths)?;
        let current = self
            .receipt_store
            .current(id)?
            .ok_or_else(|| ManagerError::new("plugin_not_installed", id.as_str()))?;
        let previous = current
            .previous
            .as_ref()
            .filter(|receipt| version.map_or(true, |version| &receipt.version == version))
            .ok_or_else(|| ManagerError::new("rollback_unavailable", "no compatible receipt"))?;
        if previous.native_trust_digest.is_some() {
            return Err(ManagerError::new(
                "rollback_requires_health",
                "native rollback must pass the native health transaction",
            ));
        }
        let source = InstallSourceRef::Catalog {
            id: id.as_str().to_owned(),
            version: Some(previous.version.to_string()),
        };
        let release = self.catalog.select(&source)?;
        if release.archive_digest != previous.package_digest
            || release.publisher_key_id != previous.publisher_key_id
            || release.publisher_lineage != previous.publisher_lineage
        {
            return Err(ManagerError::new(
                "package_revoked",
                "rollback target is absent or no longer trusted",
            ));
        }
        match self
            .version_store
            .observe(id, &previous.version, &previous.package_digest)?
        {
            VersionVisibility::Exact { .. } => {}
            _ => {
                return Err(ManagerError::new(
                    "rollback_unavailable",
                    "rollback package version is not immutable and exact",
                ));
            }
        }
        if self.lifecycle.teardown(id)? == TeardownStatus::Busy {
            return Err(ManagerError::new(
                "plugin_teardown_busy",
                "current activation still owns live resources",
            ));
        }
        let receipt = InstallReceipt {
            schema_version: INSTALL_RECEIPT_SCHEMA_VERSION,
            plugin_id: previous.plugin_id.clone(),
            version: previous.version.clone(),
            package_digest: previous.package_digest.clone(),
            publisher_key_id: previous.publisher_key_id.clone(),
            publisher_lineage: previous.publisher_lineage.clone(),
            target: previous.target,
            source: previous.source,
            enabled: false,
            granted_permissions: previous.granted_permissions.clone(),
            native_trust_digest: previous.native_trust_digest.clone(),
            installed_at_ms: self.clock.now_ms(),
            generation: current.generation.saturating_add(1),
            state_schema_version: previous.state_schema_version,
            rollback_compatible_through: previous.rollback_compatible_through,
            previous: Some(current.summary()),
        };
        let visibility = reconcile_receipt_observation(
            &self.receipt_store,
            &receipt,
            self.receipt_store.commit(&receipt)?,
        )?;
        if !matches!(visibility, ReceiptVisibility::Exact { .. }) {
            return Err(ManagerError::new(
                "install_interrupted",
                "rollback receipt is not durably visible",
            ));
        }
        Ok(receipt)
    }

    fn set_enabled(&self, id: &PluginId, enabled: bool) -> ManagerResult<Operation> {
        let _manager_lock = ManagerLock::acquire(&self.paths)?;
        let mut current = self
            .receipt_store
            .current(id)?
            .ok_or_else(|| ManagerError::new("plugin_not_installed", id.as_str()))?;
        let operation_id = self.journal.begin_with_payload(
            "set-enabled",
            id.as_str(),
            &ManagerOperationPayload::Lifecycle,
        )?;
        self.journal.transition(
            &operation_id,
            OperationState::Running,
            if enabled { "enable" } else { "disable" },
            None,
        )?;
        let result = (|| {
            if !enabled && self.lifecycle.teardown(id)? == TeardownStatus::Busy {
                self.journal.checkpoint(
                    &operation_id,
                    "pending-disable",
                    &ManagerOperationPayload::Lifecycle,
                )?;
                return self.operation(&operation_id);
            }
            if enabled {
                self.fresh_release_for_receipt(&current)?;
            }
            current.previous = Some(current.summary());
            current.generation = current.generation.saturating_add(1);
            current.installed_at_ms = self.clock.now_ms();
            current.enabled = enabled;
            let visibility = reconcile_receipt_observation(
                &self.receipt_store,
                &current,
                self.receipt_store.commit(&current)?,
            )?;
            if !matches!(visibility, ReceiptVisibility::Exact { .. }) {
                return Err(ManagerError::new(
                    "install_interrupted",
                    "enabled receipt is not durably visible",
                ));
            }
            if enabled {
                self.lifecycle.resume_activation(id)?;
            }
            self.journal
                .transition(&operation_id, OperationState::Succeeded, "succeeded", None)?;
            self.operation(&operation_id)
        })();
        if let Err(error) = &result {
            self.fail_operation(&operation_id, error);
        }
        result
    }

    fn uninstall(&self, id: &PluginId) -> ManagerResult<Operation> {
        let _manager_lock = ManagerLock::acquire(&self.paths)?;
        if self.receipt_store.current(id)?.is_none() {
            return Err(ManagerError::new("plugin_not_installed", id.as_str()));
        }
        let operation_id = self.journal.begin_with_payload(
            "uninstall",
            id.as_str(),
            &ManagerOperationPayload::Lifecycle,
        )?;
        self.journal
            .transition(&operation_id, OperationState::Running, "teardown", None)?;
        let result = (|| {
            if self.lifecycle.teardown(id)? == TeardownStatus::Busy {
                self.journal.checkpoint(
                    &operation_id,
                    "pending-disable",
                    &ManagerOperationPayload::Lifecycle,
                )?;
                return self.operation(&operation_id);
            }
            self.lifecycle.uninstall_activation(&self.paths, id)?;
            self.journal
                .transition(&operation_id, OperationState::Succeeded, "succeeded", None)?;
            self.operation(&operation_id)
        })();
        if let Err(error) = &result {
            self.fail_operation(&operation_id, error);
        }
        result
    }

    fn purge(&self, id: &PluginId, confirmation: &str) -> ManagerResult<Operation> {
        let _manager_lock = ManagerLock::acquire(&self.paths)?;
        if confirmation != id.as_str() {
            return Err(ManagerError::new(
                "purge_confirmation",
                "purge confirmation must equal the exact plugin id",
            ));
        }
        if self.receipt_store.current(id)?.is_some() || self.lifecycle.has_live_resources(id)? {
            return Err(ManagerError::new(
                "purge_plugin_active",
                "disable and uninstall the plugin before purge",
            ));
        }
        let operation_id = self.journal.begin_with_payload(
            "purge",
            id.as_str(),
            &ManagerOperationPayload::Lifecycle,
        )?;
        self.journal
            .transition(&operation_id, OperationState::Running, "purge", None)?;
        let result = (|| {
            self.lifecycle.purge_owned_data(&self.paths, id)?;
            self.journal
                .transition(&operation_id, OperationState::Succeeded, "succeeded", None)?;
            self.operation(&operation_id)
        })();
        if let Err(error) = &result {
            self.fail_operation(&operation_id, error);
        }
        result
    }

    fn doctor(&self, id: Option<&PluginId>) -> ManagerResult<DoctorReport> {
        let recoverable_operations = self
            .journal
            .recoverable()?
            .into_iter()
            .filter(|operation| id.map_or(true, |id| operation.plugin_id == id.as_str()))
            .collect::<Vec<_>>();
        let current = id
            .map(|id| self.receipt_store.current(id))
            .transpose()?
            .flatten();
        let mut issues = Vec::new();
        if !recoverable_operations.is_empty() {
            issues.push("package operations require recovery".into());
        }
        Ok(DoctorReport {
            plugin_id: id.cloned(),
            current,
            recoverable_operations,
            issues,
        })
    }
}

fn snapshot_facts(
    facts: &jarvis_package::VerifiedPackageFacts,
    release: &SelectedRelease,
) -> ManagerResult<PackageFactsSnapshot> {
    let manifest = facts.manifest();
    let requested_permissions = manifest
        .permissions
        .iter()
        .map(|permission| GrantedPermission {
            id: permission.id,
            scope: permission.scope.clone(),
            modes: permission.modes.clone(),
        })
        .collect::<Vec<_>>();
    let service_entry = manifest
        .runtime
        .service
        .as_ref()
        .map(|service| service.entry.as_str().to_owned());
    let snapshot = PackageFactsSnapshot {
        plugin_id: facts.metadata().plugin_id.clone(),
        version: facts.metadata().version.clone(),
        target: facts.metadata().target,
        archive_digest: facts.archive_digest().clone(),
        runtime_kind: manifest.runtime.kind,
        service_entry,
        requested_permissions,
        state: facts.metadata().state.clone(),
    };
    ensure_facts_match_release(&snapshot, release)?;
    Ok(snapshot)
}

fn ensure_facts_match_release(
    facts: &PackageFactsSnapshot,
    release: &SelectedRelease,
) -> ManagerResult<()> {
    if facts.plugin_id != release.plugin_id
        || facts.version != release.version
        || facts.target != release.target
        || facts.archive_digest != release.archive_digest
    {
        return Err(ManagerError::new(
            "package_catalog_mismatch",
            "verified package facts differ from the selected catalog release",
        ));
    }
    Ok(())
}

fn ensure_same_release(release: &SelectedRelease, plan: &InstallPlan) -> ManagerResult<()> {
    if release.plugin_id != plan.plugin_id
        || release.version != plan.version
        || release.target != plan.target
        || release.archive_digest != plan.package_digest
        || release.publisher_key_id != plan.publisher_key_id
        || release.publisher_lineage != plan.publisher_lineage
    {
        return Err(ManagerError::new(
            "package_catalog_mismatch",
            "current catalog release differs from the approved install plan",
        ));
    }
    Ok(())
}

fn receipt_matches_prepared(receipt: &InstallReceipt, prepared: &PreparedInstall) -> bool {
    receipt.plugin_id == prepared.plan.plugin_id
        && receipt.version == prepared.plan.version
        && receipt.package_digest == prepared.plan.package_digest
        && receipt.publisher_key_id == prepared.plan.publisher_key_id
        && receipt.publisher_lineage == prepared.plan.publisher_lineage
        && receipt.target == prepared.plan.target
        && receipt.source == InstallSource::Catalog
        && !receipt.enabled
        && receipt.granted_permissions == prepared.plan.requested_permissions
        && receipt.native_trust_digest == prepared.plan.native_trust_digest
        && receipt.state_schema_version == prepared.facts.state.schema_version
        && receipt.rollback_compatible_through == prepared.facts.state.rollback_compatible_through
}

fn cleanup_prepared_archive(paths: &PluginPaths, prepared: &PreparedInstall) {
    if let Ok(parent) = open_fixed_parent(paths, &prepared.archive) {
        let _ = parent.unlink_archive(&prepared.archive.archive_name);
    }
}

fn reconcile_version_observation(
    store: &VersionStore,
    plugin_id: &PluginId,
    version: &Version,
    package_digest: &Digest,
    observation: DurableObservation<VersionVisibility>,
) -> ManagerResult<VersionVisibility> {
    match observation {
        DurableObservation::Confirmed(visibility) => Ok(visibility),
        DurableObservation::DurabilityUnknown(_) => {
            Ok(store.observe(plugin_id, version, package_digest)?)
        }
    }
}

fn reconcile_receipt_observation(
    store: &ReceiptStore,
    receipt: &InstallReceipt,
    observation: DurableObservation<ReceiptVisibility>,
) -> ManagerResult<ReceiptVisibility> {
    match observation {
        DurableObservation::Confirmed(visibility) => Ok(visibility),
        DurableObservation::DurabilityUnknown(_) => Ok(store.observe(receipt)?),
    }
}

fn permission_diff(
    current: Option<&InstallReceipt>,
    requested: &[GrantedPermission],
    runtime: RuntimeKind,
    package_digest: &Digest,
) -> PermissionDiff {
    let previous = current
        .map(|receipt| receipt.granted_permissions.as_slice())
        .unwrap_or(&[]);
    let mut added = requested
        .iter()
        .filter(|permission| !previous.contains(permission))
        .map(|permission| permission_name(permission.id).to_owned())
        .collect::<Vec<_>>();
    if runtime == RuntimeKind::VerifiedNative
        && current.and_then(|receipt| receipt.native_trust_digest.as_ref()) != Some(package_digest)
    {
        added.push("process.native".into());
    }
    let removed = previous
        .iter()
        .filter(|permission| !requested.contains(permission))
        .map(|permission| permission_name(permission.id).to_owned())
        .collect();
    PermissionDiff { added, removed }
}

fn permission_name(permission: PermissionId) -> &'static str {
    match permission {
        PermissionId::ProjectsRead => "projects.read",
        PermissionId::FilesystemMount => "filesystem.mount",
        PermissionId::MemoryRead => "memory.read",
        PermissionId::MemoryProposeWrite => "memory.propose-write",
        PermissionId::NotificationsPublish => "notifications.publish",
        PermissionId::CredentialsRequest => "credentials.request",
        PermissionId::ProcessVmProvider => "process.vm-provider",
        PermissionId::ChatComposeContribute => "chat.compose.contribute",
        PermissionId::ChatComposerTextRead => "chat.composer.text.read",
        PermissionId::ProjectsContribute => "projects.contribute",
    }
}

fn package_error(error: jarvis_package::PackageError) -> ManagerError {
    ManagerError::new(error.code(), error.to_string())
}

fn saved_phase(phase: &str) -> SavedInstallPhase {
    match phase {
        "extract" => SavedInstallPhase::Extracted,
        "migration" | "health-running" => SavedInstallPhase::Migrated,
        "health-passed" | "version-commit" => SavedInstallPhase::HealthPassed,
        "version-committed" | "receipt-write" => SavedInstallPhase::VersionCommitted,
        "receipt-written" | "recovered" => SavedInstallPhase::ReceiptWritten,
        _ => SavedInstallPhase::Prepared,
    }
}
