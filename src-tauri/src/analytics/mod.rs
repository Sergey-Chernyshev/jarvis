//! AI workflow analytics from local files and connected nodes, separate from billing.
//! Original prompts, tool arguments and source code never enter the saved report.
mod config;
mod outcomes;
mod remote;
mod repository;
mod trace;

use crate::daemon::Daemon;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

const MAX_FILES: usize = 200;
const MAX_SCAN_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PROJECTS: usize = 20;
const MAX_HEAD_BYTES: usize = 64 * 1024;
const MAX_HEAD_SCAN_BYTES: usize = 16 * 1024 * 1024;
const MAX_DISCOVERY_ENTRIES: usize = 100_000;

#[derive(Clone, Debug)]
struct SourceKind {
    id: String,
    format: String,
    namespace: String,
    configured: bool,
    label: String,
    machine: String,
    instance_id: Option<String>,
    provider_home: Option<String>,
}
impl SourceKind {
    fn builtin(format: &str) -> Self {
        Self {
            id: format.into(),
            format: format.into(),
            namespace: format.into(),
            configured: false,
            label: format.into(),
            machine: "local".into(),
            instance_id: None,
            provider_home: None,
        }
    }

    fn instance(instance: &crate::agent_instances::AgentInstance) -> Self {
        let legacy = instance
            .sources
            .contains(&crate::agent_instances::DiscoverySource::DefaultHome);
        Self {
            id: instance.id.clone(),
            format: "codex".into(),
            namespace: if legacy {
                "codex".into()
            } else {
                format!("source:{}", instance.id)
            },
            configured: false,
            label: instance.label.clone(),
            machine: instance.machine.clone(),
            instance_id: Some(instance.id.clone()),
            provider_home: Some(instance.canonical_home.to_string_lossy().into_owned()),
        }
    }
}
type Candidate = (PathBuf, SourceKind, u64, std::time::SystemTime);

#[derive(Clone, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct Options {
    period: String,
    project: Option<String>,
    refresh: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            period: "week".into(),
            project: None,
            refresh: false,
        }
    }
}

struct CachedTrace {
    len: u64,
    modified: std::time::SystemTime,
    data: Value,
}
#[derive(Default)]
struct Cache {
    files: HashMap<PathBuf, CachedTrace>,
    report: Option<(String, i64, Value)>,
    config_key: Option<String>,
}
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

pub async fn report(d: Arc<Daemon>, options: Value) -> Result<Value, String> {
    let parsed: Options = serde_json::from_value(options.clone()).map_err(|e| e.to_string())?;
    cutoff(&parsed.period, crate::util::now_ms())?;
    if let Some(project) = parsed.project.as_deref().filter(|p| remote::is_project(p)) {
        remote::validate_project(project)?;
    }
    let cfg = tokio::task::spawn_blocking(|| config::load(&crate::util::jarvis_dir()))
        .await
        .map_err(|e| e.to_string())??;
    let remote = remote::collect(d.remotes.all(), &parsed, &cfg).await;
    let hints: Vec<(String, String)> = d
        .snapshot()
        .into_iter()
        .filter(|s| s.remote.is_none())
        .filter_map(|s| Some((s.transcript?, s.agent.unwrap_or_else(|| "claude".into()))))
        .collect();
    tokio::task::spawn_blocking(move || build_report_with_remote(options, hints, remote))
        .await
        .map_err(|e| e.to_string())?
}

pub async fn remote_session_trace(d: &Daemon, session: &crate::model::Session) -> Value {
    remote::session_trace(d, session).await
}

pub async fn save_outcome(outcome: Value) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || {
        let result = outcomes::save(&crate::util::jarvis_dir(), outcome)?;
        if let Ok(mut c) = CACHE.get_or_init(Default::default).lock() {
            c.report = None;
        }
        Ok(result)
    })
    .await
    .map_err(|e| e.to_string())?
}

pub fn default_config() -> Value {
    config::defaults()
}

pub async fn get_config() -> Result<Value, String> {
    tokio::task::spawn_blocking(|| config::load(&crate::util::jarvis_dir()))
        .await
        .map_err(|e| e.to_string())?
}

pub async fn save_config(value: Value) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || {
        let value = config::save(&crate::util::jarvis_dir(), value)?;
        if let Ok(mut cache) = CACHE.get_or_init(Default::default).lock() {
            *cache = Cache::default();
        }
        Ok(json!({"ok":true,"config":value}))
    })
    .await
    .map_err(|e| e.to_string())?
}

fn settings_hash(value: &str) -> String {
    let hash = value.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    format!("config-v1-{hash:016x}")
}

fn walk(
    dir: &Path,
    depth: usize,
    source: &SourceKind,
    files: &mut BTreeMap<PathBuf, SourceKind>,
    errors: &mut Vec<String>,
    limited: &mut bool,
    visited: &mut usize,
) {
    if depth > 8 || files.len() >= 20_000 || *visited >= MAX_DISCOVERY_ENTRIES {
        *limited = true;
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            if errors.len() < 30 {
                errors.push(format!("{}: {e}", dir.display()));
            }
            return;
        }
    };
    for entry in entries {
        if *visited >= MAX_DISCOVERY_ENTRIES {
            *limited = true;
            break;
        }
        *visited += 1;
        let Ok(entry) = entry else {
            *limited = true;
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            *limited = true;
            continue;
        };
        let path = entry.path();
        // Never follow directory symlinks or inspect arbitrary external trees.
        if kind.is_dir() {
            walk(&path, depth + 1, source, files, errors, limited, visited);
        } else if kind.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
            if files.len() >= 20_000 {
                *limited = true;
                break;
            }
            files.entry(path).or_insert_with(|| source.clone());
        }
    }
}

