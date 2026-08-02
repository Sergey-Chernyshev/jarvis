use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use jarvis_plugin_protocol::manifest::{Digest, PluginId};
use jarvis_plugin_protocol::receipt::InstallReceipt;
use semver::Version;
use uuid::Uuid;

use super::paths::{ensure_real_directory, PluginPaths};
use super::{DurableObservation, StorageError};

#[derive(Clone, Copy, Debug)]
pub(crate) enum StorageFailpoint {
    AfterVersionRename,
    VersionParentSync,
    AfterReceiptRename,
    ReceiptParentSync,
}

#[derive(Debug, Default)]
pub(crate) struct StorageFailpoints {
    after_version_rename: AtomicBool,
    version_parent_sync: AtomicBool,
    after_receipt_rename: AtomicBool,
    receipt_parent_sync: AtomicBool,
}

impl StorageFailpoints {
    #[cfg(test)]
    fn arm(&self, failpoint: StorageFailpoint) {
        self.flag(failpoint).store(true, Ordering::SeqCst);
    }

    fn take(&self, failpoint: StorageFailpoint) -> bool {
        self.flag(failpoint).swap(false, Ordering::SeqCst)
    }

    fn flag(&self, failpoint: StorageFailpoint) -> &AtomicBool {
        match failpoint {
            StorageFailpoint::AfterVersionRename => &self.after_version_rename,
            StorageFailpoint::VersionParentSync => &self.version_parent_sync,
            StorageFailpoint::AfterReceiptRename => &self.after_receipt_rename,
            StorageFailpoint::ReceiptParentSync => &self.receipt_parent_sync,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VersionVisibility {
    Exact {
        plugin_id: PluginId,
        version: Version,
        package_digest: Digest,
    },
    Absent,
    Conflict {
        package_digest: Digest,
    },
}

#[derive(Debug)]
pub struct VersionStore {
    paths: PluginPaths,
    failpoints: Arc<StorageFailpoints>,
}

impl VersionStore {
    pub fn new(paths: PluginPaths) -> Self {
        Self {
            paths,
            failpoints: Arc::new(StorageFailpoints::default()),
        }
    }

    #[cfg(test)]
    fn with_failpoints(paths: PluginPaths, failpoints: Arc<StorageFailpoints>) -> Self {
        Self { paths, failpoints }
    }

    pub fn finalize_extracted(
        &self,
        extracted: &Path,
        plugin_id: &PluginId,
        version: &Version,
        package_digest: &Digest,
    ) -> Result<DurableObservation<VersionVisibility>, StorageError> {
        let extracted_metadata = fs::symlink_metadata(extracted).map_err(|error| {
            StorageError::new(
                "plugin_version_source",
                format!("cannot inspect {}: {error}", extracted.display()),
            )
        })?;
        if extracted_metadata.file_type().is_symlink() || !extracted_metadata.is_dir() {
            return Err(StorageError::new(
                "plugin_version_source",
                format!("{} is not a real extraction directory", extracted.display()),
            ));
        }
        self.paths.prepare_plugin(plugin_id.as_str())?;
        let existing = self.observe(plugin_id, version, package_digest)?;
        if existing != VersionVisibility::Absent {
            return Ok(DurableObservation::Confirmed(existing));
        }

        let version_parent = self
            .paths
            .versions(plugin_id.as_str())
            .join(version.to_string());
        ensure_real_directory(&version_parent, 0o700)?;
        make_tree_immutable(extracted)?;
        // Darwin requires the moved directory itself to be writable while its
        // `..` entry changes across parents. Its contents remain immutable;
        // the fixed destination is restored to 0555 immediately after rename.
        fs::set_permissions(extracted, fs::Permissions::from_mode(0o700)).map_err(|error| {
            StorageError::new(
                "plugin_version_permissions",
                format!("cannot prepare {} for rename: {error}", extracted.display()),
            )
        })?;
        let destination = version_parent.join(package_digest.as_str());
        fs::rename(extracted, &destination).map_err(|error| {
            StorageError::new(
                "plugin_version_rename",
                format!(
                    "cannot rename {} to {}: {error}",
                    extracted.display(),
                    destination.display()
                ),
            )
        })?;
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o555)).map_err(|error| {
            StorageError::new(
                "plugin_version_permissions",
                format!("cannot protect {}: {error}", destination.display()),
            )
        })?;

        if self.failpoints.take(StorageFailpoint::AfterVersionRename) {
            return self
                .observe(plugin_id, version, package_digest)
                .map(DurableObservation::DurabilityUnknown);
        }

        let sync_result = (|| {
            fs::set_permissions(&version_parent, fs::Permissions::from_mode(0o555))?;
            if self.failpoints.take(StorageFailpoint::VersionParentSync) {
                return Err(std::io::Error::other(
                    "injected version destination-parent sync failure",
                ));
            }
            File::open(&version_parent)?.sync_all()
        })();
        let visibility = self.observe(plugin_id, version, package_digest)?;
        if sync_result.is_err() {
            Ok(DurableObservation::DurabilityUnknown(visibility))
        } else {
            Ok(DurableObservation::Confirmed(visibility))
        }
    }

    pub fn observe(
        &self,
        plugin_id: &PluginId,
        version: &Version,
        package_digest: &Digest,
    ) -> Result<VersionVisibility, StorageError> {
        let version_parent = self
            .paths
            .versions(plugin_id.as_str())
            .join(version.to_string());
        let exact = version_parent.join(package_digest.as_str());
        match fs::symlink_metadata(&exact) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(StorageError::new(
                    "plugin_version_type",
                    format!("{} is not a real version directory", exact.display()),
                ));
            }
            Ok(_) => {
                return Ok(VersionVisibility::Exact {
                    plugin_id: plugin_id.clone(),
                    version: version.clone(),
                    package_digest: package_digest.clone(),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(StorageError::new(
                    "plugin_version_read",
                    format!("cannot inspect {}: {error}", exact.display()),
                ));
            }
        }

        let entries = match fs::read_dir(&version_parent) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(VersionVisibility::Absent);
            }
            Err(error) => {
                return Err(StorageError::new(
                    "plugin_version_read",
                    format!("cannot read {}: {error}", version_parent.display()),
                ));
            }
        };
        for entry in entries {
            let entry = entry.map_err(|error| {
                StorageError::new(
                    "plugin_version_read",
                    format!("cannot read {}: {error}", version_parent.display()),
                )
            })?;
            let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
                StorageError::new(
                    "plugin_version_read",
                    format!("cannot inspect {}: {error}", entry.path().display()),
                )
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(StorageError::new(
                    "plugin_version_type",
                    format!("{} is not a real version directory", entry.path().display()),
                ));
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if let Ok(observed_digest) = Digest::new(name) {
                return Ok(VersionVisibility::Conflict {
                    package_digest: observed_digest,
                });
            }
        }
        Ok(VersionVisibility::Absent)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiptVisibility {
    Exact {
        plugin_id: PluginId,
        generation: u64,
        package_digest: Digest,
    },
    Absent,
    Different {
        generation: u64,
        package_digest: Digest,
    },
}

#[derive(Debug)]
pub struct ReceiptStore {
    paths: PluginPaths,
    failpoints: Arc<StorageFailpoints>,
}

impl ReceiptStore {
    pub fn new(paths: PluginPaths) -> Self {
        Self {
            paths,
            failpoints: Arc::new(StorageFailpoints::default()),
        }
    }

