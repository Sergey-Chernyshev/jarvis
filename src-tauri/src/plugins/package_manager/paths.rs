use std::ffi::CString;
use std::fs;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use jarvis_plugin_protocol::manifest::PluginId;

use super::secure_fs;
use super::StorageError;

#[derive(Clone, Debug)]
pub struct PluginPaths {
    profile: PathBuf,
}

impl PluginPaths {
    pub fn new(profile: PathBuf) -> Self {
        Self { profile }
    }

    pub fn profile(&self) -> &Path {
        &self.profile
    }

    pub fn plugins_root(&self) -> PathBuf {
        self.profile.join("plugins")
    }

    pub fn plugin(&self, plugin_id: &PluginId) -> PathBuf {
        self.plugins_root().join(plugin_id.as_str())
    }

    pub fn versions(&self, plugin_id: &PluginId) -> PathBuf {
        self.plugin(plugin_id).join("versions")
    }

    pub fn current(&self, plugin_id: &PluginId) -> PathBuf {
        self.plugin(plugin_id).join("current")
    }

    pub fn quarantine_root(&self) -> PathBuf {
        self.plugins_root().join(".quarantine")
    }

    pub fn data(&self, plugin_id: &PluginId) -> PathBuf {
        self.profile.join("plugin-data").join(plugin_id.as_str())
    }

    pub fn cache(&self, plugin_id: &PluginId) -> PathBuf {
        self.profile.join("plugin-cache").join(plugin_id.as_str())
    }

    pub fn runtime(&self, plugin_id: &PluginId) -> PathBuf {
        self.profile.join("plugin-runtime").join(plugin_id.as_str())
    }

    pub fn operations_db(&self) -> PathBuf {
        self.plugins_root().join("operations.sqlite3")
    }

    pub fn manager_lock(&self) -> PathBuf {
        self.plugins_root().join(".manager.lock")
    }

    pub fn prepare(&self) -> Result<(), StorageError> {
        ensure_real_directory(&self.profile, 0o700)?;
        for root in [
            self.plugins_root(),
            self.quarantine_root(),
            self.profile.join("plugin-data"),
            self.profile.join("plugin-cache"),
            self.profile.join("plugin-runtime"),
        ] {
            ensure_real_directory(&root, 0o700)?;
        }
        Ok(())
    }

    pub(crate) fn prepare_plugin(&self, plugin_id: &PluginId) -> Result<(), StorageError> {
        self.prepare()?;
        ensure_real_directory(&self.plugin(plugin_id), 0o700)?;
        ensure_real_directory(&self.versions(plugin_id), 0o700)?;
        for private_root in [
            self.data(plugin_id),
            self.cache(plugin_id),
            self.runtime(plugin_id),
        ] {
            ensure_real_directory(&private_root, 0o700)?;
        }
        Ok(())
    }
}

pub(crate) fn ensure_real_directory(path: &Path, mode: u32) -> Result<(), StorageError> {
    open_directory_components(path, Some(mode)).map(|_| ())
}

pub(crate) fn open_real_directory(path: &Path) -> Result<fs::File, StorageError> {
    open_directory_components(path, None)
}