fn discover_source(
    path: &Path,
    source: &SourceKind,
    files: &mut BTreeMap<PathBuf, SourceKind>,
    errors: &mut Vec<String>,
    limited: &mut bool,
    visited: &mut usize,
) {
    let resolved = match path.canonicalize() {
        Ok(path) => path,
        Err(error) => {
            if source.configured && errors.len() < 30 {
                errors.push(format!(
                    "Источник {} ({}): {error}",
                    source.id,
                    path.display()
                ));
            }
            return;
        }
    };
    match std::fs::metadata(&resolved) {
        Ok(metadata) if metadata.is_dir() => {
            walk(&resolved, 0, source, files, errors, limited, visited)
        }
        Ok(metadata)
            if metadata.is_file() && resolved.extension().is_some_and(|e| e == "jsonl") =>
        {
            if files.len() < 20_000 {
                files.entry(resolved).or_insert_with(|| source.clone());
            } else {
                *limited = true;
            }
        }
        Ok(_) => {
            if errors.len() < 30 {
                errors.push(format!(
                    "Источник {}: требуется каталог или файл .jsonl",
                    source.id
                ));
            }
        }
        Err(error) => {
            if errors.len() < 30 {
                errors.push(format!("Источник {}: {error}", source.id));
            }
        }
    }
}

fn cutoff(period: &str, now: i64) -> Result<i64, String> {
    Ok(match period {
        "all" => 0,
        "month" => now - 30 * 86_400_000,
        "week" => now - 7 * 86_400_000,
        "today" => chrono::Local::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .and_then(|d| d.and_local_timezone(chrono::Local).earliest())
            .map(|d| d.timestamp_millis())
            .unwrap_or(now - 86_400_000),
        _ => return Err("period: today, week, month или all".into()),
    })
}

fn canonical(path: &str) -> String {
    if path.is_empty() || remote::is_project(path) {
        return path.to_owned();
    }
    Path::new(path)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(path))
        .to_string_lossy()
        .into_owned()
}

fn head_cwd(path: &Path, agent: &str, limit: usize) -> std::io::Result<(Option<String>, usize)> {
    let mut bytes = Vec::new();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Trace is not a regular file",
        ));
    }
    file.take(limit as u64).read_to_end(&mut bytes)?;
    for line in bytes.split(|b| *b == b'\n') {
        let Ok(entry) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let cwd = match agent {
            "claude" => entry["cwd"].as_str(),
            "codex" if entry["type"] == "session_meta" => entry["payload"]["cwd"].as_str(),
            "normalized" if entry["schemaVersion"] == 1 => entry["cwd"].as_str(),
            _ => None,
        };
        if let Some(cwd) = cwd.filter(|cwd| Path::new(cwd).is_absolute()) {
            return Ok((Some(canonical(cwd)), bytes.len()));
        }
    }
    Ok((None, bytes.len()))
}

fn prioritize_project(candidates: &mut [Candidate], filter: Option<&str>, cache: &Cache) -> Value {
    let mut matches = HashSet::new();
    let mut projects = BTreeMap::new();
    let mut bytes_read = 0usize;
    let mut heads_read = 0usize;
    let mut cached_hints = 0usize;
    let mut known = 0usize;
    let mut read_errors = 0usize;
    let mut limited = false;
    for (path, source, len, modified) in candidates.iter() {
        let cached_cwd = cache
            .files
            .get(path)
            .filter(|c| c.len == *len && c.modified == *modified)
            .and_then(|c| c.data["cwd"].as_str())
            .filter(|cwd| Path::new(cwd).is_absolute())
            .map(canonical);
        let cwd = if let Some(cwd) = cached_cwd {
            cached_hints += 1;
            Some(cwd)
        } else {
            let remaining = MAX_HEAD_SCAN_BYTES.saturating_sub(bytes_read);
            if remaining == 0 {
                limited = true;
                continue;
            }
            heads_read += 1;
            let limit = MAX_HEAD_BYTES.min(remaining);
            match head_cwd(path, &source.format, limit) {
                Ok((cwd, bytes)) => {
                    bytes_read += bytes;
                    cwd
                }
                // Reserve the attempted allowance on errors: read_to_end may
                // already have consumed bytes before an I/O failure.
                Err(_) => {
                    bytes_read += limit;
                    read_errors += 1;
                    None
                }
            }
        };
        if let Some(cwd) = cwd {
            known += 1;
            projects.entry(cwd.clone()).or_insert_with(|| {
                json!({"path":cwd,
                "name":crate::util::basename(&cwd),"basis":"trace-cwd"})
            });
            // This is priority, not exclusion: a nested repository or a later cwd
            // change is still resolved by the full trace and Git-root checks.
            if filter.is_some_and(|filter| Path::new(&cwd).starts_with(Path::new(filter))) {
                matches.insert(path.clone());
            }
        }
    }
    candidates.sort_by(|a, b| {
        matches
            .contains(&b.0)
            .cmp(&matches.contains(&a.0))
            .then(b.3.cmp(&a.3))
            .then(a.0.cmp(&b.0))
    });
    json!({"enabled":filter.is_some(),"headsRead":heads_read,"cachedHints":cached_hints,
        "knownCwd":known,"unknownHeads":candidates.len()-known,"prioritizedFiles":matches.len(),
        "bytesBudgetUsed":bytes_read,"readErrors":read_errors,"limited":limited,
        "maxHeadBytes":MAX_HEAD_BYTES,"maxScanBytes":MAX_HEAD_SCAN_BYTES,
        "discoveredProjects":projects.into_values().collect::<Vec<_>>(),
        "caveat":"Перед настраиваемым лимитом файлов приоритет получают известные рабочие папки выбранного проекта. Заголовки без cwd и непрочитанные заголовки остаются неизвестными и не отбрасываются; окончательный фильтр применяется к прочитанной сессии."})
}

fn build_report(options: Value, hints: Vec<(String, String)>) -> Result<Value, String> {
    build_report_with_remote(options, hints, remote::Batch::default())
}

fn build_report_with_remote(
    options: Value,
    hints: Vec<(String, String)>,
    remote: remote::Batch,
) -> Result<Value, String> {
    let mut roots = vec![(
        crate::util::claude_dir().join("projects"),
        SourceKind::builtin("claude"),
    )];
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        roots.push((
            PathBuf::from(dir).join("projects"),
            SourceKind::builtin("claude"),
        ));
    }
    let registry = crate::session_identity::registry()?;
    for root in registry.roots(true) {
        if let Ok(instance) = registry.resolve(Some(&root.instance_id)) {
            roots.push((root.path, SourceKind::instance(instance)));
        }
    }
    build_report_with_typed_sources(options, hints, roots, &crate::util::jarvis_dir(), remote)
}