    #[cfg(test)]
    fn with_failpoints(paths: PluginPaths, failpoints: Arc<StorageFailpoints>) -> Self {
        Self { paths, failpoints }
    }

    pub fn current(&self, plugin_id: &str) -> Result<Option<InstallReceipt>, StorageError> {
        let path = self.paths.current(plugin_id);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(StorageError::new(
                    "plugin_receipt_read",
                    format!("cannot inspect {}: {error}", path.display()),
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(StorageError::new(
                "plugin_receipt_type",
                format!("{} is not a regular receipt file", path.display()),
            ));
        }
        let bytes = fs::read(&path).map_err(|error| {
            StorageError::new(
                "plugin_receipt_read",
                format!("cannot read {}: {error}", path.display()),
            )
        })?;
        let receipt: InstallReceipt = serde_json::from_slice(&bytes).map_err(|error| {
            StorageError::new(
                "plugin_receipt_json",
                format!("cannot parse {}: {error}", path.display()),
            )
        })?;
        receipt.validate().map_err(|error| {
            StorageError::new(error.code(), format!("invalid receipt {}", path.display()))
        })?;
        Ok(Some(receipt))
    }

    pub fn commit(
        &self,
        receipt: &InstallReceipt,
    ) -> Result<DurableObservation<ReceiptVisibility>, StorageError> {
        receipt
            .validate()
            .map_err(|error| StorageError::new(error.code(), "invalid install receipt"))?;
        self.paths.prepare_plugin(receipt.plugin_id.as_str())?;
        let plugin_dir = self.paths.plugin(receipt.plugin_id.as_str());
        let current = self.paths.current(receipt.plugin_id.as_str());
        let temp = plugin_dir.join(format!("current.next-{}", Uuid::new_v4()));
        let bytes = serde_json_canonicalizer::to_vec(receipt).map_err(|error| {
            StorageError::new(
                "plugin_receipt_json",
                format!("cannot serialize install receipt: {error}"),
            )
        })?;

        let result = (|| {
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)
                .map_err(|error| {
                    StorageError::new(
                        "plugin_receipt_write",
                        format!("cannot create {}: {error}", temp.display()),
                    )
                })?;
            output.write_all(&bytes).map_err(|error| {
                StorageError::new(
                    "plugin_receipt_write",
                    format!("cannot write {}: {error}", temp.display()),
                )
            })?;
            output.sync_all().map_err(|error| {
                StorageError::new(
                    "plugin_receipt_sync",
                    format!("cannot sync {}: {error}", temp.display()),
                )
            })?;
            drop(output);
            fs::rename(&temp, &current).map_err(|error| {
                StorageError::new(
                    "plugin_receipt_rename",
                    format!(
                        "cannot rename {} to {}: {error}",
                        temp.display(),
                        current.display()
                    ),
                )
            })?;
            if self.failpoints.take(StorageFailpoint::AfterReceiptRename) {
                return self
                    .observe(receipt)
                    .map(DurableObservation::DurabilityUnknown);
            }
            let sync_result = if self.failpoints.take(StorageFailpoint::ReceiptParentSync) {
                Err(std::io::Error::other(
                    "injected current destination-parent sync failure",
                ))
            } else {
                File::open(&plugin_dir).and_then(|directory| directory.sync_all())
            };
            let visibility = self.observe(receipt)?;
            if sync_result.is_err() {
                Ok(DurableObservation::DurabilityUnknown(visibility))
            } else {
                Ok(DurableObservation::Confirmed(visibility))
            }
        })();

        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }

