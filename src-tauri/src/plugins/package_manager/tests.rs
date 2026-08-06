use std::fs;
use std::ops::Deref;
use std::os::unix::fs::symlink;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jarvis_plugin_protocol::manifest::{Digest, PluginId, RuntimeKind, StateDeclaration};
use jarvis_plugin_protocol::operation::OperationState;
use jarvis_plugin_protocol::package::PackageTarget;
use semver::Version;

use super::consent::{validate_approval, Approval, PermissionDiff};
use super::downloader::{stage_download, DownloadLimits, Downloader};
use super::health::{HealthCheck, HealthRunner, NativeHealthRunner};
use super::manager::{
    CatalogItem, CatalogProvider, CatalogQuery, Clock, InstallPlan, InstallSourceRef,
    LifecycleHost, ManagerError, PackageEngine, PackageFactsSnapshot, PackageManagerApi,
    PluginDetails, PluginManager, SelectedRelease, TeardownStatus, VerifiedExtraction,
};
use super::migration::{
    validate_migration_set, MigrationDocument, MigrationOperation, MigrationOutcome,
    MigrationRequest, MigrationRunner,
};
use super::operation::OperationJournal;
use super::paths::PluginPaths;
use super::quarantine::{open_fixed_parent, QuarantineArchiveRef};
use super::receipt::{ReceiptStore, ReceiptVisibility, VersionVisibility};
use super::recovery::{decide_install_recovery, InstallRecoveryDecision, SavedInstallPhase};
use crate::plugins::trust::package::AllowPackageVerifier;

static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);

fn digest(fill: char) -> Digest {
    Digest::new(format!("sha256:{}", fill.to_string().repeat(64))).unwrap()
}

struct TestPaths {
    paths: PluginPaths,
    root: PathBuf,
}

impl Deref for TestPaths {
    type Target = PluginPaths;

    fn deref(&self) -> &Self::Target {
        &self.paths
    }
}

impl Drop for TestPaths {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn temp_paths(label: &str) -> TestPaths {
    let suffix = NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed);
    let temp = fs::canonicalize(std::env::temp_dir()).unwrap();
    let root = temp.join(format!("jarvis-a6-{label}-{}-{suffix}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    TestPaths {
        paths: PluginPaths::new(root.join("profile")),
        root,
    }
}

fn native_plan() -> InstallPlan {
    InstallPlan {
        operation_id: "op-native".into(),
        state: OperationState::WaitingForConsent,
        source: InstallSourceRef::Catalog {
            id: "dev.example.native".into(),
            version: Some("1.0.0".into()),
        },
        plugin_id: PluginId::new("dev.example.native").unwrap(),
        version: Version::parse("1.0.0").unwrap(),
        target: PackageTarget::DarwinArm64,
        catalog_sequence: 7,
        archive_digest: digest('a'),
        package_digest: digest('a'),
        publisher_key_id: "example.release:1".into(),
        publisher_lineage: "example.release".into(),
        permission_diff: PermissionDiff {
            added: vec!["process.native".into()],
            removed: Vec::new(),
        },
        requested_permissions: Vec::new(),
        native_trust_digest: Some(digest('a')),
        rollback_available: true,
        irreversible_migration: false,
    }
}

#[derive(Clone)]
struct StaticCatalog {
    release: SelectedRelease,
}

impl CatalogProvider for StaticCatalog {
    fn catalog(&self, _query: &CatalogQuery) -> Result<Vec<CatalogItem>, ManagerError> {
        Ok(vec![CatalogItem {
            plugin_id: self.release.plugin_id.clone(),
            name: "Native fixture".into(),
            version: self.release.version.clone(),
            target: self.release.target,
            archive_digest: self.release.archive_digest.clone(),
        }])
    }

    fn info(&self, _id: &PluginId) -> Result<PluginDetails, ManagerError> {
        Ok(PluginDetails {
            item: self.catalog(&CatalogQuery::default())?.remove(0),
            publisher_key_id: self.release.publisher_key_id.clone(),
            publisher_lineage: self.release.publisher_lineage.clone(),
            installed: None,
        })
    }

