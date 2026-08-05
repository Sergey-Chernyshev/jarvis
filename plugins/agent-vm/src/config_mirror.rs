use std::ffi::{CStr, CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
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
    /// Записи allowlist, которых нет на хосте. Без этого счётчика пустой
    /// snapshot неотличим от «у пользователя действительно нет памяти»:
    /// ошибка в выводе пути выглядела бы как штатная тишина.
    pub missing_sources: Vec<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ConfigSnapshot {
    pub files: Vec<MirroredFile>,
    pub fingerprint: String,
    pub diagnostics: MirrorDiagnostics,
}

impl ConfigSnapshot {
    /// Пишем guest-путь, а не host: диагностика уходит в journal/UI, а
    /// host-путь несёт имя пользователя и раскладку домашнего каталога.
    fn note_missing(&mut self, guest_path: &Path) {
        let value = guest_path.to_string_lossy().to_string();
        if !value.is_empty() && !self.diagnostics.missing_sources.contains(&value) {
            self.diagnostics.missing_sources.push(value);
        }
    }
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
        (
            roots.codex.join("memories/MEMORY.md"),
            ".codex/memories/MEMORY.md",
        ),
        (
            roots.codex.join("memories/memory_summary.md"),
            ".codex/memories/memory_summary.md",
        ),
        // Декларации плагинов Claude, а не их тела: cache/ — сотни мегабайт с
        // host-абсолютными installPath, гость всё равно ставит плагины сам.
        (
            roots.claude.join("plugins/installed_plugins.json"),
            ".claude/plugins/installed_plugins.json",
        ),
        (
            roots.claude.join("plugins/known_marketplaces.json"),
            ".claude/plugins/known_marketplaces.json",
        ),
    ] {
        collect_single(&source, Path::new(guest), &mut snapshot, &mut total_bytes)?;
    }
    for (source, guest) in [
        (roots.claude.join("agents"), ".claude/agents"),
        (roots.claude.join("commands"), ".claude/commands"),
        (roots.claude.join("skills"), ".claude/skills"),
        (roots.codex.join("skills"), ".codex/skills"),
        (
            roots.codex.join("memories/skills"),
            ".codex/memories/skills",
        ),
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
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            snapshot.note_missing(guest_path);
            return Ok(());
        }
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
    let file = no_follow_file(source)?;
    add_open_regular_file(file, guest_path.to_path_buf(), snapshot, total_bytes)
}

fn collect_tree(
    _allowlist_root: &Path,
    current: &Path,
    guest_root: &Path,
    snapshot: &mut ConfigSnapshot,
    total_bytes: &mut u64,
) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(current) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            snapshot.note_missing(guest_root);
            return Ok(());
        }
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
    let directory = open_directory_no_follow(current)?;
    collect_open_tree(&directory, Path::new(""), guest_root, snapshot, total_bytes)
}

fn collect_open_tree(
    directory: &File,
    relative: &Path,
    guest_root: &Path,
    snapshot: &mut ConfigSnapshot,
    total_bytes: &mut u64,
) -> Result<(), String> {
    for name in descriptor_entry_names(directory)? {
        if is_denied_name(&name) {
            snapshot.diagnostics.skipped_non_regular += 1;
            continue;
        }
        let child = match openat_no_follow(directory, &name) {
            Ok(child) => child,
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                snapshot.diagnostics.skipped_symlinks += 1;
                continue;
            }
            Err(_) => return Err("allowlisted config tree changed during traversal".into()),
        };
        let metadata = child
            .metadata()
            .map_err(|_| "не проверить opened config entry".to_string())?;
        let child_relative = relative.join(&name);
        if metadata.is_dir() {
            collect_open_tree(&child, &child_relative, guest_root, snapshot, total_bytes)?;
        } else if metadata.is_file() {
            add_open_regular_file(
                child,
                guest_root.join(child_relative),
                snapshot,
                total_bytes,
            )?;
        } else {
            snapshot.diagnostics.skipped_non_regular += 1;
        }
    }
    Ok(())
}

