//! Read-only analytics over the existing node tunnel. Guest paths are never
//! opened, canonicalized or passed to local Git commands.
use super::{trace, Options, SourceKind};
use crate::remote::{Node, NodeClient};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAX_NODES: usize = 8;
const MAX_MANIFEST: usize = 20_000;
const CHUNK_BYTES: u64 = 512 * 1024;
const MAX_REMOTE_FILES: usize = 64;
const MAX_REMOTE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Default)]
pub(super) struct Batch {
    pub config_key: String,
    pub sessions: Vec<Value>,
    pub roots: Vec<Value>,
    pub projects: Vec<Value>,
    pub nodes: Vec<Value>,
    pub errors: Vec<String>,
    pub discovered: usize,
    pub scanned: usize,
    pub attempted: usize,
    pub bytes: u64,
    pub transferred: u64,
    pub limited: bool,
}

impl Batch {
    pub fn coverage(&self) -> Value {
        json!({"nodes":self.nodes,"filesDiscovered":self.discovered,
            "filesScanned":self.scanned,"filesAttempted":self.attempted,"bytesBudgetUsed":self.bytes,
            "bytesTransferred":self.transferred,"limited":self.limited,
            "maxNodes":MAX_NODES,"maxFiles":MAX_REMOTE_FILES,"maxBytes":MAX_REMOTE_BYTES,
            "git":"not-inspected","transport":"node-file",
            "caveat":"Только подключённые узлы. Читается ограниченное начало трейса; при обрезке метрики являются наблюдаемым подытогом. Удалённые репозитории локально не проверяются."})
    }

    pub fn cache_key(&self) -> String {
        // Only sanitized metrics/metadata enter this fingerprint, never prompts.
        super::settings_hash(
            &json!({"sessions":self.sessions,"roots":self.roots,
            "nodes":self.nodes,"errors":self.errors,"limited":self.limited})
            .to_string(),
        )
    }
}

#[derive(Clone)]
struct Entry {
    source: SourceKind,
    path: String,
    sid: String,
    cwd: String,
    size: u64,
    at: i64,
    file_id: String,
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn absolute_guest_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.chars().any(char::is_control)
        && Path::new(value).is_absolute()
        && !Path::new(value)
            .components()
            .any(|p| matches!(p, Component::ParentDir))
}

pub(super) fn is_project(value: &str) -> bool {
    value.starts_with("remote://")
}

pub(super) fn validate_project(value: &str) -> Result<String, String> {
    let rest = value
        .strip_prefix("remote://")
        .ok_or("Некорректный удалённый проект")?;
    let (machine, path) = rest
        .split_once('/')
        .ok_or("Некорректный удалённый проект")?;
    if !safe_component(machine) || !absolute_guest_path(&format!("/{path}")) {
        return Err("Некорректный удалённый проект".into());
    }
    Ok(value.into())
}

fn project_key(machine: &str, cwd: &str) -> String {
    format!("remote://{machine}{cwd}")
}

pub(super) fn project_name(value: &str) -> String {
    let rest = value.strip_prefix("remote://").unwrap_or(value);
    let (machine, path) = rest.split_once('/').unwrap_or((rest, ""));
    format!(
        "{machine} · {}",
        Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("/")
    )
}