    fn select(&self, _source: &InstallSourceRef) -> Result<SelectedRelease, ManagerError> {
        Ok(self.release.clone())
    }
}

struct CountingPackageEngine {
    inspect_calls: Arc<AtomicUsize>,
    extract_calls: Arc<AtomicUsize>,
    facts: PackageFactsSnapshot,
}

impl PackageEngine for CountingPackageEngine {
    fn inspect(
        &self,
        _paths: &PluginPaths,
        _archive: &QuarantineArchiveRef,
        _release: &SelectedRelease,
    ) -> Result<PackageFactsSnapshot, ManagerError> {
        self.inspect_calls.fetch_add(1, Ordering::Relaxed);
        Ok(self.facts.clone())
    }

    fn verify_and_extract(
        &self,
        paths: &PluginPaths,
        _archive: &QuarantineArchiveRef,
        _release: &SelectedRelease,
    ) -> Result<VerifiedExtraction, ManagerError> {
        self.inspect_calls.fetch_add(1, Ordering::Relaxed);
        let sequence = self.extract_calls.fetch_add(1, Ordering::Relaxed);
        let root = paths
            .quarantine_root()
            .join(format!(".fake-extract-{sequence}"));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.join("plugin.json"), b"{}").unwrap();
        fs::set_permissions(root.join("plugin.json"), fs::Permissions::from_mode(0o600)).unwrap();
        Ok(VerifiedExtraction {
            root,
            facts: self.facts.clone(),
        })
    }
}

struct SameSchemaMigration;

impl MigrationRunner for SameSchemaMigration {
    fn migrate(&self, request: &MigrationRequest) -> Result<MigrationOutcome, ManagerError> {
        Ok(MigrationOutcome {
            schema_version: request.target.schema_version,
            rollback_available: true,
        })
    }
}

struct CountingHealth(Arc<AtomicUsize>);

impl HealthRunner for CountingHealth {
    fn check(&self, _request: &HealthCheck) -> Result<super::health::HealthReport, ManagerError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(super::health::HealthReport {
            elapsed: Duration::ZERO,
        })
    }
}

struct IdleLifecycle;

impl LifecycleHost for IdleLifecycle {
    fn teardown(&self, _plugin_id: &PluginId) -> Result<TeardownStatus, ManagerError> {
        Ok(TeardownStatus::Complete)
    }

    fn uninstall_activation(
        &self,
        _paths: &PluginPaths,
        _plugin_id: &PluginId,
    ) -> Result<(), ManagerError> {
        Ok(())
    }

    fn has_live_resources(&self, _plugin_id: &PluginId) -> Result<bool, ManagerError> {
        Ok(false)
    }

    fn purge_owned_data(
        &self,
        _paths: &PluginPaths,
        _plugin_id: &PluginId,
    ) -> Result<(), ManagerError> {
        Ok(())
    }
}

struct RecordingLifecycle {
    paths: PluginPaths,
    teardown_generations: Mutex<Vec<Option<u64>>>,
}

impl RecordingLifecycle {
    fn new(paths: PluginPaths) -> Self {
        Self {
            paths,
            teardown_generations: Mutex::new(Vec::new()),
        }
    }
}

impl LifecycleHost for RecordingLifecycle {
    fn teardown(&self, plugin_id: &PluginId) -> Result<TeardownStatus, ManagerError> {
        let generation = ReceiptStore::new(self.paths.clone())
            .current(plugin_id)
            .unwrap()
            .map(|receipt| receipt.generation);
        self.teardown_generations.lock().unwrap().push(generation);
        Ok(TeardownStatus::Complete)
    }

    fn uninstall_activation(
        &self,
        _paths: &PluginPaths,
        _plugin_id: &PluginId,
    ) -> Result<(), ManagerError> {
        Ok(())
    }

    fn has_live_resources(&self, _plugin_id: &PluginId) -> Result<bool, ManagerError> {
        Ok(false)
    }

    fn purge_owned_data(
        &self,
        _paths: &PluginPaths,
        _plugin_id: &PluginId,
    ) -> Result<(), ManagerError> {
        Ok(())
    }
}

struct FixedClock;

impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        42
    }
}

