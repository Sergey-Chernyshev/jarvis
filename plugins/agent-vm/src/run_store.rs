use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use zeroize::Zeroize;

use crate::run_event::{Backend, RunEvent};

pub const MAX_JOURNAL_LINE_BYTES: usize = 1024 * 1024;
pub const MAX_REPLAY_EVENTS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSummary {
    pub run_id: String,
    pub project_id: String,
    pub cwd: String,
    pub backend: Backend,
    pub vm: String,
    pub backend_session_id: Option<String>,
    pub last_turn_id: String,
    pub last_seq: u64,
    pub state: String,
}

#[derive(Clone)]
pub struct RunStore {
    root: PathBuf,
    last_seq: Arc<Mutex<HashMap<String, u64>>>,
}

impl RunStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            last_seq: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn append(&self, event: &RunEvent) -> Result<(), String> {
        validate_run_id(&event.run_id)?;
        self.ensure_root()?;
        let mut last_sequences = self.last_seq.lock().unwrap();
        let previous = match last_sequences.get(&event.run_id) {
            Some(seq) => *seq,
            None => last_seq_from_file(&self.path(&event.run_id)?, &event.run_id)?,
        };
        if event.seq == 0 || event.seq <= previous {
            return Err("run journal требует строго монотонный seq".into());
        }
        let mut bytes = serde_json::to_vec(event)
            .map_err(|_| "не сериализовать normalized run event".to_string())?;
        if bytes.len() > MAX_JOURNAL_LINE_BYTES {
            bytes.zeroize();
            return Err("normalized run event превышает journal line limit".into());
        }
        bytes.push(b'\n');
        let path = self.path(&event.run_id)?;
        let result = (|| -> Result<(), String> {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)
                .map_err(|_| "не открыть private run journal".to_string())?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                .map_err(|_| "не защитить private run journal".to_string())?;
            file.write_all(&bytes)
                .map_err(|_| "не записать private run journal".to_string())?;
            file.flush()
                .map_err(|_| "не flush private run journal".to_string())?;
            if event.event_type != "assistant.delta" {
                file.sync_data()
                    .map_err(|_| "не sync private run journal".to_string())?;
            }
            Ok(())
        })();
        bytes.zeroize();
        result?;
        last_sequences.insert(event.run_id.clone(), event.seq);
        Ok(())
    }

    pub fn replay(
        &self,
        run_id: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<Vec<RunEvent>, String> {
        validate_run_id(run_id)?;
        if limit == 0 || limit > MAX_REPLAY_EVENTS {
            return Err("run replay limit вне допустимого диапазона".into());
        }
        self.ensure_root()?;
        let path = self.path(run_id)?;
        let file = match open_existing(&path)? {
            Some(file) => file,
            None => return Ok(Vec::new()),
        };
        let mut reader = BufReader::new(file);
        let mut events = Vec::new();
        let mut last_seq = 0;
        while let Some(mut line) = read_bounded_line(&mut reader)? {
            let parsed = serde_json::from_slice::<RunEvent>(&line)
                .map_err(|_| "private run journal содержит invalid JSON".to_string());
            line.zeroize();
            let event = parsed?;
            if event.run_id != run_id {
                return Err("private run journal содержит чужой runId".into());
            }
            if event.seq <= last_seq {
                return Err("private run journal содержит non-monotonic seq".into());
            }
            last_seq = event.seq;
            if event.seq > after_seq {
                events.push(event);
                if events.len() == limit {
                    break;
                }
            }
        }
        Ok(events)
    }

    pub fn summary(&self, run_id: &str) -> Result<Option<RunSummary>, String> {
        validate_run_id(run_id)?;
        self.ensure_root()?;
        let Some(file) = open_existing(&self.path(run_id)?)? else {
            return Ok(None);
        };
        let mut reader = BufReader::new(file);
        let mut summary: Option<RunSummary> = None;
        while let Some(mut line) = read_bounded_line(&mut reader)? {
            let parsed = serde_json::from_slice::<RunEvent>(&line)
                .map_err(|_| "private run journal содержит invalid JSON".to_string());
            line.zeroize();
            let event = parsed?;
            if event.run_id != run_id {
                return Err("private run journal содержит чужой runId".into());
            }
            let summary = summary.get_or_insert_with(|| RunSummary {
                run_id: run_id.into(),
                project_id: String::new(),
                cwd: String::new(),
                backend: event.backend,
                vm: event.vm.clone(),
                backend_session_id: None,
                last_turn_id: String::new(),
                last_seq: 0,
                state: "interrupted".into(),
            });
            if event.seq <= summary.last_seq {
                return Err("private run journal содержит non-monotonic seq".into());
            }
            summary.backend = event.backend;
            summary.vm = event.vm.clone();
            summary.last_turn_id = event.turn_id.clone();
            summary.last_seq = event.seq;
            if let Some(project_id) = event
                .payload
                .get("projectId")
                .and_then(serde_json::Value::as_str)
            {
                summary.project_id = project_id.into();
            }
            if let Some(cwd) = event.payload.get("cwd").and_then(serde_json::Value::as_str) {
                summary.cwd = cwd.into();
            }
            if let Some(session_id) = event
                .payload
                .get("backendSessionId")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
            {
                summary.backend_session_id = Some(session_id.into());
            }
            summary.state = match event.event_type.as_str() {
                "question.opened" => "waiting",
                "result.completed" => "completed",
                "run.cancelled" => "cancelled",
                "run.failed" => "failed",
                "run.interrupted" => "interrupted",
                _ => "working",
            }
            .into();
        }
        let Some(summary) = summary else {
            return Ok(None);
        };
        if summary.project_id.is_empty() || summary.cwd.is_empty() {
            return Err("private run journal не содержит project metadata".into());
        }
        Ok(Some(summary))
    }

    fn ensure_root(&self) -> Result<(), String> {
        fs::create_dir_all(&self.root)
            .map_err(|_| "не создать private runs directory".to_string())?;
        let metadata = fs::symlink_metadata(&self.root)
            .map_err(|_| "не проверить private runs directory".to_string())?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err("private runs root имеет unsafe type".into());
        }
        fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))
            .map_err(|_| "не защитить private runs directory".to_string())
    }

    fn path(&self, run_id: &str) -> Result<PathBuf, String> {
        validate_run_id(run_id)?;
        Ok(self.root.join(format!("{run_id}.jsonl")))
    }
}

