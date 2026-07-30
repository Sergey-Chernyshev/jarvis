use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Cursor, Read};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use jarvis_secret_store::{SecretKind, SecretStore, SecretValue};
use serde::Serialize;
use tar::{Builder, EntryType, Header};
use zeroize::Zeroize;

use crate::config_mirror::ConfigSnapshot;
use crate::inventory::{is_safe_guest_workspace, VmRecord};
use crate::project::is_valid_vm_name;
use crate::runner::{CommandRunner, CommandSpec};

pub const MAX_CODEX_AUTH_BYTES: u64 = 1024 * 1024;
const GUEST_BOOTSTRAP_SCRIPT: &str = r#"
user_name="$1"
guest_home="$2"
umask 077
stage="$(mktemp -d /tmp/jarvis-vm-bootstrap.XXXXXX)"
cleanup() {
  rm -rf -- "$stage"
}
trap cleanup EXIT
tar --extract --file=- --directory="$stage" --no-same-owner --no-same-permissions
while IFS= read -r -d '' source; do
  relative="${source#"$stage"/}"
  case "$relative" in
    .claude/*|.codex/*|.jarvis-vm/*) ;;
    *) exit 64 ;;
  esac
  target="$guest_home/$relative"
  parent="$(dirname "$target")"
  install -d -m 0700 -o "$user_name" -g "$user_name" "$parent"
  mode="$(stat -c '%a' "$source")"
  case "$mode" in
    600|700) ;;
    *) exit 65 ;;
  esac
  temporary="$(mktemp "$parent/.jarvis-bootstrap.XXXXXX")"
  cat -- "$source" > "$temporary"
  chown "$user_name:$user_name" "$temporary"
  chmod "$mode" "$temporary"
  mv -f -- "$temporary" "$target"
done < <(find "$stage" -type f -print0 | sort -z)
"#;
const GUEST_CREDENTIAL_PROBE_SCRIPT: &str = r#"
user_name="$1"
guest_home="$2"
credential_ready() {
  path="$1"
  [ -f "$path" ] &&
    [ ! -L "$path" ] &&
    [ -s "$path" ] &&
    [ "$(stat -c '%U' -- "$path")" = "$user_name" ] &&
    [ "$(stat -c '%a' -- "$path")" = "600" ]
}
claude="missing"
codex="missing"
if credential_ready "$guest_home/.claude/.credentials.json"; then
  claude="ready"
fi
if credential_ready "$guest_home/.codex/auth.json"; then
  codex="ready"
fi
printf 'claude=%s\ncodex=%s\n' "$claude" "$codex"
"#;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapCredentialStatus {
    pub claude: String,
    pub codex: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct BootstrapBundle {
    pub archive: Vec<u8>,
    pub credential_status: BootstrapCredentialStatus,
}

impl std::fmt::Debug for BootstrapBundle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BootstrapBundle")
            .field("archive_bytes", &self.archive.len())
            .field("credential_status", &self.credential_status)
            .finish()
    }
}

impl Drop for BootstrapBundle {
    fn drop(&mut self) {
        self.archive.zeroize();
    }
}

pub struct LoadedCodexCredential {
    file_bytes: Option<Vec<u8>>,
    status: &'static str,
}

impl LoadedCodexCredential {
    fn file(file_bytes: Option<Vec<u8>>) -> Self {
        let status = if file_bytes.is_some() {
            "ready"
        } else {
            "missing"
        };
        Self { file_bytes, status }
    }

    fn host_keyring() -> Self {
        Self {
            file_bytes: None,
            status: "host-keyring",
        }
    }

    pub fn ready_without_copy() -> Self {
        Self {
            file_bytes: None,
            status: "ready",
        }
    }

    pub fn file_bytes(&self) -> Option<&[u8]> {
        self.file_bytes.as_deref()
    }

    pub fn status(&self) -> &'static str {
        self.status
    }
}

impl std::fmt::Debug for LoadedCodexCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoadedCodexCredential")
            .field("status", &self.status)
            .field("bytes_redacted", &self.file_bytes.as_ref().map(Vec::len))
            .finish()
    }
}

impl Drop for LoadedCodexCredential {
    fn drop(&mut self) {
        if let Some(bytes) = &mut self.file_bytes {
            bytes.zeroize();
        }
    }
}

pub fn load_codex_credential(
    snapshot: &ConfigSnapshot,
    codex_home: &Path,
) -> Result<LoadedCodexCredential, String> {
    let mode = snapshot
        .files
        .iter()
        .find(|file| file.guest_path == Path::new(".codex/config.toml"))
        .map(|file| {
            let text = std::str::from_utf8(&file.bytes)
                .map_err(|_| "Codex config.toml должен быть UTF-8".to_string())?;
            let config = toml::from_str::<toml::Value>(text)
                .map_err(|_| "Codex config.toml содержит invalid TOML".to_string())?;
            match config.get("cli_auth_credentials_store") {
                None => Ok("file".to_string()),
                Some(value) => value
                    .as_str()
                    .ok_or_else(|| {
                        "Codex cli_auth_credentials_store должен быть string".to_string()
                    })
                    .map(str::to_string),
            }
        })
        .transpose()?
        .unwrap_or_else(|| "file".to_string());
    match mode.as_str() {
        "file" => Ok(LoadedCodexCredential::file(read_codex_file_credential(
            codex_home,
        )?)),
        "keyring" | "auto" => Ok(LoadedCodexCredential::host_keyring()),
        _ => Err("Codex cli_auth_credentials_store содержит неизвестный режим".into()),
    }
}

pub fn read_codex_file_credential(codex_home: &Path) -> Result<Option<Vec<u8>>, String> {
    let path = codex_home.join("auth.json");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("не проверить Codex file credential".into()),
    };
    if !metadata.file_type().is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_CODEX_AUTH_BYTES
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err("Codex file credential имеет небезопасный тип, mode или размер".into());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|_| "не открыть Codex file credential".to_string())?;
    let opened = file
        .metadata()
        .map_err(|_| "не проверить opened Codex file credential".to_string())?;
    if !opened.is_file()
        || opened.len() == 0
        || opened.len() > MAX_CODEX_AUTH_BYTES
        || opened.permissions().mode() & 0o077 != 0
    {
        return Err("Codex file credential изменился во время проверки".into());
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.by_ref()
        .take(MAX_CODEX_AUTH_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "не прочитать Codex file credential".to_string())?;
    if let Err(error) = validate_codex_auth(&bytes) {
        bytes.zeroize();
        return Err(error);
    }
    Ok(Some(bytes))
}

pub fn build_bundle<S: SecretStore>(
    snapshot: &ConfigSnapshot,
    store: &S,
    claude_kind: Option<SecretKind>,
    host_claude_login: Option<&SecretValue>,
    codex_credential: &LoadedCodexCredential,
    private_env: &BTreeMap<String, String>,
) -> Result<BootstrapBundle, String> {
    let mut files = BTreeMap::<PathBuf, (u32, Vec<u8>)>::new();
    for file in &snapshot.files {
        validate_bundle_path(&file.guest_path)?;
        if !matches!(file.mode, 0o600 | 0o700) {
            return Err("config snapshot содержит unsafe file mode".into());
        }
        if files
            .insert(file.guest_path.clone(), (file.mode, file.bytes.clone()))
            .is_some()
        {
            return Err("config snapshot содержит duplicate path".into());
        }
    }

    let mut agent_env = String::new();
    let configured_claude = match claude_kind {
        Some(kind) => store.get(kind)?.map(|value| (kind, value)),
        None => None,
    };
    let claude = if let Some((kind, value)) = configured_claude {
        let secret = std::str::from_utf8(value.expose())
            .map_err(|_| "Claude secret должен быть UTF-8".to_string())?;
        if secret.bytes().any(|byte| matches!(byte, 0 | b'\n' | b'\r')) {
            return Err("Claude secret содержит недопустимые control bytes".into());
        }
        let variable = match kind {
            SecretKind::ClaudeApiKey => "ANTHROPIC_API_KEY",
            SecretKind::ClaudeOauthToken => "CLAUDE_CODE_OAUTH_TOKEN",
        };
        agent_env.push_str(&format!("export {variable}={}\n", shell_quote(secret)));
        "ready".into()
    } else if let Some(login) = host_claude_login {
        let credentials = normalize_claude_login(login)?;
        if files
            .insert(
                PathBuf::from(".claude/.credentials.json"),
                (0o600, credentials),
            )
            .is_some()
        {
            return Err("Claude credential конфликтует с config snapshot".into());
        }
        "ready".into()
    } else {
        "missing".into()
    };
    for (name, value) in private_env {
        if !matches!(
            name.as_str(),
            "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY" | "NO_PROXY"
        ) || value.is_empty()
            || value.len() > 64 * 1024
            || value.bytes().any(|byte| matches!(byte, 0 | b'\n' | b'\r'))
        {
            return Err("private runtime env содержит unsafe entry".into());
        }
        agent_env.push_str(&format!("export {name}={}\n", shell_quote(value)));
    }
    if !agent_env.is_empty() {
        files.insert(
            PathBuf::from(".jarvis-vm/agent.env"),
            (0o600, agent_env.into_bytes()),
        );
    }

    let codex = if let Some(auth) = codex_credential.file_bytes() {
        validate_codex_auth(auth)?;
        files.insert(PathBuf::from(".codex/auth.json"), (0o600, auth.to_vec()));
        "ready".into()
    } else {
        codex_credential.status().into()
    };
    let archive = deterministic_tar(&files);
    for (_, bytes) in files.values_mut() {
        bytes.zeroize();
    }
    let archive = archive?;
    Ok(BootstrapBundle {
        archive,
        credential_status: BootstrapCredentialStatus { claude, codex },
    })
}

struct SensitiveJson(serde_json::Value);

impl Drop for SensitiveJson {
    fn drop(&mut self) {
        zeroize_json(&mut self.0);
    }
}

fn normalize_claude_login(login: &SecretValue) -> Result<Vec<u8>, String> {
    let mut parsed = SensitiveJson(
        serde_json::from_slice(login.expose())
            .map_err(|_| "Claude Code Keychain credential содержит invalid JSON".to_string())?,
    );
    let oauth = parsed
        .0
        .as_object_mut()
        .and_then(|root| root.remove("claudeAiOauth"))
        .ok_or_else(|| "Claude Code Keychain credential не содержит claudeAiOauth".to_string())?;
    let oauth_fields = oauth
        .as_object()
        .ok_or_else(|| "Claude Code claudeAiOauth должен быть object".to_string())?;
    for field in ["accessToken", "refreshToken"] {
        let valid = oauth_fields
            .get(field)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| {
                !value.is_empty()
                    && value.len() <= jarvis_secret_store::MAX_SECRET_BYTES
                    && !value.contains('\0')
            });
        if !valid {
            let _oauth = SensitiveJson(oauth);
            return Err(format!(
                "Claude Code claudeAiOauth.{field} имеет unsafe format"
            ));
        }
    }
    let normalized = SensitiveJson(serde_json::json!({"claudeAiOauth":oauth}));
    serde_json::to_vec(&normalized.0)
        .map_err(|_| "не сериализовать Claude Code Linux credential".to_string())
}

pub fn bootstrap_spec(
    limactl: &Path,
    env: &BTreeMap<String, String>,
    record: &VmRecord,
    mut bundle: BootstrapBundle,
) -> Result<CommandSpec, String> {
    let expected_home = validated_guest_home(limactl, record)?;
    validate_archive(&bundle.archive)?;
    Ok(CommandSpec {
        program: limactl.to_path_buf(),
        args: vec![
            "shell".into(),
            "--tty=false".into(),
            "--workdir".into(),
            "/".into(),
            record.name.clone(),
            "--".into(),
            "sudo".into(),
            "/bin/bash".into(),
            "-ceu".into(),
            GUEST_BOOTSTRAP_SCRIPT.into(),
            "jarvis-bootstrap".into(),
            record.user.clone(),
            expected_home.to_string_lossy().into_owned(),
        ],
        cwd: None,
        env: env.clone(),
        stdin: Some(std::mem::take(&mut bundle.archive)),
    })
}

pub fn guest_credential_probe_spec(
    limactl: &Path,
    env: &BTreeMap<String, String>,
    record: &VmRecord,
) -> Result<CommandSpec, String> {
    let expected_home = validated_guest_home(limactl, record)?;
    Ok(CommandSpec {
        program: limactl.to_path_buf(),
        args: vec![
            "shell".into(),
            "--tty=false".into(),
            "--workdir".into(),
            "/".into(),
            record.name.clone(),
            "--".into(),
            "sudo".into(),
            "/bin/bash".into(),
            "-ceu".into(),
            GUEST_CREDENTIAL_PROBE_SCRIPT.into(),
            "jarvis-credential-probe".into(),
            record.user.clone(),
            expected_home.to_string_lossy().into_owned(),
        ],
        cwd: None,
        env: env.clone(),
        stdin: None,
    })
}

pub fn run_guest_credential_probe<R: CommandRunner>(
    runner: &R,
    spec: &CommandSpec,
) -> Result<BootstrapCredentialStatus, String> {
    let result = runner
        .run(spec)?
        .success_or_error("Agent VM guest credential probe")?;
    parse_guest_credential_probe(&result.stdout)
}

pub fn parse_guest_credential_probe(bytes: &[u8]) -> Result<BootstrapCredentialStatus, String> {
    if bytes.len() > 64 {
        return Err("Agent VM guest credential probe вернул лишние данные".into());
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "Agent VM guest credential probe вернул non-UTF-8".to_string())?;
    let mut lines = text.lines();
    let claude = parse_probe_line(lines.next(), "claude")?;
    let codex = parse_probe_line(lines.next(), "codex")?;
    if lines.next().is_some() {
        return Err("Agent VM guest credential probe вернул лишние поля".into());
    }
    Ok(BootstrapCredentialStatus { claude, codex })
}

pub fn run_bootstrap<R: CommandRunner>(runner: &R, spec: &CommandSpec) -> Result<(), String> {
    runner
        .run(spec)?
        .success_or_error("Agent VM guest bootstrap")?;
    Ok(())
}

fn validated_guest_home(limactl: &Path, record: &VmRecord) -> Result<PathBuf, String> {
    if !limactl.is_absolute() {
        return Err("limactl path должен быть absolute".into());
    }
    if !is_valid_vm_name(&record.name) || !valid_guest_user(&record.user) {
        return Err("VM record содержит unsafe guest identity".into());
    }
    if record.workspace.mode_name != "mount" {
        return Err("GuestBootstrap поддерживает только project mount".into());
    }
    if !is_safe_guest_workspace(&record.user, &record.workspace.guest_path) {
        return Err("VM record содержит unsafe guest workspace".into());
    }
    Path::new(&record.workspace.guest_path)
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "VM record не содержит guest home".to_string())
}

fn parse_probe_line(line: Option<&str>, name: &str) -> Result<String, String> {
    let value = line
        .and_then(|line| line.strip_prefix(name))
        .and_then(|line| line.strip_prefix('='))
        .filter(|value| matches!(*value, "ready" | "missing"))
        .ok_or_else(|| format!("Agent VM guest credential probe не вернул {name} status"))?;
    Ok(value.to_string())
}

fn validate_codex_auth(bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() as u64 > MAX_CODEX_AUTH_BYTES {
        return Err("Codex file credential имеет недопустимый размер".into());
    }
    let mut value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| "Codex file credential содержит некорректный JSON".to_string())?;
    let is_object = value.is_object();
    zeroize_json(&mut value);
    if !is_object {
        return Err("Codex file credential должен быть JSON object".into());
    }
    Ok(())
}

fn zeroize_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => text.zeroize(),
        serde_json::Value::Array(values) => {
            for value in values {
                zeroize_json(value);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                zeroize_json(value);
            }
        }
        _ => {}
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn validate_bundle_path(path: &Path) -> Result<(), String> {
    if path.is_absolute()
        || path.as_os_str().is_empty()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("bootstrap bundle содержит unsafe path".into());
    }
    let first = path
        .components()
        .next()
        .and_then(|part| match part {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .unwrap_or_default();
    if !matches!(first, ".claude" | ".codex" | ".jarvis-vm") {
        return Err("bootstrap bundle root не allowlisted".into());
    }
    Ok(())
}

fn deterministic_tar(files: &BTreeMap<PathBuf, (u32, Vec<u8>)>) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    {
        let mut builder = Builder::new(&mut output);
        let mut directories = BTreeMap::<PathBuf, ()>::new();
        for path in files.keys() {
            let mut parent = path.parent();
            while let Some(directory) = parent {
                if directory.as_os_str().is_empty() {
                    break;
                }
                directories.insert(directory.to_path_buf(), ());
                parent = directory.parent();
            }
        }
        for path in directories.keys() {
            let mut header = Header::new_gnu();
            header.set_entry_type(EntryType::Directory);
            header.set_mode(0o700);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            header.set_size(0);
            header.set_cksum();
            builder
                .append_data(&mut header, path, Cursor::new(Vec::<u8>::new()))
                .map_err(|_| "не собрать bootstrap directory archive".to_string())?;
        }
        for (path, (mode, bytes)) in files {
            validate_bundle_path(path)?;
            let mut header = Header::new_gnu();
            header.set_entry_type(EntryType::Regular);
            header.set_mode(*mode);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            header.set_size(bytes.len() as u64);
            header.set_cksum();
            builder
                .append_data(&mut header, path, Cursor::new(bytes))
                .map_err(|_| "не собрать bootstrap file archive".to_string())?;
        }
        builder
            .finish()
            .map_err(|_| "не завершить bootstrap archive".to_string())?;
    }
    Ok(output)
}

fn validate_archive(bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() as u64 > crate::config_mirror::MAX_MIRROR_TOTAL_BYTES * 2 {
        return Err("bootstrap archive имеет недопустимый размер".into());
    }
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries()
        .map_err(|_| "не прочитать bootstrap archive".to_string())?;
    for entry in entries {
        let entry = entry.map_err(|_| "не прочитать bootstrap archive entry".to_string())?;
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err("bootstrap archive содержит unsafe entry type".into());
        }
        let path = entry
            .path()
            .map_err(|_| "bootstrap archive содержит invalid path".to_string())?;
        validate_bundle_path(&path)?;
        let mode = entry
            .header()
            .mode()
            .map_err(|_| "bootstrap archive содержит invalid mode".to_string())?;
        if (kind.is_file() && !matches!(mode, 0o600 | 0o700)) || (kind.is_dir() && mode != 0o700) {
            return Err("bootstrap archive содержит unsafe mode".into());
        }
    }
    Ok(())
}

fn valid_guest_user(user: &str) -> bool {
    let mut bytes = user.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first == b'_')
        && user.len() <= 32
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Read;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    use jarvis_secret_store::{MemorySecretStore, SecretValue};

    use super::*;
    use crate::config_mirror::{ConfigSnapshot, MirrorDiagnostics, MirroredFile};
    use crate::inventory::{VmResources, VmWorkspace};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    fn snapshot() -> ConfigSnapshot {
        ConfigSnapshot {
            files: vec![
                MirroredFile {
                    guest_path: PathBuf::from(".claude/settings.json"),
                    bytes: br#"{"model":"synthetic"}"#.to_vec(),
                    mode: 0o600,
                },
                MirroredFile {
                    guest_path: PathBuf::from(".codex/config.toml"),
                    bytes: b"model = \"synthetic\"\n".to_vec(),
                    mode: 0o600,
                },
            ],
            fingerprint: "a".repeat(64),
            diagnostics: MirrorDiagnostics::default(),
        }
    }

    fn record() -> VmRecord {
        VmRecord {
            name: "synthetic-project-a1b2c3d4e5f6".into(),
            source: "project".into(),
            modules: vec!["claude".into(), "codex".into()],
            resources: VmResources::default(),
            user: "dev".into(),
            workspace: VmWorkspace {
                mode_name: "mount".into(),
                guest_path: "/home/dev/synthetic-project".into(),
                host_path: Some("/tmp/synthetic-project".into()),
                repo: None,
                git_ref: None,
            },
        }
    }

    fn archive_entries(bytes: &[u8]) -> Vec<(String, u32, Vec<u8>)> {
        let mut archive = tar::Archive::new(bytes);
        let mut out = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            let mode = entry.header().mode().unwrap();
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents).unwrap();
            out.push((path, mode, contents));
        }
        out
    }

    #[test]
    fn codex_auth_reader_accepts_only_owner_private_regular_json() {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-codex-auth-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let auth = root.join("auth.json");
        fs::write(&auth, br#"{"auth_mode":"synthetic"}"#).unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o600)).unwrap();

        assert_eq!(
            read_codex_file_credential(&root).unwrap(),
            Some(br#"{"auth_mode":"synthetic"}"#.to_vec())
        );

        fs::set_permissions(&auth, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_codex_file_credential(&root).is_err());
        fs::remove_file(&auth).unwrap();
        let outside = root.join("outside.json");
        fs::write(&outside, "{}").unwrap();
        symlink(&outside, &auth).unwrap();
        assert!(read_codex_file_credential(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_credential_transfer_follows_the_configured_storage_backend() {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-codex-store-{}-{id}",
            std::process::id()
        ));
        let roots = crate::config_mirror::MirrorRoots {
            claude: root.join(".claude"),
            codex: root.join(".codex"),
        };
        fs::create_dir_all(&roots.claude).unwrap();
        fs::create_dir_all(&roots.codex).unwrap();
        let auth = roots.codex.join("auth.json");
        fs::write(&auth, br#"{"auth_mode":"synthetic"}"#).unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o600)).unwrap();

        for mode in ["keyring", "auto"] {
            fs::write(
                roots.codex.join("config.toml"),
                format!("cli_auth_credentials_store = \"{mode}\"\n"),
            )
            .unwrap();
            let snapshot = crate::config_mirror::build_snapshot(&roots).unwrap();
            let credential = load_codex_credential(&snapshot, &roots.codex).unwrap();
            assert_eq!(credential.status(), "host-keyring");
            assert_eq!(credential.file_bytes(), None);
        }

        fs::write(
            roots.codex.join("config.toml"),
            "cli_auth_credentials_store = \"file\"\n",
        )
        .unwrap();
        let snapshot = crate::config_mirror::build_snapshot(&roots).unwrap();
        let credential = load_codex_credential(&snapshot, &roots.codex).unwrap();
        assert_eq!(credential.status(), "ready");
        assert_eq!(
            credential.file_bytes(),
            Some(br#"{"auth_mode":"synthetic"}"#.as_slice())
        );

        fs::write(roots.codex.join("config.toml"), "model = \"synthetic\"\n").unwrap();
        let snapshot = crate::config_mirror::build_snapshot(&roots).unwrap();
        let credential = load_codex_credential(&snapshot, &roots.codex).unwrap();
        assert_eq!(credential.status(), "ready");
        assert!(credential.file_bytes().is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bundle_contains_private_config_and_credentials_with_owner_only_modes() {
        let store = MemorySecretStore::default();
        let secret = SecretValue::new(b"SYNTHETIC_PRIVATE_VALUE".to_vec()).unwrap();
        store.set(SecretKind::ClaudeOauthToken, &secret).unwrap();

        let bundle = build_bundle(
            &snapshot(),
            &store,
            Some(SecretKind::ClaudeOauthToken),
            None,
            &LoadedCodexCredential::file(Some(br#"{"auth_mode":"synthetic"}"#.to_vec())),
            &BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(bundle.credential_status.claude, "ready");
        assert_eq!(bundle.credential_status.codex, "ready");
        let entries = archive_entries(&bundle.archive);
        assert_eq!(
            entries
                .iter()
                .map(|item| item.0.as_str())
                .collect::<Vec<_>>(),
            vec![
                ".claude/settings.json",
                ".codex/auth.json",
                ".codex/config.toml",
                ".jarvis-vm/agent.env",
            ]
        );
        assert!(entries.iter().all(|entry| entry.1 & 0o077 == 0));
        let env = entries
            .iter()
            .find(|entry| entry.0 == ".jarvis-vm/agent.env")
            .unwrap();
        assert!(String::from_utf8_lossy(&env.2).contains("CLAUDE_CODE_OAUTH_TOKEN"));
        assert!(String::from_utf8_lossy(&env.2).contains("SYNTHETIC_PRIVATE_VALUE"));
    }

    #[test]
    fn host_claude_login_copies_only_claude_oauth_as_owner_private_linux_credential() {
        let store = MemorySecretStore::default();
        let login = SecretValue::new(
            br#"{
                "claudeAiOauth":{
                    "accessToken":"SYNTHETIC_ACCESS",
                    "refreshToken":"SYNTHETIC_REFRESH",
                    "expiresAt":1785267208000,
                    "scopes":["user:inference"],
                    "subscriptionType":"max",
                    "rateLimitTier":"default"
                },
                "mcpOAuth":{"corporate":{"accessToken":"MUST_NOT_COPY"}}
            }"#
            .to_vec(),
        )
        .unwrap();

        let bundle = build_bundle(
            &snapshot(),
            &store,
            None,
            Some(&login),
            &LoadedCodexCredential::file(None),
            &BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(bundle.credential_status.claude, "ready");
        let entries = archive_entries(&bundle.archive);
        let credential = entries
            .iter()
            .find(|entry| entry.0 == ".claude/.credentials.json")
            .unwrap();
        assert_eq!(credential.1, 0o600);
        assert!(credential
            .2
            .windows(b"SYNTHETIC_ACCESS".len())
            .any(|part| part == b"SYNTHETIC_ACCESS"));
        assert!(credential
            .2
            .windows(b"SYNTHETIC_REFRESH".len())
            .any(|part| part == b"SYNTHETIC_REFRESH"));
        assert!(!bundle
            .archive
            .windows(b"MUST_NOT_COPY".len())
            .any(|part| part == b"MUST_NOT_COPY"));
        assert!(!String::from_utf8_lossy(&credential.2).contains("mcpOAuth"));
    }

    #[test]
    fn explicit_jarvis_claude_secret_wins_over_host_login() {
        let store = MemorySecretStore::default();
        let explicit = SecretValue::new(b"SYNTHETIC_EXPLICIT".to_vec()).unwrap();
        store.set(SecretKind::ClaudeOauthToken, &explicit).unwrap();
        let login = SecretValue::new(
            br#"{"claudeAiOauth":{"accessToken":"HOST_ACCESS","refreshToken":"HOST_REFRESH"}}"#
                .to_vec(),
        )
        .unwrap();

        let bundle = build_bundle(
            &snapshot(),
            &store,
            Some(SecretKind::ClaudeOauthToken),
            Some(&login),
            &LoadedCodexCredential::file(None),
            &BTreeMap::new(),
        )
        .unwrap();

        let entries = archive_entries(&bundle.archive);
        assert!(entries
            .iter()
            .any(|entry| entry.0 == ".jarvis-vm/agent.env"));
        assert!(!entries
            .iter()
            .any(|entry| entry.0 == ".claude/.credentials.json"));
        assert!(!bundle
            .archive
            .windows(b"HOST_ACCESS".len())
            .any(|part| part == b"HOST_ACCESS"));
    }

    #[test]
    fn host_claude_login_rejects_incomplete_oauth_credentials() {
        let store = MemorySecretStore::default();
        let login =
            SecretValue::new(br#"{"claudeAiOauth":{"accessToken":"SYNTHETIC_ACCESS"}}"#.to_vec())
                .unwrap();

        let error = build_bundle(
            &snapshot(),
            &store,
            None,
            Some(&login),
            &LoadedCodexCredential::file(None),
            &BTreeMap::new(),
        )
        .unwrap_err();

        assert!(error.contains("refreshToken"));
        assert!(!error.contains("SYNTHETIC_ACCESS"));
    }

    #[test]
    fn bootstrap_command_keeps_every_secret_byte_out_of_argv_and_env() {
        let store = MemorySecretStore::default();
        let secret = SecretValue::new(b"SYNTHETIC_PRIVATE_VALUE".to_vec()).unwrap();
        store.set(SecretKind::ClaudeApiKey, &secret).unwrap();
        let bundle = build_bundle(
            &snapshot(),
            &store,
            Some(SecretKind::ClaudeApiKey),
            None,
            &LoadedCodexCredential::file(Some(br#"{"private":"SYNTHETIC_CODEX_VALUE"}"#.to_vec())),
            &BTreeMap::new(),
        )
        .unwrap();

        let spec = bootstrap_spec(
            Path::new("/synthetic/bin/limactl"),
            &BTreeMap::from([("HOME".into(), "/private/runtime".into())]),
            &record(),
            bundle,
        )
        .unwrap();

        assert_eq!(spec.program, Path::new("/synthetic/bin/limactl"));
        let visible = format!("{:?}{:?}", spec.args, spec.env);
        assert!(!visible.contains("SYNTHETIC_PRIVATE_VALUE"));
        assert!(!visible.contains("SYNTHETIC_CODEX_VALUE"));
        let stdin = spec.stdin.as_ref().unwrap();
        assert!(stdin
            .windows(b"SYNTHETIC_PRIVATE_VALUE".len())
            .any(|part| part == b"SYNTHETIC_PRIVATE_VALUE"));
        assert_eq!(spec.cwd, None);
    }

    #[test]
    fn bootstrap_accepts_agent_vm_guest_mount_home() {
        let mut guest_mount = record();
        guest_mount.workspace.guest_path = "/home/dev.guest/synthetic-project-a1b2c3d4e5f6".into();
        let bundle = build_bundle(
            &snapshot(),
            &MemorySecretStore::default(),
            None,
            None,
            &LoadedCodexCredential::file(None),
            &BTreeMap::new(),
        )
        .unwrap();

        let spec = bootstrap_spec(
            Path::new("/synthetic/bin/limactl"),
            &BTreeMap::new(),
            &guest_mount,
            bundle,
        )
        .unwrap();

        assert!(spec.args.iter().any(|value| value == "/home/dev.guest"));
    }

    #[test]
    fn credential_probe_exposes_only_bounded_readiness_status() {
        let mut guest_mount = record();
        guest_mount.workspace.guest_path = "/home/dev.guest/synthetic-project-a1b2c3d4e5f6".into();

        let spec = guest_credential_probe_spec(
            Path::new("/synthetic/bin/limactl"),
            &BTreeMap::from([("HOME".into(), "/private/runtime".into())]),
            &guest_mount,
        )
        .unwrap();

        assert_eq!(spec.program, Path::new("/synthetic/bin/limactl"));
        assert!(spec.args.iter().any(|value| value == "/home/dev.guest"));
        assert!(spec.stdin.is_none());
        let visible = format!("{spec:?}");
        assert!(!visible.contains("SYNTHETIC_PRIVATE_VALUE"));

        assert_eq!(
            parse_guest_credential_probe(b"claude=ready\ncodex=missing\n").unwrap(),
            BootstrapCredentialStatus {
                claude: "ready".into(),
                codex: "missing".into(),
            }
        );
        for invalid in [
            b"claude=ready\n".as_slice(),
            b"claude=ready\ncodex=unknown\n".as_slice(),
            b"claude=ready\ncodex=ready\nextra=value\n".as_slice(),
        ] {
            assert!(parse_guest_credential_probe(invalid).is_err());
        }
    }

    #[test]
    fn invalid_guest_identity_is_rejected_before_any_command_can_run() {
        let empty = BootstrapBundle {
            archive: Vec::new(),
            credential_status: BootstrapCredentialStatus {
                claude: "missing".into(),
                codex: "missing".into(),
            },
        };
        let mut unsafe_record = record();
        unsafe_record.user = "dev;unsafe".into();

        assert!(bootstrap_spec(
            Path::new("/synthetic/bin/limactl"),
            &BTreeMap::new(),
            &unsafe_record,
            empty
        )
        .is_err());
    }
}
