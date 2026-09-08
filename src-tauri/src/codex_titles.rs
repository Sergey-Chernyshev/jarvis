//! Read-only native Codex display titles, scoped to the configured provider home.
//! The portable index wins. SQLite is an optional batch fallback for requested
//! IDs only; a missing CLI/schema never prevents rollout observation.
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

const MAX_INDEX: u64 = 2 * 1024 * 1024;
const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_IDS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Signature {
    index: Option<(u64, SystemTime)>,
    database: Option<(PathBuf, u64, SystemTime)>,
    wal: Option<(u64, SystemTime)>,
    requested: Vec<(String, PathBuf)>,
}

struct CachedHome {
    signature: Signature,
    titles: HashMap<String, String>,
}

#[derive(Default)]
pub struct TitleCache {
    homes: HashMap<PathBuf, CachedHome>,
}

fn stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = fs::metadata(path).ok()?;
    meta.is_file().then(|| {
        (
            meta.len(),
            meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        )
    })
}

fn database(home: &Path) -> Option<PathBuf> {
    fs::read_dir(home)
        .ok()?
        .take(256)
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            let version = name
                .strip_prefix("state_")?
                .strip_suffix(".sqlite")?
                .parse::<u32>()
                .ok()?;
            entry
                .file_type()
                .ok()?
                .is_file()
                .then_some((version, entry.path()))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, path)| path)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn clean_title(text: &str) -> Option<String> {
    let text = crate::backend::codex_transcript::title_text(text);
    if text.is_empty() || crate::backend::codex_transcript::needs_title_repair(&text) {
        return None;
    }
    Some(crate::util::ellipsize(&crate::util::one_line(&text), 160))
}

fn read_index(home: &Path, requested: &HashSet<&str>) -> HashMap<String, String> {
    let mut result = HashMap::new();
    let Ok(mut file) = fs::File::open(home.join("session_index.jsonl")) else {
        return result;
    };
    let Ok(meta) = file.metadata() else {
        return result;
    };
    let start = meta.len().saturating_sub(MAX_INDEX);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return result;
    }
    let mut bytes = Vec::new();
    if file.take(MAX_INDEX).read_to_end(&mut bytes).is_err() {
        return result;
    }
    let mut newest = HashMap::new();
    for (index, line) in bytes.split(|b| *b == b'\n').enumerate() {
        if start > 0 && index == 0 {
            continue;
        }
        let Ok(row) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let Some(id) = row["id"].as_str().filter(|id| requested.contains(id)) else {
            continue;
        };
        let Some(title) = row["thread_name"].as_str().and_then(clean_title) else {
            continue;
        };
        let at = row["updated_at"]
            .as_str()
            .and_then(crate::transcript::parse_ts)
            .unwrap_or(0);
        if newest.get(id).is_some_and(|previous| *previous > at) {
            continue;
        }
        newest.insert(id.to_owned(), at);
        result.insert(id.to_owned(), title);
    }
    result
}

impl TitleCache {
    /// One refresh per home/batch, with no process launch while fingerprints and
    /// requested identities are unchanged. Never use another account as fallback.
    pub fn refresh(&mut self, home: &Path, requested: &[(String, PathBuf)]) {
        let Ok(home) = home.canonicalize() else {
            return;
        };
        let requested: Vec<_> = requested
            .iter()
            .filter(|(id, path)| valid_id(id) && path.starts_with(&home))
            .take(MAX_IDS)
            .cloned()
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .collect();
        let db = database(&home);
        let signature = Signature {
            index: stamp(&home.join("session_index.jsonl")),
            database: db
                .as_ref()
                .and_then(|path| stamp(path).map(|(len, at)| (path.clone(), len, at))),
            wal: db
                .as_ref()
                .and_then(|path| stamp(&PathBuf::from(format!("{}-wal", path.display())))),
            requested,
        };
        if self
            .homes
            .get(&home)
            .is_some_and(|old| old.signature == signature)
        {
            return;
        }
        let ids: HashSet<_> = signature
            .requested
            .iter()
            .map(|(id, _)| id.as_str())
            .collect();
        let mut titles = read_index(&home, &ids);
        let missing: Vec<_> = signature
            .requested
            .iter()
            .filter(|(id, _)| !titles.contains_key(id))
            .cloned()
            .collect();
        if let Some(db) = db.filter(|_| !missing.is_empty()) {
            if let Some(bytes) = read_sqlite(&db, &missing) {
                titles.extend(parse_sqlite(&bytes, &missing));
            }
        }
        self.homes.insert(home, CachedHome { signature, titles });
    }

