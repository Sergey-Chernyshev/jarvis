//! Incremental observation of Codex rollouts. A Desktop session can be observed
//! without terminal hooks; observation never implies permission/ability to reply.

use crate::model::{Question, Status};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const READ_BUDGET: usize = 4 * 1024 * 1024;
const MAX_LINE: usize = 4 * 1024 * 1024;
const MAX_FILES: usize = 256;

#[cfg(test)]
#[path = "codex_live_tests.rs"]
mod tests;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Observation {
    #[serde(default)]
    identity_revision: u8,
    #[serde(default)]
    scope: crate::rollout_scope::RolloutScope,
    pub sid: String,
    pub cwd: String,
    pub originator: String,
    #[serde(default)]
    pub technical: bool,
    pub model: String,
    pub title: String,
    #[serde(default)]
    pub native_title: String,
    #[serde(default)]
    pub agent_label: String,
    pub last_prompt: String,
    pub last_reply: String,
    pub status: Status,
    pub turn_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub boundary_at: i64,
    /// State clock excludes metadata/token records, which cannot rewind hooks.
    #[serde(default)]
    pub state_at: i64,
    #[serde(default)]
    pub prompt_at: i64,
    #[serde(default)]
    pub reply_at: i64,
    #[serde(default)]
    pub answered_item: Option<String>,
    pub completed_at: Option<i64>,
    pub question: Option<Question>,
}

fn timestamp(value: &Value) -> i64 {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|v| v.timestamp_millis())
        .unwrap_or(0)
}

fn text_content(value: &Value, user: bool) -> String {
    let clean = |text: &str| {
        if user {
            crate::backend::codex_transcript::normalize_user_text(text)
        } else {
            text.to_owned()
        }
    };
    if let Some(s) = value.as_str() {
        return crate::util::ellipsize(&clean(s), 1000);
    }
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .map(clean)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .map(|s| crate::util::ellipsize(&s, 1000))
        .unwrap_or_default()
}

fn user_title(value: &Value) -> String {
    let texts: Vec<&str> = if let Some(text) = value.as_str() {
        vec![text]
    } else {
        value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item["text"].as_str())
                    .collect()
            })
            .unwrap_or_default()
    };
    let title = texts
        .into_iter()
        .map(crate::backend::codex_transcript::title_text)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    crate::util::ellipsize(&crate::util::one_line(&title), 160)
}