fn open_directory_components(
    path: &Path,
    create_mode: Option<u32>,
) -> Result<fs::File, StorageError> {
    if !path.is_absolute() {
        return Err(StorageError::new(
            "plugin_path_escape",
            format!("{} is not an absolute profile path", path.display()),
        ));
    }
    let mut directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")
        .map_err(|error| {
            StorageError::new(
                "plugin_path_io",
                format!("cannot open filesystem root: {error}"),
            )
        })?;
    let mut resolved = PathBuf::from("/");
    let mut saw_component = false;

    for component in path.components() {
        let std::path::Component::Normal(name) = component else {
            if matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            ) {
                return Err(StorageError::new(
                    "plugin_path_escape",
                    format!("{} contains an unsafe path component", path.display()),
                ));
            }
            continue;
        };
        saw_component = true;
        resolved.push(name);
        let name = CString::new(name.as_bytes()).map_err(|_| {
            StorageError::new(
                "plugin_path_escape",
                format!("{} contains NUL", resolved.display()),
            )
        })?;
        match secure_fs::entry_metadata(&directory, &name) {
            Ok(metadata) => {
                if secure_fs::is_type(&metadata, libc::S_IFLNK) {
                    return Err(StorageError::new(
                        "plugin_path_symlink",
                        format!("{} is a symbolic link", resolved.display()),
                    ));
                }
                if !secure_fs::is_type(&metadata, libc::S_IFDIR) {
                    return Err(StorageError::new(
                        "plugin_path_not_directory",
                        format!("{} is not a directory", resolved.display()),
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(mode) = create_mode else {
                    return Err(StorageError::new(
                        "plugin_path_io",
                        format!("{} does not exist", resolved.display()),
                    ));
                };
                if unsafe {
                    libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), mode as libc::mode_t)
                } != 0
                {
                    let error = std::io::Error::last_os_error();
                    return Err(StorageError::new(
                        "plugin_path_create",
                        format!("cannot create {}: {error}", resolved.display()),
                    ));
                }
            }
            Err(error) => {
                return Err(StorageError::new(
                    "plugin_path_io",
                    format!("cannot inspect {}: {error}", resolved.display()),
                ));
            }
        }
        let descriptor = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            let error = std::io::Error::last_os_error();
            return Err(StorageError::new(
                if error.raw_os_error() == Some(libc::ELOOP) {
                    "plugin_path_symlink"
                } else {
                    "plugin_path_io"
                },
                format!("cannot open {}: {error}", resolved.display()),
            ));
        }
        directory = unsafe { fs::File::from_raw_fd(descriptor) };
    }
    if !saw_component {
        return Err(StorageError::new(
            "plugin_path_escape",
            "filesystem root cannot be a plugin profile",
        ));
    }
    if let Some(mode) = create_mode {
        let metadata = secure_fs::metadata(&directory).map_err(|error| {
            StorageError::new(
                "plugin_path_io",
                format!("cannot inspect opened {}: {error}", path.display()),
            )
        })?;
        if !secure_fs::is_type(&metadata, libc::S_IFDIR)
            || !secure_fs::owned_by_effective_user(&metadata)
        {
            return Err(StorageError::new(
                "plugin_path_owner",
                format!("{} is not owned by the current user", path.display()),
            ));
        }
        if let Err(error) = secure_fs::chmod(&directory, mode) {
            return Err(StorageError::new(
                "plugin_path_permissions",
                format!("cannot protect {}: {error}", path.display()),
            ));
        }
    }
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::PluginPaths;
    use jarvis_plugin_protocol::manifest::PluginId;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);

    fn temp_root(label: &str) -> PathBuf {
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "jarvis-plugin-paths-{label}-{}-{}",
                std::process::id(),
                NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed)
            ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("profile")).unwrap();
        fs::create_dir_all(root.join("outside")).unwrap();
        root
    }

    #[test]
    fn profile_layout_matches_the_v2_contract() {
        let paths = PluginPaths::new(PathBuf::from("/profile"));
        let plugin_id = PluginId::new("dev.example.echo").unwrap();
        assert_eq!(
            paths.versions(&plugin_id),
            Path::new("/profile/plugins/dev.example.echo/versions")
        );
        assert_eq!(
            paths.current(&plugin_id),
            Path::new("/profile/plugins/dev.example.echo/current")
        );
        assert_eq!(
            paths.quarantine_root(),
            Path::new("/profile/plugins/.quarantine")
        );
        assert_eq!(
            paths.data(&plugin_id),
            Path::new("/profile/plugin-data/dev.example.echo")
        );
        assert_eq!(
            paths.cache(&plugin_id),
            Path::new("/profile/plugin-cache/dev.example.echo")
        );
        assert_eq!(
            paths.runtime(&plugin_id),
            Path::new("/profile/plugin-runtime/dev.example.echo")
        );
    }

    #[test]
    fn invalid_plugin_ids_cannot_reach_path_helpers() {
        assert!(PluginId::new("../outside").is_err());
        assert!(PluginId::new("/absolute").is_err());
        assert!(serde_json::from_str::<PluginId>("\"../../outside\"").is_err());
    }

    #[test]
    fn refuses_symlinked_profile_components() {
        let root = temp_root("symlink");
        symlink(root.join("outside"), root.join("profile/plugins")).unwrap();

        assert_eq!(
            PluginPaths::new(root.join("profile"))
                .prepare()
                .unwrap_err()
                .code(),
            "plugin_path_symlink"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_symlinked_profile_ancestor() {
        let root = temp_root("ancestor-symlink");
        fs::create_dir_all(root.join("outside/profile")).unwrap();
        symlink(root.join("outside"), root.join("alias")).unwrap();

        assert_eq!(
            PluginPaths::new(root.join("alias/profile"))
                .prepare()
                .unwrap_err()
                .code(),
            "plugin_path_symlink"
        );

        fs::remove_dir_all(root).unwrap();
    }
}
