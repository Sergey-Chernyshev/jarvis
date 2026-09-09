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

/// Ownership is minted before terminal creation. The async lock serializes
/// creation, delivery and explicit close; no registry lock is held over I/O.
#[derive(Debug, Default)]
pub struct LaunchControl {
    cancelled: std::sync::atomic::AtomicBool,
    pub(crate) gate: tokio::sync::Mutex<()>,
    terminal: Mutex<Option<OwnedTerminal>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedTerminal {
    pub machine: String,
    pub pane: String,
    pub name: Option<String>,
}

type IoFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;
trait TerminalIo: Sync {
    fn close<'a>(&'a self, terminal: &'a OwnedTerminal) -> IoFuture<'a, Result<(), String>>;
}
trait CreationIo: TerminalIo {
    fn reservation(&self, name: &str) -> OwnedTerminal;
    fn create<'a>(&'a self, name: &'a str) -> IoFuture<'a, Result<OwnedTerminal, String>>;
    fn attach<'a>(&'a self, terminal: &'a OwnedTerminal) -> IoFuture<'a, Result<(), String>>;
}
impl LaunchControl {
    pub fn cancelled(&self) -> bool { self.cancelled.load(std::sync::atomic::Ordering::SeqCst) }
    pub fn request_cancel(&self) { self.cancelled.store(true, std::sync::atomic::Ordering::SeqCst); }
    pub(crate) fn record_remote(&self, machine: &str, pane: &str) {
        *self.terminal.lock().unwrap() = Some(OwnedTerminal { machine: machine.into(), pane: pane.into(), name: None });
    }
    pub(crate) async fn cleanup_locked(&self, d: &Arc<Daemon>) -> Result<bool, String> { self.cleanup(&DaemonIo(d.clone())).await }
    fn owned(&self) -> Option<OwnedTerminal> { self.terminal.lock().unwrap().clone() }
    async fn cleanup(&self, io: &impl TerminalIo) -> Result<bool, String> {
        let Some(terminal) = self.owned() else { return Ok(false) };
        io.close(&terminal).await?;
        *self.terminal.lock().unwrap() = None;
        Ok(true)
    }
    async fn cancel_with(&self, io: &impl TerminalIo) -> Result<bool, String> {
        self.request_cancel();
        let _operation = self.gate.lock().await;
        self.cleanup(io).await
    }
    pub async fn cancel(&self, d: &Arc<Daemon>) -> Result<bool, String> {
        self.cancel_with(&DaemonIo(d.clone())).await
    }
    async fn create_with(&self, io: &impl CreationIo, name: &str) -> Result<String, String> {
        let _operation = self.gate.lock().await;
        if self.cancelled() { return Err("Запуск отменён до создания терминала".into()); }
        // Retain the creation token before awaiting the client: a timeout may
        // lose its reply after the server has already created the session.
        *self.terminal.lock().unwrap() = Some(io.reservation(name));
        let terminal = match io.create(name).await {
            Ok(terminal) => terminal,
            Err(error) => return match self.cleanup(io).await {
                Ok(_) => Err(error),
                Err(cleanup) => Err(format!("{error}. Не удалось проверить/закрыть терминал запуска: {cleanup}")),
            },
        };
        // Record the authoritative result even when close arrived during create.
        *self.terminal.lock().unwrap() = Some(terminal.clone());
        let result = if self.cancelled() { Err("Запуск отменён".into()) }
            else if !valid_pane(&terminal.pane) { Err("Терминал не вернул корректную пану запуска".into()) }
            else { io.attach(&terminal).await };
        if let Err(error) = result {
            return match self.cleanup(io).await {
                Ok(_) => Err(error),
                Err(cleanup) => Err(format!("{error}. Не удалось закрыть терминал этого запуска: {cleanup}")),
            };
        }
        if self.cancelled() {
            self.cleanup(io).await?;
            return Err("Запуск отменён".into());
        }
        Ok(terminal.pane)
    }
}
fn valid_pane(pane: &str) -> bool {
    pane.strip_prefix('%').is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && id.parse::<u32>().is_ok())
}
struct DaemonIo(Arc<Daemon>);
impl TerminalIo for DaemonIo {
    fn close<'a>(&'a self, terminal: &'a OwnedTerminal) -> IoFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if terminal.machine.is_empty() {
                // Only the exact newly-created local session may be cleaned up.
                if let Some(name) = &terminal.name {
                    let exact = format!("={name}");
                    match crate::tmux::tmux_j(&["show-environment", "-t", &exact, "JARVIS_LAUNCH_ID"]).await {
                        Ok(marker) if marker.trim() == format!("JARVIS_LAUNCH_ID={name}") =>
                            return crate::tmux::tmux_j(&["kill-session", "-t", &exact]).await.map(|_| ()),
                        Ok(_) => return Err("Маркер владельца терминала изменился; чужое окно не закрыто".into()),
                        Err(error) if missing_session(&error) => return Ok(()),
                        Err(error) => return Err(error),
                    }
                }
            }
            let target = pane_target(&self.0, &terminal.machine)?;
            if target.pane_state(&terminal.pane).await? { target.kill(&terminal.pane).await?; }
            Ok(())
        })
    }
}
fn pane_target(d: &Arc<Daemon>, machine: &str) -> Result<crate::tmux::Target, String> {
    if machine.is_empty() { Ok(crate::tmux::Target::Local) }
    else { d.remotes.node(machine).map(crate::tmux::Target::Remote).ok_or_else(|| "Узел пропал из настроек".into()) }
}
fn missing_session(error: &str) -> bool {
    error.contains("can't find session") || error.contains("no server running") || error.contains("no sessions")
        || (error.contains("error connecting to ") && error.contains("(No such file or directory)"))
}
struct LocalCreation<'a> { d: &'a Arc<Daemon>, cwd: &'a str, inner: &'a str, terminal: Option<(&'a str, &'a str)> }
impl TerminalIo for LocalCreation<'_> {
    fn close<'a>(&'a self, terminal: &'a OwnedTerminal) -> IoFuture<'a, Result<(), String>> {
        Box::pin(async move { DaemonIo(self.d.clone()).close(terminal).await })
    }
}
impl CreationIo for LocalCreation<'_> {
    fn reservation(&self, name: &str) -> OwnedTerminal { OwnedTerminal { machine: String::new(), pane: String::new(), name: Some(name.into()) } }
    fn create<'a>(&'a self, name: &'a str) -> IoFuture<'a, Result<OwnedTerminal, String>> {
        Box::pin(async move {
            let config = crate::util::jarvis_dir().join("tmux.conf");
            let config = config.to_string_lossy().to_string();
            let dir = format!("JARVIS_DIR={}", crate::util::jarvis_dir().display());
            let sock = format!("JARVIS_SOCK={}", crate::util::sock_path().display());
            let ownership = format!("JARVIS_LAUNCH_ID={name}");
            let inner = format!("unset JARVIS_IGNORE; {}", self.inner);
            let mut args = vec![];
            if std::path::Path::new(&config).is_file() { args.extend(["-f", config.as_str()]); }
            args.extend(["new-session", "-d", "-P", "-F", "#{pane_id}", "-s", name, "-c", self.cwd,
                "-e", dir.as_str(), "-e", sock.as_str(), "-e", ownership.as_str(), "bash", "-lc", inner.as_str()]);
            let pane = crate::tmux::tmux_j(&args).await?;
            Ok(OwnedTerminal { machine: String::new(), pane: pane.trim().into(), name: Some(name.into()) })
        })
    }
    fn attach<'a>(&'a self, owned: &'a OwnedTerminal) -> IoFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if let Some((terminal, custom)) = self.terminal {
                let command = format!("tmux -L jarvis attach-session -t {}", crate::util::shell_quote(&format!("={}", owned.name.as_deref().unwrap())));
                let command = crate::launch::inner_command(self.cwd, "", &command, &crate::launch::launch_path_dirs());
                crate::launch::spawn(terminal, custom, &command).await?;
            }
            Ok(())
        })
    }
}