fn ui_manager(paths: &PluginPaths, lifecycle: Arc<dyn LifecycleHost>) -> (PluginManager, PluginId) {
    let plugin_id = PluginId::new("dev.example.ui").unwrap();
    let package_digest =
        Digest::new("sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
            .unwrap();
    let release = SelectedRelease::new(
        7,
        plugin_id.clone(),
        Version::parse("1.0.0").unwrap(),
        PackageTarget::DarwinArm64,
        "https://plugins.example.test/ui.jarvis-plugin".into(),
        package_digest.clone(),
        "example.release:1".into(),
        "example.release".into(),
        Arc::new(AllowPackageVerifier),
    );
    let facts = PackageFactsSnapshot {
        plugin_id: plugin_id.clone(),
        version: release.version.clone(),
        target: release.target,
        archive_digest: package_digest,
        runtime_kind: RuntimeKind::UiOnly,
        service_entry: None,
        requested_permissions: Vec::new(),
        state: StateDeclaration {
            schema_version: 1,
            migrations: Vec::new(),
            rollback_compatible_through: 1,
        },
    };
    let manager = PluginManager::new(
        paths.clone(),
        Arc::new(StaticCatalog { release }),
        Arc::new(StaticDownloader(b"abc")),
        Arc::new(CountingPackageEngine {
            inspect_calls: Arc::new(AtomicUsize::new(0)),
            extract_calls: Arc::new(AtomicUsize::new(0)),
            facts,
        }),
        Arc::new(SameSchemaMigration),
        Arc::new(CountingHealth(Arc::new(AtomicUsize::new(0)))),
        lifecycle,
        Arc::new(FixedClock),
    )
    .unwrap();
    (manager, plugin_id)
}

fn approve_ui(plan: &InstallPlan) -> Approval {
    Approval {
        operation_id: plan.operation_id.clone(),
        package_digest: plan.package_digest.clone(),
        granted_permissions: plan.requested_permissions.clone(),
        native_trust_digest: None,
        approve_irreversible_migration: true,
    }
}

#[test]
fn replacement_and_rollback_teardown_the_old_generation_before_receipt_commit() {
    let paths = temp_paths("manager-replacement-lifecycle");
    let lifecycle = Arc::new(RecordingLifecycle::new(paths.paths.clone()));
    let (manager, plugin_id) = ui_manager(&paths, lifecycle.clone());

    let first_plan = manager
        .prepare_install(InstallSourceRef::Catalog {
            id: plugin_id.as_str().to_owned(),
            version: Some("1.0.0".into()),
        })
        .unwrap();
    let first = manager.commit_install(approve_ui(&first_plan)).unwrap();
    assert_eq!(first.generation, 1);
    assert!(lifecycle.teardown_generations.lock().unwrap().is_empty());

    let replacement_plan = manager
        .prepare_install(InstallSourceRef::Catalog {
            id: plugin_id.as_str().to_owned(),
            version: Some("1.0.0".into()),
        })
        .unwrap();
    let replacement = manager
        .commit_install(approve_ui(&replacement_plan))
        .unwrap();
    assert_eq!(replacement.generation, 2);
    assert_eq!(
        lifecycle.teardown_generations.lock().unwrap().as_slice(),
        &[Some(1)],
        "replacement teardown must observe the old durable receipt"
    );

    let rolled_back = manager.rollback(&plugin_id, None).unwrap();
    assert_eq!(rolled_back.generation, 3);
    assert_eq!(
        lifecycle.teardown_generations.lock().unwrap().as_slice(),
        &[Some(1), Some(2)],
        "rollback teardown must happen before generation 3 becomes visible"
    );
}

#[test]
fn recovery_tears_down_and_terminally_fails_interrupted_lifecycle_operation() {
    let paths = temp_paths("manager-lifecycle-recovery");
    paths.prepare().unwrap();
    let journal = OperationJournal::open(paths.operations_db()).unwrap();
    let operation_id = journal
        .begin_with_payload(
            "set-enabled",
            "dev.example.ui",
            &serde_json::json!({"stage":"lifecycle"}),
        )
        .unwrap();
    journal
        .transition(&operation_id, OperationState::Running, "disable", None)
        .unwrap();
    drop(journal);
    let lifecycle = Arc::new(RecordingLifecycle::new(paths.paths.clone()));

    let (_manager, _) = ui_manager(&paths, lifecycle.clone());

    assert_eq!(
        lifecycle.teardown_generations.lock().unwrap().as_slice(),
        &[None]
    );
    let recovered = OperationJournal::open(paths.operations_db())
        .unwrap()
        .load_with_payload::<serde_json::Value>(&operation_id)
        .unwrap();
    assert_eq!(recovered.operation.state, OperationState::Failed);
    assert_eq!(recovered.operation.phase, "failed");
}

#[test]
fn native_install_cannot_extract_or_execute_before_exact_digest_consent() {
    let paths = temp_paths("manager-consent");
    let package_digest =
        Digest::new("sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
            .unwrap();
    let release = SelectedRelease::new(
        7,
        PluginId::new("dev.example.native").unwrap(),
        Version::parse("1.0.0").unwrap(),
        PackageTarget::DarwinArm64,
        "https://plugins.example.test/native.jarvis-plugin".into(),
        package_digest.clone(),
        "example.release:1".into(),
        "example.release".into(),
        Arc::new(AllowPackageVerifier),
    );
    let facts = PackageFactsSnapshot {
        plugin_id: release.plugin_id.clone(),
        version: release.version.clone(),
        target: release.target,
        archive_digest: package_digest.clone(),
        runtime_kind: RuntimeKind::VerifiedNative,
        service_entry: Some("bin/health".into()),
        requested_permissions: Vec::new(),
        state: StateDeclaration {
            schema_version: 1,
            migrations: Vec::new(),
            rollback_compatible_through: 1,
        },
    };
    let inspect_calls = Arc::new(AtomicUsize::new(0));
    let extract_calls = Arc::new(AtomicUsize::new(0));
    let health_calls = Arc::new(AtomicUsize::new(0));
    let manager = PluginManager::new(
        paths.paths.clone(),
        Arc::new(StaticCatalog { release }),
        Arc::new(StaticDownloader(b"abc")),
        Arc::new(CountingPackageEngine {
            inspect_calls: inspect_calls.clone(),
            extract_calls: extract_calls.clone(),
            facts,
        }),
        Arc::new(SameSchemaMigration),
        Arc::new(CountingHealth(health_calls.clone())),
        Arc::new(IdleLifecycle),
        Arc::new(FixedClock),
    )
    .unwrap();

    let prepared = manager
        .prepare_install(InstallSourceRef::Catalog {
            id: "dev.example.native".into(),
            version: Some("1.0.0".into()),
        })
        .unwrap();
    assert_eq!(prepared.state, OperationState::WaitingForConsent);
    assert!(prepared
        .permission_diff
        .added
        .contains(&"process.native".into()));
    assert_eq!(inspect_calls.load(Ordering::Relaxed), 1);
    assert_eq!(extract_calls.load(Ordering::Relaxed), 0);
    assert_eq!(health_calls.load(Ordering::Relaxed), 0);
    assert!(!paths.versions(&prepared.plugin_id).exists());

    let wrong = Approval::native(prepared.operation_id.clone(), digest('b'));
    assert_eq!(
        manager.commit_install(wrong).unwrap_err().code(),
        "native_digest_consent_mismatch"
    );
    assert_eq!(extract_calls.load(Ordering::Relaxed), 0);
    assert_eq!(health_calls.load(Ordering::Relaxed), 0);

    let receipt = manager
        .commit_install(Approval::native(
            prepared.operation_id,
            prepared.package_digest.clone(),
        ))
        .unwrap();
    assert_eq!(receipt.package_digest, prepared.package_digest);
    assert!(!receipt.enabled);
    assert_eq!(inspect_calls.load(Ordering::Relaxed), 2);
    assert_eq!(extract_calls.load(Ordering::Relaxed), 1);
    assert_eq!(health_calls.load(Ordering::Relaxed), 1);
    assert_eq!(fs::read_dir(paths.quarantine_root()).unwrap().count(), 0);
}

#[test]
fn native_digest_consent_is_bound_to_operation_and_exact_digest() {
    let plan = native_plan();
    let wrong = Approval::native(plan.operation_id.clone(), digest('b'));

    assert_eq!(
        validate_approval(&plan, &wrong).unwrap_err().code(),
        "native_digest_consent_mismatch"
    );

    let approved = Approval::native(plan.operation_id.clone(), plan.package_digest.clone());
    validate_approval(&plan, &approved).unwrap();
}

#[test]
fn irreversible_migration_requires_an_independent_approval_bit() {
    let mut plan = native_plan();
    plan.irreversible_migration = true;
    plan.rollback_available = false;
    let approval = Approval::native(plan.operation_id.clone(), plan.package_digest.clone());

    assert_eq!(
        validate_approval(&plan, &approval).unwrap_err().code(),
        "irreversible_migration_consent_required"
    );

    validate_approval(&plan, &approval.approve_irreversible()).unwrap();
}

struct StaticDownloader(&'static [u8]);

impl Downloader for StaticDownloader {
    fn download(
        &self,
        _url: &str,
        output: &mut dyn std::io::Write,
        _deadline: Duration,
    ) -> Result<(), ManagerError> {
        output
            .write_all(self.0)
            .map_err(|error| ManagerError::new("download_io", error.to_string()))
    }
}

#[test]
fn staged_download_is_private_single_link_and_reopens_by_recorded_inode() {
    let paths = temp_paths("download");
    paths.prepare().unwrap();
    let staged = stage_download(
        &paths,
        "https://plugins.example.test/example.jarvis-plugin",
        &StaticDownloader(b"abc"),
        DownloadLimits {
            max_bytes: 1024,
            deadline: Duration::from_secs(1),
        },
    )
    .unwrap();

    assert_eq!(
        staged.archive_digest.as_str(),
        "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let held = open_fixed_parent(&paths, &staged.archive).unwrap();
    let archive = held.open_archive(&staged.archive).unwrap();
    let metadata = archive.metadata().unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert_eq!(metadata.nlink(), 1);
}

#[test]
fn oversized_download_is_removed_from_quarantine() {
    let paths = temp_paths("download-limit");
    paths.prepare().unwrap();
    let error = stage_download(
        &paths,
        "https://plugins.example.test/example.jarvis-plugin",
        &StaticDownloader(b"too large"),
        DownloadLimits {
            max_bytes: 3,
            deadline: Duration::from_secs(1),
        },
    )
    .unwrap_err();

    assert_eq!(error.code(), "download_io");
    assert_eq!(fs::read_dir(paths.quarantine_root()).unwrap().count(), 0);
}

#[test]
fn quarantine_locator_round_trip_never_serializes_a_path_or_file_descriptor() {
    let paths = temp_paths("locator");
    paths.prepare().unwrap();
    let staged = stage_download(
        &paths,
        "https://plugins.example.test/example.jarvis-plugin",
        &StaticDownloader(b"archive"),
        DownloadLimits::default(),
    )
    .unwrap();

    let json = serde_json::to_value(&staged.archive).unwrap();
    assert_no_runtime_locator_fields(&json);
    let encoded = serde_json::to_string(&json).unwrap();
    assert!(!encoded.contains(paths.profile().to_string_lossy().as_ref()));
    let decoded: QuarantineArchiveRef = serde_json::from_value(json).unwrap();
    assert_eq!(decoded, staged.archive);
}

fn assert_no_runtime_locator_fields(value: &serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, child) in fields {
                assert!(
                    !matches!(key.as_str(), "fd" | "path" | "parentPath" | "archivePath"),
                    "runtime locator field {key} must not be persisted"
                );
                assert_no_runtime_locator_fields(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                assert_no_runtime_locator_fields(item);
            }
        }
        _ => {}
    }
}

#[test]
fn quarantine_parent_symlink_and_owned_decoy_are_rejected_before_archive_open() {
    let symlink_paths = temp_paths("parent-symlink");
    symlink_paths.prepare().unwrap();
    let symlink_staged = stage_download(
        &symlink_paths,
        "https://plugins.example.test/example.jarvis-plugin",
        &StaticDownloader(b"archive"),
        DownloadLimits::default(),
    )
    .unwrap();
    let quarantine = symlink_paths.quarantine_root();
    let moved = symlink_paths.plugins_root().join(".quarantine-real");
    fs::rename(&quarantine, &moved).unwrap();
    symlink(&moved, &quarantine).unwrap();
    assert_eq!(
        open_fixed_parent(&symlink_paths, &symlink_staged.archive)
            .unwrap_err()
            .code(),
        "quarantine_parent_unsafe"
    );

    let decoy_paths = temp_paths("parent-decoy");
    decoy_paths.prepare().unwrap();
    let decoy_staged = stage_download(
        &decoy_paths,
        "https://plugins.example.test/example.jarvis-plugin",
        &StaticDownloader(b"archive"),
        DownloadLimits::default(),
    )
    .unwrap();
    let quarantine = decoy_paths.quarantine_root();
    let moved = decoy_paths.plugins_root().join(".quarantine-real");
    fs::rename(&quarantine, &moved).unwrap();
    fs::create_dir(&quarantine).unwrap();
    fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700)).unwrap();
    let decoy_archive = quarantine.join(&decoy_staged.archive.archive_name);
    fs::write(&decoy_archive, b"archive").unwrap();
    fs::set_permissions(&decoy_archive, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        open_fixed_parent(&decoy_paths, &decoy_staged.archive)
            .unwrap_err()
            .code(),
        "quarantine_parent_replaced"
    );
}

