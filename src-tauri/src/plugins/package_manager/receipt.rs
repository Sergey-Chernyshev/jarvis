use std::ffi::{CStr, CString, OsString};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::paths::{ensure_real_directory, open_real_directory, PluginPaths};
use super::secure_fs;
use super::{random_storage_id, DurableObservation, StorageError};
use jarvis_plugin_protocol::manifest::{Digest, PluginId};
use jarvis_plugin_protocol::receipt::InstallReceipt;
use semver::Version;

#[derive(Clone, Copy, Debug)]
pub(crate) enum StorageFailpoint {
    AfterVersionRename,
    QuarantineAfterMarkerParentSync,
    VersionDestinationChmod,
    VersionParentSync,
    AfterReceiptRename,
    ReceiptParentSync,
}

#[derive(Debug, Default)]
pub(crate) struct StorageFailpoints {
    after_version_rename: AtomicBool,
    quarantine_after_marker_parent_sync: AtomicBool,
    version_destination_chmod: AtomicBool,
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
            StorageFailpoint::QuarantineAfterMarkerParentSync => {
                &self.quarantine_after_marker_parent_sync
            }
            StorageFailpoint::VersionDestinationChmod => &self.version_destination_chmod,
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

type ChildInspectHook<'a> = &'a mut dyn FnMut(&Path);
type BeforeRenameHook<'a> = &'a mut dyn FnMut(&Path, &Path);
type AfterRenameHook<'a> = &'a mut dyn FnMut(&Path);

#[derive(Default)]
struct FinalizeHooks<'a> {
    before_child_open: Option<ChildInspectHook<'a>>,
    before_rename: Option<BeforeRenameHook<'a>>,
    after_rename: Option<AfterRenameHook<'a>>,
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
        self.finalize_extracted_internal(
            extracted,
            plugin_id,
            version,
            package_digest,
            FinalizeHooks::default(),
        )
    }

    #[cfg(test)]
    fn finalize_extracted_after_child_inspect(
        &self,
        extracted: &Path,
        plugin_id: &PluginId,
        version: &Version,
        package_digest: &Digest,
        mut before_child_open: impl FnMut(&Path),
    ) -> Result<DurableObservation<VersionVisibility>, StorageError> {
        self.finalize_extracted_internal(
            extracted,
            plugin_id,
            version,
            package_digest,
            FinalizeHooks {
                before_child_open: Some(&mut before_child_open),
                ..FinalizeHooks::default()
            },
        )
    }

    #[cfg(test)]
    fn finalize_extracted_before_rename(
        &self,
        extracted: &Path,
        plugin_id: &PluginId,
        version: &Version,
        package_digest: &Digest,
        mut before_rename: impl FnMut(&Path, &Path),
    ) -> Result<DurableObservation<VersionVisibility>, StorageError> {
        self.finalize_extracted_internal(
            extracted,
            plugin_id,
            version,
            package_digest,
            FinalizeHooks {
                before_rename: Some(&mut before_rename),
                ..FinalizeHooks::default()
            },
        )
    }

    #[cfg(test)]
    fn finalize_extracted_after_rename(
        &self,
        extracted: &Path,
        plugin_id: &PluginId,
        version: &Version,
        package_digest: &Digest,
        mut after_rename: impl FnMut(&Path),
    ) -> Result<DurableObservation<VersionVisibility>, StorageError> {
        self.finalize_extracted_internal(
            extracted,
            plugin_id,
            version,
            package_digest,
            FinalizeHooks {
                after_rename: Some(&mut after_rename),
                ..FinalizeHooks::default()
            },
        )
    }

    fn finalize_extracted_internal(
        &self,
        extracted: &Path,
        plugin_id: &PluginId,
        version: &Version,
        package_digest: &Digest,
        hooks: FinalizeHooks<'_>,
    ) -> Result<DurableObservation<VersionVisibility>, StorageError> {
        let FinalizeHooks {
            before_child_open,
            mut before_rename,
            mut after_rename,
        } = hooks;
        self.paths.prepare_plugin(plugin_id)?;
        let existing = self.observe(plugin_id, version, package_digest)?;
        if existing != VersionVisibility::Absent {
            return Ok(DurableObservation::Confirmed(existing));
        }

        // Resolve cleanup names before the fixed destination can become
        // visible. Randomness failure must leave the source namespace intact.
        let quarantine_rejected_name = CString::new(format!(".rejected-{}", random_storage_id()?))
            .expect("storage ID has no NUL");
        let quarantine_marker_name =
            CString::new(format!(".identity-invalid-{}", random_storage_id()?))
                .expect("storage ID has no NUL");
        let version_parent = self.paths.versions(plugin_id).join(version.to_string());
        ensure_real_directory(&version_parent, 0o700)?;
        let version_parent_directory = open_real_directory(&version_parent)?;
        let extracted_directory = make_tree_immutable(extracted, before_child_open)?;
        let expected_metadata = secure_fs::metadata(&extracted_directory).map_err(|error| {
            StorageError::new(
                "plugin_version_identity",
                format!(
                    "cannot inspect held source {}: {error}",
                    extracted.display()
                ),
            )
        })?;
        let source_parent_path = extracted.parent().ok_or_else(|| {
            StorageError::new(
                "plugin_version_source",
                format!("{} has no parent directory", extracted.display()),
            )
        })?;
        let source_name = extracted.file_name().ok_or_else(|| {
            StorageError::new(
                "plugin_version_source",
                format!("{} has no directory name", extracted.display()),
            )
        })?;
        let source_name = CString::new(source_name.as_bytes()).map_err(|_| {
            StorageError::new(
                "plugin_version_source",
                format!("{} contains NUL", extracted.display()),
            )
        })?;
        let source_parent = open_real_directory(source_parent_path)?;
        verify_directory_entry(
            &source_parent,
            &source_name,
            &extracted_directory,
            extracted,
            "plugin_version_source",
        )?;
        let destination = version_parent.join(package_digest.as_str());
        let destination_name = CString::new(package_digest.as_str()).map_err(|_| {
            StorageError::new(
                "plugin_version_path",
                format!("{} contains NUL", destination.display()),
            )
        })?;
        // Darwin requires the moved directory itself to be writable while its
        // `..` entry changes across parents. Its contents remain immutable;
        // the fixed destination is restored to 0555 immediately after rename.
        secure_fs::chmod(&extracted_directory, 0o700).map_err(|error| {
            StorageError::new(
                "plugin_version_permissions",
                format!("cannot prepare {} for rename: {error}", extracted.display()),
            )
        })?;
        if let Some(hook) = before_rename.as_mut() {
            hook(extracted, &destination);
        }
        if let Err(identity_error) = verify_directory_entry(
            &source_parent,
            &source_name,
            &extracted_directory,
            extracted,
            "plugin_version_source",
        ) {
            restore_version_rename_permissions(
                &extracted_directory,
                &version_parent_directory,
                extracted,
                &version_parent,
            )?;
            return Err(identity_error);
        }
        if unsafe {
            libc::renameatx_np(
                source_parent.as_raw_fd(),
                source_name.as_ptr(),
                version_parent_directory.as_raw_fd(),
                destination_name.as_ptr(),
                libc::RENAME_EXCL,
            )
        } != 0
        {
            let rename_error = std::io::Error::last_os_error();
            secure_fs::chmod(&extracted_directory, 0o555).map_err(|error| {
                StorageError::new(
                    "plugin_version_permissions",
                    format!(
                        "cannot restore {} after rename failed with {rename_error}: {error}",
                        extracted.display()
                    ),
                )
            })?;
            secure_fs::chmod(&version_parent_directory, 0o555).map_err(|error| {
                StorageError::new(
                    "plugin_version_permissions",
                    format!(
                        "cannot protect {} after rename failed with {rename_error}: {error}",
                        version_parent.display()
                    ),
                )
            })?;
            return Err(StorageError::new(
                "plugin_version_rename",
                format!(
                    "cannot rename {} to {}: {rename_error}",
                    extracted.display(),
                    destination.display()
                ),
            ));
        }

        // This is intentionally the first decision after namespace visibility:
        // failpoints mark durability uncertain but never bypass re-protection.
        let mut durability_unknown = self.failpoints.take(StorageFailpoint::AfterVersionRename);
        if let Some(hook) = after_rename.as_mut() {
            hook(&destination);
        }
        let destination_directory = match open_verified_directory_entry(
            &version_parent_directory,
            &destination_name,
            &destination,
        ) {
            Ok(directory) => directory,
            Err(open_error) => {
                quarantine_untrusted_destination(DestinationQuarantine {
                    failpoints: &self.failpoints,
                    source_parent: &source_parent,
                    source_parent_path,
                    version_parent_directory: &version_parent_directory,
                    version_parent: &version_parent,
                    destination_name: &destination_name,
                    destination: &destination,
                    extracted_directory: &extracted_directory,
                    rejected_name: &quarantine_rejected_name,
                    marker_name: &quarantine_marker_name,
                })?;
                return Err(StorageError::new(
                    "plugin_version_identity",
                    format!(
                        "cannot verify fixed destination {} after rename: {open_error}",
                        destination.display()
                    ),
                ));
            }
        };
        let destination_metadata = match secure_fs::metadata(&destination_directory) {
            Ok(metadata) => metadata,
            Err(error) => {
                quarantine_untrusted_destination(DestinationQuarantine {
                    failpoints: &self.failpoints,
                    source_parent: &source_parent,
                    source_parent_path,
                    version_parent_directory: &version_parent_directory,
                    version_parent: &version_parent,
                    destination_name: &destination_name,
                    destination: &destination,
                    extracted_directory: &extracted_directory,
                    rejected_name: &quarantine_rejected_name,
                    marker_name: &quarantine_marker_name,
                })?;
                return Err(StorageError::new(
                    "plugin_version_identity",
                    format!(
                        "cannot inspect fixed destination {}: {error}",
                        destination.display()
                    ),
                ));
            }
        };
        if !secure_fs::same_identity(&destination_metadata, &expected_metadata) {
            quarantine_untrusted_destination(DestinationQuarantine {
                failpoints: &self.failpoints,
                source_parent: &source_parent,
                source_parent_path,
                version_parent_directory: &version_parent_directory,
                version_parent: &version_parent,
                destination_name: &destination_name,
                destination: &destination,
                extracted_directory: &extracted_directory,
                rejected_name: &quarantine_rejected_name,
                marker_name: &quarantine_marker_name,
            })?;
            return Err(StorageError::new(
                "plugin_version_identity",
                format!(
                    "fixed destination {} does not match held source {}",
                    destination.display(),
                    extracted.display()
                ),
            ));
        }
        let first_protection = if self
            .failpoints
            .take(StorageFailpoint::VersionDestinationChmod)
        {
            Err(std::io::Error::other(
                "injected version destination chmod failure",
            ))
        } else {
            secure_fs::chmod(&destination_directory, 0o555)
        };
        if first_protection.is_err() {
            durability_unknown = true;
            secure_fs::chmod(&destination_directory, 0o555).map_err(|error| {
                StorageError::new(
                    "plugin_version_permissions",
                    format!(
                        "cannot protect {} after rename: {error}",
                        destination.display()
                    ),
                )
            })?;
        }
        if let Err(first_error) = secure_fs::chmod(&version_parent_directory, 0o555) {
            durability_unknown = true;
            secure_fs::chmod(&version_parent_directory, 0o555).map_err(|retry_error| {
                StorageError::new(
                    "plugin_version_permissions",
                    format!(
                        "cannot protect {} after {first_error}: {retry_error}",
                        version_parent.display()
                    ),
                )
            })?;
        }

        let sync_result = if self.failpoints.take(StorageFailpoint::VersionParentSync) {
            Err(std::io::Error::other(
                "injected version destination-parent sync failure",
            ))
        } else {
            version_parent_directory.sync_all()
        };
        if sync_result.is_err() {
            durability_unknown = true;
        }
        let visibility = self.observe(plugin_id, version, package_digest)?;
        if durability_unknown {
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
        let version_parent = self.paths.versions(plugin_id).join(version.to_string());
        if let Err(error) = fs::symlink_metadata(&version_parent) {
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(VersionVisibility::Absent);
            }
            return Err(StorageError::new(
                "plugin_version_read",
                format!("cannot inspect {}: {error}", version_parent.display()),
            ));
        }
        let directory = open_real_directory(&version_parent)?;
        let mut exact = false;
        let mut conflict = None;
        for name in directory_entry_names(&directory, &version_parent)? {
            let child_path = version_parent.join(&name);
            let entry_name = CString::new(name.as_bytes()).map_err(|_| {
                StorageError::new(
                    "plugin_version_type",
                    format!("{} contains NUL", child_path.display()),
                )
            })?;
            let child = open_verified_directory_entry(&directory, &entry_name, &child_path)?;
            drop(child);
            let Some(name_text) = name.to_str() else {
                continue;
            };
            let Ok(observed_digest) = Digest::new(name_text.to_owned()) else {
                continue;
            };
            if &observed_digest == package_digest {
                exact = true;
            } else if conflict.is_none() {
                conflict = Some(observed_digest);
            }
        }
        if let Some(package_digest) = conflict {
            Ok(VersionVisibility::Conflict { package_digest })
        } else if exact {
            Ok(VersionVisibility::Exact {
                plugin_id: plugin_id.clone(),
                version: version.clone(),
                package_digest: package_digest.clone(),
            })
        } else {
            Ok(VersionVisibility::Absent)
        }
    }
}