fn add_open_regular_file(
    mut file: File,
    guest_path: PathBuf,
    snapshot: &mut ConfigSnapshot,
    total_bytes: &mut u64,
) -> Result<(), String> {
    validate_guest_path(&guest_path)?;
    if snapshot.files.len() >= MAX_MIRROR_FILES {
        return Err("config mirror превышает file-count limit".into());
    }
    let opened = file
        .metadata()
        .map_err(|_| "не проверить opened config file".to_string())?;
    if !opened.is_file() {
        snapshot.diagnostics.skipped_non_regular += 1;
        return Ok(());
    }
    if opened.len() > MAX_MIRROR_FILE_BYTES {
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
    match guest_path {
        path if path == Path::new(".claude/settings.json") => {
            sanitize_claude_settings(bytes, diagnostics)
        }
        path if path == Path::new(".codex/config.toml") => {
            sanitize_codex_config(bytes, diagnostics)
        }
        path if path == Path::new(".claude/plugins/installed_plugins.json") => {
            sanitize_installed_plugins(bytes, diagnostics)
        }
        path if path == Path::new(".claude/plugins/known_marketplaces.json") => {
            sanitize_known_marketplaces(bytes, diagnostics)
        }
        _ => Ok(bytes),
    }
}

fn sanitize_claude_settings(
    bytes: Vec<u8>,
    diagnostics: &mut MirrorDiagnostics,
) -> Result<Vec<u8>, String> {
    let mut settings = serde_json::from_slice::<serde_json::Value>(&bytes)
        .map_err(|_| "Claude settings содержат invalid JSON".to_string())?;
    let object = settings
        .as_object_mut()
        .ok_or_else(|| "Claude settings должны быть JSON object".to_string())?;

    let mut portable = serde_json::Map::new();
    for key in [
        "model",
        "language",
        "outputStyle",
        "alwaysThinkingEnabled",
        "cleanupPeriodDays",
        "includeCoAuthoredBy",
        "respectGitignore",
        "showTurnDuration",
        "spinnerTipsEnabled",
        "preferredNotifChannel",
    ] {
        if let Some(value) = object.get(key).filter(|value| portable_scalar(value)) {
            portable.insert(key.into(), value.clone());
        }
    }
    if let Some(default_mode) = object
        .get("permissions")
        .and_then(serde_json::Value::as_object)
        .and_then(|permissions| permissions.get("defaultMode"))
        .and_then(serde_json::Value::as_str)
        .filter(|mode| {
            matches!(
                *mode,
                "default" | "acceptEdits" | "plan" | "bypassPermissions"
            )
        })
    {
        portable.insert(
            "permissions".into(),
            serde_json::json!({"defaultMode":default_mode}),
        );
    }

    diagnostics.removed_host_commands += object.len().saturating_sub(portable.len());
    let mut sanitized = serde_json::to_vec(&portable)
        .map_err(|_| "не подготовить guest-safe Claude settings".to_string())?;
    sanitized.push(b'\n');
    if sanitized.len() as u64 > MAX_MIRROR_FILE_BYTES {
        return Err("guest-safe Claude settings превышают size limit".into());
    }
    Ok(sanitized)
}

/// installed_plugins.json несёт host-абсолютный `installPath` у каждой записи.
/// Гость по нему не пойдёт: путь ведёт в несуществующий host-каталог. Оставляем
/// только идентичность плагина, чтобы гость доставил его сам.
fn sanitize_installed_plugins(
    bytes: Vec<u8>,
    diagnostics: &mut MirrorDiagnostics,
) -> Result<Vec<u8>, String> {
    let mut value = serde_json::from_slice::<serde_json::Value>(&bytes)
        .map_err(|_| "installed_plugins.json содержит invalid JSON".to_string())?;
    let root = value
        .as_object_mut()
        .ok_or_else(|| "installed_plugins.json должен быть JSON object".to_string())?;
    if let Some(plugins) = root.get_mut("plugins").and_then(|it| it.as_object_mut()) {
        for (_, entries) in plugins.iter_mut() {
            if let Some(list) = entries.as_array_mut() {
                for entry in list.iter_mut() {
                    if let Some(entry) = entry.as_object_mut() {
                        if entry.remove("installPath").is_some() {
                            diagnostics.removed_host_commands += 1;
                        }
                    }
                }
            }
        }
    }
    serde_json::to_vec_pretty(&value)
        .map_err(|_| "не сериализовать installed_plugins.json".to_string())
}

/// То же для known_marketplaces.json: `installLocation` — host-путь, а
/// `source` (github repo) переносим: по нему гость восстановит marketplace.
fn sanitize_known_marketplaces(
    bytes: Vec<u8>,
    diagnostics: &mut MirrorDiagnostics,
) -> Result<Vec<u8>, String> {
    let mut value = serde_json::from_slice::<serde_json::Value>(&bytes)
        .map_err(|_| "known_marketplaces.json содержит invalid JSON".to_string())?;
    let root = value
        .as_object_mut()
        .ok_or_else(|| "known_marketplaces.json должен быть JSON object".to_string())?;
    for (_, entry) in root.iter_mut() {
        if let Some(entry) = entry.as_object_mut() {
            if entry.remove("installLocation").is_some() {
                diagnostics.removed_host_commands += 1;
            }
        }
    }
    serde_json::to_vec_pretty(&value)
        .map_err(|_| "не сериализовать known_marketplaces.json".to_string())
}

fn sanitize_codex_config(
    bytes: Vec<u8>,
    diagnostics: &mut MirrorDiagnostics,
) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| "Codex config.toml должен быть UTF-8".to_string())?;
    let config = toml::from_str::<toml::Value>(text)
        .map_err(|_| "Codex config.toml содержит invalid TOML".to_string())?;
    let object = config
        .as_table()
        .ok_or_else(|| "Codex config.toml должен быть TOML table".to_string())?;
    let mut portable = toml::map::Map::new();
    for key in [
        "model",
        "model_reasoning_effort",
        "plan_mode_reasoning_effort",
        "approval_policy",
        "sandbox_mode",
        "web_search",
        "personality",
        "cli_auth_credentials_store",
        "hide_agent_reasoning",
        "show_raw_agent_reasoning",
    ] {
        if let Some(value) = object.get(key).filter(|value| portable_toml_scalar(value)) {
            portable.insert(key.into(), value.clone());
        }
    }
    if let Some(features) = object.get("features").and_then(toml::Value::as_table) {
        let features = features
            .iter()
            .filter(|(_, value)| value.is_bool())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<toml::map::Map<_, _>>();
        if !features.is_empty() {
            portable.insert("features".into(), toml::Value::Table(features));
        }
    }
    diagnostics.removed_host_commands += object.len().saturating_sub(portable.len());
    let mut sanitized = toml::to_string(&toml::Value::Table(portable))
        .map_err(|_| "не подготовить guest-safe Codex config".to_string())?
        .into_bytes();
    if !sanitized.ends_with(b"\n") {
        sanitized.push(b'\n');
    }
    if sanitized.len() as u64 > MAX_MIRROR_FILE_BYTES {
        return Err("guest-safe Codex config превышает size limit".into());
    }
    Ok(sanitized)
}

