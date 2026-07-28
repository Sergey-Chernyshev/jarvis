use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimePaths {
    pub jarvis_dir: PathBuf,
    pub state_root: PathBuf,
    pub host_home: PathBuf,
    pub lima_home: PathBuf,
    pub registry_root: PathBuf,
    pub project_links: PathBuf,
    pub runs_root: PathBuf,
}

impl RuntimePaths {
    pub fn from_socket(socket: &Path) -> Result<Self, String> {
        if !socket.is_absolute()
            || socket.file_name().and_then(|name| name.to_str()) != Some("run.sock")
        {
            return Err("JARVIS_SOCKET должен быть абсолютным путём к run.sock".into());
        }
        let jarvis_dir = socket
            .parent()
            .ok_or_else(|| "JARVIS_SOCKET не содержит каталог профиля".to_string())?
            .to_path_buf();
        let state_root = jarvis_dir.join("agent-vm");
        let host_home = state_root.join("host-home");
        Ok(Self {
            registry_root: host_home.join(".config/agent-vm"),
            lima_home: state_root.join("lima"),
            project_links: state_root.join("projects"),
            runs_root: state_root.join("runs"),
            jarvis_dir,
            state_root,
            host_home,
        })
    }

    pub fn create_private_dirs(&self) -> Result<(), String> {
        for path in [
            &self.state_root,
            &self.host_home,
            &self.lima_home,
            &self.registry_root,
            &self.project_links,
            &self.runs_root,
        ] {
            fs::create_dir_all(path)
                .map_err(|err| format!("не создать private runtime {}: {err}", path.display()))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|err| format!("не защитить private runtime {}: {err}", path.display()))?;
        }
        Ok(())
    }

    pub fn command_env(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("HOME".into(), self.host_home.to_string_lossy().into_owned()),
            ("LANG".into(), "C.UTF-8".into()),
            (
                "LIMA_HOME".into(),
                self.lima_home.to_string_lossy().into_owned(),
            ),
            (
                "PATH".into(),
                "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".into(),
            ),
            (
                "XDG_CONFIG_HOME".into(),
                self.host_home
                    .join(".config")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ])
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn all_mutable_runtime_state_is_below_jarvis_dir() {
        let paths = RuntimePaths::from_socket(Path::new("/tmp/jarvis-profile/run.sock")).unwrap();

        assert_eq!(paths.jarvis_dir, Path::new("/tmp/jarvis-profile"));
        assert!(paths.state_root.starts_with(&paths.jarvis_dir));
        assert!(paths.host_home.starts_with(&paths.jarvis_dir));
        assert!(paths.lima_home.starts_with(&paths.jarvis_dir));
        assert!(paths.registry_root.starts_with(&paths.jarvis_dir));
        assert!(paths.project_links.starts_with(&paths.jarvis_dir));
        assert!(paths.runs_root.starts_with(&paths.jarvis_dir));
    }

    #[test]
    fn child_environment_is_an_allowlist_without_host_proxy_or_credentials() {
        let paths = RuntimePaths::from_socket(Path::new("/tmp/jarvis-profile/run.sock")).unwrap();
        let env = paths.command_env();
        let keys = env.keys().map(String::as_str).collect::<Vec<_>>();

        assert_eq!(
            keys,
            vec!["HOME", "LANG", "LIMA_HOME", "PATH", "XDG_CONFIG_HOME"]
        );
        for forbidden in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "NODE_EXTRA_CA_CERTS",
        ] {
            assert!(!env.contains_key(forbidden));
        }
    }

    #[test]
    fn socket_must_be_an_absolute_run_sock() {
        assert!(RuntimePaths::from_socket(Path::new("run.sock")).is_err());
        assert!(RuntimePaths::from_socket(Path::new("/tmp/not-run.socket")).is_err());
    }
}