fn build_report_with_sources(
    options: Value,
    hints: Vec<(String, String)>,
    roots: Vec<(PathBuf, &str)>,
    data_dir: &Path,
) -> Result<Value, String> {
    build_report_with_typed_sources(
        options,
        hints,
        roots
            .into_iter()
            .map(|(path, agent)| (path, SourceKind::builtin(agent)))
            .collect(),
        data_dir,
        remote::Batch::default(),
    )
}

fn build_report_with_typed_sources(
    options: Value,
    hints: Vec<(String, String)>,
    roots: Vec<(PathBuf, SourceKind)>,
    data_dir: &Path,
    remote: remote::Batch,
) -> Result<Value, String> {
    let options: Options = serde_json::from_value(options).map_err(|e| e.to_string())?;
    let cfg = config::load(data_dir)?;
    let config_key = serde_json::to_string(&cfg).map_err(|e| e.to_string())?;
    if !remote.config_key.is_empty() && remote.config_key != settings_hash(&config_key) {
        return Err(
            "Настройки аналитики изменились во время чтения удалённых сессий. Обновите отчёт."
                .into(),
        );
    }
    let max_files = cfg["limits"]["maxFiles"]
        .as_u64()
        .unwrap_or(MAX_FILES as u64) as usize;
    let max_scan_bytes = cfg["limits"]["maxScanMiB"]
        .as_u64()
        .unwrap_or(MAX_SCAN_BYTES / 1024 / 1024)
        * 1024
        * 1024;
    let max_file_bytes = cfg["limits"]["maxFileMiB"].as_u64().unwrap_or(32) * 1024 * 1024;
    let max_projects = cfg["limits"]["maxProjects"]
        .as_u64()
        .unwrap_or(MAX_PROJECTS as u64) as usize;
    let auto_discover = cfg["autoDiscover"] == true;
    let now = crate::util::now_ms();
    let since = cutoff(&options.period, now)?;
    let filter = options
        .project
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .map(|path| {
            if remote::is_project(path) {
                remote::validate_project(path)
            } else {
                config::resolve_path(path).map(|path| canonical(&path.to_string_lossy()))
            }
        })
        .transpose()?;
    let remote_only = filter.as_deref().is_some_and(remote::is_project);
    let mut source_roots: Vec<(PathBuf, SourceKind)> = if auto_discover {
        roots.clone()
    } else {
        Vec::new()
    };
    for source in cfg["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|source| source["enabled"] == true)
    {
        let id = source["id"].as_str().unwrap_or_default();
        source_roots.push((
            config::resolve_path(source["path"].as_str().unwrap_or_default())?,
            SourceKind {
                id: id.into(),
                format: source["format"].as_str().unwrap_or_default().into(),
                namespace: format!("source:{id}"),
                configured: true,
                label: id.into(),
                machine: "local".into(),
                instance_id: None,
                provider_home: None,
            },
        ));
    }
    let key = format!(
        "{}:{filter:?}:{source_roots:?}:{}:{config_key}:{}",
        options.period,
        data_dir.display(),
        remote.cache_key()
    );
    // This lock belongs only to analytics worker threads, never the session reducer.
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "Кэш аналитики недоступен")?;
    if cache.config_key.as_deref() != Some(&config_key) {
        *cache = Cache::default();
        cache.config_key = Some(config_key.clone());
    }
    if !options.refresh {
        if let Some((cached_key, at, value)) = &cache.report {
            if *cached_key == key && now - *at < 30_000 {
                return Ok(value.clone());
            }
        }
    }
    let mut files = BTreeMap::new();
    let mut errors = remote.errors.clone();
    let mut limited = remote.limited;
    let mut visited = 0;
    // Explicit sources receive discovery budget first. Reverse configuration
    // order makes the last explicit overlapping source the stable winner.
    for (root, source) in source_roots
        .iter()
        .rev()
        .filter(|(_, source)| source.configured)
        .chain(source_roots.iter().filter(|(_, source)| !source.configured))
        .filter(|_| !remote_only)
    {
        discover_source(
            root,
            source,
            &mut files,
            &mut errors,
            &mut limited,
            &mut visited,
        );
    }
    for (file, agent) in hints.into_iter().filter(|_| auto_discover && !remote_only) {
        if matches!(agent.as_str(), "claude" | "codex") && Path::new(&file).is_absolute() {
            if files.len() >= 20_000 {
                limited = true;
                break;
            }
            // Explicit configuration takes precedence for overlapping roots.
            let path = PathBuf::from(file);
            if let Ok(path) = path.canonicalize() {
                // Codex hints must belong to an enabled registry source. A stale
                // live session must not reintroduce a disabled account.
                let source = source_roots
                    .iter()
                    .rev()
                    .find(|(root, source)| source.format == agent && path.starts_with(root))
                    .map(|(_, source)| source.clone());
                if agent == "codex" && source.is_none() {
                    continue;
                }
                files
                    .entry(path)
                    .or_insert_with(|| source.unwrap_or_else(|| SourceKind::builtin(&agent)));
            }
        }
    }
    // Canonicalization prevents aliases (e.g. CODEX_HOME and a hook path) doubling a trace.
    let mut seen = HashSet::new();
    let mut candidates: Vec<_> = files
        .into_iter()
        .filter_map(|(path, agent)| {
            let path = path.canonicalize().ok()?;
            if !seen.insert(path.clone()) {
                return None;
            }
            let metadata = std::fs::metadata(&path).ok()?;
            if !metadata.is_file() {
                return None;
            }
            let modified = metadata.modified().ok()?;
            Some((path, agent, metadata.len(), modified))
        })
        .collect();
    candidates.sort_by(|a, b| b.3.cmp(&a.3).then(a.0.cmp(&b.0)));
    let discovered = candidates.len() + remote.discovered;
    let project_selection = prioritize_project(&mut candidates, filter.as_deref(), &cache);
    if project_selection["limited"] == true
        || project_selection["readErrors"].as_u64().unwrap_or(0) > 0
    {
        limited = true;
    }
    let local_max_files = if remote_only {
        0
    } else {
        max_files.saturating_sub(remote.scanned.max(remote.attempted))
    };
    if candidates.len() > local_max_files {
        limited = true;
        candidates.truncate(local_max_files);
    }
    let keep: HashSet<_> = candidates.iter().map(|c| c.0.clone()).collect();
    cache.files.retain(|p, _| keep.contains(p));
    let mut bytes = remote.bytes;
    let mut scanned = remote.scanned;
    let mut skipped = 0usize;
    let mut sessions = Vec::new();
    let mut session_keys = HashSet::new();
    let mut evidence = Vec::new();
    let mut project_roots = HashMap::new();
    let mut consumed_files = HashSet::new();
    for (path, source, len, modified) in candidates {
        let cost = len.min(max_file_bytes);
        if bytes.saturating_add(cost) > max_scan_bytes {
            limited = true;
            break;
        }
        bytes = bytes.saturating_add(cost);
        let cached = cache
            .files
            .get(&path)
            .filter(|c| c.len == len && c.modified == modified);
        let mut session = if let Some(cached) = cached {
            cached.data.clone()
        } else {
            match trace::analyze_file_with_config(&path, &source.format, &cfg) {
                Ok(value) => {
                    cache.files.insert(
                        path.clone(),
                        CachedTrace {
                            len,
                            modified,
                            data: value.clone(),
                        },
                    );
                    value
                }
                Err(e) => {
                    if errors.len() < 30 {
                        errors.push(format!("{}: {e}", path.display()));
                    }
                    continue;
                }
            }
        };
        consumed_files.insert(path.clone());
        scanned += 1;
        if session["coverage"]["truncated"].as_bool() == Some(true) {
            limited = true;
        }
        // Period selects sessions by their final observed activity. Metrics retain
        // the entire selected session so tool pairing and token baselines survive.
        let last = session["lastAt"].as_i64();
        if since > 0 && last.map_or(true, |at| at < since) {
            skipped += 1;
            continue;
        }
        let cwd = canonical(session["cwd"].as_str().unwrap_or(""));
        let root = project_roots
            .entry(cwd.clone())
            .or_insert_with(|| repository::root(&cwd).unwrap_or_else(|| cwd.clone()))
            .clone();
        if filter.as_ref().is_some_and(|p| *p != root && *p != cwd) {
            continue;
        }
        let raw_id = session["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let id = format!("{}:{raw_id}", source.namespace);
        if !session_keys.insert(id.clone()) {
            skipped += 1;
            continue;
        }
        session["providerSessionId"] = json!(raw_id);
        session["id"] = json!(id);
        session["project"] = json!(root);
        session["sourceFile"] = json!(path);
        session["sourceId"] = json!(source.id);
        session["sourceNamespace"] = json!(source.namespace);
        session["sourceFormat"] = json!(source.format);
        session["sourceLabel"] = json!(source.label);
        session["machine"] = json!(source.machine);
        session["instanceId"] = json!(source.instance_id);
        session["providerHome"] = json!(source.provider_home);
        if let Some(edits) = session
            .as_object_mut()
            .and_then(|o| o.remove("editEvidence"))
            .and_then(|v| v.as_array().cloned())
        {
            for mut edit in edits {
                edit["cwd"] = json!(cwd);
                edit["project"] = json!(root);
                evidence.push(edit);
            }
        }
        sessions.push(session);
    }
    for session in &remote.sessions {
        let last = session["lastAt"]
            .as_i64()
            .unwrap_or(0)
            .max(session["coverage"]["manifestLastAt"].as_i64().unwrap_or(0));
        if since > 0 && last < since {
            skipped += 1;
            continue;
        }
        if filter
            .as_deref()
            .is_some_and(|filter| session["project"].as_str() != Some(filter))
        {
            continue;
        }
        let Some(id) = session["id"].as_str() else {
            continue;
        };
        if !session_keys.insert(id.to_owned()) {
            skipped += 1;
            continue;
        }
        sessions.push(session.clone());
    }
    // Bound the retained cache by this scan's byte budget as well as file count.
    // Otherwise older large parsed edits could accumulate as other traces grow.
    cache.files.retain(|path, _| consumed_files.contains(path));
    sessions.sort_by_key(|s| std::cmp::Reverse(s["lastAt"].as_i64().unwrap_or(0)));
    let mut project_counts: BTreeMap<String, usize> = BTreeMap::new();
    if let Some(path) = &filter {
        project_counts.insert(path.clone(), 0);
    }
    for session in &sessions {
        if let Some(p) = session["project"].as_str().filter(|p| !p.is_empty()) {
            *project_counts.entry(p.into()).or_default() += 1;
        }
    }
    let mut projects = Vec::new();
    for (index, (path, count)) in project_counts.into_iter().enumerate() {
        let git = if remote::is_project(&path) {
            json!({"ok":false,"source":"remote","status":"not-inspected",
                "error":"Удалённый Git не проверен; показана статистика трейсов с этого узла."})
        } else if index < max_projects {
            let edits: Vec<Value> = evidence
                .iter()
                .filter(|e| e["project"].as_str() == Some(&path))
                .cloned()
                .collect();
            repository::inspect_with_config(&path, &edits, &cfg)
        } else {
            limited = true;
            json!({"ok":false,"error":format!("Лимит {max_projects} репозиториев. Выберите конкретный проект.")})
        };
        if git["limited"] == true {
            limited = true;
        }
        projects.push(
            json!({"path":path,"name":if remote::is_project(&path){remote::project_name(&path)}else{crate::util::basename(&path)},"sessions":count,"git":git}),
        );
    }
    let all_outcomes = match outcomes::load(data_dir) {
        Ok(items) => items,
        Err(e) => {
            errors.push(e);
            vec![]
        }
    };
    let selected_outcomes: Vec<_> = all_outcomes
        .into_iter()
        .filter(|o| {
            o.recorded_at >= since
                && filter
                    .as_ref()
                    .map_or(true, |p| canonical(&o.project) == *p)
        })
        .collect();
    let summary = summarize(&sessions);
    let models = aggregate_models(&sessions);
    let mut discovered_projects: BTreeMap<String, Value> = project_selection["discoveredProjects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|project| Some((project["path"].as_str()?.into(), project.clone())))
        .collect();
    for project in &remote.projects {
        if let Some(path) = project["path"].as_str() {
            discovered_projects.insert(path.into(), project.clone());
        }
    }
    for project in &projects {
        if let Some(path) = project["path"].as_str() {
            discovered_projects.insert(
                path.into(),
                json!({"path":path,"name":project["name"],"basis":"selected-project"}),
            );
        }
    }
    if !errors.is_empty() {
        limited = true;
    }
    let coverage_roots:Vec<_>=source_roots.iter().map(|(p,s)|json!({"path":p,"agent":s.format,
        "format":s.format,"id":s.id,"namespace":s.namespace,"configured":s.configured,
        "label":s.label,"machine":s.machine,"instanceId":s.instance_id,"providerHome":s.provider_home}))
        .chain(remote.roots.iter().cloned()).collect();
    let report = json!({"schemaVersion":1,"generatedAt":now,"period":options.period,"since":since,
        "config":cfg,"settingsHash":settings_hash(&config_key),"sourceFormats":["claude","codex","normalized"],
        "toolAliasTargets":config::TOOL_ALIAS_TARGETS,"discoveredProjects":discovered_projects.into_values().collect::<Vec<_>>(),
        "scope":"whole-sessions-with-activity-in-period","summary":summary,"sessions":sessions,"projects":projects,
        "models":models,"outcomes":selected_outcomes,"economics":outcomes::economics_with_config(&selected_outcomes,&cfg),
        "coverage":{"filesDiscovered":discovered,"filesScanned":scanned,"sessions":session_keys.len(),
            "skippedSessions":skipped,"limited":limited,"errors":errors,"maxFiles":max_files,"maxScanBytes":max_scan_bytes,"projectSelection":project_selection,
            "discoveryEntries":visited,"maxDiscoveryEntries":MAX_DISCOVERY_ENTRIES,"bytesBudgetUsed":bytes,"remote":remote.coverage(),
            "roots":coverage_roots,
            "caveat":"Настроенные локальные источники и подключённые узлы. Период отбирает сессии по последней активности; метрики охватывают прочитанные записи этих сессий. Проценты покрытия относятся к прочитанным данным; общий объём незаписанной работы неизвестен."},
        "limitations":["Скор харнеса описывает наблюдаемый процесс, не качество кода или программиста.",
            "Активное время — сумма интервалов с ограничением простоя; параллельные сессии могут пересекаться. Это не рабочее время человека.",
            "Чат, токены и Git не доказывают экономию времени, бизнес-эффект или оптимальность модели.",
            "Для экономики нужны записанные результаты, время проверки и переделок, база сравнения и фактические затраты.",
            "Git-метрики показывают текущее рабочее дерево; Git AI — добавления HEAD. Они не фильтруются периодом трейсов."]});
    cache.report = Some((key, now, report.clone()));
    Ok(report)
}

fn number(v: &Value, path: &str) -> f64 {
    v.pointer(path)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite())
        .unwrap_or(0.0)
}

fn known_sum(values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    values
        .flatten()
        .filter(|n| n.is_finite())
        .fold(None, |sum, value| Some(sum.unwrap_or(0.0) + value))
}
fn summarize(sessions: &[Value]) -> Value {
    let sum = |path: &str| sessions.iter().map(|s| number(s, path)).sum::<f64>();
    let scores: Vec<f64> = sessions
        .iter()
        .filter_map(|s| s["harness"]["score"].as_f64())
        .collect();
    let first = sessions.iter().filter_map(|s| s["firstAt"].as_i64()).min();
    let last = sessions.iter().filter_map(|s| s["lastAt"].as_i64()).max();
    let models = aggregate_models(sessions);
    let active: Vec<f64> = sessions
        .iter()
        .filter_map(|s| s["timing"]["activeMs"].as_f64())
        .collect();
    let tokens = |key: &str| known_sum(models.iter().map(|m| m[key].as_f64()));
    json!({"sessions":sessions.len(),"prompts":sum("/prompts/count"),"toolCalls":sum("/tools/calls"),
        "toolErrors":sum("/tools/errors"),"toolUnknown":sum("/tools/unknown"),
        "wrapperCalls":sum("/tools/wrapperCalls"),"wrapperSuccess":sum("/tools/wrapperSuccess"),
        "wrapperErrors":sum("/tools/wrapperErrors"),"wrapperUnknown":sum("/tools/wrapperUnknown"),
        "eligibleCalls":sum("/tools/eligibleCalls"),"eligibleSuccess":sum("/tools/eligibleSuccess"),
        "eligibleErrors":sum("/tools/eligibleErrors"),"eligibleUnknown":sum("/tools/eligibleUnknown"),
        "activeMs":if active.is_empty(){None}else{Some(active.iter().sum::<f64>())},"timedSessions":active.len(),
        "wallMs":first.zip(last).map(|(a,b)|(b-a).max(0)),"harnessScore":if scores.is_empty(){None}else{Some(scores.iter().sum::<f64>()/scores.len() as f64)},
        "harnessScoredSessions":scores.len(),"inputTokens":tokens("inputTokens"),"outputTokens":tokens("outputTokens"),
        "cacheReadTokens":tokens("cacheReadTokens"),"cacheWriteTokens":tokens("cacheWriteTokens"),
        "tokenRecords":tokens("tokenRecords"),"tokenCoverage":"observed-subtotal; missing usage is not zero",
        "harnessAggregation":"mean-of-scored-sessions"})
}

fn aggregate_models(sessions: &[Value]) -> Vec<Value> {
    let mut models: BTreeMap<String, Value> = BTreeMap::new();
    for session in sessions {
        if let Some(rows) = session["models"].as_array() {
            let mut seen = HashSet::new();
            for row in rows {
                let name = row["model"].as_str().unwrap_or("unknown");
                let aggregate = models
                    .entry(name.into())
                    .or_insert_with(|| json!({"model":name,"sessions":0,"sessionsWithUsage":0,
                        "missingUsageRequests":0,"knownMissingUsageRequests":0,"missingUsageUnknownSessions":0}));
                if seen.insert(name) {
                    aggregate["sessions"] = json!(aggregate["sessions"].as_u64().unwrap_or(0) + 1);
                    if row["usageObserved"] == true
                        || row["inputTokens"].is_number()
                        || row["outputTokens"].is_number()
                    {
                        aggregate["sessionsWithUsage"] =
                            json!(aggregate["sessionsWithUsage"].as_u64().unwrap_or(0) + 1);
                    }
                }
                for key in [
                    "requests",
                    "tokenRecords",
                    "requestsWithUsage",
                    "toolCalls",
                    "toolErrors",
                    "toolUnknown",
                ] {
                    aggregate[key] = json!(
                        aggregate[key].as_f64().unwrap_or(0.0) + row[key].as_f64().unwrap_or(0.0)
                    );
                }
                for key in [
                    "inputTokens",
                    "outputTokens",
                    "cacheReadTokens",
                    "cacheWriteTokens",
                    "reasoningTokens",
                ] {
                    aggregate[key] = json!(known_sum(
                        [aggregate[key].as_f64(), row[key].as_f64()].into_iter()
                    ));
                }
                if let Some(missing) = row["missingUsageRequests"].as_u64() {
                    aggregate["knownMissingUsageRequests"] = json!(
                        aggregate["knownMissingUsageRequests"].as_u64().unwrap_or(0) + missing
                    );
                    if let Some(total) = aggregate["missingUsageRequests"].as_u64() {
                        aggregate["missingUsageRequests"] = json!(total + missing);
                    }
                } else {
                    aggregate["missingUsageRequests"] = Value::Null;
                    aggregate["missingUsageUnknownSessions"] = json!(
                        aggregate["missingUsageUnknownSessions"]
                            .as_u64()
                            .unwrap_or(0)
                            + 1
                    );
                }
            }
        }
    }
    for aggregate in models.values_mut() {
        let observed = aggregate["sessionsWithUsage"].as_u64().unwrap_or(0);
        aggregate["usageObserved"] = json!(observed > 0);
        aggregate["tokenCoverage"] = json!(if observed == 0 {
            "unknown"
        } else if aggregate["knownMissingUsageRequests"].as_u64().unwrap_or(0) > 0
            || aggregate["sessions"].as_u64().unwrap_or(0) > observed
        {
            "partial"
        } else if aggregate["missingUsageRequests"].is_null() {
            "observed-records-only"
        } else {
            "complete-observed-requests"
        });
    }
    models.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_instances_keep_same_sid_separate_and_default_legacy_namespace() {
        let dir = std::env::temp_dir().join(format!(
            "jarvis-analytics-instances-{}-{}",
            std::process::id(),
            crate::util::now_ms()
        ));
        let mut roots = Vec::new();
        for (id, label, legacy) in [
            ("work-id", "Codex Work", true),
            ("personal-id", "Codex Personal", false),
        ] {
            let home = dir.join(id);
            let traces = home.join("sessions");
            std::fs::create_dir_all(&traces).unwrap();
            let instance = crate::agent_instances::AgentInstance {
                id: id.into(),
                agent: "codex".into(),
                machine: "local".into(),
                home: home.clone(),
                canonical_home: home.clone(),
                label: label.into(),
                enabled: true,
                exists: true,
                sources: if legacy {
                    vec![crate::agent_instances::DiscoverySource::DefaultHome]
                } else {
                    vec![]
                },
                launchers: vec![],
                cli: None,
            };
            std::fs::write(traces.join("same.jsonl"),format!("{}\n",json!({"type":"session_meta","timestamp":"2026-09-05T00:00:00Z","payload":{"id":"same","cwd":dir}}))).unwrap();
            roots.push((traces, SourceKind::instance(&instance)));
        }
        let report = build_report_with_typed_sources(
            json!({"period":"all","refresh":true}),
            vec![],
            roots,
            &dir,
            remote::Batch::default(),
        )
        .unwrap();
        let rows = report["sessions"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .any(|s| s["id"] == "codex:same" && s["sourceLabel"] == "Codex Work"));
        assert!(rows
            .iter()
            .any(|s| s["id"] == "source:personal-id:same" && s["instanceId"] == "personal-id"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn remote_report_keeps_machine_projects_and_never_inspects_local_git_at_guest_cwd() {
        let dir = std::env::temp_dir().join(format!(
            "jarvis-analytics-remote-{}-{}",
            std::process::id(),
            crate::util::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let guest_cwd = dir.to_string_lossy().into_owned();
        let project = format!("remote://vm-a{guest_cwd}");
        let batch = remote::Batch {
            sessions: vec![
                json!({"id":"source:remote:vm-a:codex-v1-a:sid","cwd":guest_cwd,
            "project":project,"machine":"vm-a","remote":true,"lastAt":100,"coverage":{"manifestLastAt":100},
            "models":[{"model":"model","usageObserved":true,"inputTokens":12,"outputTokens":3}]}),
            ],
            scanned: 1,
            discovered: 1,
            bytes: 100,
            ..Default::default()
        };
        let report = build_report_with_typed_sources(
            json!({"period":"all","project":project,"refresh":true}),
            vec![],
            vec![],
            &dir,
            batch.clone(),
        )
        .unwrap();
        assert_eq!(report["summary"]["sessions"], 1);
        assert_eq!(report["summary"]["inputTokens"], 12.0);
        assert_eq!(report["projects"][0]["git"]["status"], "not-inspected");
        assert_eq!(report["projects"][0]["path"], project);
        assert_eq!(report["coverage"]["remote"]["filesScanned"], 1);
        let local = build_report_with_typed_sources(
            json!({"period":"all","project":guest_cwd,"refresh":true}),
            vec![],
            vec![],
            &dir,
            batch,
        )
        .unwrap();
        assert_eq!(local["summary"]["sessions"], 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn remote_and_local_reports_share_file_budget() {
        let dir = std::env::temp_dir().join(format!(
            "jarvis-analytics-budget-{}-{}",
            std::process::id(),
            crate::util::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        config::save(&dir, json!({"limits":{"maxFiles":1}})).unwrap();
        std::fs::write(
            dir.join("local.jsonl"),
            format!(
                "{}\n",
                json!({"type":"session_meta","payload":{"id":"local","cwd":dir}})
            ),
        )
        .unwrap();
        let batch = remote::Batch {
            sessions: vec![
                json!({"id":"source:remote:vm-a:codex-v1-a:sid","project":"remote://vm-a/repo"}),
            ],
            scanned: 1,
            discovered: 1,
            bytes: 100,
            ..Default::default()
        };
        let report = build_report_with_typed_sources(
            json!({"period":"all","refresh":true}),
            vec![],
            vec![(dir.clone(), SourceKind::builtin("codex"))],
            &dir,
            batch,
        )
        .unwrap();
        assert_eq!(report["summary"]["sessions"], 1);
        assert_eq!(report["coverage"]["filesScanned"], 1);
        assert_eq!(report["coverage"]["limited"], true);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn remote_config_race_is_rejected_instead_of_mixing_rules_in_one_report() {
        let dir = std::env::temp_dir().join(format!(
            "jarvis-analytics-race-{}-{}",
            std::process::id(),
            crate::util::now_ms()
        ));
        let batch = remote::Batch {
            config_key: "different-rules".into(),
            ..Default::default()
        };
        let error =
            build_report_with_typed_sources(json!({"period":"all"}), vec![], vec![], &dir, batch)
                .unwrap_err();
        assert!(error.contains("Настройки аналитики изменились"));
    }

    #[test]
    fn usage_aggregation_preserves_unknown_and_known_subtotals() {
        let missing = json!({"models":[{"model":"m","requests":1,"tokenRecords":0,"requestsWithUsage":0,
            "missingUsageRequests":1,"usageObserved":false,"inputTokens":null,"outputTokens":null,"cacheReadTokens":null} ]});
        let known = json!({"models":[{"model":"m","requests":1,"tokenRecords":1,"requestsWithUsage":1,
            "missingUsageRequests":0,"usageObserved":true,"inputTokens":0,"outputTokens":12,"cacheReadTokens":0} ]});
        let unknown = aggregate_models(&[missing.clone()]);
        assert!(unknown[0]["inputTokens"].is_null());
        assert!(summarize(&[missing.clone()])["outputTokens"].is_null());
        let combined = aggregate_models(&[missing, known]);
        assert_eq!(combined[0]["inputTokens"], 0.0);
        assert_eq!(combined[0]["outputTokens"], 12.0);
        assert_eq!(combined[0]["tokenRecords"], 1.0);
        assert_eq!(combined[0]["missingUsageRequests"], 1);
        assert_eq!(combined[0]["tokenCoverage"], "partial");
        let codex = json!({"models":[{"model":"m","tokenRecords":1,"usageObserved":true,"inputTokens":8,
            "outputTokens":2,"missingUsageRequests":null}]});
        let merged = aggregate_models(&[json!({"models":combined}), codex]);
        assert!(merged[0]["missingUsageRequests"].is_null());
        assert_eq!(merged[0]["inputTokens"], 8.0);
    }

    #[test]
    fn project_priority_finds_older_trace_beyond_global_file_cap() {
        let dir = std::env::temp_dir().join(format!(
            "jarvis-project-priority-{}-{}",
            std::process::id(),
            crate::util::now_ms()
        ));
        let traces = dir.join("traces");
        let project = dir.join("wanted");
        let other = dir.join("other");
        for path in [&traces, &project, &other] {
            std::fs::create_dir_all(path).unwrap();
        }
        let timestamp = chrono::Utc::now().to_rfc3339();
        let old = traces.join("wanted.jsonl");
        std::fs::write(
            &old,
            format!(
                "{}\n",
                json!({"type":"session_meta","timestamp":timestamp,
            "payload":{"id":"wanted","cwd":project}})
            ),
        )
        .unwrap();
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1)),
            )
            .unwrap();
        for index in 0..MAX_FILES + 5 {
            std::fs::write(
                traces.join(format!("other-{index}.jsonl")),
                format!(
                    "{}\n",
                    json!({"type":"session_meta",
                "timestamp":timestamp,"payload":{"id":format!("other-{index}"),"cwd":other}})
                ),
            )
            .unwrap();
        }
        let report = build_report_with_sources(
            json!({"period":"all","project":project,"refresh":true}),
            vec![],
            vec![(traces, "codex")],
            &dir,
        )
        .unwrap();
        assert_eq!(report["coverage"]["filesDiscovered"], MAX_FILES + 6);
        assert_eq!(report["coverage"]["filesScanned"], MAX_FILES);
        assert_eq!(
            report["coverage"]["projectSelection"]["prioritizedFiles"],
            1
        );
        assert_eq!(report["coverage"]["projectSelection"]["unknownHeads"], 0);
        assert_eq!(report["summary"]["sessions"], 1);
        assert_eq!(report["sessions"][0]["id"], "codex:wanted");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn project_priority_retains_unknown_heads_without_excluding_them() {
        let dir = std::env::temp_dir().join(format!(
            "jarvis-project-unknown-{}-{}",
            std::process::id(),
            crate::util::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let unknown = dir.join("unknown.jsonl");
        let known = dir.join("known.jsonl");
        std::fs::write(&unknown, b"{\"type\":\"response_item\",\"payload\":{}}\n").unwrap();
        std::fs::write(
            &known,
            format!("{}\n", json!({"type":"session_meta","payload":{"cwd":dir}})),
        )
        .unwrap();
        let now = std::time::SystemTime::now();
        let mut candidates = vec![
            (unknown.clone(), SourceKind::builtin("codex"), 0, now),
            (
                known.clone(),
                SourceKind::builtin("codex"),
                0,
                std::time::UNIX_EPOCH,
            ),
        ];
        let selection = prioritize_project(
            &mut candidates,
            Some(&canonical(dir.to_str().unwrap())),
            &Cache::default(),
        );
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].0, known);
        assert_eq!(candidates[1].0, unknown);
        assert_eq!(selection["unknownHeads"], 1);
        assert_eq!(selection["prioritizedFiles"], 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn no_trace_is_not_a_perfect_harness() {
        let value = summarize(&[]);
        assert!(value["harnessScore"].is_null());
        assert!(value["wallMs"].is_null());
    }
    #[test]
    fn configured_sources_disable_discovery_namespace_sessions_and_invalidate_cache() {
        let dir = std::env::temp_dir().join(format!(
            "jarvis-custom-sources-{}-{}",
            std::process::id(),
            crate::util::now_ms()
        ));
        let first = dir.join("first");
        let second = dir.join("second");
        let builtin = dir.join("builtin");
        for path in [&first, &second, &builtin] {
            std::fs::create_dir_all(path).unwrap();
            std::fs::write(
                path.join("trace.jsonl"),
                format!(
                    "{}\n",
                    json!({"type":"session_meta",
                "payload":{"id":"shared-id","cwd":path}})
                ),
            )
            .unwrap();
        }
        let mut settings = config::save(
            &dir,
            json!({"autoDiscover":false,"sources":[
            {"id":"first","format":"codex","path":first},
            {"id":"second","format":"codex","path":second.join("trace.jsonl")}]}),
        )
        .unwrap();
        let hints = vec![(
            builtin.join("trace.jsonl").to_string_lossy().into_owned(),
            "codex".into(),
        )];
        let sources = vec![(builtin, "codex")];
        let report = build_report_with_sources(
            json!({"period":"all"}),
            hints.clone(),
            sources.clone(),
            &dir,
        )
        .unwrap();
        assert_eq!(report["coverage"]["filesDiscovered"], 2);
        assert_eq!(report["summary"]["sessions"], 2);
        let ids: HashSet<_> = report["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains("source:first:shared-id"));
        assert!(ids.contains("source:second:shared-id"));
        assert_eq!(report["config"]["autoDiscover"], false);
        assert!(report["discoveredProjects"].as_array().unwrap().len() >= 2);
        settings["sources"][1]["enabled"] = json!(false);
        settings["rules"]["idleCapMinutes"] = json!(1);
        config::save(&dir, settings).unwrap();
        let changed =
            build_report_with_sources(json!({"period":"all"}), hints, sources, &dir).unwrap();
        assert_eq!(changed["summary"]["sessions"], 1);
        assert_ne!(changed["settingsHash"], report["settingsHash"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn explicit_missing_source_is_visible_and_same_file_is_only_counted_once() {
        let dir = std::env::temp_dir().join(format!(
            "jarvis-source-overlap-{}-{}",
            std::process::id(),
            crate::util::now_ms()
        ));
        let traces = dir.join("traces");
        std::fs::create_dir_all(&traces).unwrap();
        let file = traces.join("trace.jsonl");
        std::fs::write(
            &file,
            format!(
                "{}\n",
                json!({"type":"session_meta","payload":{"id":"one","cwd":dir}})
            ),
        )
        .unwrap();
        config::save(
            &dir,
            json!({"autoDiscover":false,"sources":[
            {"id":"dir","format":"codex","path":traces},
            {"id":"file","format":"codex","path":file},
            {"id":"missing","format":"normalized","path":dir.join("missing.jsonl")}]}),
        )
        .unwrap();
        let report =
            build_report_with_sources(json!({"period":"all"}), vec![], vec![], &dir).unwrap();
        assert_eq!(report["coverage"]["filesScanned"], 1);
        assert_eq!(report["sessions"][0]["sourceId"], "file");
        assert_eq!(report["coverage"]["limited"], true);
        assert!(report["coverage"]["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("missing")));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn zero_score_is_included_and_parallel_time_not_a_speedup() {
        let rows = vec![
            json!({"firstAt":0,"lastAt":100,"timing":{"activeMs":100},"harness":{"score":0}}),
            json!({"firstAt":50,"lastAt":150,"timing":{"activeMs":100},"harness":{"score":100}}),
        ];
        let value = summarize(&rows);
        assert_eq!(value["harnessScore"], 50.0);
        assert_eq!(value["activeMs"], 200.0);
        assert_eq!(value["wallMs"], 150);
    }
    #[test]
    fn period_options_are_validated() {
        assert!(cutoff("forever", 100).is_err());
        assert_eq!(cutoff("all", 100), Ok(0));
    }

    #[test]
    fn full_report_deduplicates_aliases_strips_code_and_respects_project_filter() {
        let dir = std::env::temp_dir().join(format!(
            "jarvis-analytics-integration-{}-{}",
            std::process::id(),
            crate::util::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let timestamp = chrono::Utc::now().to_rfc3339();
        let rows = [
            json!({"type":"session_meta","timestamp":timestamp,"payload":{"id":"fixture","cwd":dir,"type":"session_meta"}}),
            json!({"type":"response_item","timestamp":timestamp,"payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"PRIVATE_FIXTURE_PROMPT"}]}}),
            json!({"type":"response_item","timestamp":timestamp,"payload":{"type":"custom_tool_call","name":"apply_patch","call_id":"edit","input":"*** Begin Patch\n*** Add File: sample.rs\n+PRIVATE_FIXTURE_CODE\n*** End Patch"}}),
            json!({"type":"response_item","timestamp":timestamp,"payload":{"type":"custom_tool_call_output","call_id":"edit","output":"Success. Updated the following files:\nA sample.rs"}}),
        ];
        let trace = dir.join("sample.jsonl");
        std::fs::write(
            &trace,
            rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
        )
        .unwrap();
        let sources = vec![(dir.clone(), "codex")];
        let report = build_report_with_sources(
            json!({"period":"week","refresh":true}),
            vec![(trace.to_string_lossy().into(), "codex".into())],
            sources.clone(),
            &dir,
        )
        .unwrap();
        assert_eq!(report["summary"]["sessions"], 1);
        assert_eq!(report["coverage"]["filesScanned"], 1);
        assert_eq!(report["sessions"][0]["id"], "codex:fixture");
        let serialized = report.to_string();
        assert!(!serialized.contains("PRIVATE_FIXTURE_PROMPT"));
        assert!(!serialized.contains("PRIVATE_FIXTURE_CODE"));
        assert!(!serialized.contains("editEvidence"));
        let filtered = build_report_with_sources(
            json!({"period":"all","project":"/not-this-project","refresh":true}),
            vec![],
            sources,
            &dir,
        )
        .unwrap();
        assert_eq!(filtered["summary"]["sessions"], 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "manual read-only smoke against local user transcripts"]
    fn local_machine_report_smoke() {
        let report = build_report(json!({"period":"week","refresh":true}), vec![]).unwrap();
        assert_eq!(report["schemaVersion"], 1);
        assert!(!report.to_string().contains("\"editEvidence\""));
        println!(
            "{}",
            json!({"summary":report["summary"],"filesScanned":report["coverage"]["filesScanned"],
            "limited":report["coverage"]["limited"],"errors":report["coverage"]["errors"].as_array().map(Vec::len),
            "models":report["models"].as_array().map(Vec::len),"projects":report["projects"].as_array().map(Vec::len)})
        );
    }
}