pub fn validate_run_id(run_id: &str) -> Result<(), String> {
    if run_id.is_empty()
        || run_id.len() > 128
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("runId имеет unsafe format".into());
    }
    Ok(())
}

fn open_existing(path: &Path) -> Result<Option<File>, String> {
    match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => {
            let metadata = file
                .metadata()
                .map_err(|_| "не проверить private run journal".to_string())?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
                return Err("private run journal имеет unsafe type или mode".into());
            }
            Ok(Some(file))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("не открыть private run journal".into()),
    }
}

fn last_seq_from_file(path: &Path, expected_run_id: &str) -> Result<u64, String> {
    let Some(file) = open_existing(path)? else {
        return Ok(0);
    };
    let mut reader = BufReader::new(file);
    let mut last = 0;
    while let Some(mut line) = read_bounded_line(&mut reader)? {
        let parsed = serde_json::from_slice::<RunEvent>(&line)
            .map_err(|_| "private run journal содержит invalid JSON".to_string());
        line.zeroize();
        let event = parsed?;
        if event.run_id != expected_run_id || event.seq <= last {
            return Err("private run journal содержит non-monotonic seq".into());
        }
        last = event.seq;
    }
    Ok(last)
}

fn read_bounded_line<R: BufRead>(reader: &mut R) -> Result<Option<Vec<u8>>, String> {
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|_| "не прочитать private run journal".to_string())?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(available.len());
        if line.len().saturating_add(take) > MAX_JOURNAL_LINE_BYTES + 1 {
            line.zeroize();
            return Err("private run journal line превышает limit".into());
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if line.last() == Some(&b'\n') {
            line.pop();
            return Ok(Some(line));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::json;

    use super::*;
    use crate::run_event::{Backend, RunEvent};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    fn fixture() -> (PathBuf, RunStore) {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-run-store-{}-{id}",
            std::process::id()
        ));
        let store = RunStore::new(root.join("runs"));
        (root, store)
    }

    fn event(seq: u64, event_type: &str, payload: serde_json::Value) -> RunEvent {
        RunEvent {
            run_id: "run-018f000000000001".into(),
            turn_id: "turn-018f000000000002".into(),
            seq,
            at: 1_785_250_000_000 + seq as i64,
            event_type: event_type.into(),
            payload,
            backend: Backend::Claude,
            vm: "synthetic-project-a1b2c3d4e5f6".into(),
        }
    }

    #[test]
    fn append_is_owner_private_monotonic_and_replays_after_seq() {
        let (root, store) = fixture();
        store
            .append(&event(
                1,
                "run.started",
                json!({"projectId":"project-a","cwd":"/synthetic/project"}),
            ))
            .unwrap();
        store
            .append(&event(
                2,
                "run.resumed",
                json!({"backendSessionId":"018f0000-0000-7000-8000-000000000003"}),
            ))
            .unwrap();
        store
            .append(&event(3, "assistant.message", json!({"text":"готово"})))
            .unwrap();

        assert_eq!(
            fs::metadata(root.join("runs"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(root.join("runs/run-018f000000000001.jsonl"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            store
                .replay("run-018f000000000001", 1, 16)
                .unwrap()
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert!(store
            .append(&event(3, "assistant.message", json!({"text":"duplicate"})))
            .is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn summary_recovers_project_backend_session_and_terminal_state() {
        let (root, store) = fixture();
        for value in [
            event(
                1,
                "run.started",
                json!({"projectId":"project-a","cwd":"/synthetic/project"}),
            ),
            event(
                2,
                "run.resumed",
                json!({"backendSessionId":"018f0000-0000-7000-8000-000000000003"}),
            ),
            event(3, "result.completed", json!({"text":"готово"})),
        ] {
            store.append(&value).unwrap();
        }

        let summary = store.summary("run-018f000000000001").unwrap().unwrap();

        assert_eq!(summary.project_id, "project-a");
        assert_eq!(summary.cwd, "/synthetic/project");
        assert_eq!(summary.backend, Backend::Claude);
        assert_eq!(
            summary.backend_session_id.as_deref(),
            Some("018f0000-0000-7000-8000-000000000003")
        );
        assert_eq!(summary.state, "completed");
        assert_eq!(summary.last_seq, 3);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn summary_scans_past_replay_page_size_for_the_latest_resume_identity() {
        let (root, store) = fixture();
        store
            .append(&event(
                1,
                "run.started",
                json!({"projectId":"project-a","cwd":"/synthetic/project"}),
            ))
            .unwrap();
        for seq in 2..300 {
            store
                .append(&event(seq, "assistant.delta", json!({"text":"x"})))
                .unwrap();
        }
        store
            .append(&event(
                300,
                "run.resumed",
                json!({"backendSessionId":"latest-session-300"}),
            ))
            .unwrap();

        let summary = store.summary("run-018f000000000001").unwrap().unwrap();

        assert_eq!(summary.last_seq, 300);
        assert_eq!(
            summary.backend_session_id.as_deref(),
            Some("latest-session-300")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn run_id_cannot_escape_the_private_journal_root() {
        let (root, store) = fixture();
        assert!(store.replay("../outside", 0, 10).is_err());
        assert!(store.replay("bad.name", 0, 10).is_err());
        assert!(!root.join("outside.jsonl").exists());
        fs::remove_dir_all(root).ok();
    }
}
