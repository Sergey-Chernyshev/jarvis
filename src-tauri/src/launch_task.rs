//! Bind a launch to its first session before delivering the initial composer
//! message. Remote launches use the returned pane; local launches serialize
//! pending bindings per project and refuse ambiguous hook snapshots.

use crate::{daemon::Daemon, model::Session};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

type LocalKey = (String, String);
static LOCAL_PENDING: OnceLock<Mutex<HashSet<LocalKey>>> = OnceLock::new();
const TERMINAL_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_LAUNCH_TERMINALS: usize = 64;
static TERMINALS: OnceLock<Mutex<TerminalRegistry>> = OnceLock::new();

struct LaunchTerminal {
    snapshot: Session,
    registered: Instant,
}

#[derive(Default)]
struct TerminalRegistry {
    entries: HashMap<String, LaunchTerminal>,
}

impl TerminalRegistry {
    fn prune(&mut self, now: Instant) {
        self.entries.retain(|_, entry| now.saturating_duration_since(entry.registered) < TERMINAL_TTL);
    }

    fn insert(&mut self, snapshot: Session, now: Instant) -> Result<String, String> {
        self.prune(now);
        if self.entries.contains_key(&snapshot.id) {
            return Err("Терминал этого запуска уже зарегистрирован".into());
        }
        if self.entries.len() >= MAX_LAUNCH_TERMINALS {
            if let Some(oldest) = self.entries.iter().min_by_key(|(_, entry)| entry.registered).map(|(id, _)| id.clone()) {
                self.entries.remove(&oldest);
            }
        }
        let id = snapshot.id.clone();
        self.entries.insert(id.clone(), LaunchTerminal { snapshot, registered: now });
        Ok(id)
    }

    fn get(&mut self, id: &str, now: Instant) -> Option<Session> {
        self.prune(now);
        self.entries.get(id).map(|entry| entry.snapshot.clone())
    }
}

/// Terminal-only launch handles bridge onboarding before a provider emits its
/// first hook. They never become daemon sessions or writable chat transcripts.
pub(crate) fn terminal_session(id: &str) -> Option<Session> {
    TERMINALS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).get(id, Instant::now())
}

fn pending() -> &'static Mutex<HashSet<LocalKey>> {
    LOCAL_PENDING.get_or_init(|| Mutex::new(HashSet::new()))
}

struct LocalReservation(LocalKey);

impl Drop for LocalReservation {
    fn drop(&mut self) {
        pending().lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
    }
}

pub struct LaunchTask {
    pub id: String,
    machine: String,
    cwd: String,
    agent: String,
    instance_id: Option<String>,
    model: Option<String>,
    resume: Option<String>,
    since: i64,
    task: Option<String>,
    _reservation: Option<LocalReservation>,
    _continuation: Option<LocalReservation>,
}

impl LaunchTask {
    /// Call before launching, not after spawn: the first hook can arrive
    /// before AppleScript or the node's launch HTTP request has returned.
    pub fn prepare(machine: &str, cwd: &str, agent: &str, resume: Option<&str>, task: Option<String>) -> Result<Self, String> {
        let machine = if machine == "local" { "" } else { machine }.to_string();
        let cwd = normalize_cwd(cwd);
        let reservation = if machine.is_empty() {
            let key = (cwd.clone(), agent.to_string());
            let mut pending = pending().lock().unwrap_or_else(|e| e.into_inner());
            if !pending.insert(key.clone()) {
                return Err("Предыдущая сессия этого проекта ещё подключается. Дождись её появления и повтори запуск".into());
            }
            Some(LocalReservation(key))
        } else { None };
        let since = crate::util::now_ms();
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(Self {
            id: format!("launch-{since}-{}-{sequence}", std::process::id()), machine, cwd,
            agent: agent.to_string(), instance_id: None, model: None, resume: resume.map(str::to_string), since,
            task: task.map(|text| text.trim().to_string()).filter(|text| !text.is_empty()),
            _reservation: reservation, _continuation: None,
        })
    }

    pub fn with_model(mut self, model: Option<String>) -> Self { self.model = model; self }

    pub fn for_instance(mut self, instance_id: Option<String>) -> Self {
        self.instance_id = instance_id; self
    }