fn manifest(machine: &str, sources: &Value, sessions: &Value, batch: &mut Batch) -> Vec<Entry> {
    if !safe_component(machine) || sessions["protocol"].as_u64().unwrap_or(0) < 2 {
        batch
            .errors
            .push(format!("{machine}: требуется jarvis-node protocol 2"));
        return vec![];
    }
    let mut known = HashMap::new();
    for source in sources.as_array().into_iter().flatten().take(128) {
        let id = source["instanceId"].as_str().unwrap_or("");
        let agent = source["agent"].as_str().unwrap_or("");
        let home = source["providerHome"].as_str().unwrap_or("");
        if !safe_component(id)
            || !matches!(agent, "codex" | "claude")
            || !absolute_guest_path(home)
            || source["available"] != true
        {
            continue;
        }
        let kind = SourceKind {
            id: format!("remote:{machine}:{id}"),
            format: agent.into(),
            namespace: format!("source:remote:{machine}:{id}"),
            configured: false,
            label: format!(
                "{machine} · {agent} · {}",
                Path::new(home)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(home)
            ),
            machine: machine.into(),
            instance_id: Some(id.into()),
            provider_home: Some(home.into()),
        };
        batch.roots.push(
            json!({"path":home,"agent":agent,"format":agent,"id":kind.id,
            "namespace":kind.namespace,"label":kind.label,"machine":machine,"instanceId":id,
            "providerHome":home,"remote":true,"configured":false}),
        );
        known.insert(id, kind);
    }
    let mut entries = BTreeMap::new();
    let rows = sessions["sessions"].as_array();
    batch.limited |= sessions["limited"] == true || rows.is_some_and(|r| r.len() > MAX_MANIFEST);
    let manifest_errors = sessions["errors"].as_array().map_or(0, Vec::len);
    if manifest_errors > 0 {
        batch.errors.push(format!(
            "{machine}: ошибок обнаружения источников: {manifest_errors}"
        ));
    }
    for row in rows.into_iter().flatten().take(MAX_MANIFEST) {
        let Some(source) = row["instanceId"].as_str().and_then(|id| known.get(id)) else {
            batch.limited = true;
            continue;
        };
        let path = row["path"].as_str().unwrap_or("");
        let sid = row["id"].as_str().unwrap_or("");
        let home = source.provider_home.as_deref().unwrap_or("");
        let dirs: &[&str] = if source.format == "codex" {
            &["sessions", "archived_sessions"]
        } else {
            &["projects"]
        };
        let allowed = dirs
            .iter()
            .any(|dir| Path::new(path).starts_with(Path::new(home).join(dir)));
        if row["agent"] != source.format
            || !safe_component(sid)
            || !absolute_guest_path(path)
            || Path::new(path).extension().is_none_or(|e| e != "jsonl")
            || !allowed
        {
            batch.limited = true;
            continue;
        }
        let cwd = row["cwd"]
            .as_str()
            .filter(|s| absolute_guest_path(s))
            .unwrap_or("");
        let entry = Entry {
            source: source.clone(),
            path: path.into(),
            sid: sid.into(),
            cwd: cwd.into(),
            size: row["size"].as_u64().unwrap_or(0),
            at: row["at"].as_i64().unwrap_or(0),
            file_id: row["fileId"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(256)
                .collect(),
        };
        let key = (source.id.clone(), sid.to_owned());
        if entries
            .get(&key)
            .is_none_or(|old: &Entry| (entry.at, entry.size) > (old.at, old.size))
        {
            entries.insert(key, entry);
        }
    }
    entries.into_values().collect()
}

fn decorate(mut data: Value, entry: &Entry) -> Value {
    // Transcript contents cannot change manifest identity or machine routing.
    data.as_object_mut().map(|o| o.remove("editEvidence"));
    let cwd = data["cwd"]
        .as_str()
        .filter(|s| absolute_guest_path(s))
        .unwrap_or(&entry.cwd)
        .to_owned();
    data["providerSessionId"] = json!(entry.sid);
    data["id"] = json!(format!("{}:{}", entry.source.namespace, entry.sid));
    data["cwd"] = json!(cwd);
    data["project"] = json!(if cwd.is_empty() {
        String::new()
    } else {
        project_key(&entry.source.machine, &cwd)
    });
    data["sourceFile"] = json!(entry.path);
    data["sourceId"] = json!(entry.source.id);
    data["sourceNamespace"] = json!(entry.source.namespace);
    data["sourceFormat"] = json!(entry.source.format);
    data["sourceLabel"] = json!(entry.source.label);
    data["machine"] = json!(entry.source.machine);
    data["instanceId"] = json!(entry.source.instance_id);
    data["providerHome"] = json!(entry.source.provider_home);
    data["remote"] = json!(true);
    data["coverage"]["remoteGit"] = json!("not-inspected");
    data["coverage"]["manifestLastAt"] = json!(entry.at);
    if data["coverage"]["truncated"] == true && entry.at > data["lastAt"].as_i64().unwrap_or(0) {
        data["coverage"]["lastObservedAt"] = data["lastAt"].clone();
        data["lastAt"] = json!(entry.at);
        data["coverage"]["lastAtBasis"] = json!("remote-manifest; trace prefix truncated");
    }
    data
}

async fn prefix(
    client: &NodeClient,
    path: &str,
    limit: u64,
) -> Result<(Vec<u8>, u64, u64), String> {
    let mut bytes = Vec::new();
    let mut from = 0;
    let mut size = 0;
    for _ in 0..(MAX_REMOTE_BYTES / CHUNK_BYTES + 2) {
        if from >= limit {
            break;
        }
        if from > 0 && limit - from < CHUNK_BYTES && size > limit {
            break;
        }
        let chunk = tokio::time::timeout(Duration::from_secs(2), client.file(path, from))
            .await
            .map_err(|_| "тайм-аут чтения трейса")??
            .ok_or("трейс больше не существует")?;
        if chunk.from != from
            || chunk.next < from
            || chunk.next > chunk.size
            || chunk.next - from > CHUNK_BYTES
            || chunk.data.len() > (CHUNK_BYTES * 3) as usize
        {
            return Err("Некорректный курсор удалённого трейса".into());
        }
        size = chunk.size;
        if chunk.next == from {
            if chunk.eof {
                break;
            }
            return Err("Удалённый трейc не продвигает курсор".into());
        }
        let remaining = limit.saturating_sub(bytes.len() as u64) as usize;
        bytes.extend_from_slice(&chunk.data.as_bytes()[..chunk.data.len().min(remaining)]);
        from = chunk.next;
        if chunk.eof || bytes.len() as u64 >= limit {
            break;
        }
    }
    Ok((bytes, size, from))
}

#[derive(Clone)]
struct Cached {
    key: String,
    data: Value,
    bytes: u64,
}
static CACHE: OnceLock<Mutex<HashMap<String, Cached>>> = OnceLock::new();
static SESSION_CACHE: OnceLock<Mutex<HashMap<String, (i64, Value)>>> = OnceLock::new();

fn select_session(entries: Vec<Entry>, session: &crate::model::Session) -> Result<Entry, String> {
    let mut matches = entries.into_iter().filter(|e| {
        e.sid == session.agent_id()
            && session
                .agent
                .as_deref()
                .is_none_or(|a| a == e.source.format)
            && session
                .instance_id
                .as_deref()
                .is_none_or(|id| e.source.instance_id.as_deref() == Some(id))
            && session
                .provider_home
                .as_deref()
                .is_none_or(|home| e.source.provider_home.as_deref() == Some(home))
            && session
                .transcript
                .as_deref()
                .is_none_or(|path| path == e.path)
    });
    let entry = matches
        .next()
        .ok_or("Трейс этого профиля не найден на узле")?;
    if matches.next().is_some() {
        return Err("Несколько профилей содержат этот SID; источник сессии неоднозначен".into());
    }
    Ok(entry)
}

fn session_cache_key(session: &crate::model::Session) -> String {
    json!([
        session.remote,
        session.instance_id,
        session.provider_home,
        session.agent_id(),
        session.transcript
    ])
    .to_string()
}

fn session_result(
    session: &crate::model::Session,
    cached: Option<(i64, Value)>,
    error: Option<String>,
) -> Value {
    let (at, data) = cached
        .map(|(at, data)| (Some(at), data))
        .unwrap_or((None, Value::Null));
    json!({"trace":data,"source":"remote-transcripts","machine":session.remote,
        "instanceId":session.instance_id,"instanceLabel":session.instance_label,"providerHome":session.provider_home,
        "providerSessionId":session.agent_id(),"cachedAt":at,"stale":error.is_some(),"error":error})
}

async fn read_session_trace(
    client: &NodeClient,
    machine: &str,
    session: &crate::model::Session,
) -> Result<Value, String> {
    let (sources, sessions) = tokio::try_join!(client.sources(), client.sessions())?;
    let entry = select_session(
        manifest(machine, &sources, &sessions, &mut Batch::default()),
        session,
    )?;
    let (bytes, size, _) = prefix(client, &entry.path, 8 * 1024 * 1024).await?;
    let cfg = json!({"limits":{"maxFileMiB":8,"maxLines":100000}});
    tokio::task::spawn_blocking(move || {
        trace::analyze_bytes_with_config(&bytes, size, &entry.sid, &entry.source.format, &cfg)
            .map(|v| decorate(v, &entry))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// One chat reads only its declared node/source/transcript; it never triggers a
/// global report scan or substitutes account-wide /usage percentages.
pub(super) async fn session_trace(
    d: &crate::daemon::Daemon,
    session: &crate::model::Session,
) -> Value {
    let key = session_cache_key(session);
    let cached = SESSION_CACHE
        .get_or_init(Default::default)
        .lock()
        .ok()
        .and_then(|c| c.get(&key).cloned());
    let node = session
        .remote
        .as_deref()
        .and_then(|name| d.remotes.node(name));
    let Some(node) = node.filter(|node| node.status().connected) else {
        return session_result(session, cached, Some("Узел не подключён".into()));
    };
    if cached
        .as_ref()
        .is_some_and(|(at, _)| crate::util::now_ms() - *at < 15_000)
    {
        return session_result(session, cached, None);
    }
    let client = match node.client() {
        Ok(c) => c,
        Err(e) => return session_result(session, cached, Some(e)),
    };
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        read_session_trace(&client, &node.cfg.name, session),
    )
    .await;
    match result {
        Ok(Ok(data)) => {
            let at = crate::util::now_ms();
            if let Ok(mut cache) = SESSION_CACHE.get_or_init(Default::default).lock() {
                if cache.len() >= 128 {
                    cache.clear();
                }
                cache.insert(key, (at, data.clone()));
            }
            session_result(session, Some((at, data)), None)
        }
        Ok(Err(e)) => session_result(session, cached, Some(e)),
        Err(_) => session_result(
            session,
            cached,
            Some("Тайм-аут чтения расхода с узла".into()),
        ),
    }
}

pub(super) async fn collect(nodes: Vec<Arc<Node>>, options: &Options, cfg: &Value) -> Batch {
    let mut batch = Batch::default();
    if cfg["autoDiscover"] != true {
        return batch;
    }
    let filter = options.project.as_deref().filter(|s| !s.trim().is_empty());
    let remote_filter = filter.filter(|s| is_project(s));
    if filter.is_some() && remote_filter.is_none() {
        return batch;
    }
    let total_files = cfg["limits"]["maxFiles"].as_u64().unwrap_or(200) as usize;
    let total_bytes = cfg["limits"]["maxScanMiB"].as_u64().unwrap_or(256) * 1024 * 1024;
    // Reserve a bounded share for connected VMs before scanning a large local home.
    let max_files = (if remote_filter.is_some() {
        total_files
    } else {
        (total_files / 4).max(1)
    })
    .min(MAX_REMOTE_FILES);
    let max_bytes = (if remote_filter.is_some() {
        total_bytes
    } else {
        total_bytes / 4
    })
    .min(MAX_REMOTE_BYTES);
    let file_limit = cfg["limits"]["maxFileMiB"].as_u64().unwrap_or(32) * 1024 * 1024;
    let config_key = super::settings_hash(&cfg.to_string());
    batch.config_key = config_key.clone();
    let mut candidates = Vec::new();
    batch.limited = nodes.len() > MAX_NODES;
    for node in nodes.into_iter().take(MAX_NODES) {
        let machine = &node.cfg.name;
        let connected = node.status().connected;
        batch
            .nodes
            .push(json!({"machine":machine,"connected":connected}));
        if !connected {
            batch.limited = true;
            continue;
        }
        if remote_filter.is_some_and(|f| !f.starts_with(&format!("remote://{machine}/"))) {
            continue;
        }
        let client = match node.client() {
            Ok(c) => c,
            Err(_) => {
                batch.errors.push(format!("{machine}: туннель недоступен"));
                continue;
            }
        };
        match tokio::time::timeout(Duration::from_secs(2), async {
            tokio::try_join!(client.sources(), client.sessions())
        })
        .await
        {
            Ok(Ok((sources, sessions))) => {
                let entries = manifest(machine, &sources, &sessions, &mut batch);
                batch.discovered += entries.len();
                for entry in entries {
                    if !entry.cwd.is_empty() {
                        let path = project_key(machine, &entry.cwd);
                        batch.projects.push(json!({"path":path,"name":project_name(&path),"machine":machine,"basis":"remote-trace-cwd"}));
                    }
                    candidates.push((client.clone(), entry));
                }
            }
            _ => batch.errors.push(format!(
                "{machine}: не удалось получить список источников/сессий"
            )),
        }
    }
    candidates.sort_by(|a, b| {
        let selected =
            |e: &Entry| remote_filter.is_some_and(|f| project_key(&e.source.machine, &e.cwd) == f);
        selected(&b.1)
            .cmp(&selected(&a.1))
            .then(b.1.at.cmp(&a.1.at))
            .then(a.1.path.cmp(&b.1.path))
    });
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut attempted = 0;
    for (client, entry) in candidates {
        if remote_filter.is_some_and(|f| {
            !entry.cwd.is_empty() && project_key(&entry.source.machine, &entry.cwd) != f
        }) {
            continue;
        }
        if attempted >= max_files || Instant::now() >= deadline {
            batch.limited = true;
            break;
        }
        let remaining = max_bytes.saturating_sub(batch.bytes);
        // The protocol returns at most 512 KiB per call. Leave room for the final
        // response even when the desired parser prefix ends inside that chunk.
        let allowance = remaining.min(file_limit) / CHUNK_BYTES * CHUNK_BYTES;
        if allowance == 0 {
            batch.limited = true;
            break;
        }
        attempted += 1;
        batch.attempted = attempted;
        let cache_id = format!("{}:{}", entry.source.id, entry.sid);
        let key = format!(
            "{}:{}:{}:{}:{allowance}:{config_key}",
            entry.path, entry.file_id, entry.size, entry.at
        );
        let cached = CACHE
            .get_or_init(Default::default)
            .lock()
            .ok()
            .and_then(|c| {
                c.get(&cache_id)
                    .filter(|c| c.key == key && !options.refresh)
                    .cloned()
            });
        let result = if let Some(cached) = cached {
            Ok((cached.data, cached.bytes))
        } else {
            match tokio::time::timeout(
                deadline.saturating_duration_since(Instant::now()),
                prefix(&client, &entry.path, allowance),
            )
            .await
            {
                Ok(Ok((bytes, size, transferred))) => {
                    batch.transferred += transferred;
                    let cfg = cfg.clone();
                    let entry_clone = entry.clone();
                    match tokio::task::spawn_blocking(move || {
                        trace::analyze_bytes_with_config(
                            &bytes,
                            size,
                            &entry_clone.sid,
                            &entry_clone.source.format,
                            &cfg,
                        )
                        .map(|d| decorate(d, &entry_clone))
                    })
                    .await
                    {
                        Ok(Ok(data)) => {
                            if let Ok(mut cache) = CACHE.get_or_init(Default::default).lock() {
                                if cache.len() >= MAX_REMOTE_FILES {
                                    cache.clear();
                                }
                                cache.insert(
                                    cache_id,
                                    Cached {
                                        key,
                                        data: data.clone(),
                                        bytes: transferred,
                                    },
                                );
                            }
                            Ok((data, transferred))
                        }
                        _ => Err("не удалось разобрать удалённый трейс".into()),
                    }
                }
                Ok(Err(e)) => Err(e),
                Err(_) => Err("лимит времени удалённого чтения".into()),
            }
        };
        match result {
            Ok((data, bytes)) => {
                batch.bytes += bytes;
                batch.scanned += 1;
                batch.limited |= data["coverage"]["truncated"] == true;
                batch.sessions.push(data);
            }
            Err(error) => {
                // Failed reads may already have consumed their allowance.
                batch.bytes += allowance;
                if batch.errors.len() < 30 {
                    batch
                        .errors
                        .push(format!("{}: {error}", entry.source.label));
                }
            }
        }
    }
    batch.limited |= !batch.errors.is_empty();
    batch
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "manual read-only analytics probe against an isolated synthetic node"]
    async fn live_isolated_node_analytics_probe() {
        let url = std::env::var("JARVIS_ANALYTICS_QA_NODE").expect("isolated loopback node URL");
        let port = url
            .strip_prefix("http://127.0.0.1:")
            .expect("loopback only")
            .parse::<u16>()
            .expect("port");
        assert!(port > 0);
        let client = NodeClient::new(url).unwrap();
        let (sources, rows) = tokio::try_join!(client.sources(), client.sessions()).unwrap();
        let mut batch = Batch::default();
        let entries = manifest("qa-vm", &sources, &rows, &mut batch);
        assert!(
            !entries.is_empty(),
            "synthetic node has no valid scoped sessions"
        );
        assert!(entries.len() <= 8, "use a small isolated fixture node");
        let cfg = super::super::config::defaults();
        for entry in entries {
            let mut session = crate::model::Session::new(
                format!(
                    "qa-vm:codex:{}:{}",
                    entry.source.instance_id.as_deref().unwrap(),
                    entry.sid
                ),
                0,
            );
            session.remote = Some("qa-vm".into());
            session.agent = Some(entry.source.format.clone());
            session.provider_session_id = Some(entry.sid.clone());
            session.instance_id = entry.source.instance_id.clone();
            session.provider_home = entry.source.provider_home.clone();
            session.transcript = Some(entry.path.clone());
            let scoped = read_session_trace(&client, "qa-vm", &session)
                .await
                .unwrap();
            let usage = crate::usage::remote_session_usage(session_result(
                &session,
                Some((crate::util::now_ms(), scoped)),
                None,
            ));
            assert_eq!(
                usage["instanceId"],
                entry.source.instance_id.as_deref().unwrap()
            );
            assert_eq!(usage["tok"], 120.0);
            assert_eq!(usage["stale"], false);
            let (bytes, size, transferred) =
                prefix(&client, &entry.path, 1024 * 1024).await.unwrap();
            let data = trace::analyze_bytes_with_config(
                &bytes,
                size,
                &entry.sid,
                &entry.source.format,
                &cfg,
            )
            .unwrap();
            batch.sessions.push(decorate(data, &entry));
            batch.scanned += 1;
            batch.bytes += transferred;
            batch.transferred += transferred;
        }
        batch.discovered = batch.scanned;
        let dir = std::env::temp_dir().join(format!(
            "jarvis-analytics-node-qa-{}-{}",
            std::process::id(),
            crate::util::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let report = super::super::build_report_with_typed_sources(
            json!({"period":"all","refresh":true}),
            vec![],
            vec![],
            &dir,
            batch,
        )
        .unwrap();
        assert!(report["summary"]["sessions"].as_u64().unwrap() > 0);
        assert!(report["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["machine"] == "qa-vm"
                && s["id"]
                    .as_str()
                    .unwrap()
                    .starts_with("source:remote:qa-vm:")));
        assert!(report["projects"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["git"]["status"] == "not-inspected"));
        assert!(!report.to_string().contains("editEvidence"));
        println!(
            "{}",
            json!({"sessions":report["summary"]["sessions"],"inputTokens":report["summary"]["inputTokens"],
            "outputTokens":report["summary"]["outputTokens"],"filesScanned":report["coverage"]["filesScanned"],"bytes":report["coverage"]["bytesBudgetUsed"]})
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    fn inputs(path: &str) -> (Value, Value) {
        (
            json!([{"instanceId":"codex-v1-123","agent":"codex","providerHome":"/guest/.codex","available":true}]),
            json!({"protocol":2,"sessions":[{"instanceId":"codex-v1-123","agent":"codex","id":"sid","path":path,"cwd":"/same/repo","size":100,"at":100}]}),
        )
    }
    #[test]
    fn guest_manifest_requires_announced_scope_and_keeps_machine_identity() {
        let (sources, sessions) = inputs("/guest/.codex/sessions/a.jsonl");
        let a = manifest("vm-a", &sources, &sessions, &mut Batch::default());
        let b = manifest("vm-b", &sources, &sessions, &mut Batch::default());
        assert_eq!(a.len(), 1);
        let a = decorate(
            json!({"id":"forged","cwd":"/same/repo","editEvidence":[{"text":"private"}]}),
            &a[0],
        );
        let b = decorate(json!({"cwd":"/same/repo"}), &b[0]);
        assert_ne!(a["id"], b["id"]);
        assert_eq!(a["id"], "source:remote:vm-a:codex-v1-123:sid");
        assert_eq!(a["project"], "remote://vm-a/same/repo");
        assert_ne!(a["project"], b["project"]);
        assert!(!a.to_string().contains("private"));
        for path in [
            "/guest/.codex/auth.json",
            "/guest/.codex/sessions/../../outside.jsonl",
            "/guest/.codex-other/sessions/a.jsonl",
            "/etc/outside.jsonl",
        ] {
            let (_, rows) = inputs(path);
            assert!(manifest("vm-a", &sources, &rows, &mut Batch::default()).is_empty());
        }
        assert!(validate_project("remote://vm-a/same/repo").is_ok());
        assert!(validate_project("remote://vm-a/a/../repo").is_err());
        assert_eq!(project_name("remote://vm-a/same/repo"), "vm-a · repo");
    }

    #[test]
    fn per_session_selection_does_not_fallback_to_another_profile_or_path() {
        let (sources, rows) = inputs("/guest/.codex/sessions/a.jsonl");
        let first = manifest("vm-a", &sources, &rows, &mut Batch::default())
            .pop()
            .unwrap();
        let mut second = first.clone();
        second.source.instance_id = Some("codex-v1-other".into());
        second.source.provider_home = Some("/guest/other".into());
        second.path = "/guest/other/sessions/a.jsonl".into();
        let mut session = crate::model::Session::new("vm-a:sid".into(), 0);
        session.remote = Some("vm-a".into());
        session.agent = Some("codex".into());
        assert!(select_session(vec![first.clone(), second.clone()], &session).is_err());
        session.instance_id = second.source.instance_id.clone();
        session.provider_home = second.source.provider_home.clone();
        assert_eq!(
            select_session(vec![first.clone(), second.clone()], &session)
                .unwrap()
                .path,
            second.path
        );
        session.transcript = Some(first.path.clone());
        assert!(select_session(vec![first.clone(), second.clone()], &session).is_err());
        session.transcript = Some(second.path.clone());
        let key = session_cache_key(&session);
        session.instance_id = first.source.instance_id.clone();
        assert_ne!(key, session_cache_key(&session));
        let cached = session_result(
            &session,
            Some((100, json!({"models":[]}))),
            Some("offline".into()),
        );
        assert_eq!(cached["cachedAt"], 100);
        assert_eq!(cached["stale"], true);
    }
    #[tokio::test]
    async fn real_node_client_prefix_reads_http_without_opening_guest_path() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let data =
            b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"sid\",\"cwd\":\"/guest/repo\"}}\n";
        let expected = data.to_vec();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = [0u8; 4096];
            let n = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..n]);
            assert!(request.starts_with("GET /file?"));
            assert!(request.contains("from=0"));
            let body=json!({"data":String::from_utf8_lossy(data),"from":0,"next":data.len(),"size":data.len(),"eof":true}).to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        });
        let client = NodeClient::new(format!("http://{address}")).unwrap();
        let (bytes, size, transferred) = prefix(
            &client,
            "/guest/.codex/sessions/nonexistent-locally.jsonl",
            1024,
        )
        .await
        .unwrap();
        server.join().unwrap();
        assert_eq!(bytes, expected);
        assert_eq!(size, transferred);
        let parsed =
            trace::analyze_bytes_with_config(&bytes, size, "fallback", "codex", &Value::Null)
                .unwrap();
        assert_eq!(parsed["id"], "sid");
        assert_eq!(parsed["cwd"], "/guest/repo");
    }
}
