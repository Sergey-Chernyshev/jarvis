use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::path::Path;

use jarvis_plugin_protocol::manifest::Digest;
use jarvis_plugin_protocol::package::PackagePath;
use rustix::fs::{fstat, open, openat, statat, AtFlags, FileType, Mode, OFlags, Stat};
use sha2::{Digest as _, Sha256};

use crate::archive::{projected_entry_bytes, ArchiveLimits, BLOCK_SIZE};
use crate::macos_dir::read_directory_names_bounded;
use crate::spool::{SourceSnapshot, SpooledFile};
use crate::PackageError;

const MAX_SOURCE_DEPTH: usize = 64;
const COPY_BUFFER_SIZE: usize = 64 * 1024;
const SOURCE_NAMESPACE_NODE_ALLOCATION_CHARGE: u64 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SourceIdentity {
    device: libc::dev_t,
    inode: libc::ino_t,
    file_type: FileType,
    size: libc::off_t,
    mode: libc::mode_t,
    link_count: libc::nlink_t,
    modified_seconds: libc::time_t,
    modified_nanoseconds: libc::c_long,
    changed_seconds: libc::time_t,
    changed_nanoseconds: libc::c_long,
}

impl SourceIdentity {
    fn from_stat(stat: &Stat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            file_type: FileType::from_raw_mode(stat.st_mode),
            size: stat.st_size,
            mode: stat.st_mode,
            link_count: stat.st_nlink,
            modified_seconds: stat.st_mtime,
            modified_nanoseconds: stat.st_mtime_nsec,
            changed_seconds: stat.st_ctime,
            changed_nanoseconds: stat.st_ctime_nsec,
        }
    }

    fn validate_regular(self) -> Result<Self, PackageError> {
        if self.file_type != FileType::RegularFile || self.link_count != 1 || self.size < 0 {
            return Err(PackageError::source_invalid());
        }
        Ok(self)
    }

    fn validate_directory(self) -> Result<Self, PackageError> {
        if self.file_type != FileType::Directory {
            return Err(PackageError::source_invalid());
        }
        Ok(self)
    }
}

#[derive(Clone, Debug)]
struct SourceFile {
    path: PackagePath,
    identity: SourceIdentity,
}

#[derive(Clone, Debug)]
struct SourceDirectory {
    path: String,
    identity: SourceIdentity,
}

struct SourceBudget {
    limits: ArchiveLimits,
    payload_files: u64,
    unpacked_payload_bytes: u64,
    projected_physical_bytes: u64,
    namespace_nodes: u64,
    namespace_stored_bytes: u64,
    plugin_json_present: bool,
}

impl SourceBudget {
    fn production() -> Result<Self, PackageError> {
        let limits = ArchiveLimits::production();
        let package_path =
            PackagePath::new("package.json").map_err(|_| PackageError::archive_quota())?;
        let signature_path =
            PackagePath::new("SIGNATURE").map_err(|_| PackageError::archive_quota())?;
        let projected_physical_bytes = (2 * BLOCK_SIZE as u64)
            .checked_add(projected_entry_bytes(&package_path, 0)?)
            .and_then(|size| {
                projected_entry_bytes(&signature_path, 0)
                    .ok()
                    .and_then(|entry| size.checked_add(entry))
            })
            .ok_or_else(PackageError::archive_quota)?;
        Ok(Self {
            limits,
            payload_files: 0,
            unpacked_payload_bytes: 0,
            projected_physical_bytes,
            namespace_nodes: 0,
            namespace_stored_bytes: 0,
            plugin_json_present: false,
        })
    }

    fn remaining_namespace_nodes(&self) -> u64 {
        self.limits
            .max_namespace_nodes
            .saturating_sub(self.namespace_nodes)
    }

    fn remaining_namespace_stored_bytes(&self) -> u64 {
        self.limits
            .max_namespace_stored_bytes
            .saturating_sub(self.namespace_stored_bytes)
    }