#[test]
fn held_parent_keeps_archive_lookup_on_recorded_inode_after_path_swap() {
    let paths = temp_paths("held-parent");
    paths.prepare().unwrap();
    let staged = stage_download(
        &paths,
        "https://plugins.example.test/example.jarvis-plugin",
        &StaticDownloader(b"recorded"),
        DownloadLimits::default(),
    )
    .unwrap();
    let held = open_fixed_parent(&paths, &staged.archive).unwrap();
    let quarantine = paths.quarantine_root();
    let moved = paths.plugins_root().join(".quarantine-real");
    fs::rename(&quarantine, &moved).unwrap();
    fs::create_dir(&quarantine).unwrap();
    fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700)).unwrap();
    let decoy = quarantine.join(&staged.archive.archive_name);
    fs::write(&decoy, b"decoy___").unwrap();
    fs::set_permissions(&decoy, fs::Permissions::from_mode(0o600)).unwrap();

    let mut archive = held.open_archive(&staged.archive).unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut archive, &mut bytes).unwrap();
    assert_eq!(bytes, b"recorded");
}

#[test]
fn valid_looking_archive_replacement_is_rejected_by_inode() {
    let paths = temp_paths("archive-replacement");
    paths.prepare().unwrap();
    let staged = stage_download(
        &paths,
        "https://plugins.example.test/example.jarvis-plugin",
        &StaticDownloader(b"recorded"),
        DownloadLimits::default(),
    )
    .unwrap();
    let archive = paths.quarantine_root().join(&staged.archive.archive_name);
    let moved = paths.quarantine_root().join("recorded-away");
    fs::rename(&archive, moved).unwrap();
    fs::write(&archive, b"recorded").unwrap();
    fs::set_permissions(&archive, fs::Permissions::from_mode(0o600)).unwrap();

    let held = open_fixed_parent(&paths, &staged.archive).unwrap();
    assert_eq!(
        held.open_archive(&staged.archive).unwrap_err().code(),
        "quarantine_archive_replaced"
    );
}

