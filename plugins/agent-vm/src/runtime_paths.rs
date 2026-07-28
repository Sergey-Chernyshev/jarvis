use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const MAX_TRUST_FILES: usize = 64;
const MAX_TRUST_FILE_BYTES: u64 = 256 * 1024;
const MAX_TRUST_TOTAL_BYTES: u64 = 4 * 1024 * 1024;

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

    pub fn shell_command(&self, vm_name: &str, managed: bool) -> String {
        let executable = if managed { "avm" } else { "limactl" };
        format!(
            "env LIMA_HOME={} XDG_CONFIG_HOME={} {executable} shell {}",
            shell_quote(&self.lima_home.to_string_lossy()),
            shell_quote(&self.host_home.join(".config").to_string_lossy()),
            shell_quote(vm_name),
        )
    }

    pub fn sync_trust_from(&self, source_root: &Path) -> Result<usize, String> {
        self.create_private_dirs()?;
        let source = source_root.join("ca-certificates");
        let staging = self
            .registry_root
            .join(format!(".ca-certificates.tmp-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&staging)
            .map_err(|_| "не создать private CA staging directory".to_string())?;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o700))
            .map_err(|_| "не защитить private CA staging directory".to_string())?;

        let result = self.collect_trust_files(&source, &staging);
        let count = match result {
            Ok(count) => count,
            Err(error) => {
                let _ = fs::remove_dir_all(&staging);
                return Err(error);
            }
        };
        if let Err(error) =
            replace_private_directory(&staging, &self.registry_root.join("ca-certificates"))
        {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        Ok(count)
    }

    fn collect_trust_files(&self, source: &Path, staging: &Path) -> Result<usize, String> {
        let metadata = match fs::symlink_metadata(source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(_) => return Err("не проверить host CA directory".into()),
        };
        if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
            return Err("host CA directory имеет unsafe type".into());
        }
        let mut entries = fs::read_dir(source)
            .map_err(|_| "не прочитать host CA directory".to_string())?
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());

        let mut count = 0_usize;
        let mut total = 0_u64;
        for entry in entries {
            let name = entry.file_name();
            if !is_safe_pem_name(&name) {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|_| "не проверить host CA file".to_string())?;
            if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
                continue;
            }
            if metadata.len() > MAX_TRUST_FILE_BYTES {
                return Err("host CA file превышает size limit".into());
            }
            if count >= MAX_TRUST_FILES {
                return Err("host CA directory превышает file-count limit".into());
            }
            let mut source_file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(entry.path())
                .map_err(|_| "не открыть host CA file".to_string())?;
            let mut bytes = Vec::with_capacity(metadata.len() as usize);
            Read::by_ref(&mut source_file)
                .take(MAX_TRUST_FILE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "не прочитать host CA file".to_string())?;
            if bytes.len() as u64 > MAX_TRUST_FILE_BYTES || !looks_like_certificate_pem(&bytes) {
                return Err("host CA file имеет unsafe PEM format".into());
            }
            total = total
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| "host CA total size overflow".to_string())?;
            if total > MAX_TRUST_TOTAL_BYTES {
                return Err("host CA directory превышает total-size limit".into());
            }

            let target = staging.join(&name);
            let mut target_file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(target)
                .map_err(|_| "не создать private CA file".to_string())?;
            target_file
                .write_all(&bytes)
                .and_then(|_| target_file.sync_all())
                .map_err(|_| "не записать private CA file".to_string())?;
            count += 1;
        }
        Ok(count)
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn is_safe_pem_name(name: &std::ffi::OsStr) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes.ends_with(b".pem")
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn looks_like_certificate_pem(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    text.contains("-----BEGIN CERTIFICATE-----")
        && text.contains("-----END CERTIFICATE-----")
        && !text.contains("PRIVATE KEY")
        && !text.bytes().any(|byte| byte == 0)
}

fn replace_private_directory(staging: &Path, target: &Path) -> Result<(), String> {
    let backup = target.with_file_name(format!(".ca-certificates.backup-{}", uuid::Uuid::new_v4()));
    let had_target = match fs::symlink_metadata(target) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
                return Err("private CA directory имеет unsafe type".into());
            }
            fs::rename(target, &backup)
                .map_err(|_| "не подготовить private CA directory swap".to_string())?;
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return Err("не проверить private CA directory".into()),
    };
    if fs::rename(staging, target).is_err() {
        if had_target {
            let _ = fs::rename(&backup, target);
        }
        return Err("не активировать private CA directory".into());
    }
    fs::set_permissions(target, fs::Permissions::from_mode(0o700))
        .map_err(|_| "не защитить private CA directory".to_string())?;
    if had_target {
        fs::remove_dir_all(backup)
            .map_err(|_| "не удалить previous private CA directory".to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
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
    fn shell_command_targets_private_runtime_without_embedding_secrets() {
        let paths = RuntimePaths::from_socket(Path::new("/tmp/Jarvis Profile/run.sock")).unwrap();

        let command = paths.shell_command("sup-ac82ab61d14d", true);

        assert_eq!(
            command,
            "env LIMA_HOME='/tmp/Jarvis Profile/agent-vm/lima' \
XDG_CONFIG_HOME='/tmp/Jarvis Profile/agent-vm/host-home/.config' \
avm shell 'sup-ac82ab61d14d'"
        );
        for forbidden in ["TOKEN", "PASSWORD", "PROXY", "API_KEY", "settings.json"] {
            assert!(!command.to_ascii_uppercase().contains(forbidden));
        }
    }

    #[test]
    fn trust_sync_copies_only_bounded_regular_pem_files_into_private_runtime() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-trust-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let source = root.join("account/.config/agent-vm");
        let source_ca = source.join("ca-certificates");
        fs::create_dir_all(&source_ca).unwrap();
        fs::write(
            source_ca.join("trusted.pem"),
            "-----BEGIN CERTIFICATE-----\nc3ludGhldGlj\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        fs::write(source_ca.join("ignore.txt"), "not a certificate").unwrap();
        fs::write(source.join(".gitconfig"), "[user]\nname = private\n").unwrap();
        fs::create_dir_all(source.join("modules/claude")).unwrap();
        fs::write(
            source.join("modules/claude/settings.json"),
            r#"{"env":{"TOKEN":"must-not-copy"}}"#,
        )
        .unwrap();
        symlink(source_ca.join("trusted.pem"), source_ca.join("linked.pem")).unwrap();
        let paths = RuntimePaths::from_socket(&root.join("profile/run.sock")).unwrap();

        let copied = paths.sync_trust_from(&source).unwrap();

        let target = paths.registry_root.join("ca-certificates");
        assert_eq!(copied, 1);
        assert!(target.join("trusted.pem").is_file());
        assert!(!target.join("ignore.txt").exists());
        assert!(!target.join("linked.pem").exists());
        assert!(!paths.registry_root.join(".gitconfig").exists());
        assert!(!paths.registry_root.join("modules").exists());
        assert_eq!(
            fs::metadata(target.join("trusted.pem"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn socket_must_be_an_absolute_run_sock() {
        assert!(RuntimePaths::from_socket(Path::new("run.sock")).is_err());
        assert!(RuntimePaths::from_socket(Path::new("/tmp/not-run.socket")).is_err());
    }
}
