use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};
use zeroize::Zeroize;

pub const MAX_MIRROR_FILES: usize = 2_048;
pub const MAX_MIRROR_FILE_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_MIRROR_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MirrorRoots {
    pub claude: PathBuf,
    pub codex: PathBuf,
}

#[derive(Clone, PartialEq, Eq)]
pub struct MirroredFile {
    pub guest_path: PathBuf,
    pub bytes: Vec<u8>,
    pub mode: u32,
}

impl std::fmt::Debug for MirroredFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MirroredFile")
            .field("path_redacted", &true)
            .field("bytes", &self.bytes.len())
            .field("mode", &format_args!("{:o}", self.mode))
            .finish()
    }
}

impl Drop for MirroredFile {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MirrorDiagnostics {
    pub skipped_symlinks: usize,
    pub skipped_non_regular: usize,
    pub skipped_oversize: usize,
    pub removed_host_commands: usize,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ConfigSnapshot {
    pub files: Vec<MirroredFile>,
    pub fingerprint: String,
    pub diagnostics: MirrorDiagnostics,
}

impl std::fmt::Debug for ConfigSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfigSnapshot")
            .field("files", &self.files.len())
            .field("fingerprint", &self.fingerprint)
            .field("diagnostics", &self.diagnostics)
            .finish()
    }
}

pub fn build_snapshot(roots: &MirrorRoots) -> Result<ConfigSnapshot, String> {
    build_snapshot_inner(roots, None)
}

pub fn build_snapshot_for_project(
    roots: &MirrorRoots,
    host_project: &Path,
    guest_workspace: &str,
) -> Result<ConfigSnapshot, String> {
    let host_key = claude_project_key(host_project)?;
    let guest_workspace = Path::new(guest_workspace);
    let guest_key = claude_project_key(guest_workspace)?;
    let source = roots.claude.join("projects").join(host_key).join("memory");
    let guest = PathBuf::from(".claude")
        .join("projects")
        .join(guest_key)
        .join("memory");
    build_snapshot_inner(roots, Some((&source, &guest)))
}

fn build_snapshot_inner(
    roots: &MirrorRoots,
    project_memory: Option<(&Path, &Path)>,
) -> Result<ConfigSnapshot, String> {
    let mut snapshot = ConfigSnapshot {
        files: Vec::new(),
        fingerprint: String::new(),
        diagnostics: MirrorDiagnostics::default(),
    };
    let mut total_bytes = 0_u64;
    for (source, guest) in [
        (roots.claude.join("settings.json"), ".claude/settings.json"),
        (roots.claude.join("CLAUDE.md"), ".claude/CLAUDE.md"),
        (roots.codex.join("config.toml"), ".codex/config.toml"),
        (roots.codex.join("AGENTS.md"), ".codex/AGENTS.md"),
    ] {
        collect_single(&source, Path::new(guest), &mut snapshot, &mut total_bytes)?;
    }
    for (source, guest) in [
        (roots.claude.join("agents"), ".claude/agents"),
        (roots.claude.join("commands"), ".claude/commands"),
        (roots.claude.join("skills"), ".claude/skills"),
        (roots.codex.join("skills"), ".codex/skills"),
    ] {
        collect_tree(
            &source,
            &source,
            Path::new(guest),
            &mut snapshot,
            &mut total_bytes,
        )?;
    }
    if let Some((source, guest)) = project_memory {
        collect_tree(source, source, guest, &mut snapshot, &mut total_bytes)?;
    }
    snapshot
        .files
        .sort_by(|left, right| left.guest_path.cmp(&right.guest_path));
    snapshot.fingerprint = fingerprint(&snapshot.files);
    Ok(snapshot)
}

fn claude_project_key(path: &Path) -> Result<String, String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("Claude project memory требует absolute normalized path".into());
    }
    let value = path
        .to_str()
        .ok_or_else(|| "Claude project memory path должен быть UTF-8".to_string())?;
    if value.is_empty() || value.len() > 16 * 1024 {
        return Err("Claude project memory path имеет unsafe размер".into());
    }
    Ok(value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect())
}

fn collect_single(
    source: &Path,
    guest_path: &Path,
    snapshot: &mut ConfigSnapshot,
    total_bytes: &mut u64,
) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(source) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("не проверить allowlisted config file".into()),
    };
    if metadata.file_type().is_symlink() {
        snapshot.diagnostics.skipped_symlinks += 1;
        return Ok(());
    }
    if !metadata.file_type().is_file() {
        snapshot.diagnostics.skipped_non_regular += 1;
        return Ok(());
    }
    add_regular_file(
        source,
        guest_path.to_path_buf(),
        metadata,
        snapshot,
        total_bytes,
    )
}