    fn record_file(&mut self, path: &PackagePath, size: u64) -> Result<(), PackageError> {
        self.payload_files = self
            .payload_files
            .checked_add(1)
            .ok_or_else(PackageError::archive_quota)?;
        if self.payload_files > self.limits.max_payload_files
            || size > self.limits.max_single_payload_file
        {
            return Err(PackageError::archive_quota());
        }
        self.unpacked_payload_bytes = self
            .unpacked_payload_bytes
            .checked_add(size)
            .ok_or_else(PackageError::archive_quota)?;
        if self.unpacked_payload_bytes > self.limits.max_unpacked_payload_bytes {
            return Err(PackageError::archive_quota());
        }
        if path.as_str() == "plugin.json" {
            if size > self.limits.max_plugin_json_bytes {
                return Err(PackageError::archive_quota());
            }
            self.plugin_json_present = true;
        }
        self.record_namespace_path(path.as_str(), 1)?;
        let projected_size = if path.as_str() == "plugin.json" {
            0
        } else {
            size
        };
        self.projected_physical_bytes = self
            .projected_physical_bytes
            .checked_add(projected_entry_bytes(path, projected_size)?)
            .ok_or_else(PackageError::archive_quota)?;
        if self.projected_physical_bytes > self.limits.max_physical_bytes {
            return Err(PackageError::archive_quota());
        }
        Ok(())
    }

    fn record_directory(&mut self, path: &str) -> Result<(), PackageError> {
        self.record_namespace_path(path, 3)
    }

    fn record_namespace_path(
        &mut self,
        path: &str,
        retained_copies: u64,
    ) -> Result<(), PackageError> {
        self.namespace_nodes = self
            .namespace_nodes
            .checked_add(1)
            .ok_or_else(PackageError::archive_quota)?;
        if self.namespace_nodes > self.limits.max_namespace_nodes {
            return Err(PackageError::archive_quota());
        }
        let path_bytes = u64::try_from(path.len())
            .map_err(|_| PackageError::archive_quota())?
            .checked_mul(retained_copies)
            .ok_or_else(PackageError::archive_quota)?;
        let charge = path_bytes
            .checked_add(SOURCE_NAMESPACE_NODE_ALLOCATION_CHARGE)
            .ok_or_else(PackageError::archive_quota)?;
        self.namespace_stored_bytes = self
            .namespace_stored_bytes
            .checked_add(charge)
            .ok_or_else(PackageError::archive_quota)?;
        if self.namespace_stored_bytes > self.limits.max_namespace_stored_bytes {
            return Err(PackageError::archive_quota());
        }
        Ok(())
    }

    fn finish(&self) -> Result<(), PackageError> {
        if !self.plugin_json_present {
            return Err(PackageError::source_invalid());
        }
        if self.projected_physical_bytes > self.limits.max_physical_bytes {
            return Err(PackageError::archive_quota());
        }
        Ok(())
    }
}

pub(crate) trait SnapshotHook {
    fn after_enumeration(&self) {}
    fn before_open(&self, _path: &str) {}
    fn after_copy_chunk(&self, _path: &str, _copied: u64) {}
}

struct NoopHook;

impl SnapshotHook for NoopHook {}

pub(crate) fn snapshot_source(root: &Path) -> Result<SourceSnapshot, PackageError> {
    snapshot_source_with_hook(root, &NoopHook)
}

