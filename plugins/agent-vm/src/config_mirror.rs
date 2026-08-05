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
    /// Путь к `~/.claude.json`. Файл целиком не переносится (в нём OAuth,
    /// project trust и кэши) — из него извлекается только user-scoped
    /// `mcpServers`, см. §9.2 спеки.
    pub claude_json: PathBuf,
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
    /// MCP-серверы, оставленные на хосте: их адрес в гостье ведёт в саму VM.
    pub host_only_mcp_servers: Vec<String>,
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
    collect_user_mcp_servers(&roots.claude_json, &mut snapshot, &mut total_bytes)?;
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

/// Куда деть MCP-сервер при переносе в VM.
#[derive(Debug, PartialEq, Eq)]
enum McpVerdict {
    /// Работает в гостье как есть.
    Portable,
    /// Остаётся на хосте; строка — причина для пользователя.
    HostOnly(&'static str),
}

/// Хосты, которые внутри VM означают саму VM. Сервер на таком адресе молча не
/// подключится: IDE и host-сайдкары живут на хосте, а не в гостье.
fn is_guest_local_url(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let authority = lower.split_once("//").map_or(lower.as_str(), |(_, rest)| rest);
    let authority = authority.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once('@').map_or(authority, |(_, rest)| rest);
    // IPv6 в URL пишется в скобках: [::1]:port
    let host = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "0.0.0.0" | "[::1]")
        || host.ends_with(".localhost")
        || host.starts_with("127.")
}

/// Абсолютный путь хоста, которого в гостье не будет. Проектные пути сюда не
/// попадают: project root смонтирован в VM, его переписывает вызывающий.
fn is_host_absolute(value: &str) -> bool {
    value.starts_with('/') || value.starts_with("~/")
}

/// Секреты не являются data contract (§19.5): ключи, похожие на токен, в гостя
/// не уезжают, даже если пользователь положил их в MCP-конфиг.
fn looks_like_secret_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    ["TOKEN", "SECRET", "KEY", "PASSWORD", "PASSWD", "CREDENTIAL", "AUTH", "COOKIE", "SESSION"]
        .iter()
        .any(|needle| upper.contains(needle))
}

/// Классифицирует произвольный MCP-сервер. Форма конфигурации у пользователей
/// любая: stdio/http/sse/ws, свои поля, будущие типы — поэтому решение
/// принимается по фактам (url, command, args), а не по списку известных имён.
fn classify_mcp_server(server: &serde_json::Value) -> McpVerdict {
    let Some(object) = server.as_object() else {
        return McpVerdict::HostOnly("определение сервера не является объектом");
    };
    if let Some(url) = object.get("url").and_then(|it| it.as_str()) {
        if is_guest_local_url(url) {
            return McpVerdict::HostOnly("адрес ведёт в loopback: в VM это сама VM");
        }
        return McpVerdict::Portable;
    }
    if let Some(command) = object.get("command").and_then(|it| it.as_str()) {
        if is_host_absolute(command) {
            return McpVerdict::HostOnly("команда задана абсолютным путём хоста");
        }
        let host_arg = object
            .get("args")
            .and_then(|it| it.as_array())
            .map(|args| {
                args.iter()
                    .filter_map(|arg| arg.as_str())
                    .any(is_host_absolute)
            })
            .unwrap_or(false);
        if host_arg {
            return McpVerdict::HostOnly("аргумент указывает на путь хоста");
        }
        return McpVerdict::Portable;
    }
    McpVerdict::HostOnly("не указан ни url, ни command")
}

/// Убирает из `env` значения, похожие на секреты, оставляя остальные настройки.
fn strip_secret_env(server: &mut serde_json::Value, diagnostics: &mut MirrorDiagnostics) {
    let Some(env) = server
        .as_object_mut()
        .and_then(|object| object.get_mut("env"))
        .and_then(|env| env.as_object_mut())
    else {
        return;
    };
    let secret_keys = env
        .keys()
        .filter(|key| looks_like_secret_key(key))
        .cloned()
        .collect::<Vec<_>>();
    for key in secret_keys {
        env.remove(&key);
        diagnostics.removed_host_commands += 1;
    }
}

