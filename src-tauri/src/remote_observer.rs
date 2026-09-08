//! Remote transcript observation uses the same Codex lifecycle parser as local
//! Desktop imports. Hooks/tmux remain the control authority. Bootstrap and
//! reconnect catch-up never replay historical notifications.
use crate::{codex_live::{Observation, Update}, daemon::Daemon, model::Session};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const MAX_FILES: usize = 256;
const MAX_BYTES_PER_NODE: usize = 2 * 1024 * 1024;
const MAX_LINE: usize = 1024 * 1024;

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Cursor {
    offset: u64,
    initialized: bool,
    file_id: String,
    modified: u64,
    observation: Observation,
    pending: String,
    skipped_line: bool,
    replay_until: u64,
}
impl Cursor {
    fn reset_if_replaced(&mut self, row: &Value) {
        let size = row["size"].as_u64().unwrap_or(0);
        let id = row["fileId"].as_str().unwrap_or("");
        let at = row["at"].as_u64().unwrap_or(0);
        if size < self.offset || (!self.file_id.is_empty() && !id.is_empty() && self.file_id != id)
            || (size == self.offset && self.modified != 0 && self.modified != at) {
            *self = Self::default();
        }
        self.file_id = id.into(); self.modified = at;
    }
    fn consume(&mut self, data: &str) {
        for fragment in data.split_inclusive('\n') {
            if !self.skipped_line {
                if self.pending.len() + fragment.len() <= MAX_LINE { self.pending.push_str(fragment); }
                else { self.pending.clear(); self.skipped_line = true; }
            }
            if fragment.ends_with('\n') {
                if !self.skipped_line {
                    if let Ok(value) = serde_json::from_str::<Value>(&self.pending) { self.observation.apply(&value); }
                    else { self.observation.skip_record(); }
                } else { self.observation.skip_record(); }
                self.pending.clear(); self.skipped_line = false;
            }
        }
    }
}

pub fn start(d: Arc<Daemon>) {
    tauri::async_runtime::spawn(async move {
        let path = crate::util::jarvis_dir().join("remotes/observations.json");
        let mut cursors: HashMap<String, Cursor> = std::fs::read(&path).ok()
            .filter(|bytes| bytes.len() <= 32 * 1024 * 1024)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        for cursor in cursors.values_mut() {
            if cursor.observation.needs_scope_rebuild() { *cursor = Cursor::default(); }
        }
        let mut connected = HashSet::new();
        loop {
            let mut live = HashSet::new();
            for node in d.remotes.all() {
                let status = node.status();
                if !status.connected || status.protocol < 2 { connected.remove(&node.cfg.name); continue; }
                let name = node.cfg.name.clone(); live.insert(name.clone());
                let bootstrap = connected.insert(name.clone());
                let Ok(client) = node.client() else { continue; };
                let Ok(catalog) = client.sessions().await else { connected.remove(&name); continue; };
                let Some(rows) = catalog["sessions"].as_array() else { continue; };
                if bootstrap {
                    for row in rows.iter().take(MAX_FILES) {
                        if let Some((_,source,_,file)) = row_identity(row) {
                            cursors.entry(format!("{name}\0{source}\0{file}")).or_default().replay_until = row["size"].as_u64().unwrap_or(0);
                        }
                    }
                }
                let mut budget = MAX_BYTES_PER_NODE;
                for row in rows.iter().take(MAX_FILES) {
                    let Some((id, source, home, file)) = row_identity(row) else { continue; };
                    if row["agent"] == "claude" { import_claude_metadata(&d, &name, row); continue; }
                    if row["agent"] != "codex" { continue; }
                    let key = format!("{name}\0{source}\0{file}");
                    let cursor = cursors.entry(key).or_default();
                    cursor.reset_if_replaced(row);
                    let mut silent = bootstrap || !cursor.initialized || cursor.replay_until > 0;
                    let previous = cursor.initialized.then(|| cursor.observation.clone());
                    let mut changed = false;
                    let mut caught_up = cursor.offset >= row["size"].as_u64().unwrap_or(0);
                    while budget > 0 && !caught_up {
                        let chunk = match client.file(file, cursor.offset).await { Ok(Some(chunk)) => chunk, _ => break };
                        if chunk.from != cursor.offset { *cursor = Cursor::default(); silent = true; break; }
                        if chunk.next <= cursor.offset { break; }
                        budget = budget.saturating_sub(chunk.data.len());
                        cursor.consume(&chunk.data); cursor.offset = chunk.next;
                        caught_up = chunk.eof; changed = true;
                    }
                    // Never publish an intermediate state while first reading a
                    // large historical transcript: its completion may be later.
                    if !caught_up && !cursor.initialized { continue; }
                    if cursor.observation.sid.is_empty() { cursor.observation.sid = id.into(); }
                    if cursor.observation.cwd.is_empty() { cursor.observation.cwd = row["cwd"].as_str().unwrap_or("").into(); }
                    if changed || (silent && caught_up) {
                        let label = format!("{} · {}",name,home.rsplit('/').next().unwrap_or("Codex"));
                        crate::codex_live::apply_update(&d, Update { remote:Some(name.clone()), instance_id:source.into(), instance_label:label,
                            provider_home:home.into(),path:file.into(),state:cursor.observation.clone(),bootstrap:silent,previous });
                    }
                    if caught_up { cursor.initialized = true; }
                    if cursor.offset >= cursor.replay_until { cursor.replay_until = 0; }
                    if budget == 0 { break; }
                }
            }
            connected.retain(|name| live.contains(name));
            // Do not keep copied content from nodes the user has removed.
            let configured: HashSet<_> = d.remotes.all().iter().map(|node| node.cfg.name.clone()).collect();
            cursors.retain(|key,_| key.split('\0').next().is_some_and(|name| configured.contains(name)));
            if cursors.len() > 2048 { cursors.retain(|_,cursor| cursor.observation.updated_at >= crate::util::now_ms() - 30 * 86_400_000); }
            if let Ok(body) = serde_json::to_string(&cursors) { let _ = crate::stt::transcripts::write_private_atomic(&path,body.as_bytes()); }
            tokio::time::sleep(std::time::Duration::from_secs(7)).await;
        }
    });
}

