use std::ffi::CString;
use std::fs::{self, File};
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest as _, Sha256};

#[derive(Clone, Debug)]
pub struct VerifiedExecutable {
    verified_descriptor: Arc<File>,
    root_descriptor: Arc<File>,
    relative_path: PathBuf,
    display_path: PathBuf,
    verified_len: u64,
    verified_digest: [u8; 32],
}

impl VerifiedExecutable {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let display_path = path.into();
        let activation_root = display_path
            .parent()
            .ok_or_else(|| "verified plugin executable не имеет package root".to_owned())?
            .to_path_buf();
        let relative_path = display_path
            .file_name()
            .ok_or_else(|| "verified plugin executable не имеет имени".to_owned())?
            .into();
        let verified_descriptor = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&display_path)
            .map_err(|error| {
                format!(
                    "не открыть verified plugin descriptor {}: {error}",
                    display_path.display()
                )
            })?;
        Self::from_descriptor(verified_descriptor, activation_root, relative_path)
    }

    pub(crate) fn from_descriptor(
        verified_descriptor: File,
        activation_root: PathBuf,
        relative_path: PathBuf,
    ) -> Result<Self, String> {
        validate_relative_executable_path(&relative_path)?;
        let display_path = activation_root.join(&relative_path);
        let verified_metadata = verified_descriptor.metadata().map_err(|error| {
            format!(
                "не проверить verified plugin descriptor {}: {error}",
                display_path.display()
            )
        })?;
        let root_descriptor = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&activation_root)
            .map_err(|error| {
                format!(
                    "не открыть verified plugin root lease {}: {error}",
                    activation_root.display()
                )
            })?;
        let root_metadata = root_descriptor.metadata().map_err(|error| {
            format!(
                "не проверить verified plugin root lease {}: {error}",
                activation_root.display()
            )
        })?;
        if !root_metadata.is_dir()
            || root_metadata.uid() != effective_uid()
            || root_metadata.mode() & 0o7777 != 0o555
        {
            return Err(format!(
                "verified plugin root {} не является owner-owned immutable directory",
                activation_root.display()
            ));
        }
        let anchored_descriptor = open_relative_file(&root_descriptor, &relative_path)?;
        let anchored_metadata = anchored_descriptor.metadata().map_err(|error| {
            format!(
                "не проверить anchored verified plugin executable {}: {error}",
                display_path.display()
            )
        })?;
        if verified_metadata.dev() != anchored_metadata.dev()
            || verified_metadata.ino() != anchored_metadata.ino()
        {
            return Err(format!(
                "verified plugin executable {} изменился до acquisition root lease",
                display_path.display()
            ));
        }
        let verified_len = verified_metadata.len();
        let verified_digest = descriptor_digest(&verified_descriptor, verified_len)?;
        Ok(Self {
            verified_descriptor: Arc::new(verified_descriptor),
            root_descriptor: Arc::new(root_descriptor),
            relative_path,
            display_path,
            verified_len,
            verified_digest,
        })
    }

    pub fn display_path(&self) -> &std::path::Path {
        &self.display_path
    }

    pub(crate) fn prepare_exec_lease(&self, profile_root: &Path) -> Result<ExactExecLease, String> {
        // Revalidate the anchored package inode immediately before copying. The
        // executable bytes themselves are read only from the held descriptor.
        let anchored_descriptor = open_relative_file(&self.root_descriptor, &self.relative_path)?;
        let anchored_metadata = anchored_descriptor.metadata().map_err(|error| {
            format!(
                "не проверить anchored plugin executable {} перед spawn: {error}",
                self.display_path.display()
            )
        })?;
        let verified_metadata = self.verified_descriptor.metadata().map_err(|error| {
            format!(
                "не проверить held plugin executable {} перед spawn: {error}",
                self.display_path.display()
            )
        })?;
        if anchored_metadata.dev() != verified_metadata.dev()
            || anchored_metadata.ino() != verified_metadata.ino()
        {
            return Err(format!(
                "verified plugin executable {} изменился перед spawn",
                self.display_path.display()
            ));
        }

        ExactExecLease::materialize(
            profile_root,
            &self.verified_descriptor,
            self.verified_len,
            self.verified_digest,
        )
    }
}