fn portable_scalar(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => true,
        serde_json::Value::String(value) => {
            value.len() <= 1024 && !value.bytes().any(|byte| matches!(byte, 0 | b'\n' | b'\r'))
        }
        _ => false,
    }
}

fn portable_toml_scalar(value: &toml::Value) -> bool {
    match value {
        toml::Value::Boolean(_) | toml::Value::Integer(_) => true,
        toml::Value::String(value) => {
            value.len() <= 1024 && !value.bytes().any(|byte| matches!(byte, 0 | b'\n' | b'\r'))
        }
        _ => false,
    }
}

fn no_follow_file(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "не открыть allowlisted config file".to_string())
}

fn open_directory_no_follow(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "не открыть allowlisted config directory".to_string())
}

fn descriptor_entry_names(directory: &File) -> Result<Vec<std::ffi::OsString>, String> {
    let duplicate = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err("не продублировать allowlisted config directory".into());
    }
    // SAFETY: fdopendir takes ownership of the fresh duplicate. closedir below
    // releases it on every successful fdopendir path.
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        unsafe {
            libc::close(duplicate);
        }
        return Err("не открыть allowlisted config directory stream".into());
    }
    let mut names = Vec::new();
    loop {
        // SAFETY: stream remains owned and valid until closedir after the loop;
        // each returned dirent is copied before the next readdir call.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        names.push(std::ffi::OsString::from_vec(bytes.to_vec()));
    }
    let close_result = unsafe { libc::closedir(stream) };
    if close_result != 0 {
        return Err("не закрыть allowlisted config directory stream".into());
    }
    names.sort();
    Ok(names)
}