pub(crate) fn snapshot_source_with_hook<H: SnapshotHook>(
    root: &Path,
    hook: &H,
) -> Result<SourceSnapshot, PackageError> {
    let root_fd = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| PackageError::source_invalid())?;
    SourceIdentity::from_stat(&fstat(&root_fd).map_err(|_| PackageError::source_invalid())?)
        .validate_directory()?;

    let mut files = Vec::new();
    let mut directories = BTreeMap::new();
    let mut budget = SourceBudget::production()?;
    enumerate_directory(&root_fd, "", 0, &mut files, &mut directories, &mut budget)?;
    budget.finish()?;
    files.sort_by(|left, right| left.path.as_str().cmp(right.path.as_str()));
    hook.after_enumeration();

    let (mut spool, mut spooled_files) = SourceSnapshot::create()?;
    let mut spool_offset = 0_u64;
    for source_file in files {
        hook.before_open(source_file.path.as_str());
        let mut opened = reopen_file(&root_fd, &source_file, &directories)?;
        let before =
            SourceIdentity::from_stat(&fstat(&opened).map_err(|_| PackageError::source_raced())?)
                .validate_regular()
                .map_err(|_| PackageError::source_raced())?;
        if before != source_file.identity {
            return Err(PackageError::source_raced());
        }

        let expected_length =
            u64::try_from(before.size).map_err(|_| PackageError::source_invalid())?;
        let (copied, digest) = copy_and_hash(
            &mut opened,
            &mut spool,
            source_file.path.as_str(),
            expected_length,
            hook,
        )?;
        if copied != expected_length {
            return Err(PackageError::source_raced());
        }
        let after =
            SourceIdentity::from_stat(&fstat(&opened).map_err(|_| PackageError::source_raced())?)
                .validate_regular()
                .map_err(|_| PackageError::source_raced())?;
        if after != before {
            return Err(PackageError::source_raced());
        }

        spooled_files.push(SpooledFile::new(
            source_file.path,
            spool_offset,
            copied,
            digest,
            u32::from(before.mode),
        )?);
        spool_offset = spool_offset
            .checked_add(copied)
            .ok_or_else(PackageError::source_invalid)?;
    }
    spool.flush().map_err(|_| PackageError::source_invalid())?;
    SourceSnapshot::from_parts(spool, spooled_files)
}

fn enumerate_directory(
    directory_fd: &OwnedFd,
    prefix: &str,
    depth: usize,
    files: &mut Vec<SourceFile>,
    directories: &mut BTreeMap<String, SourceDirectory>,
    budget: &mut SourceBudget,
) -> Result<(), PackageError> {
    if depth >= MAX_SOURCE_DEPTH {
        return Err(PackageError::source_invalid());
    }
    let Some(mut names) = read_directory_names_bounded(
        directory_fd,
        budget.remaining_namespace_nodes(),
        budget.remaining_namespace_stored_bytes(),
        SOURCE_NAMESPACE_NODE_ALLOCATION_CHARGE,
    )
    .map_err(|_| PackageError::source_invalid())?
    else {
        return Err(PackageError::archive_quota());
    };
    names.sort_by(|left, right| left.as_encoded_bytes().cmp(right.as_encoded_bytes()));

    for name in names {
        let name_text = unicode_name(&name)?;
        let path_text = if prefix.is_empty() {
            name_text.to_owned()
        } else {
            format!("{prefix}/{name_text}")
        };
        let path =
            PackagePath::new(path_text.clone()).map_err(|_| PackageError::source_invalid())?;
        let stat = statat(directory_fd, &name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| PackageError::source_invalid())?;
        let identity = SourceIdentity::from_stat(&stat);
        match identity.file_type {
            FileType::RegularFile => {
                let identity = identity.validate_regular()?;
                let size =
                    u64::try_from(identity.size).map_err(|_| PackageError::source_invalid())?;
                budget.record_file(&path, size)?;
                files.push(SourceFile { path, identity });
            }
            FileType::Directory => {
                let identity = identity.validate_directory()?;
                budget.record_directory(&path_text)?;
                let child = openat(
                    directory_fd,
                    &name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|_| PackageError::source_raced())?;
                let opened_identity = SourceIdentity::from_stat(
                    &fstat(&child).map_err(|_| PackageError::source_raced())?,
                )
                .validate_directory()
                .map_err(|_| PackageError::source_raced())?;
                if opened_identity != identity {
                    return Err(PackageError::source_raced());
                }
                directories.insert(
                    path_text.clone(),
                    SourceDirectory {
                        path: path_text.clone(),
                        identity,
                    },
                );
                enumerate_directory(&child, &path_text, depth + 1, files, directories, budget)?;
            }
            _ => return Err(PackageError::source_invalid()),
        }
    }
    Ok(())
}