fn descriptor_digest(file: &File, len: u64) -> Result<[u8; 32], String> {
    let mut hasher = Sha256::new();
    let mut offset = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    while offset < len {
        let remaining = usize::try_from((len - offset).min(buffer.len() as u64))
            .map_err(|_| "verified plugin executable size overflow".to_string())?;
        let read = file
            .read_at(&mut buffer[..remaining], offset)
            .map_err(|error| format!("не прочитать verified plugin descriptor: {error}"))?;
        if read == 0 {
            return Err("verified plugin executable усечён во время чтения".into());
        }
        hasher.update(&buffer[..read]);
        offset += read as u64;
    }
    Ok(hasher.finalize().into())
}

pub(crate) struct ExactExecLease {
    directory: PathBuf,
    pub(crate) executable: PathBuf,
}

impl ExactExecLease {
    fn materialize(
        profile_root: &Path,
        source: &File,
        expected_len: u64,
        expected_digest: [u8; 32],
    ) -> Result<Self, String> {
        let parent = prepare_exec_lease_parent(profile_root)?;
        let directory = create_unique_exec_lease_dir(&parent)?;
        let executable = directory.join("bridge");
        let result = (|| {
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o700)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&executable)
                .map_err(|error| format!("не создать verified exec lease: {error}"))?;
            let mut hasher = Sha256::new();
            let mut offset = 0_u64;
            let mut buffer = [0_u8; 64 * 1024];
            while offset < expected_len {
                let remaining = usize::try_from((expected_len - offset).min(buffer.len() as u64))
                    .map_err(|_| "verified exec lease size overflow".to_string())?;
                let read = source
                    .read_at(&mut buffer[..remaining], offset)
                    .map_err(|error| format!("не прочитать held plugin bytes: {error}"))?;
                if read == 0 {
                    return Err("held plugin executable усечён перед spawn".into());
                }
                output
                    .write_all(&buffer[..read])
                    .map_err(|error| format!("не записать verified exec lease: {error}"))?;
                hasher.update(&buffer[..read]);
                offset += read as u64;
            }
            if <[u8; 32]>::from(hasher.finalize()) != expected_digest {
                return Err("held plugin executable digest изменился перед spawn".into());
            }
            output
                .sync_all()
                .map_err(|error| format!("не синхронизировать verified exec lease: {error}"))?;
            output
                .set_permissions(fs::Permissions::from_mode(0o500))
                .map_err(|error| format!("не заморозить verified exec lease: {error}"))?;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o500))
                .map_err(|error| format!("не заморозить verified exec lease directory: {error}"))?;
            Ok(())
        })();
        if let Err(error) = result {
            cleanup_exec_lease(&directory, &executable);
            return Err(error);
        }
        Ok(Self {
            directory,
            executable,
        })
    }
}

impl Drop for ExactExecLease {
    fn drop(&mut self) {
        cleanup_exec_lease(&self.directory, &self.executable);
    }
}

fn prepare_exec_lease_parent(profile_root: &Path) -> Result<PathBuf, String> {
    let parent = profile_root.join("plugin-exec-leases");
    match fs::create_dir(&parent) {
        Ok(()) => fs::set_permissions(&parent, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("не защитить verified exec lease root: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "не создать verified exec lease root {}: {error}",
                parent.display()
            ))
        }
    }
    let metadata = fs::symlink_metadata(&parent)
        .map_err(|error| format!("не проверить verified exec lease root: {error}"))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != effective_uid()
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(format!(
            "verified exec lease root {} должен быть owner-only directory",
            parent.display()
        ));
    }
    Ok(parent)
}

fn create_unique_exec_lease_dir(parent: &Path) -> Result<PathBuf, String> {
    for _ in 0..16 {
        let mut random = [0_u8; 16];
        getrandom::getrandom(&mut random)
            .map_err(|_| "не получить entropy для verified exec lease".to_string())?;
        let name = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let directory = parent.join(name);
        match fs::create_dir(&directory) {
            Ok(()) => {
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(
                    |error| format!("не защитить verified exec lease directory: {error}"),
                )?;
                return Ok(directory);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("не создать verified exec lease directory: {error}")),
        }
    }
    Err("не выделить уникальный verified exec lease".into())
}

fn cleanup_exec_lease(directory: &Path, executable: &Path) {
    let _ = fs::set_permissions(directory, fs::Permissions::from_mode(0o700));
    let _ = fs::set_permissions(executable, fs::Permissions::from_mode(0o700));
    let _ = fs::remove_file(executable);
    let _ = fs::remove_dir(directory);
}

