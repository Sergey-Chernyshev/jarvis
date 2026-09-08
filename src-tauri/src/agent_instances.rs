//! Codex installations/accounts, independent of the desktop runtime.
//!
//! A home is the identity boundary: two launchers may use the same home, while
//! two homes may contain the same conversation UUID. Discovery never executes a
//! launcher, reads credentials, or recursively searches a user's home directory.
//! This module intentionally depends only on std + serde so setup/node can share it.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const CONFIG_FILE: &str = "agent-instances.json";
const MAX_CONFIG_BYTES: u64 = 256 * 1024;
const MAX_ENTRIES: usize = 128;
const MAX_PATH_DIRS: usize = 32;
const MAX_DIRECTORY_ENTRIES: usize = 4096;
const MAX_WRAPPERS: usize = 64;
const MAX_WRAPPER_BYTES: u64 = 64 * 1024;
static WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn enabled() -> bool {
    true
}
fn local_machine() -> String {
    "local".into()
}
fn version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstanceEntry {
    pub home: PathBuf,
    #[serde(default)]
    pub label: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default = "local_machine")]
    pub machine: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<PathBuf>,
    /// A desktop launcher is presentation metadata, never substituted for CLI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_launcher: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstanceConfig {
    #[serde(default = "version")]
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<InstanceEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_codex_instance: Option<String>,
}