#[test]
fn migration_subset_rejects_attach_extension_absolute_paths_and_graph_gaps() {
    for sql in [
        "ATTACH DATABASE ?1 AS escaped",
        "SELECT load_extension(?1)",
        "CREATE TRIGGER escaped AFTER INSERT ON items BEGIN SELECT 1; END",
    ] {
        let documents = vec![MigrationDocument::new(
            1,
            2,
            true,
            vec![MigrationOperation::Sqlite {
                database: "state/plugin.sqlite3".into(),
                statement: sql.into(),
                parameters: Vec::new(),
            }],
        )];
        assert!(validate_migration_set(1, 2, &documents).is_err(), "{sql}");
    }

    let absolute = vec![MigrationDocument::new(
        1,
        2,
        true,
        vec![MigrationOperation::JsonDelete {
            path: "/Users/example/.ssh/config".into(),
            pointer: "/key".into(),
        }],
    )];
    assert_eq!(
        validate_migration_set(1, 2, &absolute).unwrap_err().code(),
        "migration_path"
    );

    let gap = vec![MigrationDocument::new(1, 2, true, Vec::new())];
    assert_eq!(
        validate_migration_set(1, 3, &gap).unwrap_err().code(),
        "migration_graph_gap"
    );
}

#[test]
fn migration_subset_accepts_a_contiguous_declarative_chain() {
    let documents = vec![
        MigrationDocument::new(
            1,
            2,
            true,
            vec![MigrationOperation::JsonSet {
                path: "settings/preferences.json".into(),
                pointer: "/theme".into(),
                value: serde_json::json!("dark"),
            }],
        ),
        MigrationDocument::new(
            2,
            3,
            true,
            vec![MigrationOperation::Sqlite {
                database: "state/plugin.sqlite3".into(),
                statement: "UPDATE settings SET value = ?1 WHERE key = ?2".into(),
                parameters: vec![serde_json::json!("dark"), serde_json::json!("theme")],
            }],
        ),
    ];
    let outcome = validate_migration_set(1, 3, &documents).unwrap();
    assert_eq!(outcome.schema_version, 3);
    assert!(outcome.rollback_available);
}