fn validate_relative_executable_path(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("verified plugin executable path должен быть безопасным relative path".into());
    }
    Ok(())
}

fn open_relative_file(root: &File, path: &Path) -> Result<File, String> {
    let mut directory = root.try_clone().map_err(|error| {
        format!(
            "не клонировать verified plugin root descriptor для {}: {error}",
            path.display()
        )
    })?;
    let components = path.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err("verified plugin executable path неканоничен".into());
        };
        let name = CString::new(name.as_bytes())
            .map_err(|_| "verified plugin executable path содержит NUL".to_owned())?;
        let is_last = index + 1 == components.len();
        let flags = if is_last {
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        } else {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        };
        let descriptor = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if descriptor < 0 {
            return Err(format!(
                "не открыть anchored verified plugin path {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        let opened = unsafe { File::from_raw_fd(descriptor) };
        let metadata = opened.metadata().map_err(|error| {
            format!(
                "не проверить anchored verified plugin path {}: {error}",
                path.display()
            )
        })?;
        let expected_mode = 0o555;
        if metadata.uid() != effective_uid()
            || metadata.mode() & 0o7777 != expected_mode
            || (is_last && (!metadata.is_file() || metadata.nlink() != 1))
            || (!is_last && !metadata.is_dir())
        {
            return Err(format!(
                "anchored verified plugin path {} не является immutable package entry",
                path.display()
            ));
        }
        if is_last {
            return Ok(opened);
        }
        directory = opened;
    }
    Err("verified plugin executable path пуст".into())
}

fn effective_uid() -> u32 {
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "jarvis-exec-{}-{}",
                std::process::id(),
                super::super::package_manager::random_storage_id().unwrap()
            ));
            fs::create_dir_all(root.join("visible/package/bin")).unwrap();
            let f = Self(root);
            fs::write(f.executable(), "#!/bin/sh\nprintf verified").unwrap();
            f.freeze();
            f
        }
        fn executable(&self) -> PathBuf {
            self.0.join("visible/package/bin/bridge")
        }
        fn freeze(&self) {
            for p in [
                self.executable(),
                self.0.join("visible/package/bin"),
                self.0.join("visible/package"),
            ] {
                fs::set_permissions(p, fs::Permissions::from_mode(0o555)).unwrap();
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            for container in ["visible", "held"] {
                for suffix in ["package", "package/bin"] {
                    let _ = fs::set_permissions(
                        self.0.join(container).join(suffix),
                        fs::Permissions::from_mode(0o700),
                    );
                }
            }
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn verified_receipt_executes_held_bytes_after_visible_path_swap() {
        let f = Fixture::new();
        let held = VerifiedExecutable::open(f.executable()).unwrap();
        fs::rename(f.0.join("visible"), f.0.join("held")).unwrap();
        fs::create_dir_all(f.0.join("visible/package/bin")).unwrap();
        fs::write(f.executable(), "#!/bin/sh\nprintf replaced").unwrap();
        f.freeze();
        let lease = held.prepare_exec_lease(&f.0).unwrap();
        let output = Command::new(&lease.executable)
            .env_clear()
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"verified");
        drop(lease);
        assert_eq!(
            fs::read_dir(f.0.join("plugin-exec-leases"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn replaced_or_deleted_anchored_file_fails_before_lease() {
        for replace in [false, true] {
            let f = Fixture::new();
            let held = VerifiedExecutable::open(f.executable()).unwrap();
            fs::set_permissions(
                f.0.join("visible/package/bin"),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
            fs::remove_file(f.executable()).unwrap();
            if replace {
                fs::write(f.executable(), "#!/bin/sh\nprintf replaced").unwrap();
                f.freeze();
            }
            assert!(held.prepare_exec_lease(&f.0).is_err());
        }
    }

    #[test]
    fn in_place_modified_held_bytes_fail_digest_revalidation() {
        let f = Fixture::new();
        let held = VerifiedExecutable::open(f.executable()).unwrap();
        fs::set_permissions(f.executable(), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(f.executable(), "#!/bin/sh\nprintf replaced").unwrap();
        f.freeze();
        assert!(held.prepare_exec_lease(&f.0).is_err());
    }
}