fn restore_version_rename_permissions(
    extracted_directory: &File,
    version_parent_directory: &File,
    extracted: &Path,
    version_parent: &Path,
) -> Result<(), StorageError> {
    secure_fs::chmod(extracted_directory, 0o555).map_err(|error| {
        StorageError::new(
            "plugin_version_permissions",
            format!(
                "cannot restore held source {}: {error}",
                extracted.display()
            ),
        )
    })?;
    secure_fs::chmod(version_parent_directory, 0o555).map_err(|error| {
        StorageError::new(
            "plugin_version_permissions",
            format!("cannot protect {}: {error}", version_parent.display()),
        )
    })
}

struct DestinationQuarantine<'a> {
    failpoints: &'a StorageFailpoints,
    source_parent: &'a File,
    source_parent_path: &'a Path,
    version_parent_directory: &'a File,
    version_parent: &'a Path,
    destination_name: &'a CStr,
    destination: &'a Path,
    extracted_directory: &'a File,
    rejected_name: &'a CStr,
    marker_name: &'a CStr,
}

fn quarantine_untrusted_destination(
    quarantine: DestinationQuarantine<'_>,
) -> Result<(), StorageError> {
    let DestinationQuarantine {
        failpoints,
        source_parent,
        source_parent_path,
        version_parent_directory,
        version_parent,
        destination_name,
        destination,
        extracted_directory,
        rejected_name,
        marker_name,
    } = quarantine;
    let marker_descriptor = unsafe {
        libc::openat(
            version_parent_directory.as_raw_fd(),
            marker_name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    };
    if marker_descriptor < 0 {
        let error = std::io::Error::last_os_error();
        return Err(StorageError::new(
            "plugin_version_identity_cleanup",
            format!(
                "cannot create fail-closed marker in {}: {error}",
                version_parent.display()
            ),
        ));
    }
    let marker = unsafe { File::from_raw_fd(marker_descriptor) };
    let marker_metadata = secure_fs::metadata(&marker).map_err(|error| {
        StorageError::new(
            "plugin_version_identity_cleanup",
            format!("cannot inspect fail-closed marker: {error}"),
        )
    })?;
    if !secure_fs::is_type(&marker_metadata, libc::S_IFREG)
        || !secure_fs::owned_by_effective_user(&marker_metadata)
        || !secure_fs::has_single_link(&marker_metadata)
    {
        return Err(StorageError::new(
            "plugin_version_identity_cleanup",
            "fail-closed marker is not an owned single-link regular file",
        ));
    }
    marker.sync_all().map_err(|error| {
        StorageError::new(
            "plugin_version_identity_cleanup",
            format!("cannot sync fail-closed marker: {error}"),
        )
    })?;
    version_parent_directory.sync_all().map_err(|error| {
        StorageError::new(
            "plugin_version_identity_cleanup",
            format!(
                "cannot sync fail-closed marker in {}: {error}",
                version_parent.display()
            ),
        )
    })?;
    if failpoints.take(StorageFailpoint::QuarantineAfterMarkerParentSync) {
        return Err(StorageError::new(
            "plugin_version_identity_cleanup",
            "injected interruption after fail-closed marker parent sync",
        ));
    }
    secure_fs::chmod(extracted_directory, 0o555).map_err(|error| {
        StorageError::new(
            "plugin_version_permissions",
            format!("cannot restore held source after destination mismatch: {error}"),
        )
    })?;
    if unsafe {
        libc::renameatx_np(
            version_parent_directory.as_raw_fd(),
            destination_name.as_ptr(),
            source_parent.as_raw_fd(),
            rejected_name.as_ptr(),
            libc::RENAME_EXCL,
        )
    } != 0
    {
        let rename_error = std::io::Error::last_os_error();
        if rename_error.kind() != std::io::ErrorKind::NotFound {
            secure_fs::chmod(version_parent_directory, 0o555).map_err(|error| {
                StorageError::new(
                    "plugin_version_identity_cleanup",
                    format!(
                        "cannot protect {} after quarantine failed with {rename_error}: {error}",
                        version_parent.display()
                    ),
                )
            })?;
            version_parent_directory.sync_all().map_err(|error| {
                StorageError::new(
                    "plugin_version_identity_cleanup",
                    format!(
                        "cannot sync fail-closed {}: {error}",
                        version_parent.display()
                    ),
                )
            })?;
            return Err(StorageError::new(
                "plugin_version_identity_cleanup",
                format!(
                    "cannot move untrusted {} back to {}: {rename_error}",
                    destination.display(),
                    source_parent_path.display()
                ),
            ));
        }
    }

    // Keep the durable marker in place until the quarantine rename is durable
    // in both namespaces. A crash before either sync must still prevent Exact.
    source_parent.sync_all().map_err(|error| {
        StorageError::new(
            "plugin_version_identity_cleanup",
            format!("cannot sync {}: {error}", source_parent_path.display()),
        )
    })?;
    version_parent_directory.sync_all().map_err(|error| {
        StorageError::new(
            "plugin_version_identity_cleanup",
            format!("cannot sync {}: {error}", version_parent.display()),
        )
    })?;
    if unsafe {
        libc::unlinkat(
            version_parent_directory.as_raw_fd(),
            marker_name.as_ptr(),
            0,
        )
    } != 0
    {
        let error = std::io::Error::last_os_error();
        return Err(StorageError::new(
            "plugin_version_identity_cleanup",
            format!("cannot remove fail-closed marker: {error}"),
        ));
    }
    secure_fs::chmod(version_parent_directory, 0o555).map_err(|error| {
        StorageError::new(
            "plugin_version_identity_cleanup",
            format!("cannot protect {}: {error}", version_parent.display()),
        )
    })?;
    version_parent_directory.sync_all().map_err(|error| {
        StorageError::new(
            "plugin_version_identity_cleanup",
            format!("cannot sync {}: {error}", version_parent.display()),
        )
    })
}

fn verify_directory_entry(
    parent: &File,
    name: &CStr,
    expected: &File,
    path: &Path,
    error_code: &'static str,
) -> Result<(), StorageError> {
    let anchored = secure_fs::entry_metadata(parent, name).map_err(|error| {
        StorageError::new(
            error_code,
            format!("cannot recheck {}: {error}", path.display()),
        )
    })?;
    let opened = secure_fs::metadata(expected).map_err(|error| {
        StorageError::new(
            error_code,
            format!("cannot inspect opened {}: {error}", path.display()),
        )
    })?;
    if !secure_fs::is_type(&anchored, libc::S_IFDIR)
        || !secure_fs::is_type(&opened, libc::S_IFDIR)
        || !secure_fs::same_identity(&anchored, &opened)
    {
        return Err(StorageError::new(
            error_code,
            format!("{} changed while it was opened", path.display()),
        ));
    }
    if !secure_fs::owned_by_effective_user(&anchored)
        || !secure_fs::owned_by_effective_user(&opened)
    {
        return Err(StorageError::new(
            error_code,
            format!("{} is not owned by the current user", path.display()),
        ));
    }
    Ok(())
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

    pub fn current(&self, plugin_id: &PluginId) -> Result<Option<InstallReceipt>, StorageError> {
        self.current_internal(plugin_id, Option::<fn(&Path)>::None)
    }

    #[cfg(test)]
    fn current_after_inspect(
        &self,
        plugin_id: &PluginId,
        after_inspect: impl FnOnce(&Path),
    ) -> Result<Option<InstallReceipt>, StorageError> {
        self.current_internal(plugin_id, Some(after_inspect))
    }

    fn current_internal<F>(
        &self,
        plugin_id: &PluginId,
        after_inspect: Option<F>,
    ) -> Result<Option<InstallReceipt>, StorageError>
    where
        F: FnOnce(&Path),
    {
        self.paths.prepare()?;
        let plugins = open_real_directory(&self.paths.plugins_root())?;
        let plugin_name = CString::new(plugin_id.as_str()).expect("validated plugin ID has no NUL");
        let plugin_descriptor = unsafe {
            libc::openat(
                plugins.as_raw_fd(),
                plugin_name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if plugin_descriptor < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(StorageError::new(
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ELOOP) | Some(libc::ENOTDIR)
                ) {
                    "plugin_path_symlink"
                } else {
                    "plugin_receipt_read"
                },
                format!(
                    "cannot open plugin directory {}: {error}",
                    self.paths.plugin(plugin_id).display()
                ),
            ));
        }
        let plugin_directory = unsafe { File::from_raw_fd(plugin_descriptor) };
        let plugin_path = self.paths.plugin(plugin_id);
        verify_directory_entry(
            &plugins,
            &plugin_name,
            &plugin_directory,
            &plugin_path,
            "plugin_receipt_path",
        )?;
        let path = self.paths.current(plugin_id);
        let current_name = CString::new("current").expect("fixed receipt filename has no NUL");
        let mut inspected = std::mem::MaybeUninit::<libc::stat>::zeroed();
        if unsafe {
            libc::fstatat(
                plugin_directory.as_raw_fd(),
                current_name.as_ptr(),
                inspected.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(StorageError::new(
                "plugin_receipt_read",
                format!("cannot inspect {}: {error}", path.display()),
            ));
        }
        let inspected = unsafe { inspected.assume_init() };
        if inspected.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(StorageError::new(
                "plugin_receipt_type",
                format!("{} is not a regular receipt file", path.display()),
            ));
        }
        if !secure_fs::owned_by_effective_user(&inspected) {
            return Err(StorageError::new(
                "plugin_receipt_owner",
                format!("{} is not owned by the current user", path.display()),
            ));
        }
        if !secure_fs::has_single_link(&inspected) {
            return Err(StorageError::new(
                "plugin_receipt_links",
                format!("{} has more than one hard link", path.display()),
            ));
        }
        if let Some(after_inspect) = after_inspect {
            after_inspect(&path);
        }
        let descriptor = unsafe {
            libc::openat(
                plugin_directory.as_raw_fd(),
                current_name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            let error = std::io::Error::last_os_error();
            return Err(StorageError::new(
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ELOOP) | Some(libc::ENOTDIR)
                ) {
                    "plugin_receipt_type"
                } else {
                    "plugin_receipt_read"
                },
                format!("cannot open {}: {error}", path.display()),
            ));
        }
        let mut input = unsafe { File::from_raw_fd(descriptor) };
        let mut opened = std::mem::MaybeUninit::<libc::stat>::zeroed();
        if unsafe { libc::fstat(input.as_raw_fd(), opened.as_mut_ptr()) } != 0 {
            let error = std::io::Error::last_os_error();
            return Err(StorageError::new(
                "plugin_receipt_read",
                format!("cannot inspect opened {}: {error}", path.display()),
            ));
        }
        let opened = unsafe { opened.assume_init() };
        if opened.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(StorageError::new(
                "plugin_receipt_type",
                format!("{} is not a regular receipt file", path.display()),
            ));
        }
        if !secure_fs::owned_by_effective_user(&opened) {
            return Err(StorageError::new(
                "plugin_receipt_owner",
                format!("{} is not owned by the current user", path.display()),
            ));
        }
        if !secure_fs::has_single_link(&opened) {
            return Err(StorageError::new(
                "plugin_receipt_links",
                format!("{} has more than one hard link", path.display()),
            ));
        }
        let mut anchored = std::mem::MaybeUninit::<libc::stat>::zeroed();
        if unsafe {
            libc::fstatat(
                plugin_directory.as_raw_fd(),
                current_name.as_ptr(),
                anchored.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            let error = std::io::Error::last_os_error();
            return Err(StorageError::new(
                "plugin_receipt_read",
                format!("cannot recheck {}: {error}", path.display()),
            ));
        }
        let anchored = unsafe { anchored.assume_init() };
        if anchored.st_mode & libc::S_IFMT != libc::S_IFREG
            || anchored.st_dev != opened.st_dev
            || anchored.st_ino != opened.st_ino
        {
            return Err(StorageError::new(
                "plugin_receipt_type",
                format!("{} changed while it was opened", path.display()),
            ));
        }
        if !secure_fs::owned_by_effective_user(&anchored) || !secure_fs::has_single_link(&anchored)
        {
            return Err(StorageError::new(
                "plugin_receipt_links",
                format!("{} is not an owned single-link receipt", path.display()),
            ));
        }
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes).map_err(|error| {
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
        if receipt.plugin_id != *plugin_id {
            return Err(StorageError::new(
                "plugin_receipt_id",
                format!(
                    "{} contains receipt for {}",
                    path.display(),
                    receipt.plugin_id.as_str()
                ),
            ));
        }
        verify_directory_entry(
            &plugins,
            &plugin_name,
            &plugin_directory,
            &plugin_path,
            "plugin_receipt_path",
        )?;
        Ok(Some(receipt))
    }

    pub fn commit(
        &self,
        receipt: &InstallReceipt,
    ) -> Result<DurableObservation<ReceiptVisibility>, StorageError> {
        self.commit_internal(receipt, Option::<fn(&Path)>::None)
    }

    #[cfg(test)]
    fn commit_after_temp_sync(
        &self,
        receipt: &InstallReceipt,
        after_temp_sync: impl FnOnce(&Path),
    ) -> Result<DurableObservation<ReceiptVisibility>, StorageError> {
        self.commit_internal(receipt, Some(after_temp_sync))
    }

    fn commit_internal<F>(
        &self,
        receipt: &InstallReceipt,
        after_temp_sync: Option<F>,
    ) -> Result<DurableObservation<ReceiptVisibility>, StorageError>
    where
        F: FnOnce(&Path),
    {
        receipt
            .validate()
            .map_err(|error| StorageError::new(error.code(), "invalid install receipt"))?;
        self.paths.prepare_plugin(&receipt.plugin_id)?;
        let plugin_dir = self.paths.plugin(&receipt.plugin_id);
        let current = self.paths.current(&receipt.plugin_id);
        let plugins = open_real_directory(&self.paths.plugins_root())?;
        let plugin_name =
            CString::new(receipt.plugin_id.as_str()).expect("validated plugin ID has no NUL");
        let plugin_directory = open_real_directory(&plugin_dir)?;
        verify_directory_entry(
            &plugins,
            &plugin_name,
            &plugin_directory,
            &plugin_dir,
            "plugin_receipt_path",
        )?;
        let temp_name = format!("current.next-{}", random_storage_id()?);
        let temp = plugin_dir.join(&temp_name);
        let temp_name = CString::new(temp_name).expect("storage ID has no NUL");
        let current_name = CString::new("current").expect("fixed receipt filename has no NUL");
        let bytes = serde_json_canonicalizer::to_vec(receipt).map_err(|error| {
            StorageError::new(
                "plugin_receipt_json",
                format!("cannot serialize install receipt: {error}"),
            )
        })?;

        let result = (|| {
            let descriptor = unsafe {
                libc::openat(
                    plugin_directory.as_raw_fd(),
                    temp_name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600 as libc::c_uint,
                )
            };
            if descriptor < 0 {
                let error = std::io::Error::last_os_error();
                return Err(StorageError::new(
                    "plugin_receipt_write",
                    format!("cannot create {}: {error}", temp.display()),
                ));
            }
            let mut output = unsafe { File::from_raw_fd(descriptor) };
            let output_metadata = secure_fs::metadata(&output).map_err(|error| {
                StorageError::new(
                    "plugin_receipt_write",
                    format!("cannot inspect opened {}: {error}", temp.display()),
                )
            })?;
            if !secure_fs::is_type(&output_metadata, libc::S_IFREG)
                || !secure_fs::owned_by_effective_user(&output_metadata)
                || !secure_fs::has_single_link(&output_metadata)
            {
                return Err(StorageError::new(
                    "plugin_receipt_write",
                    format!("{} is not an owned single-link file", temp.display()),
                ));
            }
            secure_fs::chmod(&output, 0o600).map_err(|error| {
                StorageError::new(
                    "plugin_receipt_write",
                    format!("cannot protect {}: {error}", temp.display()),
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
            if let Some(after_temp_sync) = after_temp_sync {
                after_temp_sync(&temp);
            }
            verify_directory_entry(
                &plugins,
                &plugin_name,
                &plugin_directory,
                &plugin_dir,
                "plugin_receipt_path",
            )?;
            if unsafe {
                libc::renameat(
                    plugin_directory.as_raw_fd(),
                    temp_name.as_ptr(),
                    plugin_directory.as_raw_fd(),
                    current_name.as_ptr(),
                )
            } != 0
            {
                let error = std::io::Error::last_os_error();
                return Err(StorageError::new(
                    "plugin_receipt_rename",
                    format!(
                        "cannot rename {} to {}: {error}",
                        temp.display(),
                        current.display()
                    ),
                ));
            }
            let mut durability_unknown = self.failpoints.take(StorageFailpoint::AfterReceiptRename);
            let sync_result = if self.failpoints.take(StorageFailpoint::ReceiptParentSync) {
                Err(std::io::Error::other(
                    "injected current destination-parent sync failure",
                ))
            } else {
                plugin_directory.sync_all()
            };
            if sync_result.is_err() {
                durability_unknown = true;
            }
            verify_directory_entry(
                &plugins,
                &plugin_name,
                &plugin_directory,
                &plugin_dir,
                "plugin_receipt_path",
            )?;
            let visibility = self.observe(receipt)?;
            if durability_unknown {
                Ok(DurableObservation::DurabilityUnknown(visibility))
            } else {
                Ok(DurableObservation::Confirmed(visibility))
            }
        })();

        if result.is_err() {
            unsafe {
                libc::unlinkat(plugin_directory.as_raw_fd(), temp_name.as_ptr(), 0);
            }
        }
        result
    }

    pub fn observe(&self, expected: &InstallReceipt) -> Result<ReceiptVisibility, StorageError> {
        let Some(observed) = self.current(&expected.plugin_id)? else {
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

fn make_tree_immutable(
    path: &Path,
    before_child_open: Option<&mut dyn FnMut(&Path)>,
) -> Result<File, StorageError> {
    let directory = open_real_directory(path)?;
    let metadata = secure_fs::metadata(&directory).map_err(|error| {
        StorageError::new(
            "plugin_version_permissions",
            format!("cannot inspect opened {}: {error}", path.display()),
        )
    })?;
    if !secure_fs::is_type(&metadata, libc::S_IFDIR)
        || !secure_fs::owned_by_effective_user(&metadata)
    {
        return Err(StorageError::new(
            "plugin_version_owner",
            format!("{} is not an owned directory", path.display()),
        ));
    }
    let mut before_child_open = before_child_open;
    protect_directory_tree(&directory, path, &mut before_child_open)?;
    Ok(directory)
}

fn protect_directory_tree(
    directory: &File,
    path: &Path,
    before_child_open: &mut Option<&mut dyn FnMut(&Path)>,
) -> Result<(), StorageError> {
    for name in directory_entry_names(directory, path)? {
        let child_path = path.join(&name);
        let name = CString::new(name.as_bytes()).map_err(|_| {
            StorageError::new(
                "plugin_version_type",
                format!("{} contains NUL", child_path.display()),
            )
        })?;
        let mut inspected = std::mem::MaybeUninit::<libc::stat>::zeroed();
        if unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                name.as_ptr(),
                inspected.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            let error = std::io::Error::last_os_error();
            return Err(StorageError::new(
                "plugin_version_permissions",
                format!("cannot inspect {}: {error}", child_path.display()),
            ));
        }
        let inspected = unsafe { inspected.assume_init() };
        if inspected.st_mode & libc::S_IFMT == libc::S_IFLNK {
            return Err(StorageError::new(
                "plugin_version_symlink",
                format!("{} is a symbolic link", child_path.display()),
            ));
        }
        if let Some(hook) = before_child_open.as_mut() {
            hook(&child_path);
        }
        let file_type = inspected.st_mode & libc::S_IFMT;
        let flags = if file_type == libc::S_IFDIR {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        } else if file_type == libc::S_IFREG {
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        } else {
            return Err(StorageError::new(
                "plugin_version_type",
                format!(
                    "{} is not a regular file or directory",
                    child_path.display()
                ),
            ));
        };
        let descriptor = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if descriptor < 0 {
            let error = std::io::Error::last_os_error();
            return Err(StorageError::new(
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ELOOP) | Some(libc::ENOTDIR)
                ) {
                    "plugin_version_symlink"
                } else {
                    "plugin_version_permissions"
                },
                format!("cannot open {}: {error}", child_path.display()),
            ));
        }
        let child = unsafe { File::from_raw_fd(descriptor) };
        let mut opened = std::mem::MaybeUninit::<libc::stat>::zeroed();
        if unsafe { libc::fstat(child.as_raw_fd(), opened.as_mut_ptr()) } != 0 {
            let error = std::io::Error::last_os_error();
            return Err(StorageError::new(
                "plugin_version_permissions",
                format!("cannot inspect opened {}: {error}", child_path.display()),
            ));
        }
        let opened = unsafe { opened.assume_init() };
        if opened.st_mode & libc::S_IFMT != file_type
            || opened.st_dev != inspected.st_dev
            || opened.st_ino != inspected.st_ino
        {
            return Err(StorageError::new(
                "plugin_version_type",
                format!("{} changed while it was opened", child_path.display()),
            ));
        }
        if !secure_fs::owned_by_effective_user(&opened) {
            return Err(StorageError::new(
                "plugin_version_owner",
                format!("{} is not owned by the current user", child_path.display()),
            ));
        }
        if file_type == libc::S_IFREG && !secure_fs::has_single_link(&opened) {
            return Err(StorageError::new(
                "plugin_version_links",
                format!("{} has more than one hard link", child_path.display()),
            ));
        }
        if file_type == libc::S_IFDIR {
            protect_directory_tree(&child, &child_path, before_child_open)?;
        }
        let mode = if file_type == libc::S_IFDIR || opened.st_mode & 0o111 != 0 {
            0o555
        } else {
            0o444
        };
        if unsafe { libc::fchmod(child.as_raw_fd(), mode as libc::mode_t) } != 0 {
            let error = std::io::Error::last_os_error();
            return Err(StorageError::new(
                "plugin_version_permissions",
                format!("cannot protect {}: {error}", child_path.display()),
            ));
        }
    }
    if unsafe { libc::fchmod(directory.as_raw_fd(), 0o555 as libc::mode_t) } != 0 {
        let error = std::io::Error::last_os_error();
        return Err(StorageError::new(
            "plugin_version_permissions",
            format!("cannot protect {}: {error}", path.display()),
        ));
    }
    Ok(())
}

fn directory_entry_names(directory: &File, path: &Path) -> Result<Vec<OsString>, StorageError> {
    let duplicate = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        let error = std::io::Error::last_os_error();
        return Err(StorageError::new(
            "plugin_version_permissions",
            format!("cannot duplicate {}: {error}", path.display()),
        ));
    }
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::close(duplicate);
        }
        return Err(StorageError::new(
            "plugin_version_permissions",
            format!("cannot read {}: {error}", path.display()),
        ));
    }
    struct DirectoryStream(*mut libc::DIR);
    impl Drop for DirectoryStream {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let stream = DirectoryStream(stream);
    let mut names = Vec::new();
    loop {
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        names.push(OsString::from(std::ffi::OsStr::from_bytes(name)));
    }
    names.sort();
    Ok(names)
}

fn open_verified_directory_entry(
    parent: &File,
    name: &CStr,
    path: &Path,
) -> Result<File, StorageError> {
    let mut inspected = std::mem::MaybeUninit::<libc::stat>::zeroed();
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            inspected.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        let error = std::io::Error::last_os_error();
        return Err(StorageError::new(
            "plugin_version_read",
            format!("cannot inspect {}: {error}", path.display()),
        ));
    }
    let inspected = unsafe { inspected.assume_init() };
    if inspected.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(StorageError::new(
            "plugin_version_type",
            format!("{} is not a real version directory", path.display()),
        ));
    }
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        let error = std::io::Error::last_os_error();
        return Err(StorageError::new(
            if matches!(
                error.raw_os_error(),
                Some(libc::ELOOP) | Some(libc::ENOTDIR)
            ) {
                "plugin_version_type"
            } else {
                "plugin_version_read"
            },
            format!("cannot open {}: {error}", path.display()),
        ));
    }
    let child = unsafe { File::from_raw_fd(descriptor) };
    let mut opened = std::mem::MaybeUninit::<libc::stat>::zeroed();
    if unsafe { libc::fstat(child.as_raw_fd(), opened.as_mut_ptr()) } != 0 {
        let error = std::io::Error::last_os_error();
        return Err(StorageError::new(
            "plugin_version_read",
            format!("cannot inspect opened {}: {error}", path.display()),
        ));
    }
    let opened = unsafe { opened.assume_init() };
    if opened.st_mode & libc::S_IFMT != libc::S_IFDIR
        || opened.st_dev != inspected.st_dev
        || opened.st_ino != inspected.st_ino
    {
        return Err(StorageError::new(
            "plugin_version_type",
            format!("{} changed while it was opened", path.display()),
        ));
    }
    if !secure_fs::owned_by_effective_user(&opened) {
        return Err(StorageError::new(
            "plugin_version_owner",
            format!("{} is not owned by the current user", path.display()),
        ));
    }
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::{
        ReceiptStore, ReceiptVisibility, StorageFailpoint, StorageFailpoints, VersionStore,
        VersionVisibility,
    };
    use crate::plugins::package_manager::operation::{OperationJournal, OperationState};
    use crate::plugins::package_manager::paths::PluginPaths;
    use crate::plugins::package_manager::DurableObservation;
    use jarvis_plugin_protocol::manifest::{Digest, PluginId};
    use jarvis_plugin_protocol::operation::Operation;
    use jarvis_plugin_protocol::package::PackageTarget;
    use jarvis_plugin_protocol::receipt::{
        InstallReceipt, InstallSource, ReceiptSummary, INSTALL_RECEIPT_SCHEMA_VERSION,
    };
    use semver::Version;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);

    fn fixture_store() -> ReceiptStore {
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
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
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
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

    fn seeded_operation_snapshot(journal: &OperationJournal) -> Vec<Operation> {
        let id = journal.begin("install", "dev.example.preserved").unwrap();
        journal
            .transition(&id, OperationState::Running, "preserved-phase", None)
            .unwrap();
        journal.recoverable().unwrap()
    }

    fn assert_no_operation_transitions(journal: &OperationJournal, before: &[Operation]) {
        assert_eq!(journal.recoverable().unwrap(), before);
    }

    fn assert_finalized_modes(
        paths: &PluginPaths,
        plugin_id: &PluginId,
        version: &Version,
        package_digest: &Digest,
    ) {
        let version_parent = paths.versions(plugin_id).join(version.to_string());
        let destination = version_parent.join(package_digest.as_str());
        for directory in [
            version_parent,
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

        assert_eq!(store.current(&second.plugin_id).unwrap().unwrap(), second);
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
            .versions(&plugin_id)
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
    fn version_observation_reports_conflict_when_exact_and_other_digest_both_exist() {
        let paths = fixture_paths("exact-and-conflict");
        let store = VersionStore::new(paths.clone());
        let plugin_id = PluginId::new("dev.example.echo").unwrap();
        let version = Version::parse("1.0.0").unwrap();
        let exact_digest = digest('a');
        let conflicting_digest = digest('b');
        store
            .finalize_extracted(
                &extracted_fixture(&paths, "exact"),
                &plugin_id,
                &version,
                &exact_digest,
            )
            .unwrap();

        let version_parent = paths.versions(&plugin_id).join(version.to_string());
        fs::set_permissions(&version_parent, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(version_parent.join(conflicting_digest.as_str())).unwrap();
        fs::set_permissions(
            version_parent.join(conflicting_digest.as_str()),
            fs::Permissions::from_mode(0o555),
        )
        .unwrap();
        fs::set_permissions(&version_parent, fs::Permissions::from_mode(0o555)).unwrap();

        assert_eq!(
            store.observe(&plugin_id, &version, &exact_digest).unwrap(),
            VersionVisibility::Conflict {
                package_digest: conflicting_digest,
            }
        );
    }

    #[test]
    fn extracted_child_swap_to_symlink_is_rejected_before_open() {
        let paths = fixture_paths("tree-swap");
        let store = VersionStore::new(paths.clone());
        let plugin_id = PluginId::new("dev.example.echo").unwrap();
        let version = Version::parse("1.0.0").unwrap();
        let package_digest = digest('a');
        let extracted = extracted_fixture(&paths, "tree-swap");
        let child = extracted.join("bin/plugin");
        let original = extracted.join("bin/plugin.original");
        let outside = paths.profile().parent().unwrap().join("outside-executable");
        fs::write(&outside, b"outside-must-not-change").unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
        let mut swapped = false;

        let error = store
            .finalize_extracted_after_child_inspect(
                &extracted,
                &plugin_id,
                &version,
                &package_digest,
                |inspected| {
                    if inspected == child {
                        fs::rename(&child, &original).unwrap();
                        symlink(&outside, &child).unwrap();
                        swapped = true;
                    }
                },
            )
            .unwrap_err();

        assert!(swapped);
        assert_eq!(error.code(), "plugin_version_symlink");
        assert_eq!(fs::read(&outside).unwrap(), b"outside-must-not-change");
        assert_eq!(
            fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read(&original).unwrap(), b"executable");
    }

    #[test]
    fn version_source_real_directory_swap_before_rename_is_rejected() {
        let paths = fixture_paths("source-real-directory-swap");
        let store = VersionStore::new(paths.clone());
        let plugin_id = PluginId::new("dev.example.echo").unwrap();
        let version = Version::parse("1.0.0").unwrap();
        let package_digest = digest('a');
        let extracted = extracted_fixture(&paths, "source");
        let replacement = extracted_fixture(&paths, "replacement");
        let held_original = paths.quarantine_root().join("held-original");
        let destination = paths
            .versions(&plugin_id)
            .join(version.to_string())
            .join(package_digest.as_str());

        let error = store
            .finalize_extracted_before_rename(
                &extracted,
                &plugin_id,
                &version,
                &package_digest,
                |source, fixed_destination| {
                    assert_eq!(source, extracted);
                    assert_eq!(fixed_destination, destination);
                    fs::rename(&extracted, &held_original).unwrap();
                    fs::rename(&replacement, &extracted).unwrap();
                },
            )
            .unwrap_err();

        assert_eq!(error.code(), "plugin_version_source");
        assert!(!destination.exists());
        assert_eq!(
            fs::metadata(extracted.join("bin/plugin"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(held_original.join("bin/plugin"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o555
        );
    }

    #[test]
    fn fixed_destination_real_directory_swap_after_rename_is_quarantined() {
        let paths = fixture_paths("destination-real-directory-swap");
        let store = VersionStore::new(paths.clone());
        let plugin_id = PluginId::new("dev.example.echo").unwrap();
        let version = Version::parse("1.0.0").unwrap();
        let package_digest = digest('a');
        let extracted = extracted_fixture(&paths, "source");
        let attacker = extracted_fixture(&paths, "attacker");
        let version_parent = paths.versions(&plugin_id).join(version.to_string());
        let destination = version_parent.join(package_digest.as_str());
        let held_original = version_parent.join("held-original");

        let error = store
            .finalize_extracted_after_rename(
                &extracted,
                &plugin_id,
                &version,
                &package_digest,
                |fixed_destination| {
                    assert_eq!(fixed_destination, destination);
                    fs::rename(&destination, &held_original).unwrap();
                    fs::rename(&attacker, &destination).unwrap();
                },
            )
            .unwrap_err();

        assert_eq!(error.code(), "plugin_version_identity");
        assert!(!destination.exists());
        assert_eq!(
            store
                .observe(&plugin_id, &version, &package_digest)
                .unwrap(),
            VersionVisibility::Absent
        );
        let rejected = fs::read_dir(paths.quarantine_root())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".rejected-"))
            })
            .expect("attacker destination must be moved out of the fixed digest name");
        assert_eq!(
            fs::metadata(rejected.join("bin/plugin"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(rejected.join("ui/index.html"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(held_original.join("bin/plugin"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o555
        );
    }

    #[test]
    fn quarantine_marker_is_parent_synced_before_destination_cleanup() {
        let paths = fixture_paths("destination-quarantine-order");
        let failpoints = Arc::new(StorageFailpoints::default());
        failpoints.arm(StorageFailpoint::QuarantineAfterMarkerParentSync);
        let store = VersionStore::with_failpoints(paths.clone(), failpoints);
        let plugin_id = PluginId::new("dev.example.echo").unwrap();
        let version = Version::parse("1.0.0").unwrap();
        let package_digest = digest('a');
        let extracted = extracted_fixture(&paths, "source");
        let attacker = extracted_fixture(&paths, "attacker");
        let version_parent = paths.versions(&plugin_id).join(version.to_string());
        let destination = version_parent.join(package_digest.as_str());
        let held_original = version_parent.join("held-original");

        let error = store
            .finalize_extracted_after_rename(
                &extracted,
                &plugin_id,
                &version,
                &package_digest,
                |fixed_destination| {
                    fs::rename(fixed_destination, &held_original).unwrap();
                    fs::rename(&attacker, fixed_destination).unwrap();
                },
            )
            .unwrap_err();

        assert_eq!(error.code(), "plugin_version_identity_cleanup");
        assert!(destination.exists());
        assert!(fs::read_dir(&version_parent).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(".identity-invalid-"))
        }));
        assert_eq!(
            store
                .observe(&plugin_id, &version, &package_digest)
                .unwrap_err()
                .code(),
            "plugin_version_type"
        );
    }

    #[test]
    fn regular_tree_hardlink_is_rejected_without_chmodding_outside_inode() {
        let paths = fixture_paths("tree-hardlink");
        let store = VersionStore::new(paths.clone());
        let plugin_id = PluginId::new("dev.example.echo").unwrap();
        let extracted = extracted_fixture(&paths, "tree-hardlink");
        let linked_child = extracted.join("ui/index.html");
        let outside = paths.profile().parent().unwrap().join("outside-hardlink");
        fs::write(&outside, b"outside-must-not-change").unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
        fs::remove_file(&linked_child).unwrap();
        fs::hard_link(&outside, &linked_child).unwrap();

        let error = store
            .finalize_extracted(
                &extracted,
                &plugin_id,
                &Version::parse("1.0.0").unwrap(),
                &digest('a'),
            )
            .unwrap_err();

        assert_eq!(error.code(), "plugin_version_links");
        assert_eq!(fs::read(&outside).unwrap(), b"outside-must-not-change");
        assert_eq!(
            fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn version_rename_reports_exact_visibility_without_operation_transition() {
        let paths = fixture_paths("version-after-rename");
        let journal = OperationJournal::open(paths.operations_db()).unwrap();
        let before = seeded_operation_snapshot(&journal);
        let failpoints = Arc::new(StorageFailpoints::default());
        failpoints.arm(StorageFailpoint::AfterVersionRename);
        let store = VersionStore::with_failpoints(paths.clone(), failpoints);
        let plugin_id = PluginId::new("dev.example.echo").unwrap();
        let version = Version::parse("1.0.0").unwrap();
        let package_digest = digest('a');
        let extracted = extracted_fixture(&paths, "after-rename");

        let observed_plugin_id = plugin_id.clone();
        let observed_version = version.clone();
        let observed_digest = package_digest.clone();
        assert_eq!(
            store
                .finalize_extracted(&extracted, &plugin_id, &version, &package_digest)
                .unwrap(),
            DurableObservation::DurabilityUnknown(VersionVisibility::Exact {
                plugin_id: observed_plugin_id,
                version: observed_version,
                package_digest: observed_digest,
            })
        );
        assert!(!extracted.exists());
        assert_finalized_modes(&paths, &plugin_id, &version, &package_digest);
        assert_no_operation_transitions(&journal, &before);
    }

    #[test]
    fn destination_chmod_failure_is_repaired_and_reports_reobserved_visibility() {
        let paths = fixture_paths("version-destination-chmod");
        let journal = OperationJournal::open(paths.operations_db()).unwrap();
        let before = seeded_operation_snapshot(&journal);
        let failpoints = Arc::new(StorageFailpoints::default());
        failpoints.arm(StorageFailpoint::VersionDestinationChmod);
        let store = VersionStore::with_failpoints(paths.clone(), failpoints);
        let plugin_id = PluginId::new("dev.example.echo").unwrap();
        let version = Version::parse("1.0.0").unwrap();
        let package_digest = digest('a');
        let extracted = extracted_fixture(&paths, "destination-chmod");

        assert_eq!(
            store
                .finalize_extracted(&extracted, &plugin_id, &version, &package_digest)
                .unwrap(),
            DurableObservation::DurabilityUnknown(VersionVisibility::Exact {
                plugin_id: plugin_id.clone(),
                version: version.clone(),
                package_digest: package_digest.clone(),
            })
        );
        assert!(!extracted.exists());
        assert_finalized_modes(&paths, &plugin_id, &version, &package_digest);
        assert_no_operation_transitions(&journal, &before);
    }

    #[test]
    fn version_rename_parent_sync_failure_reports_durability_unknown() {
        let paths = fixture_paths("version-parent-sync");
        let journal = OperationJournal::open(paths.operations_db()).unwrap();
        let before = seeded_operation_snapshot(&journal);
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
        assert_no_operation_transitions(&journal, &before);
    }

    #[test]
    fn current_rename_reports_exact_generation_without_operation_transition() {
        let paths = fixture_paths("receipt-after-rename");
        let journal = OperationJournal::open(paths.operations_db()).unwrap();
        let before = seeded_operation_snapshot(&journal);
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
        assert_no_operation_transitions(&journal, &before);
    }

    #[test]
    fn current_parent_sync_failure_reports_durability_unknown_with_reobserved_visibility() {
        let paths = fixture_paths("receipt-parent-sync");
        let journal = OperationJournal::open(paths.operations_db()).unwrap();
        let before = seeded_operation_snapshot(&journal);
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
        assert_eq!(store.current(&expected.plugin_id).unwrap(), Some(expected));
        assert_no_operation_transitions(&journal, &before);
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
    fn current_rejects_receipt_for_a_different_plugin_id() {
        let paths = fixture_paths("receipt-id-mismatch");
        let requested = PluginId::new("dev.example.requested").unwrap();
        let observed = receipt("dev.example.other", "1.0.0", 1, None);
        paths.prepare_plugin(&requested).unwrap();
        fs::write(
            paths.current(&requested),
            serde_json_canonicalizer::to_vec(&observed).unwrap(),
        )
        .unwrap();
        let store = ReceiptStore::new(paths);

        assert_eq!(
            store.current(&requested).unwrap_err().code(),
            "plugin_receipt_id"
        );
    }

    #[test]
    fn current_swap_to_symlink_is_rejected_before_open() {
        let paths = fixture_paths("receipt-swap");
        let plugin_id = PluginId::new("dev.example.echo").unwrap();
        let expected = receipt("dev.example.echo", "1.0.0", 1, None);
        paths.prepare_plugin(&plugin_id).unwrap();
        let current = paths.current(&plugin_id);
        let original = paths.plugin(&plugin_id).join("current.original");
        let outside = paths.profile().parent().unwrap().join("outside-receipt");
        let outside_bytes =
            serde_json_canonicalizer::to_vec(&receipt("dev.example.outside", "1.0.0", 1, None))
                .unwrap();
        fs::write(
            &current,
            serde_json_canonicalizer::to_vec(&expected).unwrap(),
        )
        .unwrap();
        fs::write(&outside, &outside_bytes).unwrap();
        let store = ReceiptStore::new(paths);

        let error = store
            .current_after_inspect(&plugin_id, |inspected| {
                assert_eq!(inspected, current);
                fs::rename(&current, &original).unwrap();
                symlink(&outside, &current).unwrap();
            })
            .unwrap_err();

        assert_eq!(error.code(), "plugin_receipt_type");
        assert_eq!(fs::read(&outside).unwrap(), outside_bytes);
        assert_eq!(
            fs::read(&original).unwrap(),
            serde_json_canonicalizer::to_vec(&expected).unwrap()
        );
    }

    #[test]
    fn current_hardlink_is_rejected_without_changing_outside_inode() {
        let paths = fixture_paths("receipt-hardlink");
        let plugin_id = PluginId::new("dev.example.echo").unwrap();
        let expected = receipt("dev.example.echo", "1.0.0", 1, None);
        paths.prepare_plugin(&plugin_id).unwrap();
        let outside = paths.profile().parent().unwrap().join("outside-receipt");
        let outside_bytes = serde_json_canonicalizer::to_vec(&expected).unwrap();
        fs::write(&outside, &outside_bytes).unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o640)).unwrap();
        fs::hard_link(&outside, paths.current(&plugin_id)).unwrap();

        let error = ReceiptStore::new(paths).current(&plugin_id).unwrap_err();

        assert_eq!(error.code(), "plugin_receipt_links");
        assert_eq!(fs::read(&outside).unwrap(), outside_bytes);
        assert_eq!(
            fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[test]
    fn commit_rejects_plugin_directory_swap_before_current_rename() {
        let paths = fixture_paths("receipt-commit-parent-swap");
        let expected = receipt("dev.example.echo", "1.0.0", 1, None);
        paths.prepare_plugin(&expected.plugin_id).unwrap();
        let plugin_directory = paths.plugin(&expected.plugin_id);
        let original_directory = paths.plugins_root().join("echo.original");
        let outside_directory = paths.profile().parent().unwrap().join("outside-plugin");
        fs::create_dir(&outside_directory).unwrap();
        let outside_current = outside_directory.join("current");
        fs::write(&outside_current, b"outside-current-must-not-change").unwrap();
        let store = ReceiptStore::new(paths.clone());

        let error = store
            .commit_after_temp_sync(&expected, |temp| {
                let temp_name = temp.file_name().unwrap();
                fs::write(outside_directory.join(temp_name), b"outside-temp").unwrap();
                fs::rename(&plugin_directory, &original_directory).unwrap();
                symlink(&outside_directory, &plugin_directory).unwrap();
            })
            .unwrap_err();

        assert_eq!(error.code(), "plugin_receipt_path");
        assert_eq!(
            fs::read(&outside_current).unwrap(),
            b"outside-current-must-not-change"
        );
        assert!(!original_directory.join("current").exists());
    }

    #[test]
    fn current_receipt_is_canonical_owner_only_json() {
        let paths = fixture_paths("receipt-canonical");
        let store = ReceiptStore::new(paths.clone());
        let expected = receipt("dev.example.echo", "1.0.0", 1, None);

        store.commit(&expected).unwrap();

        let current = paths.current(&expected.plugin_id);
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
        let data_file = paths.data(&plugin_id).join("state.json");
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