impl Default for InstanceConfig {
    fn default() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
            default_codex_instance: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum DiscoverySource {
    DefaultHome,
    Environment,
    PathWrapper,
    Explicit,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum LauncherKind {
    Cli,
    Desktop,
    Wrapper,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct Launcher {
    pub kind: LauncherKind,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInstance {
    pub id: String,
    pub agent: String,
    pub machine: String,
    pub home: PathBuf,
    pub canonical_home: PathBuf,
    pub label: String,
    pub enabled: bool,
    pub exists: bool,
    pub sources: Vec<DiscoverySource>,
    pub launchers: Vec<Launcher>,
    pub cli: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Registry {
    pub instances: Vec<AgentInstance>,
    pub default_codex_instance: String,
    /// The registry is scoped to one filesystem, including when hosted on a VM.
    pub machine: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceRoot {
    pub instance_id: String,
    pub home: PathBuf,
    pub path: PathBuf,
    pub archived: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchSpec {
    pub instance_id: String,
    pub program: PathBuf,
    /// Set with Command::env or proper shell quoting; do not inherit another home.
    pub codex_home: PathBuf,
}

/// Explicit inputs make discovery deterministic in tests and portable to node.
#[derive(Debug, Clone)]
pub struct DiscoveryContext {
    pub machine: String,
    pub home: PathBuf,
    pub codex_home: Option<PathBuf>,
    pub path_dirs: Vec<PathBuf>,
    pub cli_candidates: Vec<PathBuf>,
    /// Excludes Jarvis's own shim directory to avoid recursively launching itself.
    pub excluded_dirs: Vec<PathBuf>,
}

impl DiscoveryContext {
    pub fn from_env() -> Result<Self, String> {
        let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
            "HOME не задан: укажите домашний каталог для обнаружения Codex".to_string()
        })?;
        checked_path(&home, "HOME")?;
        let mut path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| {
                std::env::split_paths(&p)
                    .filter(|p| p.is_absolute())
                    .collect()
            })
            .unwrap_or_default();
        for path in [
            home.join(".local/bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ] {
            if !path_dirs.contains(&path) {
                path_dirs.push(path);
            }
        }
        let mut cli_candidates = Vec::new();
        for app in [PathBuf::from("/Applications"), home.join("Applications")] {
            for name in ["ChatGPT.app", "Codex.app"] {
                cli_candidates.push(app.join(name).join("Contents/Resources/codex"));
            }
        }
        let jarvis = std::env::var_os("JARVIS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".jarvis"));
        Ok(Self {
            machine: local_machine(),
            codex_home: std::env::var_os("CODEX_HOME")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
            home: home.clone(),
            path_dirs,
            cli_candidates,
            excluded_dirs: vec![
                jarvis.join("shims"),
                home.join(".jarvis/shims"),
                home.join(".jarvis-dev/shims"),
            ],
        })
    }
}

fn checked_path(path: &Path, field: &str) -> Result<(), String> {
    let raw = path
        .to_str()
        .ok_or_else(|| format!("{field}: путь должен быть в UTF-8"))?;
    if !path.is_absolute() || raw.len() > 4096 || raw.chars().any(char::is_control) {
        return Err(format!(
            "{field}: нужен абсолютный путь без управляющих символов, не длиннее 4096 байт"
        ));
    }
    Ok(())
}

fn checked_machine(machine: &str) -> Result<(), String> {
    if machine.is_empty()
        || machine.len() > 128
        || !machine
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("machine: допустимы латиница, цифры, точка, дефис и подчёркивание".into());
    }
    Ok(())
}

/// Resolve symlinks in existing ancestors even for a not-yet-created home.
/// Missing explicit entries remain configurable and retain a stable identity.
pub fn canonical_home(path: &Path) -> Result<PathBuf, String> {
    checked_path(path, "home")?;
    for ancestor in path.ancestors() {
        if let Ok(mut canonical) = fs::canonicalize(ancestor) {
            for component in path
                .strip_prefix(ancestor)
                .map_err(|e| e.to_string())?
                .components()
            {
                match component {
                    Component::ParentDir => {
                        canonical.pop();
                    }
                    Component::CurDir => {}
                    other => canonical.push(other.as_os_str()),
                }
            }
            return Ok(canonical);
        }
    }
    Err("Не удалось разрешить абсолютный путь инстанса".into())
}

/// Versioned deterministic ID; DefaultHasher is intentionally not used because
/// its algorithm is not a persistence contract across Rust releases.
pub fn instance_id(machine: &str, home: &Path) -> Result<String, String> {
    provider_instance_id("codex", machine, home)
}

/// Also used by a remote node for Claude source identity. The provider is a
/// prefix namespace; machine + canonical home are the exact bytes being hashed.
pub fn provider_instance_id(provider: &str, machine: &str, home: &Path) -> Result<String, String> {
    if !matches!(provider, "codex" | "claude") {
        return Err("Неизвестный провайдер инстанса".into());
    }
    checked_machine(machine)?;
    let canonical = canonical_home(home)?;
    let mut hash = 0xcbf29ce484222325u64;
    for byte in machine
        .as_bytes()
        .iter()
        .copied()
        .chain([0])
        .chain(canonical.to_string_lossy().bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Ok(format!("{provider}-v1-{hash:016x}"))
}

fn validate(config: &InstanceConfig) -> Result<(), String> {
    if config.version != 1 {
        return Err(format!(
            "Версия реестра {} не поддерживается",
            config.version
        ));
    }
    if config.entries.len() > MAX_ENTRIES {
        return Err(format!(
            "В реестре может быть не больше {MAX_ENTRIES} инстансов"
        ));
    }
    let mut seen = BTreeSet::new();
    for entry in &config.entries {
        checked_machine(&entry.machine)?;
        checked_path(&entry.home, "home")?;
        if entry.label.len() > 160 || entry.label.chars().any(char::is_control) {
            return Err("label: не больше 160 байт, без управляющих символов".into());
        }
        for (name, path) in [
            ("cli", &entry.cli),
            ("desktopLauncher", &entry.desktop_launcher),
        ] {
            if let Some(path) = path {
                checked_path(path, name)?;
            }
        }
        if !seen.insert((entry.machine.clone(), canonical_home(&entry.home)?)) {
            return Err("Один home указан несколько раз через одинаковые пути или ссылки".into());
        }
    }
    if config
        .default_codex_instance
        .as_ref()
        .is_some_and(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
    {
        return Err("defaultCodexInstance: некорректный идентификатор".into());
    }
    Ok(())
}

pub fn load(data_dir: &Path) -> Result<InstanceConfig, String> {
    let path = data_dir.join(CONFIG_FILE);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(InstanceConfig::default()),
        Err(e) => return Err(format!("Не удалось прочитать реестр инстансов: {e}")),
    };
    let mut raw = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|e| format!("Не удалось прочитать реестр: {e}"))?;
    if raw.len() as u64 > MAX_CONFIG_BYTES {
        return Err("Файл реестра инстансов слишком большой".into());
    }
    let config: InstanceConfig = serde_json::from_slice(&raw)
        .map_err(|e| format!("Некорректный agent-instances.json: {e}"))?;
    validate(&config)?;
    Ok(config)
}

pub fn load_registry(data_dir: &Path) -> Result<Registry, String> {
    discover(&load(data_dir)?)
}

pub fn discover(config: &InstanceConfig) -> Result<Registry, String> {
    discover_with(config, &DiscoveryContext::from_env()?)
}

fn executable(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn path_dirs(context: &DiscoveryContext) -> Vec<PathBuf> {
    let excluded: BTreeSet<PathBuf> = context
        .excluded_dirs
        .iter()
        .filter_map(|p| canonical_home(p).ok())
        .collect();
    let mut seen = BTreeSet::new();
    context
        .path_dirs
        .iter()
        .filter_map(|p| canonical_home(p).ok())
        .filter(|p| !excluded.contains(p) && seen.insert(p.clone()))
        .take(MAX_PATH_DIRS)
        .collect()
}

fn default_cli(context: &DiscoveryContext) -> Option<PathBuf> {
    path_dirs(context)
        .into_iter()
        .map(|p| p.join("codex"))
        .chain(context.cli_candidates.iter().cloned())
        .find(|p| executable(p) && !jarvis_shim(p))
}

fn jarvis_shim(path: &Path) -> bool {
    // Alternate dev/staging installations may be anywhere in PATH. Recognize
    // our own header rather than assuming that only today's JARVIS_DIR exists.
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut bytes = Vec::new();
    if file.take(1024).read_to_end(&mut bytes).is_err() {
        return false;
    }
    let head = String::from_utf8_lossy(&bytes);
    head.contains("# jarvis agent shim") || head.contains("# jarvis-custom-agent")
}

fn add_instance(
    instances: &mut BTreeMap<PathBuf, AgentInstance>,
    context: &DiscoveryContext,
    home: &Path,
    source: DiscoverySource,
    cli: &Option<PathBuf>,
) -> Result<(), String> {
    let canonical = canonical_home(home)?;
    let id = instance_id(&context.machine, &canonical)?;
    let instance = instances
        .entry(canonical.clone())
        .or_insert_with(|| AgentInstance {
            id,
            agent: "codex".into(),
            machine: context.machine.clone(),
            home: canonical.clone(),
            canonical_home: canonical.clone(),
            label: "Codex".into(),
            enabled: true,
            exists: canonical.is_dir(),
            sources: Vec::new(),
            launchers: Vec::new(),
            cli: cli.clone(),
        });
    if !instance.sources.contains(&source) {
        instance.sources.push(source);
    }
    Ok(())
}

pub fn discover_with(
    config: &InstanceConfig,
    context: &DiscoveryContext,
) -> Result<Registry, String> {
    validate(config)?;
    checked_machine(&context.machine)?;
    checked_path(&context.home, "HOME")?;
    let default_home = context.home.join(".codex");
    let mut instances = BTreeMap::new();
    let cli = default_cli(context);
    add_instance(
        &mut instances,
        context,
        &default_home,
        DiscoverySource::DefaultHome,
        &cli,
    )?;
    if let Some(home) = &context.codex_home {
        add_instance(
            &mut instances,
            context,
            home,
            DiscoverySource::Environment,
            &cli,
        )?;
    }
    for wrapper in discover_wrappers(context) {
        add_instance(
            &mut instances,
            context,
            &wrapper.home,
            DiscoverySource::PathWrapper,
            &cli,
        )?;
        let canonical = canonical_home(&wrapper.home)?;
        let instance = instances.get_mut(&canonical).expect("inserted above");
        if instance.label == "Codex" {
            instance.label = wrapper.label;
        }
        instance.launchers.push(Launcher {
            kind: LauncherKind::Wrapper,
            path: wrapper.path,
        });
        if let Some(path) = wrapper.desktop {
            instance.launchers.push(Launcher {
                kind: LauncherKind::Desktop,
                path,
            });
        }
    }
    for entry in config
        .entries
        .iter()
        .filter(|entry| entry.machine == context.machine)
    {
        add_instance(
            &mut instances,
            context,
            &entry.home,
            DiscoverySource::Explicit,
            &cli,
        )?;
        let instance = instances
            .get_mut(&canonical_home(&entry.home)?)
            .expect("inserted above");
        instance.enabled = entry.enabled;
        if !entry.label.trim().is_empty() {
            instance.label = entry.label.trim().into();
        }
        if let Some(path) = &entry.cli {
            instance.cli = Some(path.clone());
        }
        if let Some(path) = &entry.desktop_launcher {
            instance.launchers.push(Launcher {
                kind: LauncherKind::Desktop,
                path: path.clone(),
            });
        }
    }
    for instance in instances.values_mut() {
        if let Some(path) = &instance.cli {
            instance.launchers.push(Launcher {
                kind: LauncherKind::Cli,
                path: path.clone(),
            });
        }
        instance.sources.sort();
        instance.launchers.sort();
        instance.launchers.dedup();
    }
    let preferred = context.codex_home.as_ref().unwrap_or(&default_home);
    let default_codex_instance = config
        .default_codex_instance
        .clone()
        .unwrap_or(instance_id(&context.machine, preferred)?);
    let mut registry = Registry {
        instances: instances.into_values().collect(),
        default_codex_instance,
        machine: context.machine.clone(),
    };
    registry.instances.sort_by(|a, b| {
        a.label
            .to_lowercase()
            .cmp(&b.label.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    // Explicit invalid defaults are actionable errors. Do not silently resume
    // a conversation using a different account after a rename/disable/removal.
    if config.default_codex_instance.is_some() {
        registry.resolve(None)?;
    }
    Ok(registry)
}

impl Registry {
    pub fn resolve(&self, id: Option<&str>) -> Result<&AgentInstance, String> {
        let id = id.unwrap_or(&self.default_codex_instance);
        let instance = self.instances.iter().find(|i| i.id == id).ok_or_else(|| {
            "Инстанс Codex не найден. Обновите список или выберите другой инстанс".to_string()
        })?;
        if !instance.enabled {
            return Err(format!("Инстанс «{}» отключён", instance.label));
        }
        Ok(instance)
    }

    pub fn roots(&self, include_archived: bool) -> Vec<InstanceRoot> {
        self.instances
            .iter()
            .filter(|i| i.enabled)
            .flat_map(|instance| {
                [false, true]
                    .into_iter()
                    .filter(move |archived| !archived || include_archived)
                    .map(move |archived| InstanceRoot {
                        instance_id: instance.id.clone(),
                        home: instance.canonical_home.clone(),
                        path: instance.canonical_home.join(if archived {
                            "archived_sessions"
                        } else {
                            "sessions"
                        }),
                        archived,
                    })
            })
            .collect()
    }

    /// Match only actual rollout directories, not arbitrary files under a home.
    /// Longest-root wins if an explicit home is nested inside another instance.
    pub fn instance_for_transcript(&self, path: &Path) -> Option<&AgentInstance> {
        let canonical = canonical_home(path).ok()?;
        self.instances
            .iter()
            .filter(|i| {
                i.enabled
                    && ["sessions", "archived_sessions"]
                        .iter()
                        .any(|directory| canonical.starts_with(i.canonical_home.join(directory)))
            })
            .max_by_key(|i| i.canonical_home.components().count())
    }

    pub fn launch_spec(&self, id: Option<&str>) -> Result<LaunchSpec, String> {
        let instance = self.resolve(id)?;
        let program = instance
            .cli
            .as_ref()
            .filter(|path| executable(path) && !jarvis_shim(path))
            .ok_or_else(|| format!("Для «{}» не найден исполняемый Codex CLI", instance.label))?;
        if !instance.canonical_home.is_dir() {
            return Err(format!("Каталог инстанса «{}» недоступен", instance.label));
        }
        Ok(LaunchSpec {
            instance_id: instance.id.clone(),
            program: program.clone(),
            codex_home: instance.canonical_home.clone(),
        })
    }

    /// Open the profile's Desktop wrapper, including its user-data directory.
    /// Opening the common .app directly would activate a different account.
    pub fn desktop_program(&self, id: &str) -> Result<PathBuf, String> {
        let instance = self.resolve(Some(id))?;
        let context = DiscoveryContext::from_env()?;
        if let Some(wrapper) = discover_wrappers(&context).into_iter().find(|wrapper|
            wrapper.desktop.is_some() && canonical_home(&wrapper.home).ok().as_ref() == Some(&instance.canonical_home)) {
            return Ok(wrapper.path);
        }
        instance.launchers.iter().find(|launcher| launcher.kind == LauncherKind::Desktop)
            .map(|launcher| launcher.path.clone())
            .ok_or_else(|| format!("Для «{}» не указан запуск Codex Desktop. Добавь ярлык профиля в настройках",instance.label))
    }
}

pub fn save(data_dir: &Path, config: &InstanceConfig) -> Result<Registry, String> {
    save_with(data_dir, config, &DiscoveryContext::from_env()?)
}

pub fn save_with(
    data_dir: &Path,
    config: &InstanceConfig,
    context: &DiscoveryContext,
) -> Result<Registry, String> {
    let registry = discover_with(config, context)?;
    let mut raw = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
    raw.push(b'\n');
    if raw.len() as u64 > MAX_CONFIG_BYTES {
        return Err("Файл реестра инстансов слишком большой".into());
    }
    fs::create_dir_all(data_dir).map_err(|e| format!("Не удалось создать каталог реестра: {e}"))?;
    let nonce = WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = data_dir.join(format!(
        ".{CONFIG_FILE}.{}.{}.tmp",
        std::process::id(),
        nonce
    ));
    let mut created = false;
    let result = (|| -> std::io::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        created = true;
        file.write_all(&raw)?;
        file.sync_all()?;
        fs::rename(&temp, data_dir.join(CONFIG_FILE))?;
        let _ = File::open(data_dir).and_then(|directory| directory.sync_all());
        Ok(())
    })();
    if result.is_err() && created {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|e| format!("Не удалось сохранить реестр инстансов: {e}"))?;
    Ok(registry)
}

#[derive(Debug)]
struct Wrapper {
    path: PathBuf,
    label: String,
    home: PathBuf,
    desktop: Option<PathBuf>,
}

fn discover_wrappers(context: &DiscoveryContext) -> Vec<Wrapper> {
    let mut paths = BTreeSet::new();
    for dir in path_dirs(context) {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.take(MAX_DIRECTORY_ENTRIES).flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with("codex-") && name.len() <= 100 && executable(&entry.path()) {
                paths.insert(entry.path());
                if paths.len() >= MAX_WRAPPERS {
                    break;
                }
            }
        }
        if paths.len() >= MAX_WRAPPERS {
            break;
        }
    }
    paths
        .into_iter()
        .filter_map(|p| read_wrapper(&p, context))
        .collect()
}

fn codex_desktop(path: &Path) -> bool {
    path.components().any(|component| {
        let name = component.as_os_str().to_string_lossy().to_lowercase();
        name.ends_with(".app") && (name.contains("codex") || name == "chatgpt.app")
    })
}

fn read_wrapper(path: &Path, context: &DiscoveryContext) -> Option<Wrapper> {
    let canonical = fs::canonicalize(path).ok()?;
    let metadata = fs::metadata(&canonical).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_WRAPPER_BYTES {
        return None;
    }
    let mut raw = String::new();
    File::open(&canonical)
        .ok()?
        .take(MAX_WRAPPER_BYTES + 1)
        .read_to_string(&mut raw)
        .ok()?;
    if raw.len() as u64 > MAX_WRAPPER_BYTES {
        return None;
    }
    let shebang = raw.lines().next()?;
    if !shebang.starts_with("#!")
        || !["/sh", "/bash", "/zsh", "env sh", "env bash", "env zsh"]
            .iter()
            .any(|shell| shebang.trim_end().ends_with(shell))
    {
        return None;
    }
    let script_dir = canonical.parent()?.to_str()?;
    let mut vars = BTreeMap::from([("HOME".to_string(), context.home.to_str()?.to_string())]);
    let mut home = None;
    let mut saw_home = false;
    let mut desktop = None;
    let mut launches_desktop = false;
    for line in raw.lines().take(512) {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let assignment = line.strip_prefix("export ").unwrap_or(line);
        if let Some((name, value)) = assignment.split_once('=') {
            if valid_var(name) {
                if name == "CODEX_HOME" {
                    saw_home = true;
                }
                let value = if value == "${0:A:h}" || value == "\"${0:A:h}\"" {
                    Some(script_dir.to_string())
                } else {
                    static_value(value, &vars)
                };
                match value {
                    Some(value) => {
                        if name == "CODEX_HOME" {
                            home = Some(PathBuf::from(&value));
                        }
                        vars.insert(name.into(), value);
                    }
                    None => {
                        vars.remove(name);
                        if name == "CODEX_HOME" {
                            home = None;
                        }
                    }
                }
                continue;
            }
        }
        // Recognition is deliberately narrow. Unknown launch scripts remain
        // addable explicitly instead of guessing their account or running them.
        let command = line
            .strip_prefix("exec ")
            .or_else(|| line.strip_prefix("nohup "))
            .unwrap_or(line);
        let executable = first_shell_word(command.strip_prefix("open -a ").unwrap_or(command));
        if let Some(path) = static_value(executable, &vars)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute() && codex_desktop(path))
        {
            launches_desktop = true;
            desktop = Some(path);
        }
    }
    if !launches_desktop {
        return None;
    }
    let home = if saw_home {
        home?
    } else {
        context.home.join(".codex")
    };
    checked_path(&home, "CODEX_HOME").ok()?;
    Some(Wrapper {
        path: path.to_path_buf(),
        label: path.file_name()?.to_str()?.to_string(),
        home,
        desktop,
    })
}

fn valid_var(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn first_shell_word(command: &str) -> &str {
    let mut quote = None;
    for (index, character) in command.char_indices() {
        match (quote, character) {
            (Some(open), close) if open == close => quote = None,
            (None, '\'' | '"') => quote = Some(character),
            (None, space) if space.is_whitespace() => return &command[..index],
            _ => {}
        }
    }
    command
}

/// Only literals and references to earlier literal assignments. No substitutions,
/// shell operators, parameter operators, or inherited arbitrary environment.
fn static_value(raw: &str, vars: &BTreeMap<String, String>) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Some(String::new());
    }
    let (body, expand) = if raw.starts_with('\'') && raw.ends_with('\'') && raw.len() >= 2 {
        (&raw[1..raw.len() - 1], false)
    } else if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
        (&raw[1..raw.len() - 1], true)
    } else {
        if raw.chars().any(char::is_whitespace) {
            return None;
        }
        (raw, true)
    };
    if body.chars().any(|c| {
        c.is_control()
            || matches!(
                c,
                '`' | '\\' | '\'' | '"' | ';' | '|' | '&' | '(' | ')' | '<' | '>'
            )
    }) {
        return None;
    }
    if !expand {
        return Some(body.into());
    }
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '$' {
            out.push(chars[index]);
            index += 1;
            continue;
        }
        index += 1;
        let braced = chars.get(index) == Some(&'{');
        if braced {
            index += 1;
        }
        let start = index;
        while index < chars.len() && (chars[index].is_ascii_alphanumeric() || chars[index] == '_') {
            index += 1;
        }
        let name: String = chars[start..index].iter().collect();
        if !valid_var(&name) {
            return None;
        }
        if braced {
            if chars.get(index) != Some(&'}') {
                return None;
            }
            index += 1;
        }
        out.push_str(vars.get(&name)?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        root: PathBuf,
        context: DiscoveryContext,
    }
    impl Fixture {
        fn new() -> Self {
            let sequence = WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "jarvis-instances-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(root.join("home/.codex/sessions")).unwrap();
            fs::create_dir_all(root.join("bin")).unwrap();
            let context = DiscoveryContext {
                machine: "local".into(),
                home: root.join("home"),
                codex_home: None,
                path_dirs: vec![root.join("bin")],
                cli_candidates: Vec::new(),
                excluded_dirs: Vec::new(),
            };
            Self { root, context }
        }
        fn executable(&self, path: &Path, text: &str) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        fn entry(&self, home: PathBuf) -> InstanceEntry {
            InstanceEntry {
                home,
                label: String::new(),
                enabled: true,
                machine: "local".into(),
                cli: None,
                desktop_launcher: None,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn default_and_environment_homes_are_both_retained_with_explicit_entry() {
        let mut f = Fixture::new();
        let personal = f.root.join("personal");
        f.context.codex_home = Some(personal.clone());
        let mut entry = f.entry(personal.clone());
        entry.label = "Personal".into();
        let registry = discover_with(
            &InstanceConfig {
                entries: vec![entry],
                ..Default::default()
            },
            &f.context,
        )
        .unwrap();
        assert_eq!(registry.instances.len(), 2);
        let selected = registry.resolve(None).unwrap();
        assert_eq!(selected.label, "Personal");
        assert_eq!(
            selected.sources,
            vec![DiscoverySource::Environment, DiscoverySource::Explicit]
        );
        assert_eq!(selected.canonical_home, canonical_home(&personal).unwrap());
        assert!(registry
            .instances
            .iter()
            .any(|i| i.sources.contains(&DiscoverySource::DefaultHome)));
    }

    #[test]
    fn actual_dual_account_wrappers_resolve_to_personal_and_default_work() {
        let f = Fixture::new();
        let dir = f.root.join("dual account");
        fs::create_dir_all(dir.join("personal/codex-home/sessions")).unwrap();
        fs::create_dir_all(dir.join("work/codex-home")).unwrap();
        let personal = dir.join("codex-personal");
        f.executable(&personal, "#!/bin/zsh\nset -eu\nSCRIPT_DIR=${0:A:h}\nCODEX_PROFILE_HOME=\"$SCRIPT_DIR/personal/codex-home\"\nCODEX_APP=\"/Applications/ChatGPT.app/Contents/MacOS/ChatGPT\"\nexport CODEX_HOME=\"$CODEX_PROFILE_HOME\"\nnohup \"$CODEX_APP\" \\\n --user-data-dir=\"$SCRIPT_DIR/personal/ui-data\" &\n");
        let work = dir.join("codex-work");
        f.executable(&work, "#!/bin/zsh\nset -eu\nCODEX_APP=\"/Applications/ChatGPT.app\"\nexec open -a \"$CODEX_APP\" \"$@\"\n");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&personal, f.root.join("bin/codex-personal")).unwrap();
            std::os::unix::fs::symlink(&work, f.root.join("bin/codex-work")).unwrap();
        }
        #[cfg(not(unix))]
        {
            fs::copy(&personal, f.root.join("bin/codex-personal")).unwrap();
            fs::copy(&work, f.root.join("bin/codex-work")).unwrap();
        }
        let registry = discover_with(&InstanceConfig::default(), &f.context).unwrap();
        assert_eq!(registry.instances.len(), 2);
        let personal = registry
            .instances
            .iter()
            .find(|i| i.label == "codex-personal")
            .unwrap();
        assert_eq!(
            personal.canonical_home,
            canonical_home(&dir.join("personal/codex-home")).unwrap()
        );
        let work = registry
            .instances
            .iter()
            .find(|i| i.label == "codex-work")
            .unwrap();
        assert_eq!(
            work.canonical_home,
            canonical_home(&f.context.home.join(".codex")).unwrap()
        );
        assert!(registry.instances.iter().all(|i| i.cli.is_none()));
        assert!(
            registry.launch_spec(None).is_err(),
            "desktop wrappers must never become CLI binaries"
        );
    }

    #[test]
    #[cfg(unix)]
    fn aliases_share_identity_and_explicit_preferences_survive_discovery() {
        let mut f = Fixture::new();
        let real = f.context.home.join(".codex");
        let alias = f.root.join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        f.context.codex_home = Some(alias.clone());
        let mut entry = f.entry(alias.clone());
        entry.label = "Work".into();
        entry.enabled = false;
        let registry = discover_with(
            &InstanceConfig {
                entries: vec![entry],
                ..Default::default()
            },
            &f.context,
        )
        .unwrap();
        assert_eq!(registry.instances.len(), 1);
        assert_eq!(
            instance_id("local", &real).unwrap(),
            instance_id("local", &alias).unwrap()
        );
        assert_eq!(registry.instances[0].label, "Work");
        assert!(registry.resolve(None).is_err());
        assert!(registry.roots(true).is_empty());
        assert!(registry
            .instance_for_transcript(&real.join("sessions/file.jsonl"))
            .is_none());
    }

    #[test]
    fn selected_instance_never_falls_back_and_launch_sets_its_own_home() {
        let mut f = Fixture::new();
        let cli = f.root.join("app/Contents/Resources/codex");
        f.executable(&cli, "#!/bin/sh\nexit 0\n");
        f.context.cli_candidates.push(cli.clone());
        let second = f.root.join("personal");
        fs::create_dir_all(&second).unwrap();
        let entry = f.entry(second.clone());
        let id = instance_id("local", &second).unwrap();
        let cfg = InstanceConfig {
            entries: vec![entry],
            default_codex_instance: Some(id.clone()),
            ..Default::default()
        };
        let registry = discover_with(&cfg, &f.context).unwrap();
        let spec = registry.launch_spec(Some(&id)).unwrap();
        assert_eq!(spec.codex_home, canonical_home(&second).unwrap());
        assert_eq!(spec.program, cli);
        assert!(registry.launch_spec(Some("missing")).is_err());
        assert!(registry.resolve(Some("")).is_err());
        let mut disabled = cfg.clone();
        disabled.entries[0].enabled = false;
        assert!(discover_with(&disabled, &f.context).is_err());
        let mut missing = cfg;
        missing.default_codex_instance = Some("missing".into());
        assert!(discover_with(&missing, &f.context).is_err());
    }

    #[test]
    fn disabled_explicit_home_overrides_enabled_auto_discovery() {
        let f = Fixture::new();
        let mut entry = f.entry(f.context.home.join(".codex"));
        entry.enabled = false;
        let registry = discover_with(
            &InstanceConfig {
                entries: vec![entry],
                ..Default::default()
            },
            &f.context,
        )
        .unwrap();
        assert!(!registry.instances[0].enabled);
        assert!(registry.launch_spec(None).is_err());
    }

    #[test]
    fn unknown_wrapper_assignments_are_not_executed_or_misclassified_as_default_home() {
        let f = Fixture::new();
        let marker = f.root.join("never-created");
        let text = format!("#!/bin/sh\nCODEX_APP='/Applications/ChatGPT.app'\nexport CODEX_HOME=$(touch '{}'; echo /tmp/evil)\nexec open -a \"$CODEX_APP\"\n", marker.display());
        let path = f.root.join("bin/codex-evil");
        f.executable(&path, &text);
        assert!(read_wrapper(&path, &f.context).is_none());
        assert!(!marker.exists());
        assert_eq!(
            discover_with(&InstanceConfig::default(), &f.context)
                .unwrap()
                .instances
                .len(),
            1
        );
        for expression in [
            "$(id)",
            "`id`",
            "${HOME:-/tmp}",
            "${HOME/old/new}",
            "$UNKNOWN/x",
            "foo;bar",
            "foo && bar",
        ] {
            assert!(
                static_value(expression, &BTreeMap::new()).is_none(),
                "{expression}"
            );
        }
    }

    #[test]
    fn a_desktop_path_in_an_unused_assignment_does_not_identify_a_launcher() {
        let f = Fixture::new();
        let path = f.root.join("bin/codex-unrelated");
        f.executable(&path, "#!/bin/sh\nCODEX_APP='/Applications/ChatGPT.app'\nexec open -a '/Applications/Calculator.app'\n");
        assert!(read_wrapper(&path, &f.context).is_none());
    }

    #[test]
    fn explicit_entries_are_validated_before_atomic_persistence() {
        let f = Fixture::new();
        let data = f.root.join("data");
        let cfg = InstanceConfig {
            entries: vec![f.entry(f.root.join("personal"))],
            ..Default::default()
        };
        save_with(&data, &cfg, &f.context).unwrap();
        let before = fs::read(data.join(CONFIG_FILE)).unwrap();
        assert_eq!(load(&data).unwrap(), cfg);
        let mut bad = cfg.clone();
        bad.entries[0].home = PathBuf::from("relative");
        assert!(save_with(&data, &bad, &f.context).is_err());
        assert_eq!(fs::read(data.join(CONFIG_FILE)).unwrap(), before);
        bad = cfg.clone();
        bad.entries[0].label = "unsafe\nlabel".into();
        assert!(save_with(&data, &bad, &f.context).is_err());
        bad = cfg.clone();
        bad.entries.push(bad.entries[0].clone());
        assert!(save_with(&data, &bad, &f.context).is_err());
        assert_eq!(fs::read_dir(&data).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(data.join(CONFIG_FILE))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn malformed_or_future_config_is_reported_without_overwriting() {
        let f = Fixture::new();
        assert_eq!(load(&f.root).unwrap(), InstanceConfig::default());
        for text in ["{broken", "{\"version\":999}", "{\"unexpected\":true}"] {
            fs::write(f.root.join(CONFIG_FILE), text).unwrap();
            assert!(load(&f.root).is_err());
            assert_eq!(fs::read_to_string(f.root.join(CONFIG_FILE)).unwrap(), text);
        }
    }

    #[test]
    fn rollout_roots_and_identity_are_machine_and_directory_scoped() {
        let f = Fixture::new();
        let registry = discover_with(&InstanceConfig::default(), &f.context).unwrap();
        let home = &registry.instances[0].canonical_home;
        assert_ne!(
            instance_id("local", home).unwrap(),
            instance_id("vm-a", home).unwrap()
        );
        assert_eq!(registry.roots(false).len(), 1);
        assert_eq!(registry.roots(true).len(), 2);
        assert!(registry
            .instance_for_transcript(&home.join("sessions/day/chat.jsonl"))
            .is_some());
        assert!(registry
            .instance_for_transcript(&home.join("archived_sessions/chat.jsonl"))
            .is_some());
        assert!(registry
            .instance_for_transcript(&home.join("sessions-other/chat.jsonl"))
            .is_none());
        assert!(registry
            .instance_for_transcript(&home.join("auth.json"))
            .is_none());
    }

    #[test]
    #[cfg(unix)]
    fn canonical_missing_child_preserves_symlink_and_parent_semantics() {
        let f = Fixture::new();
        fs::create_dir_all(f.root.join("destination/nested")).unwrap();
        std::os::unix::fs::symlink(f.root.join("destination/nested"), f.root.join("link")).unwrap();
        let missing = f.root.join("link/../not-created/home");
        assert_eq!(
            canonical_home(&missing).unwrap(),
            canonical_home(&f.root.join("destination/not-created/home")).unwrap()
        );
    }

    #[test]
    fn jarvis_shims_and_nonexecutables_are_not_selected_as_cli() {
        let mut f = Fixture::new();
        let shims = f.root.join("shims");
        f.executable(&shims.join("codex"), "#!/bin/sh\nexit 0\n");
        fs::write(f.root.join("bin/codex"), "not executable").unwrap();
        f.context.path_dirs.insert(0, shims.clone());
        f.context.excluded_dirs.push(shims);
        assert!(discover_with(&InstanceConfig::default(), &f.context)
            .unwrap()
            .instances[0]
            .cli
            .is_none());
    }

    #[test]
    fn alternate_dev_shim_is_skipped_even_outside_the_current_jarvis_directory() {
        let mut f = Fixture::new();
        f.executable(
            &f.root.join("bin/codex"),
            "#!/bin/sh\n# jarvis agent shim — dev install\nexit 0\n",
        );
        let actual = f.root.join("app/codex");
        f.executable(&actual, "#!/bin/sh\nexit 0\n");
        f.context.cli_candidates.push(actual.clone());
        assert_eq!(
            discover_with(&InstanceConfig::default(), &f.context)
                .unwrap()
                .instances[0]
                .cli
                .as_ref(),
            Some(&actual)
        );
    }

    #[test]
    fn node_and_desktop_identity_algorithm_has_fixed_test_vectors() {
        let path = Path::new("/jarvis-instance-test/codex-home");
        assert_eq!(
            provider_instance_id("codex", "local", path).unwrap(),
            "codex-v1-145efd823f8f97d5"
        );
        assert_eq!(
            provider_instance_id("claude", "local", path).unwrap(),
            "claude-v1-145efd823f8f97d5"
        );
        assert_eq!(
            provider_instance_id("codex", "vm-a", path).unwrap(),
            "codex-v1-2f6cc7b5e26fd447"
        );
    }
}