fn reopen_file(
    root_fd: &OwnedFd,
    source_file: &SourceFile,
    directories: &BTreeMap<String, SourceDirectory>,
) -> Result<File, PackageError> {
    let mut current =
        rustix::io::fcntl_dupfd_cloexec(root_fd, 0).map_err(|_| PackageError::source_raced())?;
    let mut components = source_file.path.as_str().split('/').peekable();
    let mut prefix = String::new();
    while let Some(component) = components.next() {
        if components.peek().is_none() {
            let file = openat(
                &current,
                component,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| PackageError::source_raced())?;
            return Ok(File::from(file));
        }
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(component);
        let expected = directories
            .get(&prefix)
            .ok_or_else(PackageError::source_raced)?;
        if expected.path != prefix {
            return Err(PackageError::source_raced());
        }
        let child = openat(
            &current,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| PackageError::source_raced())?;
        let identity =
            SourceIdentity::from_stat(&fstat(&child).map_err(|_| PackageError::source_raced())?)
                .validate_directory()
                .map_err(|_| PackageError::source_raced())?;
        if identity != expected.identity {
            return Err(PackageError::source_raced());
        }
        current = child;
    }
    Err(PackageError::source_raced())
}

fn copy_and_hash<H: SnapshotHook>(
    source: &mut File,
    spool: &mut File,
    path: &str,
    expected_length: u64,
    hook: &H,
) -> Result<(u64, Digest), PackageError> {
    let mut hasher = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = [0_u8; COPY_BUFFER_SIZE];
    while copied < expected_length {
        let remaining = expected_length - copied;
        let requested = usize::try_from(remaining.min(COPY_BUFFER_SIZE as u64))
            .map_err(|_| PackageError::source_invalid())?;
        let read = source
            .read(&mut buffer[..requested])
            .map_err(|_| PackageError::source_raced())?;
        if read == 0 {
            return Err(PackageError::source_raced());
        }
        spool
            .write_all(&buffer[..read])
            .map_err(|_| PackageError::source_invalid())?;
        hasher.update(&buffer[..read]);
        copied = copied
            .checked_add(u64::try_from(read).map_err(|_| PackageError::source_invalid())?)
            .ok_or_else(PackageError::source_invalid)?;
        hook.after_copy_chunk(path, copied);
    }
    let mut extra = [0_u8; 1];
    if source
        .read(&mut extra)
        .map_err(|_| PackageError::source_raced())?
        != 0
    {
        return Err(PackageError::source_raced());
    }

    let digest_bytes: [u8; 32] = hasher.finalize().into();
    let mut digest_text = String::with_capacity(71);
    digest_text.push_str("sha256:");
    for byte in digest_bytes {
        write!(&mut digest_text, "{byte:02x}").expect("writing into a String cannot fail");
    }
    let digest = Digest::new(digest_text).map_err(|_| PackageError::source_invalid())?;
    Ok((copied, digest))
}