pub struct LaunchTask {
    pub id: String,
    pub control: Arc<LaunchControl>,
    machine: String,
    cwd: String,
    agent: String,
    instance_id: Option<String>,
    model: Option<String>,
    bind: Option<crate::capability::native::spawn::Bind>,
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
            id: format!("launch-{since}-{}-{sequence}", std::process::id()), control: Arc::default(), machine, cwd,
            agent: agent.to_string(), bind: None, instance_id: None, model: None, resume: resume.map(str::to_string), since,
            task: task.map(|text| text.trim().to_string()).filter(|text| !text.is_empty()),
            _reservation: reservation, _continuation: None,
        })
    }

    pub fn with_bind(mut self, bind: Option<crate::capability::native::spawn::Bind>, spawns: &crate::capability::native::spawn::Spawns) -> Self {
        if let Some(record) = bind.as_ref().and_then(|b| spawns.find(&b.ticket)) { self.control = record.control; }
        self.bind = bind; self
    }
    pub fn needs_owned_terminal(&self) -> bool { self.task.is_some() || self.bind.is_some() || self._continuation.is_some() }
    pub async fn create_local(&self, d: &Arc<Daemon>, cwd: &str, inner: &str, terminal: Option<(&str, &str)>) -> Result<String, String> {
        self.control.create_with(&LocalCreation { d, cwd, inner, terminal }, &self.id).await
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

    pub fn deliver(self, d: &Arc<Daemon>, pane: Option<String>) {
        let io = DaemonIo(d.clone());
        tauri::async_runtime::spawn(async move {
            let attempts = if self._continuation.is_some() { TERMINAL_TTL.as_secs() * 4 } else { 360 };
            self.run_delivery(&io, pane, attempts).await;
        });
    }

    async fn run_delivery(&self, io: &impl DeliveryIo, pane: Option<String>, attempts: u64) {
        let result = self.delivery_loop(io, pane.as_deref(), attempts).await;
        if let Err(error) = result {
            // Cancellation owns its own completion; it must not be overwritten
            // by a late timeout or transport response.
            let _operation = self.control.gate.lock().await;
            if !self.control.cancelled() { io.failed(self, &error); }
        }
    }

    async fn delivery_loop(&self, io: &impl DeliveryIo, pane: Option<&str>, attempts: u64) -> Result<(), String> {
        let prompt_first = !crate::backend::backend(crate::backend::Agent::from_label(&self.agent)).session_before_prompt();
        let mut ready = crate::launch::ready::ReadyGate::default();
        let mut prompt_sent = false;
        let owned = self.control.owned();
        if self.task.is_some() && (!pane.is_some_and(valid_pane) || !owned.as_ref().is_some_and(|owned| Some(owned.pane.as_str()) == pane && owned.machine == self.machine)) {
            return Err("Нет подтверждённой паны этого запуска. Задача не отправлена".into());
        }
        for _ in 0..attempts {
            {
                let _operation = self.control.gate.lock().await;
                if self.control.cancelled() { return Ok(()); }
                if let Some(pane) = pane {
                    if !io.alive(&self.machine, pane).await? {
                        return Err("Терминал агента завершился до подтверждения подключения. Проверь результат перед повтором задачи".into());
                    }
                }
                if self.control.cancelled() { return Ok(()); }
                if prompt_first && self.task.is_some() && !prompt_sent {
                    let pane = pane.unwrap(); // authority was checked above
                    let screen = io.screen(&self.machine, pane).await?;
                    if self.control.cancelled() { return Ok(()); }
                    match ready.feed(&screen) {
                        Some(crate::launch::ready::Screen::Ready) => {
                            io.reply(&self.machine, pane, self.task.as_deref().unwrap()).await?;
                            prompt_sent = true;
                            io.emit(self, "sent", None);
                        }
                        Some(crate::launch::ready::Screen::Blocked(reason)) => return Err(reason),
                        Some(crate::launch::ready::Screen::Modal) => return Err("CLI ждёт выбора в терминале. Задача не отправлена".into()),
                        _ => {}
                    }
                }
                if self.control.cancelled() { return Ok(()); }
                let sessions = io.snapshot();
                if let Some(session) = self.select(&sessions, pane)? {
                    if !io.bind(self, session).await? { return Ok(()); }
                    if self.control.cancelled() { return Ok(()); }
                    io.emit(self, "connected", Some(&session.id));
                    if !prompt_sent {
                        if let Some(text) = &self.task {
                            io.reply(&self.machine, session.tmux_pane.as_deref().unwrap(), text).await?;
                            io.emit(self, "sent", Some(&session.id));
                        }
                    }
                    io.emit(self, "ready", Some(&session.id));
                    return Ok(());
                }
            }
            io.pause().await;
        }
        Err(if prompt_sent {
            "Задача передана в терминал, но агент не подтвердил подключение. Проверь результат в терминале перед повтором задачи"
        } else {
            "Агент не подключился за отведённое время. Задача не отправлена; проверь терминал и повтори в нужном чате"
        }.into())
    }

}