fn collect_tree(
    allowlist_root: &Path,
    current: &Path,
    guest_root: &Path,
    snapshot: &mut ConfigSnapshot,
    total_bytes: &mut u64,
) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(current) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("не проверить allowlisted config tree".into()),
    };
    if metadata.file_type().is_symlink() {
        snapshot.diagnostics.skipped_symlinks += 1;
        return Ok(());
    }
    if !metadata.file_type().is_dir() {
        snapshot.diagnostics.skipped_non_regular += 1;
        return Ok(());
    }
    let mut entries = fs::read_dir(current)
        .map_err(|_| "не прочитать allowlisted config tree".to_string())?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let source = entry.path();
        let metadata = match fs::symlink_metadata(&source) {
            Ok(metadata) => metadata,
            Err(_) => {
                snapshot.diagnostics.skipped_non_regular += 1;
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            snapshot.diagnostics.skipped_symlinks += 1;
            continue;
        }
        if is_denied_name(&entry.file_name()) {
            snapshot.diagnostics.skipped_non_regular += 1;
            continue;
        }
        if metadata.file_type().is_dir() {
            collect_tree(allowlist_root, &source, guest_root, snapshot, total_bytes)?;
        } else if metadata.file_type().is_file() {
            let relative = source
                .strip_prefix(allowlist_root)
                .map_err(|_| "allowlisted config path escaped its root".to_string())?;
            let guest_path = guest_root.join(relative);
            add_regular_file(&source, guest_path, metadata, snapshot, total_bytes)?;
        } else {
            snapshot.diagnostics.skipped_non_regular += 1;
        }
    }
    Ok(())
}

fn add_regular_file(
    source: &Path,
    guest_path: PathBuf,
    metadata: fs::Metadata,
    snapshot: &mut ConfigSnapshot,
    total_bytes: &mut u64,
) -> Result<(), String> {
    validate_guest_path(&guest_path)?;
    if metadata.len() > MAX_MIRROR_FILE_BYTES {
        snapshot.diagnostics.skipped_oversize += 1;
        return Ok(());
    }
    if snapshot.files.len() >= MAX_MIRROR_FILES {
        return Err("config mirror превышает file-count limit".into());
    }
    let canonical_parent = source
        .parent()
        .and_then(|parent| fs::canonicalize(parent).ok())
        .ok_or_else(|| "не проверить allowlisted config parent".to_string())?;
    let canonical_source = fs::canonicalize(source)
        .map_err(|_| "не canonicalize allowlisted config file".to_string())?;
    if canonical_source.parent() != Some(canonical_parent.as_path()) {
        return Err("allowlisted config file escaped its parent".into());
    }
    let mut file = no_follow_file(source)?;
    let opened = file
        .metadata()
        .map_err(|_| "не проверить opened config file".to_string())?;
    if !opened.is_file() || opened.len() > MAX_MIRROR_FILE_BYTES {
        snapshot.diagnostics.skipped_oversize += 1;
        return Ok(());
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.by_ref()
        .take(MAX_MIRROR_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "не прочитать allowlisted config file".to_string())?;
    if bytes.len() as u64 > MAX_MIRROR_FILE_BYTES {
        snapshot.diagnostics.skipped_oversize += 1;
        return Ok(());
    }
    let bytes = sanitize_mirrored_bytes(&guest_path, bytes, &mut snapshot.diagnostics)?;
    let next_total = total_bytes
        .checked_add(bytes.len() as u64)
        .ok_or_else(|| "config mirror size overflow".to_string())?;
    if next_total > MAX_MIRROR_TOTAL_BYTES {
        return Err("config mirror превышает total-size limit".into());
    }
    *total_bytes = next_total;
    let mode = if opened.permissions().mode() & 0o100 != 0 {
        0o700
    } else {
        0o600
    };
    snapshot.files.push(MirroredFile {
        guest_path,
        bytes,
        mode,
    });
    Ok(())
}

fn sanitize_mirrored_bytes(
    guest_path: &Path,
    bytes: Vec<u8>,
    diagnostics: &mut MirrorDiagnostics,
) -> Result<Vec<u8>, String> {
    if guest_path != Path::new(".claude/settings.json") {
        return Ok(bytes);
    }
    let mut settings = serde_json::from_slice::<serde_json::Value>(&bytes)
        .map_err(|_| "Claude settings содержат invalid JSON".to_string())?;
    let object = settings
        .as_object_mut()
        .ok_or_else(|| "Claude settings должны быть JSON object".to_string())?;
    let removed = ["hooks", "statusLine"]
        .into_iter()
        .filter(|key| object.remove(*key).is_some())
        .count();
    if removed == 0 {
        return Ok(bytes);
    }
    diagnostics.removed_host_commands += removed;
    let mut sanitized = serde_json::to_vec(&settings)
        .map_err(|_| "не подготовить guest-safe Claude settings".to_string())?;
    sanitized.push(b'\n');
    if sanitized.len() as u64 > MAX_MIRROR_FILE_BYTES {
        return Err("guest-safe Claude settings превышают size limit".into());
    }
    Ok(sanitized)
}

fn no_follow_file(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| "не открыть allowlisted config file".to_string())
}