    pub fn get(&self, home: &Path, id: &str) -> Option<&str> {
        self.homes.get(home)?.titles.get(id).map(String::as_str)
    }

    pub fn retain_homes(&mut self, homes: &HashSet<PathBuf>) {
        self.homes.retain(|home, _| homes.contains(home));
    }
}

fn parse_sqlite(bytes: &[u8], requested: &[(String, PathBuf)]) -> HashMap<String, String> {
    let mut titles = HashMap::new();
    let Ok(rows) = serde_json::from_slice::<Value>(bytes) else {
        return titles;
    };
    let Some(rows) = rows.as_array().filter(|rows| rows.len() <= MAX_IDS) else {
        return titles;
    };
    let expected: HashMap<_, _> = requested
        .iter()
        .map(|(id, path)| (id.as_str(), path))
        .collect();
    for row in rows {
        let Some(id) = row["id"].as_str() else {
            continue;
        };
        let Some(path) = expected.get(id) else {
            continue;
        };
        let Some(stored) = row["rollout_path"].as_str() else {
            continue;
        };
        if Path::new(stored) != path.as_path()
            && !Path::new(stored)
                .canonicalize()
                .ok()
                .zip(path.canonicalize().ok())
                .is_some_and(|(a, b)| a == b)
        {
            continue;
        }
        if row["thread_source"] == "guardian_review"
            || row["model"]
                .as_str()
                .is_some_and(crate::backend::codex_transcript::is_technical_model)
        {
            continue;
        }
        if let Some(title) = row["name"]
            .as_str()
            .and_then(clean_title)
            .or_else(|| row["title"].as_str().and_then(clean_title))
        {
            titles.insert(id.to_owned(), title);
        }
    }
    titles
}