impl Observation {
    pub(crate) fn needs_scope_rebuild(&self) -> bool {
        self.identity_revision < 3
    }
    pub(crate) fn skip_record(&mut self) {
        self.scope.skip_record();
    }
    fn state_clock(&self) -> i64 {
        self.state_at
            .max(self.boundary_at)
            .max(self.question.as_ref().map_or(0, |q| q.at))
    }
    /// Only explicit lifecycle records determine Working/Done. Filesystem mtime
    /// is a discovery optimization, never a substitute for a provider event.
    pub fn apply(&mut self, record: &Value) {
        if !self.scope.accept(record) {
            return;
        }
        let at = timestamp(record);
        let kind = record.get("type").and_then(Value::as_str).unwrap_or("");
        let p = &record["payload"];
        if crate::backend::codex_transcript::is_technical_session_entry(record) {
            self.technical = true;
        }
        if kind == "session_meta" {
            // A fork contains its own metadata first, then copied parent
            // metadata. Only the first valid owner can name this rollout.
            if self.identity_revision >= 1 {
                return;
            }
            let Some(id) = p["id"].as_str().filter(|id| !id.is_empty()) else {
                return;
            };
            self.sid = id.into();
            self.identity_revision = 3;
            if let Some(cwd) = p["cwd"].as_str() {
                self.cwd = cwd.into();
            }
            if let Some(origin) = p["originator"].as_str().or_else(|| p["source"].as_str()) {
                self.originator = origin.into();
            }
            let created_at = timestamp(p);
            self.created_at = if created_at > 0 { created_at } else { at };
            if let Some(spawn) = p.pointer("/source/subagent/thread_spawn") {
                let nickname = p["agent_nickname"]
                    .as_str()
                    .or_else(|| spawn["agent_nickname"].as_str());
                let role = p["agent_role"]
                    .as_str()
                    .or_else(|| spawn["agent_role"].as_str());
                self.agent_label = [nickname, role]
                    .into_iter()
                    .flatten()
                    .map(crate::backend::codex_transcript::title_text)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ");
                self.agent_label =
                    crate::util::ellipsize(&crate::util::one_line(&self.agent_label), 160);
            }
        }
        if kind == "turn_context" {
            if let Some(model) = p["model"].as_str() {
                self.model = model.into();
            }
            if let Some(cwd) = p["cwd"].as_str() {
                self.cwd = cwd.into();
            }
        }
        if self.technical {
            self.title.clear();
            self.native_title.clear();
            self.agent_label.clear();
            self.last_prompt.clear();
            self.last_reply.clear();
            self.question = None;
            self.updated_at = self.updated_at.max(at);
            return;
        }
        if kind == "event_msg" {
            let event = p["type"].as_str().unwrap_or("");
            let turn = p["turn_id"].as_str().or_else(|| p["turnId"].as_str());
            match event {
                "task_started" | "turn_started" => {
                    if at >= self.boundary_at {
                        self.status = Status::Working;
                        self.turn_id = turn.map(String::from);
                        self.boundary_at = at;
                        self.state_at = at;
                        self.completed_at = None;
                        self.question = None;
                        self.answered_item = None;
                    }
                }
                "task_complete" | "turn_completed" | "turn_aborted" | "task_aborted" => {
                    let current = turn
                        .zip(self.turn_id.as_deref())
                        .map_or(true, |(a, b)| a == b);
                    if current && at >= self.boundary_at {
                        self.status = if event.ends_with("aborted") {
                            Status::Idle
                        } else {
                            Status::Done
                        };
                        self.boundary_at = at;
                        self.state_at = at;
                        self.completed_at = Some(at);
                        self.question = None;
                        if let Some(text) = p["last_agent_message"].as_str() {
                            self.last_reply = crate::util::ellipsize(text, 1000);
                        }
                    }
                }
                "user_message" => {
                    let text = text_content(&p["message"], true);
                    if !text.is_empty() && at >= self.prompt_at && at >= self.boundary_at {
                        if crate::backend::codex_transcript::needs_title_repair(&self.title) {
                            self.title = user_title(&p["message"]);
                        }
                        self.last_prompt = text;
                        self.prompt_at = at;
                    }
                }
                "agent_message" => {
                    let text = text_content(&p["message"], false);
                    if !text.is_empty() && at >= self.reply_at && at >= self.boundary_at {
                        self.last_reply = text;
                        self.reply_at = at;
                    }
                }
                "request_user_input" => self.observe_question(p, Value::Null, at),
                _ => {}
            }
        }
        if kind == "response_item" {
            let item_turn = p["internal_chat_message_metadata_passthrough"]["turn_id"].as_str();
            if item_turn
                .zip(self.turn_id.as_deref())
                .is_some_and(|(incoming, current)| incoming != current)
            {
                return;
            }
            match p["type"].as_str().unwrap_or("") {
                "message" => {
                    let text = text_content(&p["content"], p["role"] == "user");
                    if p["role"] == "user"
                        && !text.is_empty()
                        && at >= self.prompt_at
                        && at >= self.boundary_at
                    {
                        if crate::backend::codex_transcript::needs_title_repair(&self.title) {
                            self.title = user_title(&p["content"]);
                        }
                        self.last_prompt = text;
                        self.prompt_at = at;
                    } else if p["role"] == "assistant"
                        && !text.is_empty()
                        && at >= self.reply_at
                        && at >= self.boundary_at
                    {
                        self.last_reply = text;
                        self.reply_at = at;
                    }
                }
                "function_call" if p["name"] == "request_user_input" => {
                    if let Some(args) = p["arguments"]
                        .as_str()
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    {
                        self.observe_question(&args, p["call_id"].clone(), at);
                    }
                }
                "function_call_output" => {
                    let call = p["call_id"].as_str();
                    if self.question.as_ref().is_some_and(|q| {
                        q.provider_item_id.as_deref() == call
                            && call.is_some()
                            && (at == 0 || at >= q.at)
                    }) {
                        let output = p["output"]
                            .as_str()
                            .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
                        if output
                            .as_ref()
                            .and_then(|v| v["answers"].as_object())
                            .is_some_and(|answers| !answers.is_empty())
                        {
                            self.answered_item = call.map(String::from);
                        }
                        self.question = None;
                        self.status = Status::Working;
                        self.state_at = self.state_at.max(at);
                    }
                }
                _ => {}
            }
        }
        self.updated_at = self.updated_at.max(at);
    }