trait DeliveryIo: Sync {
    fn snapshot(&self) -> Vec<Session>;
    fn alive<'a>(&'a self, machine: &'a str, pane: &'a str) -> IoFuture<'a, Result<bool, String>>;
    fn screen<'a>(&'a self, machine: &'a str, pane: &'a str) -> IoFuture<'a, Result<String, String>>;
    fn reply<'a>(&'a self, machine: &'a str, pane: &'a str, text: &'a str) -> IoFuture<'a, Result<(), String>>;
    fn bind<'a>(&'a self, task: &'a LaunchTask, session: &'a Session) -> IoFuture<'a, Result<bool, String>>;
    fn pause(&self) -> IoFuture<'_, ()>;
    fn emit(&self, task: &LaunchTask, status: &str, session: Option<&str>);
    fn spawns(&self) -> &crate::capability::native::spawn::Spawns;
    fn failure_event(&self, task: &LaunchTask, reason: &str, notify: bool);
    fn failed(&self, task: &LaunchTask, reason: &str) {
        let notify = task.bind.as_ref().map_or(true, |bind| self.spawns().fail(&bind.ticket, reason));
        self.failure_event(task, reason, notify);
    }
}
impl DeliveryIo for DaemonIo {
    fn snapshot(&self) -> Vec<Session> { self.0.snapshot() }
    fn alive<'a>(&'a self, machine: &'a str, pane: &'a str) -> IoFuture<'a, Result<bool, String>> {
        Box::pin(async move { pane_target(&self.0, machine)?.pane_state(pane).await })
    }
    fn screen<'a>(&'a self, machine: &'a str, pane: &'a str) -> IoFuture<'a, Result<String, String>> {
        Box::pin(async move { pane_target(&self.0, machine)?.screen(pane).await })
    }
    fn reply<'a>(&'a self, machine: &'a str, pane: &'a str, text: &'a str) -> IoFuture<'a, Result<(), String>> {
        Box::pin(async move { pane_target(&self.0, machine)?.reply(pane, text).await })
    }
    fn bind<'a>(&'a self, task: &'a LaunchTask, session: &'a Session) -> IoFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            if let Some(bind) = &task.bind {
                let mut bind = bind.clone(); bind.model = None;
                if !crate::capability::native::spawn::on_bound(&self.0, &bind, &session.id).await { return Ok(false); }
            }
            if let Some(model) = &task.model {
                self.0.with_session(&session.id, |session| { session.model = Some(model.clone()); session.model_at = Some(crate::util::now_ms()); });
                self.0.push();
            }
            Ok(true)
        })
    }
    fn pause(&self) -> IoFuture<'_, ()> { Box::pin(tokio::time::sleep(Duration::from_millis(250))) }
    fn emit(&self, task: &LaunchTask, status: &str, session: Option<&str>) { task.emit(&self.0, status, session, None); }
    fn spawns(&self) -> &crate::capability::native::spawn::Spawns { &self.0.spawns }
    fn failure_event(&self, task: &LaunchTask, reason: &str, notify: bool) {
        task.emit(&self.0, "failed", None, Some(reason));
        if notify {
            self.0.notify("Не удалось подтвердить запуск агента", &format!("{}: {reason}", task.agent), task.bind.as_ref().and_then(|b| b.parent.as_deref()), "error");
        }
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
        LaunchTask { id: "request".into(), control: Arc::default(), machine: machine.into(), cwd: "/repo".into(), agent: "codex".into(),
            instance_id: None, model: None, bind: None, resume: None, since: 10, task: Some("task".into()), _reservation: None, _continuation: None }
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

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::capability::native::spawn::{Bind, Plan, Spawns, pending_json};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct Hold { entered: tokio::sync::Notify, release: tokio::sync::Semaphore }
    impl Default for Hold { fn default() -> Self { Self { entered: tokio::sync::Notify::new(), release: tokio::sync::Semaphore::new(0) } } }
    impl Hold {
        async fn wait(&self) { self.entered.notify_one(); self.release.acquire().await.unwrap().forget(); }
        fn open(&self) { self.release.add_permits(1); }
    }
    struct FakeIo {
        spawns: Spawns,
        machine: String,
        next: AtomicUsize,
        sessions: Mutex<Vec<Session>>,
        after_reply: Mutex<Option<Session>>,
        writes: Mutex<Vec<(String, String, String)>>,
        closed: Mutex<Vec<OwnedTerminal>>,
        attached: AtomicUsize,
        notices: AtomicUsize,
        phase_error: Option<&'static str>,
        screen_text: String,
        alive: bool,
        close_fails: AtomicBool,
        hold_create: Option<Arc<Hold>>,
        hold_alive: Option<Arc<Hold>>,
        hold_screen: Option<Arc<Hold>>,
        hold_bind: Option<Arc<Hold>>,
    }
    impl Default for FakeIo {
        fn default() -> Self { Self { spawns: Spawns::new(), machine: String::new(), next: AtomicUsize::new(0), sessions: Mutex::default(),
            after_reply: Mutex::default(), writes: Mutex::default(), closed: Mutex::default(), attached: AtomicUsize::new(0), notices: AtomicUsize::new(0),
            phase_error: None, screen_text: "❯".into(), alive: true, close_fails: AtomicBool::new(false),
            hold_create: None, hold_alive: None, hold_screen: None, hold_bind: None } }
    }
    impl TerminalIo for FakeIo {
        fn close<'a>(&'a self, terminal: &'a OwnedTerminal) -> IoFuture<'a, Result<(), String>> {
            Box::pin(async move {
                if self.close_fails.load(Ordering::SeqCst) { return Err("close transport failed".into()); }
                self.closed.lock().unwrap().push(terminal.clone()); Ok(())
            })
        }
    }
    impl CreationIo for FakeIo {
        fn reservation(&self, name: &str) -> OwnedTerminal { OwnedTerminal { machine: self.machine.clone(), pane: String::new(), name: Some(name.into()) } }
        fn create<'a>(&'a self, name: &'a str) -> IoFuture<'a, Result<OwnedTerminal, String>> {
            Box::pin(async move {
                let n = self.next.fetch_add(1, Ordering::SeqCst) + 1;
                if let Some(hold) = &self.hold_create { hold.wait().await; }
                if self.phase_error == Some("create") { return Err("create failed".into()); }
                Ok(OwnedTerminal { machine: self.machine.clone(), pane: format!("%{n}"), name: Some(name.into()) })
            })
        }
        fn attach<'a>(&'a self, _: &'a OwnedTerminal) -> IoFuture<'a, Result<(), String>> {
            Box::pin(async move {
                self.attached.fetch_add(1, Ordering::SeqCst);
                if self.phase_error == Some("attach") { Err("attach failed".into()) } else { Ok(()) }
            })
        }
    }
    impl DeliveryIo for FakeIo {
        fn snapshot(&self) -> Vec<Session> { self.sessions.lock().unwrap().clone() }
        fn alive<'a>(&'a self, _: &'a str, _: &'a str) -> IoFuture<'a, Result<bool, String>> {
            Box::pin(async move {
                if let Some(hold) = &self.hold_alive { hold.wait().await; }
                if self.phase_error == Some("alive") { Err("alive transport failed".into()) } else { Ok(self.alive) }
            })
        }
        fn screen<'a>(&'a self, _: &'a str, _: &'a str) -> IoFuture<'a, Result<String, String>> {
            Box::pin(async move {
                if let Some(hold) = &self.hold_screen { hold.wait().await; }
                if self.phase_error == Some("screen") { Err("screen failed".into()) } else { Ok(self.screen_text.clone()) }
            })
        }
        fn reply<'a>(&'a self, machine: &'a str, pane: &'a str, text: &'a str) -> IoFuture<'a, Result<(), String>> {
            Box::pin(async move {
                if self.phase_error == Some("reply") { return Err("reply failed".into()); }
                self.writes.lock().unwrap().push((machine.into(), pane.into(), text.into()));
                if let Some(session) = self.after_reply.lock().unwrap().take() { self.sessions.lock().unwrap().push(session); }
                Ok(())
            })
        }
        fn bind<'a>(&'a self, task: &'a LaunchTask, session: &'a Session) -> IoFuture<'a, Result<bool, String>> {
            Box::pin(async move {
                if let Some(hold) = &self.hold_bind { hold.wait().await; }
                Ok(task.bind.as_ref().map_or(true, |b| self.spawns.bind(&b.ticket, &session.id)))
            })
        }
        fn pause(&self) -> IoFuture<'_, ()> { Box::pin(tokio::task::yield_now()) }
        fn emit(&self, _: &LaunchTask, _: &str, _: Option<&str>) {}
        fn spawns(&self) -> &Spawns { &self.spawns }
        fn failure_event(&self, _: &LaunchTask, _: &str, notify: bool) {
            if notify { self.notices.fetch_add(1, Ordering::SeqCst); }
        }
    }
    fn make_task(io: &FakeIo, agent: &str) -> LaunchTask {
        let plan = Plan { agent: crate::backend::Agent::from_label(agent), name: "owned".into(), task: "first task".into(), cwd: "/same-project".into(), model: None };
        let ticket = io.spawns.open("agent", Some("requester".into()), &plan, 10);
        let bind = Bind { ticket: ticket.clone(), name: plan.name, model: None, by: "agent".into(), parent: Some("requester".into()), task: plan.task.clone(), at: 10 };
        LaunchTask { id: format!("owned-{ticket}"), control: Arc::default(), machine: io.machine.clone(), cwd: plan.cwd, agent: agent.into(), instance_id: None,
            model: None, bind: None, resume: None, since: 10, task: Some(plan.task), _reservation: None, _continuation: None }.with_bind(Some(bind), &io.spawns)
    }
    fn hook(task: &LaunchTask, pane: &str) -> Session {
        let mut session = Session::new(format!("hook-{}", task.id), 11);
        session.agent = Some(task.agent.clone()); session.cwd = Some(task.cwd.clone()); session.tmux_pane = Some(pane.into());
        session.remote = (!task.machine.is_empty()).then(|| task.machine.clone()); session
    }
    fn assert_unbound(io: &FakeIo, task: &LaunchTask) {
        assert!(io.spawns.find(&task.bind.as_ref().unwrap().ticket).unwrap().session_id.is_none());
        assert!(io.writes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn close_before_creation_fences_both_startup_orders() {
        for agent in ["claude", "kimi"] {
            let io = FakeIo::default(); let task = make_task(&io, agent);
            assert!(!task.control.cancel_with(&io).await.unwrap());
            io.spawns.mark_closed(&task.bind.as_ref().unwrap().ticket);
            assert!(task.control.create_with(&io, &task.id).await.is_err());
            io.sessions.lock().unwrap().push(hook(&task, "%1"));
            task.run_delivery(&io, Some("%1".into()), 2).await;
            assert_eq!(io.next.load(Ordering::SeqCst), 0); assert_unbound(&io, &task);
            assert_eq!(pending_json(&io.spawns.find(&task.bind.as_ref().unwrap().ticket).unwrap())["state"], "cancelled");
        }
    }

    #[tokio::test]
    async fn close_during_creation_waits_for_exact_owned_cleanup_before_completing() {
        for agent in ["claude", "kimi"] {
            let hold = Arc::new(Hold::default());
            let io = Arc::new(FakeIo { hold_create: Some(hold.clone()), ..Default::default() });
            let task = Arc::new(make_task(&io, agent));
            let creating = { let task = task.clone(); let io = io.clone(); tokio::spawn(async move { task.control.create_with(io.as_ref(), &task.id).await }) };
            hold.entered.notified().await;
            task.control.request_cancel();
            let closing = { let task = task.clone(); let io = io.clone(); tokio::spawn(async move { task.control.cancel_with(io.as_ref()).await }) };
            tokio::task::yield_now().await; assert!(!closing.is_finished());
            hold.open(); assert!(creating.await.unwrap().is_err()); closing.await.unwrap().unwrap();
            assert_eq!(io.attached.load(Ordering::SeqCst), 0);
            assert_eq!(io.closed.lock().unwrap().as_slice(), &[OwnedTerminal { machine: "".into(), pane: "%1".into(), name: Some(task.id.clone()) }]);
            io.sessions.lock().unwrap().push(hook(&task, "%1")); task.run_delivery(io.as_ref(), Some("%1".into()), 2).await;
            assert_unbound(&io, &task);
        }
    }

    #[tokio::test]
    async fn close_during_pre_hook_or_pre_prompt_read_blocks_late_delivery_and_binding() {
        for agent in ["claude", "kimi"] {
            let hold = Arc::new(Hold::default());
            let io = Arc::new(FakeIo { hold_alive: (agent == "claude").then(|| hold.clone()), hold_screen: (agent == "kimi").then(|| hold.clone()), ..Default::default() });
            let task = Arc::new(make_task(&io, agent)); let pane = task.control.create_with(io.as_ref(), &task.id).await.unwrap();
            let running = { let task = task.clone(); let io = io.clone(); let pane = pane.clone(); tokio::spawn(async move { task.run_delivery(io.as_ref(), Some(pane), 3).await }) };
            hold.entered.notified().await; task.control.request_cancel();
            io.sessions.lock().unwrap().push(hook(&task, &pane));
            let closing = { let task = task.clone(); let io = io.clone(); tokio::spawn(async move { task.control.cancel_with(io.as_ref()).await }) };
            hold.open(); running.await.unwrap(); closing.await.unwrap().unwrap();
            assert_unbound(&io, &task); assert_eq!(io.closed.lock().unwrap().len(), 1);
            assert_eq!(io.notices.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn close_racing_the_hook_cannot_rebind_a_cancelled_ticket() {
        let hold = Arc::new(Hold::default());
        let io = Arc::new(FakeIo { hold_bind: Some(hold.clone()), ..Default::default() });
        let task = Arc::new(make_task(&io, "claude")); let pane = task.control.create_with(io.as_ref(), &task.id).await.unwrap();
        io.sessions.lock().unwrap().push(hook(&task, &pane));
        let running = { let task = task.clone(); let io = io.clone(); tokio::spawn(async move { task.run_delivery(io.as_ref(), Some(pane), 2).await }) };
        hold.entered.notified().await; task.control.request_cancel(); hold.open(); running.await.unwrap();
        task.control.cancel_with(io.as_ref()).await.unwrap(); assert_unbound(&io, &task);
    }

    #[tokio::test]
    async fn mixed_provider_startup_never_claims_the_earlier_unregistered_pane() {
        let io = FakeIo::default(); let claude = make_task(&io, "claude"); let kimi = make_task(&io, "kimi");
        let first = claude.control.create_with(&io, &claude.id).await.unwrap();
        let mine = kimi.control.create_with(&io, &kimi.id).await.unwrap();
        assert_eq!(first, "%1"); assert_eq!(mine, "%2");
        // Both panes display a ready prompt, and Claude has no hook yet.
        *io.after_reply.lock().unwrap() = Some(hook(&kimi, &mine));
        kimi.run_delivery(&io, Some(mine.clone()), 3).await;
        assert_eq!(io.writes.lock().unwrap().as_slice(), &[("".into(), mine, "first task".into())]);
        assert!(io.spawns.find(&kimi.bind.as_ref().unwrap().ticket).unwrap().session_id.is_some());
        let unowned = make_task(&io, "kimi"); unowned.run_delivery(&io, None, 3).await;
        assert_eq!(io.writes.lock().unwrap().len(), 1, "no pane ownership must fail closed");
    }

    #[tokio::test]
    async fn all_delivery_failures_complete_the_original_ticket_and_notify_once() {
        for (agent, error, alive, screen, hook_first) in [
            ("claude", Some("alive"), true, "❯", false),
            ("claude", None, false, "❯", false),
            ("kimi", Some("screen"), true, "❯", false),
            ("kimi", None, true, "Trust this folder?", false),
            ("kimi", Some("reply"), true, "❯", false),
            ("claude", Some("reply"), true, "❯", true),
            ("claude", None, true, "starting", false),
            ("kimi", None, true, "starting", false),
            ("claude", Some("ambiguous"), true, "❯", true),
        ] {
            let io = FakeIo { phase_error: error, alive, screen_text: screen.into(), ..Default::default() };
            let task = make_task(&io, agent); let pane = task.control.create_with(&io, &task.id).await.unwrap();
            if hook_first {
                io.sessions.lock().unwrap().push(hook(&task, &pane));
                if error == Some("ambiguous") { let mut other = hook(&task, &pane); other.id.push_str("-other"); io.sessions.lock().unwrap().push(other); }
            }
            task.run_delivery(&io, Some(pane), 3).await;
            let record = io.spawns.find(&task.bind.as_ref().unwrap().ticket).unwrap(); let status = pending_json(&record);
            assert_eq!(status["state"], "failed", "{agent} {error:?} {screen}: {status}");
            assert_eq!(status["pending"], false); assert!(!status["error"].as_str().unwrap().is_empty());
            assert_eq!(io.notices.load(Ordering::SeqCst), 1);
            assert!(io.spawns.child(&record.ticket, "agent").is_ok(), "failed terminal stays explicitly closeable");
        }
    }

    #[test]
    fn absent_tmux_server_is_distinct_from_an_uncertain_cleanup_failure() {
        assert!(missing_session("error connecting to /tmp/tmux-user/jarvis (No such file or directory)"));
        assert!(missing_session("can't find session: =owned-token"));
        for error in ["tmux: таймаут", "Permission denied", "error connecting to /tmp/server (Connection refused)"] {
            assert!(!missing_session(error));
        }
    }

    #[tokio::test]
    async fn lost_creation_reply_retains_the_creation_token_for_cleanup() {
        let io = FakeIo { phase_error: Some("create"), ..Default::default() };
        let task = make_task(&io, "kimi");
        assert!(task.control.create_with(&io, &task.id).await.unwrap_err().contains("create failed"));
        assert_eq!(io.closed.lock().unwrap().as_slice(), &[OwnedTerminal { machine: "".into(), pane: "".into(), name: Some(task.id.clone()) }]);
        assert!(task.control.owned().is_none());
        assert_unbound(&io, &task);
    }

    #[tokio::test]
    async fn attach_failure_cleans_only_its_created_terminal_and_failed_close_is_retryable() {
        let io = FakeIo { phase_error: Some("attach"), ..Default::default() }; let task = make_task(&io, "kimi");
        assert!(task.control.create_with(&io, &task.id).await.unwrap_err().contains("attach"));
        assert_eq!(io.closed.lock().unwrap()[0].name.as_deref(), Some(task.id.as_str()));
        let io = FakeIo { machine: "vps".into(), ..Default::default() }; let task = make_task(&io, "kimi");
        task.control.create_with(&io, &task.id).await.unwrap(); io.close_fails.store(true, Ordering::SeqCst);
        assert!(task.control.cancel_with(&io).await.is_err()); assert!(task.control.owned().is_some());
        assert_unbound(&io, &task); io.close_fails.store(false, Ordering::SeqCst);
        assert!(task.control.cancel_with(&io).await.unwrap());
        assert_eq!(io.closed.lock().unwrap()[0].machine, "vps", "same-number local pane is never targeted");
    }
}