fn unicode_name(name: &OsString) -> Result<&str, PackageError> {
    OsStr::new(name)
        .to_str()
        .ok_or_else(PackageError::source_invalid)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::{Path, PathBuf};

    use super::{snapshot_source, snapshot_source_with_hook, SnapshotHook};

    struct Hook<F, G, H> {
        after_enumeration: F,
        before_open: G,
        after_copy_chunk: H,
    }

    impl<F, G, H> SnapshotHook for Hook<F, G, H>
    where
        F: Fn(),
        G: Fn(&str),
        H: Fn(&str, u64),
    {
        fn after_enumeration(&self) {
            (self.after_enumeration)();
        }

        fn before_open(&self, path: &str) {
            (self.before_open)(path);
        }

        fn after_copy_chunk(&self, path: &str, copied: u64) {
            (self.after_copy_chunk)(path, copied);
        }
    }

    fn no_op_hook() -> Hook<impl Fn(), impl Fn(&str), impl Fn(&str, u64)> {
        Hook {
            after_enumeration: no_action,
            before_open: no_path_action,
            after_copy_chunk: no_chunk_action,
        }
    }

    fn no_action() {}

    fn no_path_action(_: &str) {}

    fn no_chunk_action(_: &str, _: u64) {}

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("plugin.json"), b"{}").unwrap();
        fs::create_dir(source.join("nested")).unwrap();
        fs::write(source.join("nested/payload.txt"), b"original").unwrap();
        (root, source)
    }

    fn assert_raced(result: Result<crate::spool::SourceSnapshot, crate::PackageError>) {
        assert_eq!(result.unwrap_err().code(), "source_raced");
    }

    #[test]
    fn source_file_replaced_after_enumeration_never_packages_outside_bytes() {
        let (_root, source) = fixture();
        let payload = source.join("nested/payload.txt");
        let original = source.join("nested/original.txt");
        let hook = Hook {
            after_enumeration: || {
                fs::rename(&payload, &original).unwrap();
                fs::write(&payload, b"attacker").unwrap();
            },
            before_open: no_path_action,
            after_copy_chunk: no_chunk_action,
        };

        assert_raced(snapshot_source_with_hook(&source, &hook));
    }

    #[test]
    fn source_file_changed_to_symlink_before_open_is_source_raced() {
        let (root, source) = fixture();
        let outside = root.path().join("outside");
        fs::write(&outside, b"outside-secret").unwrap();
        let payload = source.join("nested/payload.txt");
        let hook = Hook {
            after_enumeration: || {
                fs::remove_file(&payload).unwrap();
                symlink(&outside, &payload).unwrap();
            },
            before_open: no_path_action,
            after_copy_chunk: no_chunk_action,
        };

        assert_raced(snapshot_source_with_hook(&source, &hook));
    }

    #[test]
    fn source_parent_directory_swap_is_source_raced() {
        let (_root, source) = fixture();
        let nested = source.join("nested");
        let original = source.join("held-original");
        let hook = Hook {
            after_enumeration: || {
                fs::rename(&nested, &original).unwrap();
                fs::create_dir(&nested).unwrap();
                fs::write(nested.join("payload.txt"), b"attacker").unwrap();
            },
            before_open: no_path_action,
            after_copy_chunk: no_chunk_action,
        };

        assert_raced(snapshot_source_with_hook(&source, &hook));
    }

    #[test]
    fn source_file_mutated_during_copy_is_source_raced() {
        let (_root, source) = fixture();
        let payload = source.join("nested/payload.txt");
        fs::write(&payload, vec![b'a'; 192 * 1024]).unwrap();
        let mutated = Cell::new(false);
        let hook = Hook {
            after_enumeration: no_action,
            before_open: no_path_action,
            after_copy_chunk: |path: &str, copied: u64| {
                if path == "nested/payload.txt" && copied > 0 && !mutated.replace(true) {
                    fs::write(&payload, vec![b'b'; 192 * 1024]).unwrap();
                }
            },
        };

        assert_raced(snapshot_source_with_hook(&source, &hook));
    }

    #[test]
    fn source_inode_reused_with_different_metadata_is_source_raced() {
        let (_root, source) = fixture();
        let payload = source.join("nested/payload.txt");
        let hook = Hook {
            after_enumeration: || {
                fs::set_permissions(&payload, fs::Permissions::from_mode(0o600)).unwrap();
            },
            before_open: no_path_action,
            after_copy_chunk: no_chunk_action,
        };

        assert_raced(snapshot_source_with_hook(&source, &hook));
    }

    #[test]
    fn tar_writer_reads_only_spool_after_source_snapshot() {
        let (_root, source) = fixture();
        let snapshot = snapshot_source(&source).unwrap();
        fs::write(source.join("nested/payload.txt"), b"changed").unwrap();

        assert_eq!(
            snapshot.read_file("nested/payload.txt").unwrap(),
            b"original"
        );
    }

    #[test]
    fn clean_snapshot_accepts_only_regular_files_and_directories() {
        let (_root, source) = fixture();
        let snapshot = snapshot_source_with_hook(&source, &no_op_hook()).unwrap();
        assert_eq!(snapshot.read_file("plugin.json").unwrap(), b"{}");

        let link = source.join("link");
        symlink(Path::new("plugin.json"), link).unwrap();
        assert!(snapshot_source(&source).is_err());
    }

    #[test]
    fn source_quotas_reject_oversized_plugin_before_copy() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        let plugin = fs::File::create(source.join("plugin.json")).unwrap();
        plugin.set_len(256 * 1024 + 1).unwrap();
        let copied = Cell::new(false);
        let hook = Hook {
            after_enumeration: no_action,
            before_open: no_path_action,
            after_copy_chunk: |_: &str, _: u64| copied.set(true),
        };

        let error = snapshot_source_with_hook(&source, &hook).unwrap_err();
        assert_eq!(error.code(), "archive_quota");
        assert!(!copied.get(), "oversized source was copied into the spool");
    }
}