fn read_sqlite(database: &Path, requested: &[(String, PathBuf)]) -> Option<Vec<u8>> {
    if requested.is_empty()
        || requested.len() > MAX_IDS
        || requested.iter().any(|(id, _)| !valid_id(id))
    {
        return None;
    }
    let ids = requested
        .iter()
        .map(|(id, _)| format!("'{id}'"))
        .collect::<Vec<_>>()
        .join(",");
    // Explicit IDs use Codex's primary-key lookup. No prompt/preview columns are
    // selected, and substr limits each title before JSON serialization.
    let query=format!("PRAGMA query_only=ON; SELECT id,rollout_path,substr(name,1,512) AS name,substr(title,1,512) AS title,model,thread_source FROM threads WHERE id IN ({ids}) LIMIT {MAX_IDS};");
    let mut child = Command::new("sqlite3")
        .args([
            "-batch",
            "-readonly",
            "-json",
            "-init",
            "/dev/null",
            "-cmd",
            ".timeout 100",
        ])
        .arg(database)
        .arg(query)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let result = (|| {
        use std::os::fd::AsRawFd;
        let mut stdout = child.stdout.take()?;
        let fd = stdout.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return None;
        }
        let start = Instant::now();
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 16384];
        let mut eof = false;
        loop {
            if start.elapsed() > Duration::from_millis(750) {
                return None;
            }
            if !eof {
                match stdout.read(&mut buffer) {
                    Ok(0) => eof = true,
                    Ok(n) => {
                        if bytes.len() + n > MAX_OUTPUT {
                            return None;
                        }
                        bytes.extend_from_slice(&buffer[..n]);
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => return None,
                }
            }
            if let Some(status) = child.try_wait().ok()? {
                if eof {
                    return status.success().then_some(bytes);
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })();
    if result.is_none() {
        let _ = child.kill();
        if !matches!(child.try_wait(), Ok(Some(_))) {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "jarvis-native-titles-{}-{}",
                std::process::id(),
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn index_titles_are_scoped_to_home_requested_id_and_latest_revision() {
        let a = Fixture::new();
        let b = Fixture::new();
        let path = a.0.join("sessions/rollout-id.jsonl");
        fs::write(a.0.join("session_index.jsonl"),format!("{}\n{}\n{}\n{{broken",json!({"id":"same","thread_name":"New native title","updated_at":"2026-09-05T10:00:00Z"}),json!({"id":"same","thread_name":"Stale title","updated_at":"2026-09-04T10:00:00Z"}),json!({"id":"unrequested","thread_name":"Unrelated"}))).unwrap();
        fs::write(
            b.0.join("session_index.jsonl"),
            format!("{}\n", json!({"id":"same","thread_name":"Other account"})),
        )
        .unwrap();
        let mut cache = TitleCache::default();
        cache.refresh(&a.0, &[("same".into(), path.clone())]);
        cache.refresh(
            &b.0,
            &[("same".into(), b.0.join("sessions/rollout-id.jsonl"))],
        );
        assert_eq!(cache.get(&a.0, "same"), Some("New native title"));
        assert_eq!(cache.get(&b.0, "same"), Some("Other account"));
        assert!(cache.get(&a.0, "unrequested").is_none());
        fs::write(
            a.0.join("session_index.jsonl"),
            format!("{}\n", json!({"id":"same","thread_name":"Changed"})),
        )
        .unwrap();
        cache.refresh(&a.0, &[("same".into(), path)]);
        assert_eq!(cache.get(&a.0, "same"), Some("Changed"));
    }

    #[test]
    fn sqlite_requires_matching_rollout_and_preserves_only_human_titles() {
        let fixture = Fixture::new();
        let path = fixture.0.join("rollout.jsonl");
        fs::write(&path, "").unwrap();
        let rows = json!([
            {"id":"good","rollout_path":path,"title":"First request","name":"Native short title"},
            {"id":"wrong","rollout_path":"/different/rollout.jsonl","title":"Wrong account"},
            {"id":"guardian","rollout_path":path,"title":"Review","model":"codex-auto-review"},
            {"id":"context","rollout_path":path,"title":"<recommended_plugins>context"}
        ]);
        let requested =
            ["good", "wrong", "guardian", "context"].map(|id| (id.into(), path.clone()));
        let result = parse_sqlite(rows.to_string().as_bytes(), &requested);
        assert_eq!(result.len(), 1);
        assert_eq!(result["good"], "Native short title");
        assert!(!valid_id("id');DROP TABLE threads;--"));
    }

    #[test]
    fn optional_sqlite_batch_reads_only_requested_titles_and_does_not_modify_database() {
        if Command::new("sqlite3")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_err()
        {
            return;
        }
        let fixture = Fixture::new();
        let db = fixture.0.join("state_5.sqlite");
        let rollout = fixture.0.join("rollout.jsonl");
        fs::write(&rollout, "").unwrap();
        let quoted = rollout.to_string_lossy().replace('\'', "''");
        let sql=format!("CREATE TABLE threads(id TEXT PRIMARY KEY,rollout_path TEXT,name TEXT,title TEXT,model TEXT,thread_source TEXT); INSERT INTO threads VALUES ('wanted','{quoted}','Native title','Original prompt','gpt-6','user'); INSERT INTO threads VALUES ('unrequested','{quoted}','Unrelated','Private unrelated text','gpt-6','user');");
        assert!(Command::new("sqlite3")
            .arg(&db)
            .arg(sql)
            .status()
            .unwrap()
            .success());
        let before = fs::read(&db).unwrap();
        let requested = [("wanted".into(), rollout)];
        let bytes = read_sqlite(&db, &requested)
            .expect("available sqlite should support the native schema");
        let titles = parse_sqlite(&bytes, &requested);
        assert_eq!(titles.len(), 1);
        assert_eq!(titles["wanted"], "Native title");
        assert!(!String::from_utf8_lossy(&bytes).contains("Unrelated"));
        assert_eq!(fs::read(db).unwrap(), before);
    }
}