    pub fn continuing(mut self, source_id: &str) -> Result<Self, String> {
        let key = ("continuation".into(), source_id.to_string());
        if !pending().lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone()) {
            return Err("Продолжение этого чата уже подключается".into());
        }
        self._continuation = Some(LocalReservation(key));
        Ok(self)
    }

    /// Call only after an explicit continuation successfully created this pane.
    /// The renderer receives the opaque launch ID, never authority to choose a
    /// pane, account, or machine. Keeping this apart from daemon state prevents
    /// provisional handles from being mistaken for provider conversation IDs.
    pub fn register_terminal(&self, pane: &str, provider_home: Option<String>) -> Result<String, String> {
        if self._continuation.is_none() {
            return Err("Терминал запуска доступен только для продолжения чата".into());
        }
        let snapshot = self.terminal_snapshot(pane, provider_home)?;
        TERMINALS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).insert(snapshot, Instant::now())
    }

    fn terminal_snapshot(&self, pane: &str, provider_home: Option<String>) -> Result<Session, String> {
        if !pane.strip_prefix('%').is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && id.parse::<u32>().is_ok()) {
            return Err("Терминал не вернул корректную пану нового запуска".into());
        }
        let mut session = Session::new(self.id.clone(), self.since);
        session.cwd = Some(self.cwd.clone());
        session.agent = Some(self.agent.clone());
        session.instance_id = self.instance_id.clone();
        session.provider_home = provider_home;
        session.remote = (!self.machine.is_empty()).then(|| self.machine.clone());
        session.control_mode = Some("managed".into());
        session.tmux_pane = Some(pane.into());
        Ok(session)
    }

    fn select<'a>(&self, sessions: &'a [Session], pane: Option<&str>) -> Result<Option<&'a Session>, String> {
        if !self.machine.is_empty() && pane.is_none() {
            return Err("Узел не вернул пану запущенного агента — обнови jarvis-node. Сообщение не отправлено".into());
        }
        let mut candidates = sessions.iter().filter(|session| {
            let same_host = if self.machine.is_empty() { session.remote.is_none() }
                else { session.remote.as_deref() == Some(self.machine.as_str()) };
            let same_agent = session.agent.as_deref().unwrap_or("claude") == self.agent;
            let same_instance = self.instance_id.as_deref().map_or(true, |id| session.instance_id.as_deref() == Some(id));
            let same_cwd = (self.cwd.is_empty() && self.resume.is_some())
                || session.cwd.as_deref().map(normalize_cwd).as_deref() == Some(self.cwd.as_str());
            let same_pane = pane.map_or(true, |pane| session.tmux_pane.as_deref() == Some(pane));
            let fresh = match self.resume.as_deref() {
                Some(id) => (session.id == id || session.agent_id() == id) && session.updated_at >= self.since,
                None if self._continuation.is_some() => session.id != self._continuation.as_ref().unwrap().0.1 && session.updated_at >= self.since,
                None => session.created_at >= self.since,
            };
            // A remote pane is authoritative even when cwd resolves through
            // a symlink on that host; the laptop cannot canonicalize it.
            same_host && same_agent && same_instance && (same_cwd || pane.is_some()) && same_pane && fresh && session.tmux_pane.is_some()
                && session.control_mode.as_deref() != Some("external")
        });
        let first = candidates.next();
        if candidates.next().is_some() {
            return Err("Появилось несколько подходящих сессий. Выбери нужный чат и отправь сообщение вручную".into());
        }
        Ok(first)
    }

    fn emit(&self, d: &Arc<Daemon>, status: &str, session_id: Option<&str>, error: Option<&str>) {
        crate::windows::emit_to_panel(&d.app, "session:launch-task", &json!({
            "launchId": self.id, "status": status, "machine": if self.machine.is_empty() { "local" } else { &self.machine },
            "cwd": self.cwd, "sessionId": session_id, "error": error,
        }));
    }

    pub fn deliver(mut self, d: &Arc<Daemon>, pane: Option<String>) {
        let d = d.clone();
        tauri::async_runtime::spawn(async move {
            let attempts = if self._continuation.is_some() { TERMINAL_TTL.as_secs() * 4 } else { 360 };
            for attempt in 0..attempts {
                if attempt == 360 {
                    self.emit(&d, "failed", None, Some("Агент ещё не подключился. Открой терминал запуска: возможно, он ждёт входа или выбора проекта."));
                }
                let sessions = d.snapshot();
                let selected = match self.select(&sessions, pane.as_deref()) {
                    Ok(selected) => selected.cloned(),
                    Err(error) => { self.emit(&d, "failed", None, Some(&error)); return; }
                };
                if let Some(session) = selected {
                    if let Some(model) = &self.model {
                        d.with_session(&session.id, |session| { session.model = Some(model.clone()); session.model_at = Some(crate::util::now_ms()); });
                        d.push();
                    }
                    self.emit(&d, "connected", Some(&session.id), None);
                    if let Some(cwd) = &session.cwd { self.cwd = normalize_cwd(cwd); }
                    let Some(text) = &self.task else { self.emit(&d, "ready", Some(&session.id), None); return; };
                    let result = match d.pane_target(&session) {
                        Ok(target) => target.reply(session.tmux_pane.as_deref().unwrap(), text).await,
                        Err(error) => Err(error),
                    };
                    match result {
                        Ok(()) => self.emit(&d, "sent", Some(&session.id), None),
                        Err(error) => self.emit(&d, "failed", Some(&session.id), Some(&error)),
                    }
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            self.emit(&d, "failed", None, Some(if self._continuation.is_some() { "Время подключения истекло. Открой запущенную сессию в списке чатов." } else { "Агент не подключился за 90 секунд. Сообщение не отправлено — проверь терминал и повтори в нужном чате" }));
        });
    }
}

fn normalize_cwd(cwd: &str) -> String {
    let trimmed = cwd.trim().trim_end_matches('/');
    if trimmed.is_empty() && cwd.trim().starts_with('/') { "/".into() } else { trimmed.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(machine: &str) -> LaunchTask {
        LaunchTask { id: "request".into(), machine: machine.into(), cwd: "/repo".into(), agent: "codex".into(),
            instance_id: None, model: None, resume: None, since: 10, task: Some("task".into()), _reservation: None, _continuation: None }
    }
    fn session(id: &str, machine: &str, pane: &str, created: i64) -> Session {
        let mut session = Session::new(id.into(), created);
        session.remote = (!machine.is_empty()).then(|| machine.into());
        session.agent = Some("codex".into());
        session.cwd = Some("/repo/".into());
        session.tmux_pane = Some(pane.into());
        session
    }

    #[test]
    fn remote_message_only_binds_the_pane_returned_by_its_launch() {
        let request = request("vps");
        let sessions = [session("vps:mine", "vps", "%4", 10), session("vps:other", "vps", "%5", 11), session("local", "", "%4", 12)];
        assert_eq!(request.select(&sessions, Some("%4")).unwrap().unwrap().id, "vps:mine");
        assert!(request.select(&sessions, None).is_err());
        assert!(request.select(&sessions, Some("%6")).unwrap().is_none());
        let mut resolved = session("vps:linked", "vps", "%9", 10);
        resolved.cwd = Some("/real/repo".into());
        assert_eq!(request.select(&[resolved], Some("%9")).unwrap().unwrap().id, "vps:linked");
    }

    #[test]
    fn initial_message_is_bound_to_the_selected_profile() {
        let request = request("").for_instance(Some("personal".into()));
        let mut personal = session("personal:same-sid", "", "%1", 10);
        personal.instance_id = Some("personal".into());
        let mut work = session("same-sid", "", "%2", 10);
        work.instance_id = Some("work".into());
        assert!(request.select(&[work.clone()], None).unwrap().is_none());
        assert_eq!(request.select(&[work, personal], None).unwrap().unwrap().id, "personal:same-sid");
    }

    #[test]
    fn startup_hook_at_prelaunch_cutoff_is_kept_but_old_and_ambiguous_sessions_are_refused() {
        let request = request("");
        let sessions = [session("old", "", "%1", 9), session("mine", "", "%2", 10)];
        assert_eq!(request.select(&sessions, None).unwrap().unwrap().id, "mine");
        let sessions = [session("one", "", "%1", 10), session("two", "", "%2", 11)];
        assert!(request.select(&sessions, None).is_err());
        let mut wrong_agent = session("claude", "", "%1", 10);
        wrong_agent.agent = Some("claude".into());
        assert!(request.select(&[wrong_agent], None).unwrap().is_none());
    }

    #[test]
    fn resume_binds_existing_id_only_after_a_fresh_hook() {
        let mut request = request("vps");
        request.resume = Some("vps:old".into());
        let mut resumed = session("vps:old", "vps", "%4", 1);
        assert!(request.select(&[resumed.clone()], Some("%4")).unwrap().is_none());
        resumed.updated_at = 10;
        assert_eq!(request.select(&[resumed], Some("%4")).unwrap().unwrap().id, "vps:old");
    }

    #[test]
    fn resume_without_a_known_cwd_still_binds_the_exact_session_id() {
        let mut request = request("");
        request.resume = Some("resumed".into());
        request.cwd.clear();
        let mut resumed = session("resumed", "", "%4", 1);
        resumed.updated_at = 10;
        assert_eq!(request.select(&[resumed], None).unwrap().unwrap().id, "resumed");
    }

    #[test]
    fn overlapping_local_project_launches_are_rejected_until_binding_finishes() {
        let first = LaunchTask::prepare("local", "/qa-launch-reservation", "codex", None, Some("one".into())).unwrap();
        assert!(LaunchTask::prepare("", "/qa-launch-reservation/", "codex", None, Some("two".into())).is_err());
        // Other machines and other projects can launch independently.
        assert!(LaunchTask::prepare("vps", "/qa-launch-reservation", "codex", None, None).is_ok());
        drop(first);
        assert!(LaunchTask::prepare("", "/qa-launch-reservation", "codex", None, None).is_ok());
    }

    #[test]
    fn continuation_reservation_is_shared_across_windows_and_released_on_drop() {
        let first = request("vps").continuing("qa-original-chat").unwrap();
        assert!(request("vps").continuing("qa-original-chat").is_err());
        assert!(request("vps").continuing("qa-another-chat").is_ok());
        drop(first);
        assert!(request("vps").continuing("qa-original-chat").is_ok());
    }

    #[test]
    fn fork_binds_only_new_managed_sid_on_returned_pane_and_profile() {
        let request = request("vps").for_instance(Some("personal".into())).continuing("original").unwrap();
        let mut original = session("original", "vps", "%8", 1);
        original.updated_at = 12;
        original.instance_id = Some("personal".into());
        let mut external = session("observed-fork", "vps", "%8", 11);
        external.instance_id = Some("personal".into());
        external.control_mode = Some("external".into());
        let mut other_profile = session("work", "vps", "%8", 11);
        other_profile.instance_id = Some("work".into());
        // A transcript observer can see copied timestamps before the hook.
        let mut fork = session("new-fork", "vps", "%8", 1);
        fork.updated_at = 11;
        fork.instance_id = Some("personal".into());
        let sessions = [original, external, other_profile, fork];
        assert_eq!(request.select(&sessions, Some("%8")).unwrap().unwrap().id, "new-fork");
        assert!(request.select(&sessions, Some("%9")).unwrap().is_none());
    }

    #[test]
    fn provisional_terminal_validates_pane_and_preserves_source_scope() {
        let request = request("vps").for_instance(Some("personal".into()));
        assert!(request.register_terminal("%1", None).is_err(), "only continuations can register");
        for pane in ["", "%", "%1\n%2", "%1; echo bad", "%4294967296", "1", "other:0"] {
            assert!(request.terminal_snapshot(pane, None).is_err(), "{pane}");
        }
        let snapshot = request.terminal_snapshot("%8", Some("/home/user/.codex-personal".into())).unwrap();
        assert_eq!(snapshot.id, request.id);
        assert_eq!(snapshot.remote.as_deref(), Some("vps"));
        assert_eq!(snapshot.instance_id.as_deref(), Some("personal"));
        assert_eq!(snapshot.provider_home.as_deref(), Some("/home/user/.codex-personal"));
        assert_eq!(snapshot.tmux_pane.as_deref(), Some("%8"));
        assert_eq!(snapshot.control_mode.as_deref(), Some("managed"));
    }

    #[test]
    fn provisional_terminal_expires_is_bounded_and_cannot_be_retargeted() {
        let now = Instant::now();
        let mut registry = TerminalRegistry::default();
        for i in 0..=MAX_LAUNCH_TERMINALS {
            registry.insert(session(&format!("launch-{i}"), "vps", "%8", 1), now + Duration::from_secs(i as u64)).unwrap();
        }
        assert_eq!(registry.entries.len(), MAX_LAUNCH_TERMINALS);
        assert!(registry.get("launch-0", now).is_none());
        assert!(registry.insert(session("launch-1", "other-host", "%9", 1), now).is_err());
        assert_eq!(registry.get("launch-1", now).unwrap().remote.as_deref(), Some("vps"));
        assert!(registry.get("launch-1", now + TERMINAL_TTL + Duration::from_secs(1)).is_none());
        assert!(registry.get("launch-64", now + TERMINAL_TTL + Duration::from_secs(64)).is_none());
    }
}