/// Codex описывает MCP в TOML. Политика та же, что у Claude: решаем по форме
/// (url/command/args), а не по именам, — конфигурации у пользователей любые.
fn classify_codex_mcp_server(server: &toml::Value) -> McpVerdict {
    let Some(table) = server.as_table() else {
        return McpVerdict::HostOnly("определение сервера не является таблицей");
    };
    if let Some(url) = table.get("url").and_then(toml::Value::as_str) {
        if is_guest_local_url(url) {
            return McpVerdict::HostOnly("адрес ведёт в loopback: в VM это сама VM");
        }
        return McpVerdict::Portable;
    }
    if let Some(command) = table.get("command").and_then(toml::Value::as_str) {
        // относительный путь тоже host-only: он резолвится от каталога хоста
        if is_host_absolute(command) || command.starts_with("./") || command.starts_with("../") {
            return McpVerdict::HostOnly("команда указывает на путь хоста");
        }
        let host_arg = table
            .get("args")
            .and_then(toml::Value::as_array)
            .map(|args| {
                args.iter()
                    .filter_map(toml::Value::as_str)
                    .any(is_host_absolute)
            })
            .unwrap_or(false);
        if host_arg {
            return McpVerdict::HostOnly("аргумент указывает на путь хоста");
        }
        return McpVerdict::Portable;
    }
    McpVerdict::HostOnly("не указан ни url, ни command")
}

/// TOML-версия `strip_secret_env`.
fn strip_secret_toml_env(server: &mut toml::Value, diagnostics: &mut MirrorDiagnostics) {
    let Some(env) = server
        .as_table_mut()
        .and_then(|table| table.get_mut("env"))
        .and_then(toml::Value::as_table_mut)
    else {
        return;
    };
    let secret_keys = env
        .keys()
        .filter(|key| looks_like_secret_key(key))
        .cloned()
        .collect::<Vec<_>>();
    for key in secret_keys {
        env.remove(&key);
        diagnostics.removed_host_commands += 1;
    }
}