    pub fn observe(&self, expected: &InstallReceipt) -> Result<ReceiptVisibility, StorageError> {
        let Some(observed) = self.current(expected.plugin_id.as_str())? else {
            return Ok(ReceiptVisibility::Absent);
        };
        if observed == *expected {
            Ok(ReceiptVisibility::Exact {
                plugin_id: observed.plugin_id,
                generation: observed.generation,
                package_digest: observed.package_digest,
            })
        } else {
            Ok(ReceiptVisibility::Different {
                generation: observed.generation,
                package_digest: observed.package_digest,
            })
        }
    }
}

fn make_tree_immutable(path: &Path) -> Result<(), StorageError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        StorageError::new(
            "plugin_version_permissions",
            format!("cannot inspect {}: {error}", path.display()),
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(StorageError::new(
            "plugin_version_symlink",
            format!("{} is a symbolic link", path.display()),
        ));
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(|error| {
            StorageError::new(
                "plugin_version_permissions",
                format!("cannot read {}: {error}", path.display()),
            )
        })? {
            let entry = entry.map_err(|error| {
                StorageError::new(
                    "plugin_version_permissions",
                    format!("cannot read {}: {error}", path.display()),
                )
            })?;
            make_tree_immutable(&entry.path())?;
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o555)).map_err(|error| {
            StorageError::new(
                "plugin_version_permissions",
                format!("cannot protect {}: {error}", path.display()),
            )
        })
    } else if metadata.is_file() {
        let mode = if metadata.permissions().mode() & 0o111 != 0 {
            0o555
        } else {
            0o444
        };
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| {
            StorageError::new(
                "plugin_version_permissions",
                format!("cannot protect {}: {error}", path.display()),
            )
        })
    } else {
        Err(StorageError::new(
            "plugin_version_type",
            format!("{} is not a regular file or directory", path.display()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ReceiptStore, ReceiptVisibility, StorageFailpoint, StorageFailpoints, VersionStore,
        VersionVisibility,
    };
    use crate::plugins::package_manager::operation::OperationJournal;
    use crate::plugins::package_manager::paths::PluginPaths;
    use crate::plugins::package_manager::DurableObservation;
    use jarvis_plugin_protocol::manifest::{Digest, PluginId};
    use jarvis_plugin_protocol::package::PackageTarget;
    use jarvis_plugin_protocol::receipt::{
        InstallReceipt, InstallSource, ReceiptSummary, INSTALL_RECEIPT_SCHEMA_VERSION,
    };
    use semver::Version;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);

    fn fixture_store() -> ReceiptStore {
        let root = std::env::temp_dir().join(format!(
            "jarvis-plugin-receipts-{}-{}",
            std::process::id(),
            NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        let paths = PluginPaths::new(root.join("profile"));
        paths.prepare().unwrap();
        ReceiptStore::new(paths)
    }

    fn fixture_paths(label: &str) -> PluginPaths {
        let root = std::env::temp_dir().join(format!(
            "jarvis-plugin-storage-{label}-{}-{}",
            std::process::id(),
            NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        let paths = PluginPaths::new(root.join("profile"));
        paths.prepare().unwrap();
        paths
    }

    fn extracted_fixture(paths: &PluginPaths, label: &str) -> std::path::PathBuf {
        let extracted = paths.quarantine_root().join(format!("extracted-{label}"));
        fs::create_dir_all(extracted.join("bin")).unwrap();
        fs::create_dir_all(extracted.join("ui")).unwrap();
        fs::write(extracted.join("bin/plugin"), b"executable").unwrap();
        fs::set_permissions(
            extracted.join("bin/plugin"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        fs::write(extracted.join("ui/index.html"), b"<main>fixture</main>").unwrap();
        fs::set_permissions(
            extracted.join("ui/index.html"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        extracted
    }

    fn assert_no_operation_transitions(journal: &OperationJournal, before: usize) {
        assert_eq!(journal.recoverable().unwrap().len(), before);
    }

    fn digest(fill: char) -> Digest {
        Digest::new(format!("sha256:{}", fill.to_string().repeat(64))).unwrap()
    }

    fn receipt(
        plugin_id: &str,
        version: &str,
        generation: u64,
        previous: Option<ReceiptSummary>,
    ) -> InstallReceipt {
        InstallReceipt {
            schema_version: INSTALL_RECEIPT_SCHEMA_VERSION,
            plugin_id: PluginId::new(plugin_id).unwrap(),
            version: Version::parse(version).unwrap(),
            package_digest: digest(if generation == 1 { 'a' } else { 'b' }),
            publisher_key_id: "publisher-key-1".into(),
            publisher_lineage: "publisher-lineage-1".into(),
            target: PackageTarget::DarwinArm64,
            source: InstallSource::Catalog,
            enabled: true,
            granted_permissions: Vec::new(),
            native_trust_digest: None,
            installed_at_ms: generation as i64,
            generation,
            state_schema_version: 1,
            rollback_compatible_through: 1,
            previous,
        }
    }

    #[test]
    fn current_receipt_round_trips_with_previous_generation() {
        let store = fixture_store();
        let first = receipt("dev.example.echo", "1.0.0", 1, None);
        store.commit(&first).unwrap();
        let second = receipt("dev.example.echo", "1.1.0", 2, Some(first.summary()));

        store.commit(&second).unwrap();

        assert_eq!(store.current("dev.example.echo").unwrap().unwrap(), second);
    }

    #[test]
    fn finalized_versions_are_immutable_and_conflicting_digest_is_visible() {
        let paths = fixture_paths("immutable");
        let store = VersionStore::new(paths.clone());
        let plugin_id = PluginId::new("dev.example.echo").unwrap();
        let version = Version::parse("1.0.0").unwrap();
        let first_digest = digest('a');
        let first = extracted_fixture(&paths, "first");

        assert_eq!(
            store
                .finalize_extracted(&first, &plugin_id, &version, &first_digest)
                .unwrap(),
            DurableObservation::Confirmed(VersionVisibility::Exact {
                plugin_id: plugin_id.clone(),
                version: version.clone(),
                package_digest: first_digest.clone(),
            })
        );
        let destination = paths
            .versions(plugin_id.as_str())
            .join(version.to_string())
            .join(first_digest.as_str());
        for directory in [
            destination.clone(),
            destination.join("bin"),
            destination.join("ui"),
        ] {
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o555
            );
        }
        assert_eq!(
            fs::metadata(destination.join("bin/plugin"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o555
        );
        assert_eq!(
            fs::metadata(destination.join("ui/index.html"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o444
        );

        let conflicting = extracted_fixture(&paths, "conflict");
        assert_eq!(
            store
                .finalize_extracted(&conflicting, &plugin_id, &version, &digest('b'))
                .unwrap(),
            DurableObservation::Confirmed(VersionVisibility::Conflict {
                package_digest: first_digest,
            })
        );
        assert!(conflicting.exists());
    }

    #[test]
    fn version_rename_reports_exact_visibility_without_operation_transition() {
        let paths = fixture_paths("version-after-rename");
        let journal = OperationJournal::open(paths.operations_db()).unwrap();
        let before = journal.recoverable().unwrap().len();
        let failpoints = Arc::new(StorageFailpoints::default());
        failpoints.arm(StorageFailpoint::AfterVersionRename);
        let store = VersionStore::with_failpoints(paths.clone(), failpoints);
        let plugin_id = PluginId::new("dev.example.echo").unwrap();
        let version = Version::parse("1.0.0").unwrap();
        let package_digest = digest('a');

        assert_eq!(
            store
                .finalize_extracted(
                    &extracted_fixture(&paths, "after-rename"),
                    &plugin_id,
                    &version,
                    &package_digest,
                )
                .unwrap(),
            DurableObservation::DurabilityUnknown(VersionVisibility::Exact {
                plugin_id,
                version,
                package_digest,
            })
        );
        assert_no_operation_transitions(&journal, before);
    }

    #[test]
    fn version_rename_parent_sync_failure_reports_durability_unknown() {
        let paths = fixture_paths("version-parent-sync");
        let journal = OperationJournal::open(paths.operations_db()).unwrap();
        let before = journal.recoverable().unwrap().len();
        let failpoints = Arc::new(StorageFailpoints::default());
        failpoints.arm(StorageFailpoint::VersionParentSync);
        let store = VersionStore::with_failpoints(paths.clone(), failpoints);
        let plugin_id = PluginId::new("dev.example.echo").unwrap();
        let version = Version::parse("1.0.0").unwrap();
        let package_digest = digest('a');

        assert_eq!(
            store
                .finalize_extracted(
                    &extracted_fixture(&paths, "parent-sync"),
                    &plugin_id,
                    &version,
                    &package_digest,
                )
                .unwrap(),
            DurableObservation::DurabilityUnknown(VersionVisibility::Exact {
                plugin_id,
                version,
                package_digest,
            })
        );
        assert_no_operation_transitions(&journal, before);
    }

    #[test]
    fn current_rename_reports_exact_generation_without_operation_transition() {
        let paths = fixture_paths("receipt-after-rename");
        let journal = OperationJournal::open(paths.operations_db()).unwrap();
        let before = journal.recoverable().unwrap().len();
        let failpoints = Arc::new(StorageFailpoints::default());
        failpoints.arm(StorageFailpoint::AfterReceiptRename);
        let store = ReceiptStore::with_failpoints(paths, failpoints);
        let expected = receipt("dev.example.echo", "1.0.0", 1, None);

        assert_eq!(
            store.commit(&expected).unwrap(),
            DurableObservation::DurabilityUnknown(ReceiptVisibility::Exact {
                plugin_id: expected.plugin_id.clone(),
                generation: 1,
                package_digest: expected.package_digest.clone(),
            })
        );
        assert_no_operation_transitions(&journal, before);
    }

    #[test]
    fn current_parent_sync_failure_reports_durability_unknown_with_reobserved_visibility() {
        let paths = fixture_paths("receipt-parent-sync");
        let journal = OperationJournal::open(paths.operations_db()).unwrap();
        let before = journal.recoverable().unwrap().len();
        let failpoints = Arc::new(StorageFailpoints::default());
        failpoints.arm(StorageFailpoint::ReceiptParentSync);
        let store = ReceiptStore::with_failpoints(paths, failpoints);
        let expected = receipt("dev.example.echo", "1.0.0", 1, None);

        assert_eq!(
            store.commit(&expected).unwrap(),
            DurableObservation::DurabilityUnknown(ReceiptVisibility::Exact {
                plugin_id: expected.plugin_id.clone(),
                generation: 1,
                package_digest: expected.package_digest.clone(),
            })
        );
        assert_eq!(
            store.current(expected.plugin_id.as_str()).unwrap(),
            Some(expected)
        );
        assert_no_operation_transitions(&journal, before);
    }

    #[test]
    fn receipt_observation_distinguishes_absent_and_different_state() {
        let paths = fixture_paths("receipt-observe");
        let store = ReceiptStore::new(paths);
        let first = receipt("dev.example.echo", "1.0.0", 1, None);
        let second = receipt("dev.example.echo", "1.1.0", 2, Some(first.summary()));

        assert_eq!(store.observe(&second).unwrap(), ReceiptVisibility::Absent);
        store.commit(&first).unwrap();
        assert_eq!(
            store.observe(&second).unwrap(),
            ReceiptVisibility::Different {
                generation: first.generation,
                package_digest: first.package_digest,
            }
        );
    }

    #[test]
    fn receipt_observation_requires_the_full_expected_receipt() {
        let paths = fixture_paths("receipt-observe-full");
        let store = ReceiptStore::new(paths);
        let first = receipt("dev.example.echo", "1.0.0", 1, None);
        let mut changed = first.clone();
        changed.enabled = false;
        store.commit(&first).unwrap();

        assert_eq!(
            store.observe(&changed).unwrap(),
            ReceiptVisibility::Different {
                generation: first.generation,
                package_digest: first.package_digest,
            }
        );
    }

    #[test]
    fn current_receipt_is_canonical_owner_only_json() {
        let paths = fixture_paths("receipt-canonical");
        let store = ReceiptStore::new(paths.clone());
        let expected = receipt("dev.example.echo", "1.0.0", 1, None);

        store.commit(&expected).unwrap();

        let current = paths.current(expected.plugin_id.as_str());
        assert_eq!(
            fs::read(&current).unwrap(),
            serde_json_canonicalizer::to_vec(&expected).unwrap()
        );
        assert_eq!(
            fs::metadata(current).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn storage_observation_never_deletes_plugin_data() {
        let paths = fixture_paths("preserve-data");
        let plugin_id = PluginId::new("dev.example.echo").unwrap();
        let data_file = paths.data(plugin_id.as_str()).join("state.json");
        fs::create_dir_all(data_file.parent().unwrap()).unwrap();
        fs::write(&data_file, b"{\"preserve\":true}").unwrap();
        let failpoints = Arc::new(StorageFailpoints::default());
        failpoints.arm(StorageFailpoint::AfterVersionRename);
        VersionStore::with_failpoints(paths.clone(), failpoints)
            .finalize_extracted(
                &extracted_fixture(&paths, "preserve-data"),
                &plugin_id,
                &Version::parse("1.0.0").unwrap(),
                &digest('a'),
            )
            .unwrap();

        assert_eq!(fs::read(data_file).unwrap(), b"{\"preserve\":true}");
    }
}