fn openat_no_follow(directory: &File, name: &OsStr) -> Result<File, std::io::Error> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: successful openat returns a new descriptor now owned by File.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
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
    fn plugin_declarations_travel_without_host_install_paths() {
        let (root, roots) = fixture("plugins");
        fs::create_dir_all(roots.claude.join("plugins/cache/official/tool")).unwrap();
        fs::write(
            roots.claude.join("plugins/installed_plugins.json"),
            br#"{"version":2,"plugins":{"tool@official":[{"scope":"user",
                "installPath":"/Users/synthetic/.claude/plugins/cache/official/tool",
                "version":"unknown"}]}}"#,
        )
        .unwrap();
        fs::write(
            roots.claude.join("plugins/known_marketplaces.json"),
            br#"{"official":{"source":{"source":"github","repo":"anthropics/x"},
                "installLocation":"/Users/synthetic/.claude/plugins/marketplaces/official"}}"#,
        )
        .unwrap();
        // тело плагина живёт в cache/: сотни мегабайт и host-пути внутри
        fs::write(
            roots
                .claude
                .join("plugins/cache/official/tool/SKILL.md"),
            "plugin body must stay host-only",
        )
        .unwrap();

        let snapshot = build_snapshot(&roots).unwrap();
        let paths = guest_paths(&snapshot);
        assert!(paths.contains(&Path::new(".claude/plugins/installed_plugins.json")));
        assert!(paths.contains(&Path::new(".claude/plugins/known_marketplaces.json")));
        assert!(
            !paths
                .iter()
                .any(|path| path.starts_with(".claude/plugins/cache")),
            "тела плагинов не переносятся: {paths:?}"
        );

        let all = snapshot
            .files
            .iter()
            .flat_map(|file| file.bytes.iter().copied())
            .collect::<Vec<_>>();
        let text = String::from_utf8_lossy(&all);
        assert!(!text.contains("host-only"));
        assert!(!text.contains("installPath"));
        assert!(!text.contains("installLocation"));
        // идентичность плагина и источник marketplace обязаны доехать
        assert!(text.contains("tool@official"));
        assert!(text.contains("anthropics/x"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn absent_allowlist_entries_are_reported_instead_of_silently_empty() {
        let (root, roots) = fixture("missing");
        // ничего не создаём поверх fixture: skills-каталоги есть, файлов нет
        let snapshot = build_snapshot(&roots).unwrap();

        assert!(snapshot.files.is_empty());
        let missing = &snapshot.diagnostics.missing_sources;
        assert!(
            missing.contains(&".claude/CLAUDE.md".to_string()),
            "отсутствие глобальной памяти должно быть видно: {missing:?}"
        );
        assert!(missing.contains(&".claude/plugins/installed_plugins.json".to_string()));
        // host-пути в диагностику не попадают
        assert!(missing.iter().all(|value| !value.contains("/host/")));
        fs::remove_dir_all(root).ok();
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
        fs::create_dir_all(roots.codex.join("memories/skills/reviewer")).unwrap();
        fs::write(
            roots.codex.join("memories/MEMORY.md"),
            "synthetic durable memory",
        )
        .unwrap();
        fs::write(
            roots.codex.join("memories/memory_summary.md"),
            "synthetic memory summary",
        )
        .unwrap();
        fs::write(
            roots.codex.join("memories/skills/reviewer/SKILL.md"),
            "synthetic memory skill",
        )
        .unwrap();
        fs::create_dir_all(roots.codex.join("memories/sessions")).unwrap();
        fs::write(
            roots.codex.join("memories/sessions/transcript.jsonl"),
            "memory history must stay host-only",
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
                Path::new(".codex/memories/MEMORY.md"),
                Path::new(".codex/memories/memory_summary.md"),
                Path::new(".codex/memories/skills/reviewer/SKILL.md"),
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
                "env":{"ANTHROPIC_API_KEY":"SYNTHETIC_CLAUDE_SECRET"},
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
        assert!(value.get("futurePortableSetting").is_none());
        assert!(value.get("env").is_none());
        assert!(value.get("hooks").is_none());
        assert!(value.get("statusLine").is_none());
        assert_eq!(snapshot.diagnostics.removed_host_commands, 4);
        assert!(!String::from_utf8_lossy(&settings.bytes).contains("/host/"));
        assert!(!String::from_utf8_lossy(&settings.bytes).contains("SYNTHETIC_CLAUDE_SECRET"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_config_keeps_portable_preferences_but_drops_provider_and_mcp_secrets() {
        let (root, roots) = fixture("codex-secret-config");
        fs::write(
            roots.codex.join("config.toml"),
            r#"
model = "synthetic"
model_reasoning_effort = "high"
cli_auth_credentials_store = "file"
future_unknown = "SYNTHETIC_UNKNOWN_SECRET"

[features]
web_search = true

[mcp_servers.private]
command = "/host/private-server"
env = { API_TOKEN = "SYNTHETIC_MCP_SECRET" }

[model_providers.private]
base_url = "https://private.invalid"
env_key = "SYNTHETIC_PROVIDER_SECRET"
"#,
        )
        .unwrap();

        let snapshot = build_snapshot(&roots).unwrap();
        let config = snapshot
            .files
            .iter()
            .find(|file| file.guest_path == Path::new(".codex/config.toml"))
            .unwrap();
        let value =
            toml::from_str::<toml::Value>(std::str::from_utf8(&config.bytes).unwrap()).unwrap();

        assert_eq!(value["model"].as_str(), Some("synthetic"));
        assert_eq!(value["model_reasoning_effort"].as_str(), Some("high"));
        assert_eq!(value["cli_auth_credentials_store"].as_str(), Some("file"));
        assert_eq!(value["features"]["web_search"].as_bool(), Some(true));
        assert!(value.get("mcp_servers").is_none());
        assert!(value.get("model_providers").is_none());
        assert!(value.get("future_unknown").is_none());
        let text = String::from_utf8_lossy(&config.bytes);
        for secret in [
            "SYNTHETIC_UNKNOWN_SECRET",
            "SYNTHETIC_MCP_SECRET",
            "SYNTHETIC_PROVIDER_SECRET",
            "/host/private-server",
        ] {
            assert!(!text.contains(secret), "{secret} escaped sanitization");
        }
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
    fn opened_tree_descriptor_cannot_be_retargeted_by_directory_symlink_swap() {
        let (root, roots) = fixture("directory-swap");
        let allowed = roots.claude.join("skills/reviewer");
        fs::write(allowed.join("SAFE.md"), "SYNTHETIC_SAFE_VALUE").unwrap();
        let directory = open_directory_no_follow(&allowed).unwrap();
        let held = roots.claude.join("skills/reviewer-held");
        fs::rename(&allowed, &held).unwrap();
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("SECRET.md"), "SYNTHETIC_HOST_SECRET").unwrap();
        symlink(&outside, &allowed).unwrap();
        let mut snapshot = ConfigSnapshot {
            files: Vec::new(),
            fingerprint: String::new(),
            diagnostics: MirrorDiagnostics::default(),
        };
        let mut total_bytes = 0;

        collect_open_tree(
            &directory,
            Path::new(""),
            Path::new(".claude/skills/reviewer"),
            &mut snapshot,
            &mut total_bytes,
        )
        .unwrap();

        assert_eq!(
            guest_paths(&snapshot),
            vec![Path::new(".claude/skills/reviewer/SAFE.md")]
        );
        let mirrored = String::from_utf8_lossy(&snapshot.files[0].bytes);
        assert!(mirrored.contains("SYNTHETIC_SAFE_VALUE"));
        assert!(!mirrored.contains("SYNTHETIC_HOST_SECRET"));
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
