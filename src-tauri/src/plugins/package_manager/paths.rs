use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

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

    pub fn plugin(&self, plugin_id: &str) -> PathBuf {
        self.plugins_root().join(plugin_id)
    }

    pub fn versions(&self, plugin_id: &str) -> PathBuf {
        self.plugin(plugin_id).join("versions")
    }

    pub fn current(&self, plugin_id: &str) -> PathBuf {
        self.plugin(plugin_id).join("current")
    }

    pub fn quarantine_root(&self) -> PathBuf {
        self.plugins_root().join(".quarantine")
    }

    pub fn data(&self, plugin_id: &str) -> PathBuf {
        self.profile.join("plugin-data").join(plugin_id)
    }

    pub fn cache(&self, plugin_id: &str) -> PathBuf {
        self.profile.join("plugin-cache").join(plugin_id)
    }

    pub fn runtime(&self, plugin_id: &str) -> PathBuf {
        self.profile.join("plugin-runtime").join(plugin_id)
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

    pub(crate) fn prepare_plugin(&self, plugin_id: &str) -> Result<(), StorageError> {
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
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(StorageError::new(
            "plugin_path_symlink",
            format!("{} is a symbolic link", path.display()),
        )),
        Ok(metadata) if !metadata.is_dir() => Err(StorageError::new(
            "plugin_path_not_directory",
            format!("{} is not a directory", path.display()),
        )),
        Ok(_) => fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| {
            StorageError::new(
                "plugin_path_permissions",
                format!("cannot protect {}: {error}", path.display()),
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or_else(|| {
                StorageError::new(
                    "plugin_path_parent",
                    format!("{} has no parent", path.display()),
                )
            })?;
            if !parent.as_os_str().is_empty() && !parent.exists() {
                ensure_real_directory(parent, mode)?;
            } else if !parent.as_os_str().is_empty() {
                let metadata = fs::symlink_metadata(parent).map_err(|error| {
                    StorageError::new(
                        "plugin_path_io",
                        format!("cannot inspect {}: {error}", parent.display()),
                    )
                })?;
                if metadata.file_type().is_symlink() {
                    return Err(StorageError::new(
                        "plugin_path_symlink",
                        format!("{} is a symbolic link", parent.display()),
                    ));
                }
                if !metadata.is_dir() {
                    return Err(StorageError::new(
                        "plugin_path_not_directory",
                        format!("{} is not a directory", parent.display()),
                    ));
                }
            }
            fs::create_dir(path).map_err(|error| {
                StorageError::new(
                    "plugin_path_create",
                    format!("cannot create {}: {error}", path.display()),
                )
            })?;
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| {
                StorageError::new(
                    "plugin_path_permissions",
                    format!("cannot protect {}: {error}", path.display()),
                )
            })
        }
        Err(error) => Err(StorageError::new(
            "plugin_path_io",
            format!("cannot inspect {}: {error}", path.display()),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::PluginPaths;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
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
        assert_eq!(
            paths.versions("dev.example.echo"),
            Path::new("/profile/plugins/dev.example.echo/versions")
        );
        assert_eq!(
            paths.current("dev.example.echo"),
            Path::new("/profile/plugins/dev.example.echo/current")
        );
        assert_eq!(
            paths.quarantine_root(),
            Path::new("/profile/plugins/.quarantine")
        );
        assert_eq!(
            paths.data("dev.example.echo"),
            Path::new("/profile/plugin-data/dev.example.echo")
        );
        assert_eq!(
            paths.cache("dev.example.echo"),
            Path::new("/profile/plugin-cache/dev.example.echo")
        );
        assert_eq!(
            paths.runtime("dev.example.echo"),
            Path::new("/profile/plugin-runtime/dev.example.echo")
        );
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
}