#[test]
fn native_health_rejects_parent_traversal_before_spawn() {
    let error = NativeHealthRunner
        .check(&HealthCheck {
            package_root: PathBuf::from("/verified/package"),
            program_relative: "../escape".into(),
            args: Vec::new(),
            timeout: Duration::from_secs(1),
            package_digest: digest('a'),
        })
        .unwrap_err();
    assert_eq!(error.code(), "health_program");
}

#[test]
fn native_health_never_executes_without_an_exact_exec_primitive() {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = temp_paths("native-health-exact-exec");
    let root = fixture.root.join("package");
    fs::create_dir_all(&root).unwrap();
    let program = root.join("health.sh");
    let marker = root.join("executed");
    fs::write(
        &program,
        format!("#!/bin/sh\nprintf executed > '{}'\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();

    let error = NativeHealthRunner
        .check(&HealthCheck {
            package_root: root.clone(),
            program_relative: "health.sh".into(),
            args: Vec::new(),
            timeout: Duration::from_secs(1),
            package_digest: digest('a'),
        })
        .unwrap_err();

    assert_eq!(error.code(), "health_exact_exec_unavailable");
    assert!(
        !marker.exists(),
        "unverified native health code was executed"
    );
}

#[test]
fn recovery_requires_fresh_trust_before_exact_storage_can_succeed() {
    let expected_id = PluginId::new("dev.example.echo").unwrap();
    let expected_version = Version::parse("1.0.0").unwrap();
    let expected_digest = digest('a');
    let version = VersionVisibility::Exact {
        plugin_id: expected_id.clone(),
        version: expected_version.clone(),
        package_digest: expected_digest.clone(),
    };
    let receipt = ReceiptVisibility::Exact {
        plugin_id: expected_id,
        generation: 1,
        package_digest: expected_digest,
    };

    let rejected = decide_install_recovery(
        SavedInstallPhase::ReceiptWritten,
        Err(ManagerError::new("package_revoked", "revoked")),
        version.clone(),
        receipt.clone(),
    )
    .unwrap_err();
    assert_eq!(rejected.code(), "package_revoked");

    assert_eq!(
        decide_install_recovery(SavedInstallPhase::ReceiptWritten, Ok(()), version, receipt,)
            .unwrap(),
        InstallRecoveryDecision::Succeeded
    );
}

#[test]
fn recovery_does_not_guess_health_or_activation_from_partial_visibility() {
    let interrupted = decide_install_recovery(
        SavedInstallPhase::Extracted,
        Ok(()),
        VersionVisibility::Absent,
        ReceiptVisibility::Absent,
    )
    .unwrap();
    assert_eq!(
        interrupted,
        InstallRecoveryDecision::Failed {
            code: "install_interrupted"
        }
    );

    let resume = decide_install_recovery(
        SavedInstallPhase::HealthPassed,
        Ok(()),
        VersionVisibility::Exact {
            plugin_id: PluginId::new("dev.example.echo").unwrap(),
            version: Version::parse("1.0.0").unwrap(),
            package_digest: digest('a'),
        },
        ReceiptVisibility::Absent,
    )
    .unwrap();
    assert_eq!(resume, InstallRecoveryDecision::ResumeReceiptCommit);
}