    fn observe_question(&mut self, params: &Value, call: Value, at: i64) {
        if at < self.boundary_at || at < self.state_at || matches!(self.status, Status::Done) {
            return;
        }
        if params
            .get("turnId")
            .and_then(Value::as_str)
            .zip(self.turn_id.as_deref())
            .is_some_and(|(incoming, current)| incoming != current)
        {
            return;
        }
        let mut params = params.clone();
        let Some(object) = params.as_object_mut() else {
            return;
        };
        object
            .entry("threadId")
            .or_insert_with(|| Value::String(self.sid.clone()));
        object
            .entry("turnId")
            .or_insert_with(|| Value::String(self.turn_id.clone().unwrap_or_default()));
        object.entry("itemId").or_insert_with(|| call.clone());
        object.entry("isBlocking").or_insert(Value::Bool(true));
        if let Ok(mut question) = crate::question_delivery::from_codex_request(&params, call, false)
        {
            question.at = at;
            self.question = Some(question);
            self.answered_item = None;
            self.status = Status::Waiting;
            self.state_at = at;
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Cursor {
    // Offset at a complete newline: incomplete appends are retried after restart.
    committed: u64,
    inode: u64,
    modified_ns: u128,
    initialized: bool,
    observation: Observation,
    #[serde(skip)]
    read_offset: u64,
    #[serde(skip)]
    pending: Vec<u8>,
    #[serde(skip)]
    skipping_line: bool,
}

#[derive(Clone, Debug)]
pub struct Update {
    pub remote: Option<String>,
    pub instance_id: String,
    pub instance_label: String,
    pub provider_home: String,
    pub path: PathBuf,
    pub state: Observation,
    pub bootstrap: bool,
    pub previous: Option<Observation>,
}

pub struct Monitor {
    cursors: HashMap<PathBuf, Cursor>,
    files: Vec<(String, String, String, PathBuf)>,
    persistence: PathBuf,
    discovery_at: i64,
    titles: crate::codex_titles::TitleCache,
}

pub fn start(d: std::sync::Arc<crate::daemon::Daemon>) {
    tauri::async_runtime::spawn(async move {
        let mut monitor = Monitor::new(&crate::util::jarvis_dir());
        let mut persisted_at = 0;
        loop {
            let now = crate::util::now_ms();
            let result = tokio::task::spawn_blocking(move || {
                let updates = crate::session_identity::registry()
                    .map(|registry| monitor.scan(&registry, now));
                if now - persisted_at >= 30_000 {
                    let _ = monitor.persist();
                    persisted_at = now;
                }
                (monitor, persisted_at, updates)
            })
            .await;
            let Ok((next, saved_at, updates)) = result else {
                return;
            };
            monitor = next;
            persisted_at = saved_at;
            if let Ok(updates) = updates {
                for update in updates {
                    apply_update(&d, update);
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
}

fn observation_is_current(session: &crate::model::Session, observed: &Observation) -> bool {
    let clock = session.provider_event_at.unwrap_or(0);
    if observed.state_clock() < clock {
        return false;
    }
    let different_turn = observed
        .turn_id
        .as_deref()
        .zip(session.provider_turn_id.as_deref())
        .is_some_and(|(incoming, current)| incoming != current);
    if different_turn && observed.boundary_at < clock {
        return false;
    }
    // Metadata or a delayed tool must not reopen an explicitly completed turn.
    if session
        .done_at
        .is_some_and(|done| observed.boundary_at <= done)
        && matches!(observed.status, Status::Working | Status::Waiting)
    {
        return false;
    }
    true
}

fn apply_observed_state(session: &mut crate::model::Session, observed: &Observation) -> bool {
    if !observation_is_current(session, observed) {
        return false;
    }
    session.provider_turn_id = observed.turn_id.clone();
    session.status = observed.status;
    session.done_at = observed.completed_at;
    session.detail = match observed.status {
        Status::Working => "Агент работает",
        Status::Waiting => "Нужен ответ",
        Status::Done => "Готово",
        _ => "",
    }
    .into();
    session.question = observed.question.clone();
    if session.tmux_pane.is_some() {
        session.control_mode = Some("tmux".into());
        if let Some(question) = &mut session.question {
            question.transport = "tmux".into();
        }
    }
    session.provider_event_at = Some(
        observed
            .state_clock()
            .max(session.provider_event_at.unwrap_or(0)),
    );
    true
}

/// Presentation repairs do not advance the hook/provider state clock. A recent
/// hook can supersede lifecycle records while an older rollout still contains
/// the genuine task needed to replace a cached generated-context title.
fn repair_display_text(
    session: &mut crate::model::Session,
    observed: &Observation,
    previous: Option<&Observation>,
) -> bool {
    let before = (session.title.clone(), session.last_prompt.clone());
    let preferred = [
        &observed.native_title,
        &observed.title,
        &observed.agent_label,
    ]
    .into_iter()
    .find(|s| !s.is_empty());
    let current = session.title.as_deref().unwrap_or("");
    let known_generated = (!observed.title.is_empty()
        && (current == observed.title || current == crate::util::ellipsize(&observed.title, 60)))
        || (!observed.agent_label.is_empty() && current == observed.agent_label)
        || previous.is_some_and(|old| !old.native_title.is_empty() && current == old.native_title);
    if crate::backend::codex_transcript::needs_title_repair(current) || known_generated {
        session.title = preferred.cloned();
    }
    if session
        .last_prompt
        .as_deref()
        .is_some_and(crate::backend::codex_transcript::needs_title_repair)
    {
        session.last_prompt =
            (!observed.last_prompt.is_empty()).then(|| observed.last_prompt.clone());
    }
    before != (session.title.clone(), session.last_prompt.clone())
}

pub(crate) fn apply_update(d: &std::sync::Arc<crate::daemon::Daemon>, update: Update) {
    use crate::model::Session;
    let o = &update.state;
    if o.sid.is_empty() || o.updated_at <= 0 {
        return;
    }
    let sid = crate::session_identity::key(
        &update.instance_id,
        &update.provider_home,
        &o.sid,
        update.remote.as_deref(),
    );
    if o.technical {
        let mut sessions = d.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let mut removed = sessions.remove(&sid).is_some();
        if update.remote.is_none()
            && sid != o.sid
            && sessions
                .get(&o.sid)
                .is_some_and(|session| session.transcript.as_deref() == update.path.to_str())
        {
            removed |= sessions.remove(&o.sid).is_some();
        }
        drop(sessions);
        if removed {
            d.push();
        }
        return;
    }
    let previous = d.session(&sid);
    // Hooks with a live terminal remain the control authority. Rollouts fill
    // missed lifecycle boundaries, but cannot rewind a more recent hook.
    if previous
        .as_ref()
        .and_then(|s| s.provider_event_at)
        .is_some_and(|at| at > o.updated_at)
    {
        let repaired = d
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&sid)
            .is_some_and(|session| repair_display_text(session, o, update.previous.as_ref()));
        if repaired {
            d.push();
        }
        return;
    }
    let accepts_state = previous
        .as_ref()
        .map_or(true, |session| observation_is_current(session, o));
    let was_waiting = previous.as_ref().is_some_and(|s| s.question.is_some());
    let question_changed = previous
        .as_ref()
        .and_then(|s| s.question.as_ref())
        .map(|q| (&q.request_id, q.revision))
        != o.question.as_ref().map(|q| (&q.request_id, q.revision));
    let fresh_completion = !update.bootstrap
        && o.status == Status::Done
        && accepts_state
        && previous.as_ref().is_some_and(|s| {
            s.status != Status::Done
                || s.provider_turn_id != o.turn_id
                || s.done_at != o.completed_at
        });
    let applied;
    {
        let mut sessions = d.sessions.lock().unwrap_or_else(|e| e.into_inner());
        // Migrate a legacy row only when the transcript proves its owner.
        if update.remote.is_none()
            && sid != o.sid
            && sessions
                .get(&o.sid)
                .is_some_and(|s| s.transcript.as_deref() == update.path.to_str())
        {
            if let Some(mut old) = sessions.remove(&o.sid) {
                old.id = sid.clone();
                sessions.entry(sid.clone()).or_insert(old);
            }
        }
        let s = sessions
            .entry(sid.clone())
            .or_insert_with(|| Session::new(sid.clone(), o.created_at));
        s.agent = Some("codex".into());
        s.instance_id = Some(update.instance_id.clone());
        s.instance_label = Some(update.instance_label.clone());
        s.provider_home = Some(update.provider_home.clone());
        s.provider_session_id = Some(o.sid.clone());
        s.transcript = Some(update.path.to_string_lossy().into_owned());
        s.remote = update.remote.clone();
        s.cwd = Some(o.cwd.clone());
        s.project = Some(crate::util::basename(&o.cwd));
        repair_display_text(s, o, update.previous.as_ref());
        if !o.last_prompt.is_empty() && o.prompt_at >= s.provider_event_at.unwrap_or(0) {
            s.last_prompt = Some(o.last_prompt.clone());
        }
        if !o.model.is_empty()
            && s.model_at
                .map_or(true, |at| crate::util::now_ms() - at > 30_000)
        {
            s.model = Some(o.model.clone());
        }
        if s.tmux_pane.is_none() {
            s.monitor_source = Some("rollout".into());
            s.control_mode = Some("external".into());
            s.host = Some(
                if o.originator.contains("desktop") {
                    "Codex Desktop"
                } else {
                    "Codex"
                }
                .into(),
            );
        }
        applied = apply_observed_state(s, o);
        // The reducer owns completion effects, including de-duplication. A
        // new completed turn can be observed between polls while the old row
        // still says Done, so expose its Working predecessor to that reducer.
        if fresh_completion && applied {
            s.status = Status::Working;
        }
        s.updated_at = s.updated_at.max(o.updated_at);
    }
    if applied {
        if let Some(question) = previous.as_ref().and_then(|s| s.question.as_ref()) {
            if question
                .provider_item_id
                .as_ref()
                .is_some_and(|item| Some(item) == o.answered_item.as_ref())
            {
                crate::question_delivery::confirm(&sid, question);
            }
        }
    }
    if fresh_completion {
        d.reduce(&serde_json::json!({"agent":"codex","event":"stop","monitorSource":"rollout", "remote":update.remote,
            "providerHome":update.provider_home,"instanceId":update.instance_id,"providerAt":o.completed_at.unwrap_or(o.state_at),
            "payload":{"session_id":o.sid,"turn_id":o.turn_id,"cwd":o.cwd,
                "transcript_path":update.path,"last_assistant_message":o.last_reply}}));
    } else {
        d.push();
    }
    if applied && !update.bootstrap && question_changed && o.question.is_some() {
        d.notify_id(
            &format!("q-{sid}"),
            &format!("{} · нужен ответ", update.instance_label),
            o.question
                .as_ref()
                .and_then(|q| q.questions.first())
                .map(|q| q.question.as_str())
                .unwrap_or("Откройте чат агента"),
            Some(&sid),
            "waiting",
        );
    } else if applied && was_waiting && o.question.is_none() {
        crate::windows::toast_remove(d, &format!("q-{sid}"));
    }
}

impl Monitor {
    pub fn new(data_dir: &Path) -> Self {
        let persistence = data_dir.join("codex-observations.json");
        let cursors = fs::metadata(&persistence)
            .ok()
            .filter(|m| m.len() <= 8 * 1024 * 1024)
            .and_then(|_| fs::read(&persistence).ok())
            .and_then(|bytes| serde_json::from_slice::<HashMap<PathBuf, Cursor>>(&bytes).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|(path, mut cursor)| {
                // Old cursors may already contain a parent's SID from copied
                // fork metadata. Replay from the beginning, silently.
                if cursor.observation.needs_scope_rebuild() {
                    cursor = Cursor::default();
                }
                cursor.read_offset = cursor.committed;
                // Restoring the in-memory session after an app restart is silent.
                cursor.initialized = false;
                (path, cursor)
            })
            .collect();
        Self {
            cursors,
            files: Vec::new(),
            persistence,
            discovery_at: 0,
            titles: crate::codex_titles::TitleCache::default(),
        }
    }

    pub fn scan(&mut self, registry: &crate::agent_instances::Registry, now: i64) -> Vec<Update> {
        if self.discovery_at == 0 || now - self.discovery_at >= 15_000 {
            self.discovery_at = now;
            let mut found = Vec::new();
            for root in registry.roots(false) {
                let Some(instance) = registry.instance_for_transcript(&root.path) else {
                    continue;
                };
                let mut paths = Vec::new();
                discover_files(&root.path, &root.path, 0, &mut paths);
                for (mtime, path) in paths {
                    found.push((
                        mtime,
                        root.instance_id.clone(),
                        instance.label.clone(),
                        root.home.to_string_lossy().into_owned(),
                        path,
                    ));
                }
            }
            found.sort_by(|a, b| b.0.cmp(&a.0).then(a.4.cmp(&b.4)));
            let mut seen = HashSet::new();
            self.files = found
                .into_iter()
                .filter(|v| seen.insert(v.4.clone()))
                .take(MAX_FILES)
                .map(|(_, i, l, h, p)| (i, l, h, p))
                .collect();
            let tracked: HashSet<_> = self.files.iter().map(|v| v.3.clone()).collect();
            self.cursors.retain(|p, _| tracked.contains(p));
        }
        let mut scanned = Vec::new();
        let mut total_bytes = 0;
        for (id, label, home, path) in &self.files {
            // Settings can disable an instance before the next directory
            // refresh. Never resurrect it from the cached transcript list.
            if registry.resolve(Some(id)).is_err() {
                continue;
            }
            if total_bytes >= 64 * 1024 * 1024 {
                break;
            }
            let cursor = self.cursors.entry(path.clone()).or_default();
            match read_increment(cursor, path) {
                Ok((bytes, changed, bootstrap, previous)) => {
                    total_bytes += bytes;
                    if cursor.initialized && !cursor.observation.sid.is_empty() {
                        scanned.push((
                            id.clone(),
                            label.clone(),
                            home.clone(),
                            path.clone(),
                            changed,
                            bootstrap,
                            previous,
                        ));
                    }
                }
                Err(_) => {} // A transient inaccessible/rotated file is retried; it is not a Done event.
            }
        }
        let mut requests: HashMap<PathBuf, Vec<(String, PathBuf)>> = HashMap::new();
        for (_, _, home, path, _, _, _) in &scanned {
            let observation = &self.cursors[path].observation;
            if !observation.technical {
                requests
                    .entry(PathBuf::from(home))
                    .or_default()
                    .push((observation.sid.clone(), path.clone()));
            }
        }
        self.titles.retain_homes(
            &registry
                .roots(false)
                .into_iter()
                .map(|root| root.home)
                .collect(),
        );
        for (home, requested) in requests {
            self.titles.refresh(&home, &requested);
        }
        let mut updates = Vec::new();
        for (id, label, home, path, changed, bootstrap, mut previous) in scanned {
            let observation = &mut self.cursors.get_mut(&path).unwrap().observation;
            let native = self
                .titles
                .get(Path::new(&home), &observation.sid)
                .map(str::to_owned);
            let title_changed = !observation.technical
                && native
                    .as_ref()
                    .is_some_and(|title| *title != observation.native_title);
            if title_changed {
                if previous.is_none() {
                    previous = Some(observation.clone());
                }
                observation.native_title = native.unwrap();
            }
            if changed || title_changed {
                updates.push(Update {
                    remote: None,
                    instance_id: id,
                    instance_label: label,
                    provider_home: home,
                    path,
                    state: observation.clone(),
                    bootstrap,
                    previous,
                });
            }
        }
        updates
    }

    pub fn persist(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec(&self.cursors).map_err(|e| e.to_string())?;
        let tmp = self
            .persistence
            .with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&tmp, bytes)
            .and_then(|_| fs::rename(&tmp, &self.persistence))
            .map_err(|e| e.to_string())
    }
}

fn discover_files(
    root: &Path,
    dir: &Path,
    depth: usize,
    found: &mut Vec<(std::time::SystemTime, PathBuf)>,
) {
    if depth > 4 || found.len() >= 20_000 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        let path = entry.path();
        if kind.is_dir() {
            discover_files(root, &path, depth + 1, found);
        } else if kind.is_file()
            && path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
        {
            if let Ok(path) = path.canonicalize() {
                if path.starts_with(root.canonicalize().unwrap_or_else(|_| root.to_owned())) {
                    if let Ok(at) = entry.metadata().and_then(|m| m.modified()) {
                        found.push((at, path));
                    }
                }
            }
        }
    }
}

fn read_increment(
    cursor: &mut Cursor,
    path: &Path,
) -> Result<(usize, bool, bool, Option<Observation>), String> {
    use std::os::unix::fs::MetadataExt;
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let replaced = cursor.inode != 0
        && (meta.ino() != cursor.inode
            || meta.len() < cursor.read_offset
            || (meta.len() == cursor.read_offset && cursor.modified_ns != modified));
    if replaced {
        *cursor = Cursor::default();
    }
    cursor.inode = meta.ino();
    let bootstrap = !cursor.initialized;
    let previous = if cursor.initialized {
        Some(cursor.observation.clone())
    } else {
        None
    };
    file.seek(SeekFrom::Start(cursor.read_offset))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(READ_BUDGET as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let count = bytes.len();
    for chunk in bytes.split_inclusive(|b| *b == b'\n') {
        cursor.read_offset += chunk.len() as u64;
        if !cursor.skipping_line {
            if cursor.pending.len() + chunk.len() <= MAX_LINE {
                cursor.pending.extend_from_slice(chunk);
            } else {
                cursor.pending.clear();
                cursor.skipping_line = true;
            }
        }
        if chunk.last() == Some(&b'\n') {
            if !cursor.skipping_line {
                if let Ok(value) = serde_json::from_slice::<Value>(&cursor.pending) {
                    cursor.observation.apply(&value);
                } else {
                    cursor.observation.skip_record();
                }
            } else {
                cursor.observation.skip_record();
            }
            cursor.pending.clear();
            cursor.skipping_line = false;
            cursor.committed = cursor.read_offset;
        }
    }
    cursor.modified_ns = modified;
    let caught_up = cursor.read_offset >= meta.len();
    if bootstrap && !caught_up {
        return Ok((count, false, true, None));
    }
    cursor.initialized = true;
    let changed = bootstrap
        || previous.as_ref().is_some_and(|p| {
            p.updated_at != cursor.observation.updated_at
                || p.technical != cursor.observation.technical
                || p.status != cursor.observation.status
                || p.turn_id != cursor.observation.turn_id
                || p.last_reply != cursor.observation.last_reply
                || p.question.as_ref().map(|q| (&q.request_id, q.revision))
                    != cursor
                        .observation
                        .question
                        .as_ref()
                        .map(|q| (&q.request_id, q.revision))
        });
    Ok((count, changed, bootstrap, previous))
}
