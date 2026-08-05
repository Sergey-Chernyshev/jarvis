use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use super::manager::{ManagerError, ManagerResult};
use super::paths::PluginPaths;
use super::secure_fs;

pub const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuarantineParentKey {
    ProfilePluginsQuarantineV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuarantineParentIdentity {
    pub device: u64,
    pub inode: u64,
    pub owner_uid: u32,
    pub mode: u32,
    pub link_count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuarantineArchiveIdentity {
    pub device: u64,
    pub inode: u64,
    pub owner_uid: u32,
    pub mode: u32,
    pub link_count: u64,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuarantineArchiveRef {
    pub parent_key: QuarantineParentKey,
    pub parent: QuarantineParentIdentity,
    pub archive_name: String,
    pub archive: QuarantineArchiveIdentity,
}

#[derive(Debug)]
pub struct HeldQuarantineParent {
    fd: OwnedFd,
    identity: QuarantineParentIdentity,
}

pub fn open_fixed_parent(
    paths: &PluginPaths,
    archive: &QuarantineArchiveRef,
) -> ManagerResult<HeldQuarantineParent> {
    if archive.parent_key != QuarantineParentKey::ProfilePluginsQuarantineV1 {
        return Err(ManagerError::new(
            "quarantine_parent_key",
            "unsupported fixed quarantine parent key",
        ));
    }
    let held = open_quarantine_parent(paths)?;
    if !held.matches_recorded_parent(&archive.parent) {
        return Err(ManagerError::new(
            "quarantine_parent_replaced",
            "quarantine parent identity differs from the prepared operation",
        ));
    }
    Ok(held)
}

pub(crate) fn open_quarantine_parent(paths: &PluginPaths) -> ManagerResult<HeldQuarantineParent> {
    let target = paths.quarantine_root();
    if !target.is_absolute() {
        return Err(ManagerError::new(
            "quarantine_parent_unsafe",
            "quarantine root must be absolute",
        ));
    }

    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")
        .map_err(|error| parent_io("/", error))?;
    verify_ancestor(&root, Path::new("/"))?;
    let mut current: OwnedFd = root.into();
    let mut opened_ancestors = Vec::new();
    let mut resolved = std::path::PathBuf::from("/");

    for component in target.components() {
        match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => {
                let name = CString::new(name.as_bytes()).map_err(|_| {
                    ManagerError::new("quarantine_parent_unsafe", "quarantine path contains NUL")
                })?;
                let descriptor = unsafe {
                    libc::openat(
                        current.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if descriptor < 0 {
                    return Err(parent_io(&resolved, io::Error::last_os_error()));
                }
                let child = unsafe { OwnedFd::from_raw_fd(descriptor) };
                resolved.push(component.as_os_str().to_str().ok_or_else(|| {
                    ManagerError::new(
                        "quarantine_parent_unsafe",
                        "quarantine path is not valid UTF-8",
                    )
                })?);
                verify_ancestor_fd(&child, &resolved)?;
                opened_ancestors.push(current);
                current = child;
            }
            Component::ParentDir | Component::Prefix(_) => {
                return Err(ManagerError::new(
                    "quarantine_parent_unsafe",
                    "quarantine path contains an unsafe component",
                ));
            }
        }
    }

    let metadata = metadata_fd(&current, "quarantine_parent_unsafe")?;
    let identity = parent_identity(&metadata)?;
    let effective_uid = unsafe { libc::geteuid() };
    if identity.owner_uid != effective_uid
        || identity.mode != 0o700
        || identity.link_count == 0
        || !secure_fs::is_type(&metadata, libc::S_IFDIR)
    {
        return Err(ManagerError::new(
            "quarantine_parent_unsafe",
            format!(
                "{} must be an owned 0700 directory with a live inode",
                target.display()
            ),
        ));
    }
    drop(opened_ancestors);
    Ok(HeldQuarantineParent {
        fd: current,
        identity,
    })
}

impl HeldQuarantineParent {
    fn matches_recorded_parent(&self, recorded: &QuarantineParentIdentity) -> bool {
        self.identity.device == recorded.device
            && self.identity.inode == recorded.inode
            && self.identity.owner_uid == recorded.owner_uid
            && self.identity.mode == recorded.mode
    }

    pub fn open_archive(&self, archive: &QuarantineArchiveRef) -> ManagerResult<File> {
        if !self.matches_recorded_parent(&archive.parent) {
            return Err(ManagerError::new(
                "quarantine_parent_replaced",
                "held quarantine parent differs from the recorded parent",
            ));
        }
        let name = archive_name(&archive.archive_name)?;
        let descriptor = unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(archive_io(
                &archive.archive_name,
                io::Error::last_os_error(),
            ));
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        let metadata = metadata_file(&file, "quarantine_archive_unsafe")?;
        let observed = archive_identity(&metadata)?;
        let effective_uid = unsafe { libc::geteuid() };
        if observed.owner_uid != effective_uid
            || observed.mode != 0o600
            || observed.link_count != 1
            || observed.size > MAX_ARCHIVE_BYTES
            || !secure_fs::is_type(&metadata, libc::S_IFREG)
        {
            return Err(ManagerError::new(
                "quarantine_archive_unsafe",
                "quarantine archive must be an owned 0600 single-link regular file",
            ));
        }
        if observed != archive.archive {
            return Err(ManagerError::new(
                "quarantine_archive_replaced",
                "quarantine archive identity differs from the prepared operation",
            ));
        }
        Ok(file)
    }

    pub(crate) fn create_archive(&self, name: &str) -> ManagerResult<File> {
        let name = archive_name(name)?;
        let descriptor = unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if descriptor < 0 {
            return Err(ManagerError::new(
                "quarantine_archive_create",
                io::Error::last_os_error().to_string(),
            ));
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        let metadata = metadata_file(&file, "quarantine_archive_create")?;
        let observed = archive_identity(&metadata)?;
        if observed.owner_uid != unsafe { libc::geteuid() }
            || observed.mode != 0o600
            || observed.link_count != 1
            || !secure_fs::is_type(&metadata, libc::S_IFREG)
        {
            return Err(ManagerError::new(
                "quarantine_archive_create",
                "created quarantine archive is not a private regular file",
            ));
        }
        Ok(file)
    }

    pub(crate) fn record_archive(
        &self,
        archive_name: String,
        file: &File,
    ) -> ManagerResult<QuarantineArchiveRef> {
        let metadata = metadata_file(file, "quarantine_archive_unsafe")?;
        let archive = archive_identity(&metadata)?;
        if archive.owner_uid != unsafe { libc::geteuid() }
            || archive.mode != 0o600
            || archive.link_count != 1
            || archive.size > MAX_ARCHIVE_BYTES
            || !secure_fs::is_type(&metadata, libc::S_IFREG)
        {
            return Err(ManagerError::new(
                "quarantine_archive_unsafe",
                "downloaded archive is not a private single-link regular file",
            ));
        }
        Ok(QuarantineArchiveRef {
            parent_key: QuarantineParentKey::ProfilePluginsQuarantineV1,
            parent: self.identity.clone(),
            archive_name,
            archive,
        })
    }

    pub(crate) fn sync(&self) -> ManagerResult<()> {
        if unsafe { libc::fsync(self.fd.as_raw_fd()) } == 0 {
            Ok(())
        } else {
            Err(ManagerError::new(
                "quarantine_parent_sync",
                io::Error::last_os_error().to_string(),
            ))
        }
    }

    pub(crate) fn unlink_archive(&self, name: &str) -> ManagerResult<()> {
        let name = archive_name(name)?;
        if unsafe { libc::unlinkat(self.fd.as_raw_fd(), name.as_ptr(), 0) } == 0 {
            self.sync()
        } else {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(ManagerError::new(
                    "quarantine_archive_cleanup",
                    error.to_string(),
                ))
            }
        }
    }

    pub(crate) fn raw_fd(&self) -> &OwnedFd {
        &self.fd
    }
}

fn archive_name(value: &str) -> ManagerResult<CString> {
    if value.is_empty()
        || value.len() > 255
        || matches!(value, "." | "..")
        || value.as_bytes().contains(&b'/')
    {
        return Err(ManagerError::new(
            "quarantine_archive_name",
            "archive name must be one safe path component",
        ));
    }
    CString::new(value).map_err(|_| {
        ManagerError::new(
            "quarantine_archive_name",
            "archive name contains a NUL byte",
        )
    })
}

fn verify_ancestor(file: &File, path: &Path) -> ManagerResult<()> {
    let metadata = metadata_file(file, "quarantine_parent_unsafe")?;
    verify_ancestor_metadata(&metadata, path)
}

fn verify_ancestor_fd(file: &OwnedFd, path: &Path) -> ManagerResult<()> {
    let metadata = metadata_fd(file, "quarantine_parent_unsafe")?;
    verify_ancestor_metadata(&metadata, path)
}

fn verify_ancestor_metadata(metadata: &libc::stat, path: &Path) -> ManagerResult<()> {
    let owner = metadata.st_uid;
    let effective_uid = unsafe { libc::geteuid() };
    if !secure_fs::is_type(metadata, libc::S_IFDIR)
        || metadata.st_nlink == 0
        || (owner != 0 && owner != effective_uid)
        || mode(metadata) & 0o022 != 0
    {
        return Err(ManagerError::new(
            "quarantine_parent_unsafe",
            format!("{} is not a safe directory ancestor", path.display()),
        ));
    }
    Ok(())
}

fn parent_identity(metadata: &libc::stat) -> ManagerResult<QuarantineParentIdentity> {
    Ok(QuarantineParentIdentity {
        device: nonnegative(metadata.st_dev, "quarantine_parent_unsafe")?,
        inode: nonnegative(metadata.st_ino, "quarantine_parent_unsafe")?,
        owner_uid: metadata.st_uid,
        mode: mode(metadata),
        link_count: nonnegative(metadata.st_nlink, "quarantine_parent_unsafe")?,
    })
}

fn archive_identity(metadata: &libc::stat) -> ManagerResult<QuarantineArchiveIdentity> {
    Ok(QuarantineArchiveIdentity {
        device: nonnegative(metadata.st_dev, "quarantine_archive_unsafe")?,
        inode: nonnegative(metadata.st_ino, "quarantine_archive_unsafe")?,
        owner_uid: metadata.st_uid,
        mode: mode(metadata),
        link_count: nonnegative(metadata.st_nlink, "quarantine_archive_unsafe")?,
        size: nonnegative(metadata.st_size, "quarantine_archive_unsafe")?,
    })
}

fn metadata_file(file: &File, code: &'static str) -> ManagerResult<libc::stat> {
    secure_fs::metadata(file).map_err(|error| ManagerError::new(code, error.to_string()))
}

fn metadata_fd(file: &OwnedFd, code: &'static str) -> ManagerResult<libc::stat> {
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::zeroed();
    if unsafe { libc::fstat(file.as_raw_fd(), metadata.as_mut_ptr()) } == 0 {
        Ok(unsafe { metadata.assume_init() })
    } else {
        Err(ManagerError::new(
            code,
            io::Error::last_os_error().to_string(),
        ))
    }
}

fn nonnegative<T>(value: T, code: &'static str) -> ManagerResult<u64>
where
    u64: TryFrom<T>,
{
    u64::try_from(value).map_err(|_| ManagerError::new(code, "negative filesystem metadata"))
}

fn mode(metadata: &libc::stat) -> u32 {
    (metadata.st_mode as u32) & 0o7777
}

fn parent_io(path: impl AsRef<Path>, error: io::Error) -> ManagerError {
    ManagerError::new(
        "quarantine_parent_unsafe",
        format!("cannot open {}: {error}", path.as_ref().display()),
    )
}

fn archive_io(name: &str, error: io::Error) -> ManagerError {
    if error.kind() == io::ErrorKind::NotFound {
        return ManagerError::new(
            "quarantine_archive_replaced",
            format!("prepared quarantine archive {name} is missing"),
        );
    }
    ManagerError::new(
        "quarantine_archive_unsafe",
        format!("cannot open quarantine archive {name}: {error}"),
    )
}
