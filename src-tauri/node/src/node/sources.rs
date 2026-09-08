//! Provider homes and bounded transcript metadata. Never read credentials or
//! infer a provider home from an arbitrary hook-supplied filesystem path.
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const MAX_SOURCES: usize = 32;
const MAX_FILES: usize = 12_000;
const MAX_ENTRIES: usize = 40_000;
const MAX_SESSIONS: usize = 4_000;
const HEADER_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug)]
pub struct Source { pub id: String, pub agent: String, pub home: PathBuf }
impl Source {
    pub fn json(&self) -> Value { json!({"id":self.id,"instanceId":self.id,"agent":self.agent,"label":self.label(),"providerHome":self.home,"available":self.home.is_dir()}) }
    fn label(&self) -> String {
        let provider = if self.agent == "codex" { "Codex" } else { "Claude" };
        let basename = self.home.file_name().and_then(|name| name.to_str()).unwrap_or("");
        let conventional = format!(".{}", self.agent);
        if basename == conventional { return provider.into(); }
        let named = basename.strip_prefix(&format!("{conventional}-")).unwrap_or(basename).trim_start_matches('.');
        if named.is_empty() { provider.into() } else { format!("{provider} · {named}") }
    }
    pub fn roots(&self) -> Vec<PathBuf> {
        if self.agent == "codex" { vec![self.home.join("sessions"), self.home.join("archived_sessions")] }
        else { vec![self.home.join("projects")] }
    }
}
fn stable_id(agent: &str, home: &Path) -> String {
    let key = format!("local\0{}", home.display());
    let hash = key.as_bytes().iter().fold(0xcbf29ce484222325u64, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3));
    format!("{agent}-v1-{hash:016x}")
}
fn source(agent: &str, path: &Path) -> Option<Source> {
    if !matches!(agent, "claude" | "codex") || !path.is_absolute() || path.parent().is_none() || path.to_string_lossy().chars().any(char::is_control) { return None; }
    let home = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if home.parent().is_none() { return None; }
    Some(Source { id: stable_id(agent, &home), agent: agent.into(), home })
}
/// A private manifest is installed beside node.sock. Only explicit homes and
/// conventional direct children of HOME are discovered; no whole-disk search.
pub fn discover(home: &Path, jarvis: &Path) -> Vec<Source> {
    let mut explicit = Vec::new();
    for (agent, key) in [("claude", "CLAUDE_CONFIG_DIR"), ("codex", "CODEX_HOME")] {
        if let Some(path) = std::env::var_os(key).filter(|s| !s.is_empty()) { explicit.push((agent.into(), PathBuf::from(path))); }
    }
    discover_with_explicit(home, jarvis, explicit)
}
// These names commonly hold provider backups, not independent accounts. Match
// complete separator-delimited components, never substrings such as "archiver"
// or "backupworks". Explicit environment/manifest homes bypass this heuristic.
fn conventional_agent(name: &str) -> Option<&'static str> {
    let (agent, suffix) = name.strip_prefix(".codex-").map(|suffix| ("codex", suffix))
        .or_else(|| name.strip_prefix(".claude-").map(|suffix| ("claude", suffix)))?;
    if suffix.is_empty() || suffix.split(['-', '_', '.']).any(|part| {
        matches!(part.to_ascii_lowercase().as_str(), "backup" | "backups" | "bak")
    }) { return None; }
    Some(agent)
}
fn discover_with_explicit(home: &Path, jarvis: &Path, explicit: Vec<(String, PathBuf)>) -> Vec<Source> {
    let mut candidates = vec![("claude".to_string(), home.join(".claude")), ("codex".to_string(), home.join(".codex"))];
    candidates.extend(explicit);
    // Additional instances are conventional homes, not user data crawls.
    if let Ok(entries) = std::fs::read_dir(home) {
        for entry in entries.flatten().take(2048) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(agent) = conventional_agent(&name) else { continue };
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) { candidates.push((agent.into(), entry.path())); }
        }
    }
    let manifest = jarvis.join("provider-roots.json");
    if std::fs::metadata(&manifest).is_ok_and(|meta| meta.is_file() && meta.len() <= 256 * 1024) {
        if let Ok(value) = std::fs::read(&manifest).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()).ok_or(()) {
            for row in value.get("sources").and_then(Value::as_array).into_iter().flatten().take(MAX_SOURCES) {
                if let (Some(agent), Some(path)) = (row["agent"].as_str(), row["providerHome"].as_str()) { candidates.push((agent.into(), PathBuf::from(path))); }
            }
        }
    }
    sources_from(candidates)
}
fn sources_from(candidates: Vec<(String, PathBuf)>) -> Vec<Source> {
    let mut out = BTreeMap::new();
    for (agent, path) in candidates {
        if let Some(source) = source(&agent, &path) { out.entry(source.id.clone()).or_insert(source); }
    }
    out.into_values().take(MAX_SOURCES).collect()
}
fn walk(dir: &Path, depth: usize, files: &mut Vec<PathBuf>, visited: &mut HashSet<PathBuf>, budget: &mut usize, limited: &mut bool) {
    if depth > 9 || files.len() >= MAX_FILES || *budget >= MAX_ENTRIES { *limited = true; return; }
    let Ok(real) = std::fs::canonicalize(dir) else { return };
    if !visited.insert(real) { return; }
    let Ok(entries) = std::fs::read_dir(dir) else { return; };
    for entry in entries.flatten() {
        *budget += 1;
        if *budget > MAX_ENTRIES || files.len() >= MAX_FILES { *limited = true; break; }
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() { walk(&entry.path(), depth + 1, files, visited, budget, limited); }
        else if kind.is_file() && entry.path().extension().is_some_and(|x| x == "jsonl") { files.push(entry.path()); }
    }
}
fn metadata(path: &Path, source: &Source) -> Option<Value> {
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(HEADER_BYTES).read_to_end(&mut bytes).ok()?;
    let header = String::from_utf8_lossy(&bytes);
    let meta = std::fs::metadata(path).ok()?;
    use std::os::unix::fs::MetadataExt;
    let file_id = format!("{}:{}", meta.dev(), meta.ino());
    let at = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as u64;
    let mut sid = None; let mut cwd = None;
    for line in header.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else { continue };
        let value = if source.agent == "codex" {
            if value["type"] != "session_meta" { continue; } value.get("payload")?
        } else { &value };
        sid = value.get(if source.agent == "codex" { "id" } else { "sessionId" }).and_then(Value::as_str).map(str::to_string).or(sid);
        cwd = value["cwd"].as_str().filter(|path| path.starts_with('/')).map(str::to_string).or(cwd);
        if sid.is_some() && cwd.is_some() { break; }
    }
    let id = sid.or_else(|| if source.agent == "claude" { path.file_stem()?.to_str().map(str::to_string) } else { None })?;
    if id.trim().is_empty() || id.len() > 256 { return None; }
    Some(json!({"id":id,"agent":source.agent,"instanceId":source.id,"sourceId":source.id,
        "providerHome":source.home,"cwd":cwd,"path":path,"at":at,"size":meta.len(),"fileId":file_id}))
}
pub fn sessions(sources: &[Source]) -> Value {
    let mut sessions = vec![]; let mut limited = false; let mut budget = 0;
    let mut seen = HashSet::new();
    for source in sources {
        let mut files = vec![];
        for root in source.roots() { walk(&root, 0, &mut files, &mut seen, &mut budget, &mut limited); }
        for file in files { if let Some(row) = metadata(&file, source) { sessions.push(row); } }
    }
    sessions.sort_by_key(|row| std::cmp::Reverse(row["at"].as_u64().unwrap_or(0)));
    let mut ids = HashSet::new();
    sessions.retain(|row| ids.insert((row["sourceId"].as_str().unwrap_or("").to_string(), row["id"].as_str().unwrap_or("").to_string())));
    if sessions.len() > MAX_SESSIONS { sessions.truncate(MAX_SESSIONS); limited = true; }
    json!({"sessions":sessions,"limited":limited,"errors":[],"protocol":2})
}
pub fn projects(sources: &[Source]) -> Value {
    let report = sessions(sources);
    let mut groups: BTreeMap<(String, String), Value> = BTreeMap::new();
    for row in report["sessions"].as_array().into_iter().flatten() {
        let Some(cwd) = row["cwd"].as_str() else { continue; };
        let key = (row["sourceId"].as_str().unwrap_or("").to_string(), cwd.to_string());
        let group = groups.entry(key).or_insert_with(|| json!({"cwd":cwd,"agent":row["agent"],"sourceId":row["sourceId"],"instanceId":row["instanceId"],"providerHome":row["providerHome"],"count":0,"lastAt":0,"sessions":[]}));
        group["count"] = json!(group["count"].as_u64().unwrap_or(0) + 1);
        group["lastAt"] = json!(group["lastAt"].as_u64().unwrap_or(0).max(row["at"].as_u64().unwrap_or(0)));
        if group["sessions"].as_array().unwrap().len() < 50 { group["sessions"].as_array_mut().unwrap().push(row.clone()); }
    }
    let mut projects: Vec<_> = groups.into_values().collect();
    projects.sort_by_key(|row| std::cmp::Reverse(row["lastAt"].as_u64().unwrap_or(0)));
    json!({"projects":projects,"limited":report["limited"]})
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("jarvis-node-discovery-{}-{unique}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

    #[test]
    fn conventional_names_exclude_backup_components_only() {
        for name in [".claude-backups", ".codex-backup-2026-09-05", ".claude-work.BACKUP", ".codex-personal_bak", ".claude-work-BackUps-2026"] {
            assert_eq!(conventional_agent(name), None, "backup directory {name} must not become an account");
        }
        for (name, agent) in [
            (".claude-work", "claude"), (".codex-personal", "codex"),
            (".claude-old", "claude"), (".codex-archive", "codex"), (".claude-archives-2026", "claude"),
            (".claude-backupworks", "claude"), (".codex-gold", "codex"), (".codex-bakery", "codex"),
        ] { assert_eq!(conventional_agent(name), Some(agent), "legitimate profile {name} must remain discoverable"); }
        assert_eq!(conventional_agent(".claude-"), None);
        assert_eq!(conventional_agent("ordinary-directory"), None);
    }

    #[test]
    fn automatic_discovery_omits_backup_roots_but_keeps_distinct_accounts() {
        let fixture = Fixture::new();
        let home = fixture.0.join("home");
        for name in [".claude", ".codex", ".claude-work", ".codex-personal", ".claude-backups", ".codex-personal.backup", ".claude-old", ".codex-archive"] {
            std::fs::create_dir_all(home.join(name)).unwrap();
        }
        let found = discover_with_explicit(&home, &fixture.0.join("jarvis"), Vec::new());
        let names: HashSet<_> = found.iter().map(|source| source.home.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, HashSet::from([".claude", ".codex", ".claude-work", ".codex-personal", ".claude-old", ".codex-archive"].map(str::to_string)));
        assert_ne!(found.iter().find(|source| source.home.ends_with(".claude")).unwrap().id, found.iter().find(|source| source.home.ends_with(".claude-work")).unwrap().id);
    }

    #[test]
    fn explicit_environment_and_manifest_backup_names_bypass_auto_heuristic() {
        let fixture = Fixture::new();
        let home = fixture.0.join("home");
        let jarvis = fixture.0.join("jarvis");
        std::fs::create_dir_all(&jarvis).unwrap();
        for name in [".claude-backups", ".codex-backup", ".claude-work.backup", ".codex-personal_bak"] {
            std::fs::create_dir_all(home.join(name)).unwrap();
        }
        std::fs::write(jarvis.join("provider-roots.json"), serde_json::to_vec(&json!({"sources": [
            {"agent":"claude", "providerHome":home.join(".claude-work.backup")},
            {"agent":"codex", "providerHome":home.join(".codex-personal_bak")}
        ]})).unwrap()).unwrap();
        // Pass the environment candidates directly rather than mutating the
        // process environment while unrelated discovery tests run in parallel.
        let found = discover_with_explicit(&home, &jarvis, vec![
            ("claude".into(), home.join(".claude-backups")),
            ("codex".into(), home.join(".codex-backup")),
        ]);
        for name in [".claude-backups", ".codex-backup", ".claude-work.backup", ".codex-personal_bak"] {
            assert!(found.iter().any(|source| source.home.ends_with(name)), "explicit profile {name} was hidden");
        }
    }

    #[test]
    fn json_labels_name_the_profile_without_changing_canonical_ids() {
        for (agent, basename, label) in [
            ("claude", ".claude", "Claude"), ("codex", ".codex", "Codex"),
            ("claude", ".claude-work", "Claude · work"), ("codex", ".codex-personal", "Codex · personal"),
            ("claude", "team-account", "Claude · team-account"),
        ] {
            let path = Path::new("/jarvis-instance-test").join(basename);
            let item = source(agent, &path).unwrap();
            assert_eq!(item.json()["label"], label);
            assert_eq!(item.json()["id"], stable_id(agent, &item.home));
            assert_eq!(item.json()["instanceId"], item.id);
        }
        let canonical = source("codex", Path::new("/jarvis-instance-test/codex-home")).unwrap();
        assert_eq!(canonical.id, "codex-v1-145efd823f8f97d5");
    }

    #[test] fn profiles_keep_same_session_ids_separate_without_exposing_auth() {
        let root = std::env::temp_dir().join(format!("jarvis-node-sources-{}",std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for profile in ["personal","work"] {
            let home = root.join(profile); std::fs::create_dir_all(home.join("sessions")).unwrap();
            std::fs::write(home.join("sessions/a.jsonl"), "{\"type\":\"session_meta\",\"payload\":{\"id\":\"same\",\"cwd\":\"/repo\"}}\n").unwrap();
            std::fs::write(home.join("auth.json"), "SECRET_SHOULD_NOT_BE_READ").unwrap();
        }
        let sources = sources_from(vec![("codex".into(),root.join("personal")),("codex".into(),root.join("work"))]);
        let report = sessions(&sources);
        assert_eq!(report["sessions"].as_array().unwrap().len(),2);
        assert_ne!(report["sessions"][0]["sourceId"],report["sessions"][1]["sourceId"]);
        assert!(!report.to_string().contains("SECRET_SHOULD_NOT_BE_READ"));
        assert!(source("codex",Path::new("/")).is_none());
        assert!(source("codex",Path::new("relative")).is_none());
        let _ = std::fs::remove_dir_all(root);
    }
    #[test] fn ids_match_desktop_registry_vectors() {
        assert_eq!(stable_id("codex", Path::new("/jarvis-instance-test/codex-home")), "codex-v1-145efd823f8f97d5");
        assert_eq!(stable_id("claude", Path::new("/jarvis-instance-test/codex-home")), "claude-v1-145efd823f8f97d5");
    }

}