fn row_identity(row: &Value) -> Option<(&str,&str,&str,&str)> {
    let id = row["id"].as_str()?.trim(); let source = row["sourceId"].as_str()?.trim();
    let home = row["providerHome"].as_str()?; let path = row["path"].as_str()?;
    if id.is_empty() || source.is_empty() || !home.starts_with('/') || !path.starts_with('/') { return None; }
    Some((id,source,home,path))
}
fn import_claude_metadata(d: &Arc<Daemon>, remote: &str, row: &Value) {
    let Some((id, source, home, path)) = row_identity(row) else { return; };
    let sid = format!("{remote}:{id}");
    if d.session(&sid).is_some() { return; }
    let at = row["at"].as_i64().unwrap_or_else(crate::util::now_ms);
    let mut session = Session::new(sid.clone(),at);
    session.remote = Some(remote.into()); session.agent = Some("claude".into());
    session.instance_id = Some(source.into()); session.provider_home = Some(home.into()); session.provider_session_id = Some(id.into());
    session.transcript = Some(path.into()); session.cwd = row["cwd"].as_str().map(str::to_string);
    session.project = session.cwd.as_deref().map(crate::util::basename);
    session.monitor_source = Some("remote-transcript".into()); session.control_mode = Some("external".into());
    { d.sessions.lock().unwrap_or_else(|e|e.into_inner()).entry(sid.clone()).or_insert(session); }
    d.refresh_meta(sid); d.push();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn partial_remote_lines_do_not_publish_partial_lifecycle() {
        let mut c = Cursor::default();
        c.consume("{\"timestamp\":\"2026-09-05T00:00:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"turn\"}}");
        assert_eq!(c.observation.status,crate::model::Status::Idle);
        c.consume("\n"); assert_eq!(c.observation.status,crate::model::Status::Working);
        c.consume("{\"timestamp\":\"2026-09-05T00:00:01Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"turn\"}}\n");
        assert_eq!(c.observation.status,crate::model::Status::Done);
    }
    #[test] fn replacement_resets_remote_observation_and_bootstrap() {
        let mut c = Cursor { offset:200,initialized:true,file_id:"old".into(),..Default::default() };
        c.reset_if_replaced(&serde_json::json!({"fileId":"new","size":300,"at":1}));
        assert!(!c.initialized); assert_eq!(c.offset,0);
    }
    #[test] fn remote_fork_chunks_preserve_first_owner_and_skip_inherited_completion() {
        let record=|ordinal,kind,payload| format!("{}\n",serde_json::json!({"ordinal":ordinal,"type":kind,"timestamp":"2026-09-05T01:20:40.906Z","payload":payload}));
        let mut c=Cursor::default();
        c.consume(&record(0,"session_meta",serde_json::json!({"id":"child","cwd":"/child","subagent_history_start_ordinal":3})));
        c.consume(&record(1,"session_meta",serde_json::json!({"id":"parent","cwd":"/parent"})));
        c.consume(&record(2,"event_msg",serde_json::json!({"type":"task_complete","turn_id":"parent-turn"})));
        assert_eq!(c.observation.sid,"child");assert_eq!(c.observation.cwd,"/child");assert_eq!(c.observation.status,crate::model::Status::Idle);
        let mut c:Cursor=serde_json::from_value(serde_json::to_value(c).unwrap()).unwrap();
        c.consume(&record(3,"event_msg",serde_json::json!({"type":"task_started","turn_id":"child-turn"})));
        assert_eq!(c.observation.sid,"child");assert_eq!(c.observation.status,crate::model::Status::Working);
    }
}