fn validate_guest_path(path: &Path) -> Result<(), String> {
    if path.is_absolute()
        || path.as_os_str().is_empty()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("config mirror produced unsafe guest path".into());
    }
    let mut parts = path.components();
    let root = parts
        .next()
        .and_then(|part| match part {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .unwrap_or_default();
    if !matches!(root, ".claude" | ".codex") {
        return Err("config mirror guest root is not allowlisted".into());
    }
    if path
        .as_os_str()
        .as_bytes()
        .iter()
        .any(|byte| *byte == 0 || *byte == b'\n' || *byte == b'\r')
    {
        return Err("config mirror path contains control bytes".into());
    }
    Ok(())
}

fn is_denied_name(name: &std::ffi::OsStr) -> bool {
    let value = name.to_string_lossy().to_ascii_lowercase();
    value == ".env"
        || value.starts_with(".env.")
        || matches!(
            value.as_str(),
            ".git"
                | ".credentials.json"
                | ".secrets"
                | "auth.json"
                | "cache"
                | "credentials"
                | "credentials.json"
                | "debug"
                | "file-history"
                | "history"
                | "logs"
                | "node_modules"
                | "secrets"
                | "sessions"
                | "tokens"
                | "tokens.json"
        )
        || value.ends_with(".key")
        || value.ends_with(".pem")
        || value.ends_with(".p12")
        || value.ends_with(".pfx")
}

fn fingerprint(files: &[MirroredFile]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"jarvis-config-mirror-v2\0");
    for file in files {
        hash.update(file.guest_path.as_os_str().as_bytes());
        hash.update([0]);
        hash.update(file.mode.to_be_bytes());
        hash.update((file.bytes.len() as u64).to_be_bytes());
        hash.update(&file.bytes);
    }
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    fn fixture(tag: &str) -> (PathBuf, MirrorRoots) {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-config-{tag}-{}-{id}",
            std::process::id()
        ));
        let claude = root.join("host/.claude");
        let codex = root.join("host/.codex");
        fs::create_dir_all(claude.join("skills/reviewer")).unwrap();
        fs::create_dir_all(codex.join("skills/planner")).unwrap();
        (root, MirrorRoots { claude, codex })
    }

    fn guest_paths(snapshot: &ConfigSnapshot) -> Vec<&Path> {
        snapshot
            .files
            .iter()
            .map(|file| file.guest_path.as_path())
            .collect()
    }

    #[test]
    fn snapshot_is_deterministic_allowlisted_and_never_includes_auth_or_history() {
        let (root, roots) = fixture("allowlist");
        fs::write(
            roots.claude.join("settings.json"),
            br#"{"model":"synthetic"}"#,
        )
        .unwrap();
        fs::write(
            roots.claude.join("skills/reviewer/SKILL.md"),
            "synthetic skill",
        )
        .unwrap();
        fs::write(
            roots.claude.join("skills/reviewer/client.pem"),
            "must stay host-only",
        )
        .unwrap();
        fs::write(
            roots.claude.join(".credentials.json"),
            "must stay host-only",
        )
        .unwrap();
        fs::create_dir_all(roots.claude.join("history")).unwrap();
        fs::write(roots.claude.join("history/session.jsonl"), "host-only").unwrap();
        fs::write(roots.codex.join("config.toml"), "model = \"synthetic\"\n").unwrap();
        fs::write(roots.codex.join("AGENTS.md"), "synthetic instructions").unwrap();
        fs::write(
            roots.codex.join("skills/planner/SKILL.md"),
            "synthetic planner",
        )
        .unwrap();
        fs::write(roots.codex.join("auth.json"), "must stay credential-only").unwrap();
        fs::create_dir_all(roots.codex.join("sessions")).unwrap();
        fs::write(roots.codex.join("sessions/rollout.jsonl"), "host-only").unwrap();

        let first = build_snapshot(&roots).unwrap();
        let second = build_snapshot(&roots).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.fingerprint.len(), 64);
        assert_eq!(
            guest_paths(&first),
            vec![
                Path::new(".claude/settings.json"),
                Path::new(".claude/skills/reviewer/SKILL.md"),
                Path::new(".codex/AGENTS.md"),
                Path::new(".codex/config.toml"),
                Path::new(".codex/skills/planner/SKILL.md"),
            ]
        );
        let all_bytes = first
            .files
            .iter()
            .flat_map(|file| file.bytes.iter().copied())
            .collect::<Vec<_>>();
        assert!(!String::from_utf8_lossy(&all_bytes).contains("credential-only"));
        assert!(!String::from_utf8_lossy(&all_bytes).contains("host-only"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_memory_is_rekeyed_for_guest_cwd_without_copying_session_history() {
        let (root, roots) = fixture("project-memory");
        let host_project = root.join("host/work/sup");
        fs::create_dir_all(&host_project).unwrap();
        let host_key = claude_project_key(&host_project).unwrap();
        let host_state = roots.claude.join("projects").join(host_key);
        fs::create_dir_all(host_state.join("memory")).unwrap();
        fs::write(
            host_state.join("memory/MEMORY.md"),
            "SYNTHETIC_PROJECT_MEMORY",
        )
        .unwrap();
        fs::write(
            host_state.join("session.jsonl"),
            "SYNTHETIC_SESSION_HISTORY_MUST_STAY_HOST_ONLY",
        )
        .unwrap();

        let snapshot =
            build_snapshot_for_project(&roots, &host_project, "/home/dev.guest/sup-a1b2c3d4e5f6")
                .unwrap();

        assert_eq!(
            claude_project_key(Path::new("/home/dev.guest/sup-a1b2c3d4e5f6")).unwrap(),
            "-home-dev-guest-sup-a1b2c3d4e5f6"
        );
        assert!(guest_paths(&snapshot).contains(&Path::new(
            ".claude/projects/-home-dev-guest-sup-a1b2c3d4e5f6/memory/MEMORY.md"
        )));
        let all_bytes = snapshot
            .files
            .iter()
            .flat_map(|file| file.bytes.iter().copied())
            .collect::<Vec<_>>();
        assert!(String::from_utf8_lossy(&all_bytes).contains("SYNTHETIC_PROJECT_MEMORY"));
        assert!(!String::from_utf8_lossy(&all_bytes).contains("SESSION_HISTORY"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn claude_settings_drop_host_commands_but_keep_portable_preferences() {
        let (root, roots) = fixture("claude-host-commands");
        fs::write(
            roots.claude.join("settings.json"),
            br#"{
                "model":"synthetic",
                "permissions":{"defaultMode":"bypassPermissions"},
                "hooks":{"Stop":[{"hooks":[{"type":"command","command":"/host/jarvis-hook"}]}]},
                "statusLine":{"type":"command","command":"uv run /host/status.py"},
                "futurePortableSetting":true
            }"#,
        )
        .unwrap();

        let snapshot = build_snapshot(&roots).unwrap();
        let settings = snapshot
            .files
            .iter()
            .find(|file| file.guest_path == Path::new(".claude/settings.json"))
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&settings.bytes).unwrap();

        assert_eq!(value["model"], "synthetic");
        assert_eq!(value["permissions"]["defaultMode"], "bypassPermissions");
        assert_eq!(value["futurePortableSetting"], true);
        assert!(value.get("hooks").is_none());
        assert!(value.get("statusLine").is_none());
        assert_eq!(snapshot.diagnostics.removed_host_commands, 2);
        assert!(!String::from_utf8_lossy(&settings.bytes).contains("/host/"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn symlinks_are_counted_but_never_followed_even_inside_an_allowlisted_tree() {
        let (root, roots) = fixture("symlink");
        let outside = root.join("outside.txt");
        fs::write(&outside, "SYNTHETIC_PRIVATE_VALUE").unwrap();
        symlink(&outside, roots.claude.join("skills/reviewer/linked.md")).unwrap();

        let snapshot = build_snapshot(&roots).unwrap();

        assert_eq!(snapshot.diagnostics.skipped_symlinks, 1);
        assert!(snapshot.files.is_empty());
        assert!(!format!("{snapshot:?}").contains("SYNTHETIC_PRIVATE_VALUE"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn executable_owner_bit_is_preserved_without_group_or_world_permissions() {
        let (root, roots) = fixture("mode");
        let script = roots.codex.join("skills/planner/run.sh");
        fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let snapshot = build_snapshot(&roots).unwrap();

        assert_eq!(snapshot.files.len(), 1);
        assert_eq!(snapshot.files[0].mode, 0o700);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn oversized_file_is_skipped_without_placing_its_name_in_diagnostics() {
        let (root, roots) = fixture("oversize");
        let path = roots.claude.join("skills/reviewer/large.bin");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_MIRROR_FILE_BYTES + 1).unwrap();

        let snapshot = build_snapshot(&roots).unwrap();

        assert_eq!(snapshot.diagnostics.skipped_oversize, 1);
        assert!(snapshot.files.is_empty());
        assert!(!format!("{:?}", snapshot.diagnostics).contains("large.bin"));
        fs::remove_dir_all(root).unwrap();
    }
}