/// Из `~/.claude.json` берём только user-scoped `mcpServers` и записываем их
/// отдельным guest-файлом. Файл целиком копировать нельзя: он смешивает OAuth,
/// project trust и кэши.
///
/// Непереносимый сервер не копируется и не выбрасывается молча: он попадает в
/// диагностику с причиной — тихо запускать сломанную конфигурацию нельзя (§9.2).
fn collect_user_mcp_servers(
    claude_json: &Path,
    snapshot: &mut ConfigSnapshot,
    total_bytes: &mut u64,
) -> Result<(), String> {
    let guest_path = PathBuf::from(".claude/mcp-servers.json");
    let metadata = match fs::symlink_metadata(claude_json) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            snapshot.note_missing(&guest_path);
            return Ok(());
        }
        Err(_) => return Err("не проверить ~/.claude.json".into()),
    };
    if metadata.file_type().is_symlink() {
        snapshot.diagnostics.skipped_symlinks += 1;
        return Ok(());
    }
    if !metadata.file_type().is_file() {
        snapshot.diagnostics.skipped_non_regular += 1;
        return Ok(());
    }
    if metadata.len() > MAX_MIRROR_FILE_BYTES {
        snapshot.diagnostics.skipped_oversize += 1;
        return Ok(());
    }
    let mut file = no_follow_file(claude_json)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(MAX_MIRROR_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "не прочитать ~/.claude.json".to_string())?;
    let parsed = serde_json::from_slice::<serde_json::Value>(&bytes)
        .map_err(|_| "~/.claude.json содержит invalid JSON".to_string())?;
    bytes.zeroize();

    let mut portable = serde_json::Map::new();
    if let Some(servers) = parsed.get("mcpServers").and_then(|it| it.as_object()) {
        for (name, server) in servers {
            match classify_mcp_server(server) {
                McpVerdict::HostOnly(reason) => {
                    snapshot
                        .diagnostics
                        .host_only_mcp_servers
                        .push(format!("{name}: {reason}"));
                }
                McpVerdict::Portable => {
                    let mut copy = server.clone();
                    strip_secret_env(&mut copy, &mut snapshot.diagnostics);
                    portable.insert(name.clone(), copy);
                }
            }
        }
    }
    if portable.is_empty() {
        return Ok(());
    }
    let mut document = serde_json::Map::new();
    document.insert("mcpServers".into(), serde_json::Value::Object(portable));
    let rendered = serde_json::to_vec_pretty(&serde_json::Value::Object(document))
        .map_err(|_| "не сериализовать перенесённые mcpServers".to_string())?;

    validate_guest_path(&guest_path)?;
    if snapshot.files.len() >= MAX_MIRROR_FILES {
        return Err("config mirror превышает file-count limit".into());
    }
    let next_total = total_bytes
        .checked_add(rendered.len() as u64)
        .ok_or_else(|| "config mirror size overflow".to_string())?;
    if next_total > MAX_MIRROR_TOTAL_BYTES {
        return Err("config mirror превышает total-size limit".into());
    }
    *total_bytes = next_total;
    snapshot.files.push(MirroredFile {
        guest_path,
        bytes: rendered,
        mode: 0o600,
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
    // MCP-серверы Codex: раньше вырезались целиком, из-за чего MCP в VM не
    // работал вовсе. Переносим те, что в гостье действительно поднимутся.
    if let Some(servers) = object.get("mcp_servers").and_then(toml::Value::as_table) {
        let mut travelling = toml::map::Map::new();
        for (name, server) in servers {
            match classify_codex_mcp_server(server) {
                McpVerdict::Portable => {
                    let mut copy = server.clone();
                    strip_secret_toml_env(&mut copy, diagnostics);
                    travelling.insert(name.clone(), copy);
                }
                McpVerdict::HostOnly(reason) => {
                    diagnostics
                        .host_only_mcp_servers
                        .push(format!("{name}: {reason}"));
                }
            }
        }
        if !travelling.is_empty() {
            portable.insert("mcp_servers".into(), toml::Value::Table(travelling));
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
        let claude_json = root.join("host/.claude.json");
        (
            root,
            MirrorRoots {
                claude,
                codex,
                claude_json,
            },
        )
    }

    fn guest_paths(snapshot: &ConfigSnapshot) -> Vec<&Path> {
        snapshot
            .files
            .iter()
            .map(|file| file.guest_path.as_path())
            .collect()
    }

    #[test]
    fn codex_mcp_servers_travel_when_the_guest_can_actually_reach_them() {
        let (root, roots) = fixture("codex-mcp");
        fs::write(
            roots.codex.join("config.toml"),
            r#"
model = "synthetic"

[mcp_servers.docs]
url = "https://developers.example.com/mcp"

[mcp_servers.packaged]
command = "npx"
args = ["-y", "@scope/server"]
env = { LOG_LEVEL = "debug", API_TOKEN = "SYNTHETIC_CODEX_MCP_SECRET" }

[mcp_servers.bundled-app]
command = "./Some.app/Contents/MacOS/helper"
args = ["mcp"]

[mcp_servers.ide]
url = "http://127.0.0.1:8080/mcp"
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
        let servers = value["mcp_servers"].as_table().unwrap();

        let mut travelled = servers.keys().cloned().collect::<Vec<_>>();
        travelled.sort();
        assert_eq!(travelled, vec!["docs", "packaged"]);
        assert!(servers["packaged"]["env"].get("API_TOKEN").is_none());
        assert_eq!(
            servers["packaged"]["env"]["LOG_LEVEL"].as_str(),
            Some("debug")
        );

        let reported = snapshot.diagnostics.host_only_mcp_servers.join("\n");
        assert!(reported.contains("bundled-app"), "{reported}");
        assert!(reported.contains("ide"), "{reported}");
        assert!(!String::from_utf8_lossy(&config.bytes).contains("SYNTHETIC_CODEX_MCP_SECRET"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn user_mcp_servers_travel_by_shape_not_by_known_names() {
        let (root, roots) = fixture("mcp");
        // произвольный набор пользователя: разные типы, свои поля, секреты
        fs::write(
            &roots.claude_json,
            br#"{
              "oauthAccount": {"emailAddress":"must-stay-host@example.com"},
              "mcpServers": {
                "remote-http": {"type":"http","url":"https://api.example.com/mcp"},
                "remote-ws":   {"type":"ws","url":"wss://example.com/socket"},
                "stdio-npx":   {"type":"stdio","command":"npx","args":["-y","@scope/server"],
                                "env":{"LOG_LEVEL":"debug","API_TOKEN":"must-not-travel"}},
                "ide-bridge":  {"type":"sse","url":"http://localhost:64342/sse"},
                "loopback-ip": {"type":"http","url":"http://127.0.0.1:8080/mcp"},
                "ipv6-local":  {"type":"http","url":"http://[::1]:9000/mcp"},
                "host-binary": {"type":"stdio","command":"/opt/homebrew/bin/thing"},
                "host-arg":    {"type":"stdio","command":"node","args":["/Users/someone/x.js"]},
                "malformed":   "not-an-object"
              }
            }"#,
        )
        .unwrap();

        let snapshot = build_snapshot(&roots).unwrap();
        let mirrored = snapshot
            .files
            .iter()
            .find(|file| file.guest_path == Path::new(".claude/mcp-servers.json"))
            .expect("перенесённые mcpServers");
        let value: serde_json::Value = serde_json::from_slice(&mirrored.bytes).unwrap();
        let servers = value.get("mcpServers").unwrap().as_object().unwrap();

        // переносимое — по форме, а не по имени
        let mut travelled = servers.keys().cloned().collect::<Vec<_>>();
        travelled.sort();
        assert_eq!(travelled, vec!["remote-http", "remote-ws", "stdio-npx"]);

        // секрет вырезан, обычная настройка осталась
        let env = servers["stdio-npx"].get("env").unwrap();
        assert!(env.get("API_TOKEN").is_none());
        assert_eq!(env.get("LOG_LEVEL").unwrap(), "debug");

        // остальное объяснено, а не выброшено молча
        let reported = snapshot.diagnostics.host_only_mcp_servers.join("\n");
        for name in [
            "ide-bridge",
            "loopback-ip",
            "ipv6-local",
            "host-binary",
            "host-arg",
            "malformed",
        ] {
            assert!(reported.contains(name), "нет причины для {name}: {reported}");
        }

        // из .claude.json не утекает ничего, кроме mcpServers
        let text = String::from_utf8_lossy(&mirrored.bytes);
        assert!(!text.contains("must-stay-host"));
        assert!(!text.contains("must-not-travel"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn missing_or_empty_claude_json_is_not_an_error() {
        // у нового пользователя ~/.claude.json может не быть вовсе
        let (root, roots) = fixture("mcp-absent");
        let snapshot = build_snapshot(&roots).unwrap();
        assert!(!snapshot
            .files
            .iter()
            .any(|file| file.guest_path == Path::new(".claude/mcp-servers.json")));
        assert!(snapshot
            .diagnostics
            .missing_sources
            .contains(&".claude/mcp-servers.json".to_string()));

        // либо файл есть, но без mcpServers — тоже штатно, пустышку не пишем
        let (root2, roots2) = fixture("mcp-empty");
        fs::write(&roots2.claude_json, br#"{"autoUpdates":true}"#).unwrap();
        let snapshot2 = build_snapshot(&roots2).unwrap();
        assert!(!snapshot2
            .files
            .iter()
            .any(|file| file.guest_path == Path::new(".claude/mcp-servers.json")));
        assert!(snapshot2.diagnostics.host_only_mcp_servers.is_empty());
        fs::remove_dir_all(root).ok();
        fs::remove_dir_all(root2).ok();
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
        // host-абсолютная команда делает сервер непереносимым — и это объяснено
        assert!(value.get("mcp_servers").is_none());
        assert!(snapshot
            .diagnostics
            .host_only_mcp_servers
            .iter()
            .any(|entry| entry.starts_with("private:")));
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
