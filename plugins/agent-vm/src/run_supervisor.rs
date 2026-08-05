use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use zeroize::Zeroize;

use crate::host::HostApi;
use crate::inventory::VmRecord;
use crate::project::ProjectIdentity;
use crate::run_event::{map_guest_path, Backend, BackendEvent, RunEvent};
use crate::run_executor::{
    validate_backend_session_id, BackendEventSink, ExecutionOutcome, TurnExecution, TurnExecutor,
};
use crate::run_store::{validate_run_id, RunStore, RunSummary};
use crate::runtime_paths::RuntimePaths;
use crate::service::{RuntimeService, RuntimeSnapshot};
use crate::vm_entity::VmEntityPublisher;

const DELTA_FLUSH_INTERVAL: Duration = Duration::from_millis(60);
const MAX_DELTA_CHARS: usize = 8 * 1024;
const MAX_RESULT_FILES: usize = 64;
const MAX_DISPLAY_PATH_CHARS: usize = 1024;
const MAX_ENTITY_ATTRS_BYTES: usize = 60 * 1024;
const MAX_QUEUED_TURNS: usize = 8;

pub struct SendRequest {
    pub cwd: PathBuf,
    pub project_id: Option<String>,
    pub backend: Backend,
    pub run_id: Option<String>,
    pub message: String,
}

impl Drop for SendRequest {
    fn drop(&mut self) {
        self.message.zeroize();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitReceipt {
    pub run_id: String,
    pub turn_id: String,
    pub queued: bool,
}

struct QueuedTurn {
    turn_id: String,
    message: String,
}

impl Drop for QueuedTurn {
    fn drop(&mut self) {
        self.message.zeroize();
    }
}

struct ActiveRun {
    run_id: String,
    backend: Backend,
    vm_name: Option<String>,
    backend_session_id: Option<String>,
    cancel_requested: Arc<AtomicBool>,
    queued: VecDeque<QueuedTurn>,
}

pub struct RunSupervisor<H: HostApi> {
    host: H,
    store: RunStore,
    executor: Arc<dyn TurnExecutor>,
    active: Arc<Mutex<HashMap<String, ActiveRun>>>,
    runtime_paths: Option<RuntimePaths>,
    vm_entities: VmEntityPublisher<H>,
}

impl<H: HostApi> Clone for RunSupervisor<H> {
    fn clone(&self) -> Self {
        Self {
            host: self.host.clone(),
            store: self.store.clone(),
            executor: self.executor.clone(),
            active: self.active.clone(),
            runtime_paths: self.runtime_paths.clone(),
            vm_entities: self.vm_entities.clone(),
        }
    }
}

impl<H: HostApi> RunSupervisor<H> {
    pub fn new(host: H, store: RunStore, executor: Arc<dyn TurnExecutor>) -> Self {
        let vm_entities = VmEntityPublisher::new(host.clone());
        Self {
            host,
            store,
            executor,
            active: Arc::new(Mutex::new(HashMap::new())),
            runtime_paths: None,
            vm_entities,
        }
    }

    pub fn with_runtime_paths(mut self, paths: RuntimePaths) -> Self {
        self.runtime_paths = Some(paths);
        self
    }

    fn shell_command(&self, vm_name: &str) -> String {
        self.runtime_paths
            .as_ref()
            .map(|paths| paths.shell_command(vm_name, true))
            .unwrap_or_else(|| format!("avm shell {vm_name}"))
    }

    pub fn submit<S: RuntimeService>(
        &self,
        service: S,
        mut request: SendRequest,
    ) -> Result<SubmitReceipt, String> {
        validate_message(&request.message)?;
        let project = ProjectIdentity::from_path(&request.cwd)?;
        if request
            .project_id
            .as_deref()
            .is_some_and(|id| id != project.project_id)
        {
            return Err("projectId не соответствует canonical cwd".into());
        }
        let turn_id = new_id("turn");
        let queued = {
            let mut active = self.active.lock().unwrap();
            if let Some(run) = active.get_mut(&project.project_id) {
                if run.backend != request.backend {
                    return Err("active project run использует другой backend".into());
                }
                if request
                    .run_id
                    .as_deref()
                    .is_some_and(|run_id| run_id != run.run_id)
                {
                    return Err("runId не соответствует active project run".into());
                }
                if run.queued.len() >= MAX_QUEUED_TURNS {
                    return Err(format!(
                        "очередь Agent VM заполнена (максимум {MAX_QUEUED_TURNS} сообщений)"
                    ));
                }
                run.queued.push_back(QueuedTurn {
                    turn_id: turn_id.clone(),
                    message: std::mem::take(&mut request.message),
                });
                Some((
                    run.run_id.clone(),
                    run.backend,
                    run.vm_name
                        .clone()
                        .unwrap_or_else(|| project.vm_name.clone()),
                    run.backend_session_id.clone(),
                ))
            } else {
                None
            }
        };
        if let Some((run_id, backend, vm_name, backend_session_id)) = queued {
            self.publish_queue_state(
                &project,
                &run_id,
                backend,
                &vm_name,
                backend_session_id.as_deref(),
                true,
            )?;
            return Ok(SubmitReceipt {
                run_id,
                turn_id,
                queued: true,
            });
        }

        let backend = request.backend;
        let (run_id, initial_seq, backend_session_id) =
            self.resolve_run(&project, backend, request.run_id.as_deref())?;
        let cancel_requested = Arc::new(AtomicBool::new(false));
        self.active.lock().unwrap().insert(
            project.project_id.clone(),
            ActiveRun {
                run_id: run_id.clone(),
                backend,
                vm_name: Some(project.vm_name.clone()),
                backend_session_id: backend_session_id.clone(),
                cancel_requested: cancel_requested.clone(),
                queued: VecDeque::new(),
            },
        );
        if let Err(error) = self.publish_queue_state(
            &project,
            &run_id,
            backend,
            &project.vm_name,
            backend_session_id.as_deref(),
            false,
        ) {
            self.active.lock().unwrap().remove(&project.project_id);
            return Err(error);
        }
        let first = QueuedTurn {
            turn_id: turn_id.clone(),
            message: std::mem::take(&mut request.message),
        };
        let supervisor = self.clone();
        let project_for_worker = project.clone();
        let run_for_worker = run_id.clone();
        if thread::Builder::new()
            .name(format!("agent-vm-{}", short_id(&run_id)))
            .spawn(move || {
                supervisor.run_worker(
                    service,
                    project_for_worker,
                    run_for_worker,
                    backend,
                    initial_seq,
                    backend_session_id,
                    cancel_requested,
                    first,
                );
            })
            .is_err()
        {
            self.active.lock().unwrap().remove(&project.project_id);
            let shell_command = self.shell_command(&project.vm_name);
            let mut attrs = base_attrs(
                &project,
                &run_id,
                backend,
                &project.vm_name,
                &shell_command,
                None,
                false,
            );
            if let Some(fields) = attrs.as_object_mut() {
                fields.insert(
                    "error".into(),
                    Value::String("Agent VM run worker failed to start".into()),
                );
            }
            let _ = self
                .host
                .publish_entity("upsert", "agent_run", &run_id, "failed", attrs);
            return Err("не запустить Agent VM run worker".into());
        }
        Ok(SubmitReceipt {
            run_id,
            turn_id,
            queued: false,
        })
    }

    pub fn cancel(&self, run_id: &str) -> Result<bool, String> {
        validate_run_id(run_id)?;
        let (active_found, vm_name) = {
            let mut active = self.active.lock().unwrap();
            if let Some(run) = active.values_mut().find(|run| run.run_id == run_id) {
                run.cancel_requested.store(true, Ordering::Release);
                run.queued.clear();
                (true, run.vm_name.clone())
            } else {
                (false, None)
            }
        };
        if active_found {
            let _ = self.executor.cancel(run_id, vm_name.as_deref())?;
            return Ok(true);
        }
        let Some(summary) = self.store.summary(run_id)? else {
            return Ok(false);
        };
        if !matches!(summary.state.as_str(), "working" | "waiting" | "interrupted") {
            return Ok(false);
        }
        self.executor.cancel(run_id, Some(&summary.vm))
    }

    fn cleanup_recovered_run(&self, summary: &RunSummary) -> Result<(), String> {
        if self
            .executor
            .cancel(&summary.run_id, Some(&summary.vm))?
        {
            Ok(())
        } else {
            Err("не очистить persisted Agent VM run".into())
        }
    }

    pub fn replay(
        &self,
        run_id: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<Vec<RunEvent>, String> {
        self.store.replay(run_id, after_seq, limit)
    }

    pub fn is_active(&self, run_id: &str) -> bool {
        self.active
            .lock()
            .unwrap()
            .values()
            .any(|run| run.run_id == run_id)
    }

    pub fn commands(&self, run_id: &str) -> Result<Value, String> {
        let summary = self
            .store
            .summary(run_id)?
            .ok_or_else(|| "runId не найден в private RunStore".to_string())?;
        Ok(json!({
            "runId":run_id,
            "backend":summary.backend,
            "shellCommand":self.shell_command(&summary.vm),
            "resumeCommand":summary
                .backend_session_id
                .as_deref()
                .and_then(|id| terminal_resume_command(summary.backend, id))
        }))
    }

    pub fn recover(&self) -> Result<usize, String> {
        let mut latest_by_project = HashMap::<String, RunSummary>::new();
        for summary in self.store.summaries()? {
            latest_by_project
                .entry(summary.project_id.clone())
                .or_insert(summary);
        }
        let mut summaries = latest_by_project.into_values().collect::<Vec<_>>();
        summaries.sort_by(|left, right| left.project_id.cmp(&right.project_id));

        for summary in &mut summaries {
            if matches!(summary.state.as_str(), "working" | "waiting") {
                self.cleanup_recovered_run(summary)?;
                let event = RunEvent {
                    run_id: summary.run_id.clone(),
                    turn_id: summary.last_turn_id.clone(),
                    seq: summary
                        .last_seq
                        .checked_add(1)
                        .ok_or_else(|| "run event seq overflow".to_string())?,
                    at: now_ms(),
                    event_type: "run.interrupted".into(),
                    payload: json!({
                        "projectId":summary.project_id,
                        "project":summary.project,
                        "cwd":summary.cwd,
                        "backendSessionId":summary.backend_session_id,
                        "reason":"host-restarted",
                        "guestCleanup":"completed"
                    }),
                    backend: summary.backend,
                    vm: summary.vm.clone(),
                };
                self.store.append(&event)?;
                summary.last_seq = event.seq;
                summary.last_at = event.at;
                summary.state = "interrupted".into();
                summary.latest_event = event;
            }
            self.publish_recovered(summary)?;
        }
        Ok(summaries.len())
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    pub fn store(&self) -> &RunStore {
        &self.store
    }

    pub fn vm_entities(&self) -> &VmEntityPublisher<H> {
        &self.vm_entities
    }

    fn publish_recovered(&self, summary: &RunSummary) -> Result<(), String> {
        let project = if summary.project.is_empty() {
            Path::new(&summary.cwd)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("project")
                .to_string()
        } else {
            summary.project.clone()
        };
        let files = summary
            .files
            .iter()
            .map(|(path, change)| json!({"path":path,"change":change}))
            .collect::<Vec<_>>();
        let mut attrs = json!({
            "runId":summary.run_id,
            "projectId":summary.project_id,
            "project":project,
            "cwd":summary.cwd,
            "vmName":summary.vm,
            "backend":summary.backend,
            "backendSessionId":summary.backend_session_id,
            "shellCommand":self.shell_command(&summary.vm),
            "resumeCommand":summary
                .backend_session_id
                .as_deref()
                .and_then(|id| terminal_resume_command(summary.backend, id)),
            "queued":false,
            "turnId":summary.last_turn_id,
            "seq":summary.last_seq,
            "latestEvent":summary.latest_event,
            "files":files,
            "recovered":true
        });
        trim_attrs(&mut attrs)?;
        self.host.publish_entity(
            "upsert",
            "agent_run",
            &summary.run_id,
            &summary.state,
            attrs,
        )
    }

    fn resolve_run(
        &self,
        project: &ProjectIdentity,
        backend: Backend,
        supplied: Option<&str>,
    ) -> Result<(String, u64, Option<String>), String> {
        let Some(run_id) = supplied else {
            return Ok((new_id("run"), 0, None));
        };
        validate_run_id(run_id)?;
        let summary = self
            .store
            .summary(run_id)?
            .ok_or_else(|| "runId не найден в private RunStore".to_string())?;
        if summary.project_id != project.project_id
            || Path::new(&summary.cwd) != project.canonical_path
            || summary.backend != backend
        {
            return Err("runId не соответствует project/backend".into());
        }
        Ok((run_id.into(), summary.last_seq, summary.backend_session_id))
    }

    #[allow(clippy::too_many_arguments)]
    fn run_worker<S: RuntimeService>(
        &self,
        service: S,
        project: ProjectIdentity,
        run_id: String,
        backend: Backend,
        initial_seq: u64,
        mut backend_session_id: Option<String>,
        cancel_requested: Arc<AtomicBool>,
        mut turn: QueuedTurn,
    ) {
        let mut publisher = RunPublisher::new(
            self.host.clone(),
            self.store.clone(),
            run_id.clone(),
            project.clone(),
            backend,
            project.vm_name.clone(),
            self.shell_command(&project.vm_name),
            initial_seq,
            backend_session_id.clone(),
        );
        let snapshot = match service.ensure(&project.canonical_path) {
            Ok(snapshot) => snapshot,
            Err(_) => {
                let _ = publisher.emit(
                    &turn.turn_id,
                    "run.failed",
                    json!({"error":"Agent VM environment setup failed"}),
                    "failed",
                );
                self.active.lock().unwrap().remove(&project.project_id);
                return;
            }
        };
        let _ = self.vm_entities.publish_snapshot(&snapshot);
        let Some(record) = runnable_record(&snapshot).cloned() else {
            let _ = publisher.emit(
                &turn.turn_id,
                "run.failed",
                json!({"error":"Agent VM is not ready for a headless run"}),
                "failed",
            );
            self.active.lock().unwrap().remove(&project.project_id);
            return;
        };
        publisher.vm_name = record.name.clone();
        publisher.shell_command = self.shell_command(&record.name);
        if !record
            .modules
            .iter()
            .any(|module| module == backend.as_str())
        {
            let backend_name = match backend {
                Backend::Claude => "Claude",
                Backend::Codex => "Codex",
            };
            let _ = publisher.emit(
                &turn.turn_id,
                "run.failed",
                json!({
                    "error":format!(
                        "{backend_name} не установлен в этой VM. Добавьте модуль в .agent-vm.yaml и пересоздайте VM"
                    )
                }),
                "failed",
            );
            self.active.lock().unwrap().remove(&project.project_id);
            return;
        }
        if let Some((code, error)) = backend_credential_failure(&snapshot, backend) {
            let _ = publisher.emit(
                &turn.turn_id,
                "run.failed",
                json!({
                    "code":code,
                    "error":error,
                    "action":"open_agent_settings"
                }),
                "failed",
            );
            self.active.lock().unwrap().remove(&project.project_id);
            return;
        }
        {
            if let Some(active) = self.active.lock().unwrap().get_mut(&project.project_id) {
                active.vm_name = Some(record.name.clone());
            }
        }
        if cancel_requested.load(Ordering::Acquire) {
            let _ = publisher.emit(
                &turn.turn_id,
                "run.cancelled",
                json!({"beforeAgentStart":true}),
                "cancelled",
            );
            self.active.lock().unwrap().remove(&project.project_id);
            return;
        }

        loop {
            let resumed = backend_session_id.is_some();
            let lifecycle_type = if resumed {
                "run.resumed"
            } else {
                "run.started"
            };
            if publisher
                .emit(
                    &turn.turn_id,
                    lifecycle_type,
                    json!({
                        "projectId": project.project_id,
                        "project": project.display_name,
                        "cwd": project.canonical_path,
                        "backendSessionId": backend_session_id,
                    }),
                    "working",
                )
                .is_err()
            {
                break;
            }
            if publisher
                .emit(
                    &turn.turn_id,
                    "user.message",
                    json!({"text": turn.message}),
                    "working",
                )
                .is_err()
            {
                break;
            }
            let execution = TurnExecution {
                run_id: run_id.clone(),
                turn_id: turn.turn_id.clone(),
                backend,
                backend_session_id: backend_session_id.clone(),
                new_claude_session_id: uuid::Uuid::new_v4().to_string(),
                prompt: std::mem::take(&mut turn.message),
                record: record.clone(),
            };
            let mut sink = PublishingSink::new(&mut publisher, &record, resumed);
            let outcome = self.executor.execute(execution, &mut sink);
            let sink_state = sink.finish();
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(_) => ExecutionOutcome {
                    exit_code: -1,
                    backend_reported_error: true,
                    ..ExecutionOutcome::default()
                },
            };
            if let Some(session_id) = outcome.backend_session_id.clone() {
                publisher.backend_session_id = Some(session_id.clone());
                backend_session_id = Some(session_id.clone());
                if let Some(active) = self.active.lock().unwrap().get_mut(&project.project_id) {
                    active.backend_session_id = Some(session_id);
                }
            } else if publisher.backend_session_id.is_some() {
                backend_session_id = publisher.backend_session_id.clone();
            }

            let cancelled = cancel_requested.load(Ordering::Acquire);
            let failed = outcome.exit_code != 0
                || outcome.backend_reported_error
                || sink_state.backend_failure;
            let terminal = if cancelled {
                publisher.emit(&turn.turn_id, "run.cancelled", json!({}), "cancelled")
            } else if sink_state.waiting {
                Ok(())
            } else if failed {
                publisher.emit(
                    &turn.turn_id,
                    "run.failed",
                    json!({
                        "error":"Agent backend failed",
                        "exitCode":outcome.exit_code,
                        "stderrBytes":outcome.stderr_bytes
                    }),
                    "failed",
                )
            } else {
                let result = outcome
                    .result
                    .or(sink_state.result)
                    .or(sink_state.last_assistant)
                    .unwrap_or_default();
                publisher.emit(
                    &turn.turn_id,
                    "result.completed",
                    json!({"text":result,"files":publisher.files_payload()}),
                    "completed",
                )
            };
            if terminal.is_err() {
                break;
            }

            let next = {
                let mut active = self.active.lock().unwrap();
                let Some(run) = active.get_mut(&project.project_id) else {
                    break;
                };
                if cancelled || failed {
                    active.remove(&project.project_id);
                    None
                } else if let Some(queued) = run.queued.pop_front() {
                    Some(queued)
                } else {
                    active.remove(&project.project_id);
                    None
                }
            };
            let Some(next) = next else {
                break;
            };
            cancel_requested.store(false, Ordering::Release);
            turn = next;
        }
        self.active.lock().unwrap().remove(&project.project_id);
    }

    fn publish_queue_state(
        &self,
        project: &ProjectIdentity,
        run_id: &str,
        backend: Backend,
        vm_name: &str,
        backend_session_id: Option<&str>,
        queued: bool,
    ) -> Result<(), String> {
        let state = if queued { "working" } else { "starting" };
        self.host.publish_entity(
            "upsert",
            "agent_run",
            run_id,
            state,
            base_attrs(
                project,
                run_id,
                backend,
                vm_name,
                &self.shell_command(vm_name),
                backend_session_id,
                queued,
            ),
        )
    }
}

struct SinkState {
    waiting: bool,
    backend_failure: bool,
    result: Option<String>,
    last_assistant: Option<String>,
}

struct PublishingSink<'a, H: HostApi> {
    publisher: &'a mut RunPublisher<H>,
    record: &'a VmRecord,
    resumed: bool,
    pending_delta: String,
    last_delta_flush: Instant,
    waiting: bool,
    backend_failure: bool,
    result: Option<String>,
    last_assistant: Option<String>,
}

impl<'a, H: HostApi> PublishingSink<'a, H> {
    fn new(publisher: &'a mut RunPublisher<H>, record: &'a VmRecord, resumed: bool) -> Self {
        Self {
            publisher,
            record,
            resumed,
            pending_delta: String::new(),
            last_delta_flush: Instant::now(),
            waiting: false,
            backend_failure: false,
            result: None,
            last_assistant: None,
        }
    }

    fn flush_delta(&mut self) -> Result<(), String> {
        if self.pending_delta.is_empty() {
            return Ok(());
        }
        let text = std::mem::take(&mut self.pending_delta);
        self.last_delta_flush = Instant::now();
        self.publisher
            .emit_current("assistant.delta", json!({"text":text}), "working")
    }

    fn finish(mut self) -> SinkState {
        if self.flush_delta().is_err() {
            self.backend_failure = true;
        }
        SinkState {
            waiting: self.waiting,
            backend_failure: self.backend_failure,
            result: self.result,
            last_assistant: self.last_assistant,
        }
    }
}

impl<H: HostApi> BackendEventSink for PublishingSink<'_, H> {
    fn emit(&mut self, event: BackendEvent) -> Result<(), String> {
        if let BackendEvent::AssistantDelta { text } = event {
            self.pending_delta.push_str(&text);
            if self.pending_delta.chars().count() >= MAX_DELTA_CHARS
                || self.last_delta_flush.elapsed() >= DELTA_FLUSH_INTERVAL
            {
                self.flush_delta()?;
            }
            return Ok(());
        }
        self.flush_delta()?;
        match event {
            BackendEvent::Session { id, model } => {
                self.publisher.backend_session_id = Some(id.clone());
                self.publisher.emit_current(
                    if self.resumed {
                        "run.resumed"
                    } else {
                        "run.started"
                    },
                    json!({"backendSessionId":id,"model":model}),
                    "working",
                )
            }
            BackendEvent::AssistantMessage { text } => {
                self.last_assistant = Some(text.clone());
                self.publisher
                    .emit_current("assistant.message", json!({"text":text}), "working")
            }
            BackendEvent::ToolStarted { id, name, detail } => self.publisher.emit_current(
                "tool.started",
                json!({"id":id,"name":name,"detail":detail.map(|value| safe_detail(&value))}),
                "working",
            ),
            BackendEvent::ToolCompleted {
                id,
                is_error,
                detail,
            } => self.publisher.emit_current(
                if is_error {
                    "tool.failed"
                } else {
                    "tool.completed"
                },
                json!({"id":id,"detail":detail.map(|value| safe_detail(&value))}),
                "working",
            ),
            BackendEvent::FileChanged { guest_path, change } => {
                if self.record.mount_roots().is_empty() {
                    return self.publisher.emit_current(
                        "backend.unmapped",
                        json!({"upstreamType":"file.path","reason":"host mount unavailable"}),
                        "working",
                    );
                }
                match map_record_guest_path(&self.record, Path::new(&guest_path)) {
                    Some((host_root, path)) => {
                        let relative = path
                            .strip_prefix(&host_root)
                            .ok()
                            .map(display_path)
                            .unwrap_or_default();
                        self.publisher.remember_file(&path, &change);
                        self.publisher.emit_current(
                            "file.changed",
                            json!({
                                "path":path.to_string_lossy(),
                                "relativePath":relative,
                                "change":change
                            }),
                            "working",
                        )
                    }
                    None => self.publisher.emit_current(
                        "backend.unmapped",
                        json!({"upstreamType":"file.path","reason":"outside granted mounts"}),
                        "working",
                    ),
                }
            }
            BackendEvent::Question { id, payload } => {
                self.waiting = true;
                self.publisher.emit_current(
                    "question.opened",
                    json!({"id":id,"question":payload}),
                    "waiting",
                )
            }
            BackendEvent::Usage { payload } => {
                self.publisher
                    .emit_current("usage.updated", payload, "working")
            }
            BackendEvent::Result {
                text,
                is_error,
                session_id,
            } => {
                self.result = Some(text);
                self.backend_failure |= is_error;
                if let Some(session_id) = session_id {
                    self.publisher.backend_session_id = Some(session_id);
                }
                Ok(())
            }
            BackendEvent::TurnCompleted => Ok(()),
            BackendEvent::Failure { .. } => {
                self.backend_failure = true;
                Ok(())
            }
            BackendEvent::Unmapped {
                upstream_type,
                keys,
            } => self.publisher.emit_current(
                "backend.unmapped",
                json!({"upstreamType":upstream_type,"keys":keys}),
                "working",
            ),
            BackendEvent::AssistantDelta { .. } => unreachable!(),
        }
    }
}

fn map_record_guest_path(record: &VmRecord, guest_path: &Path) -> Option<(PathBuf, PathBuf)> {
    record
        .mount_roots()
        .into_iter()
        .find_map(|(host_root, guest_root)| {
            map_guest_path(&guest_root, &host_root, guest_path)
                .ok()
                .map(|path| (host_root, path))
        })
}

struct RunPublisher<H: HostApi> {
    host: H,
    store: RunStore,
    run_id: String,
    project: ProjectIdentity,
    backend: Backend,
    vm_name: String,
    shell_command: String,
    seq: u64,
    turn_id: String,
    backend_session_id: Option<String>,
    files: BTreeMap<String, String>,
}

impl<H: HostApi> RunPublisher<H> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        host: H,
        store: RunStore,
        run_id: String,
        project: ProjectIdentity,
        backend: Backend,
        vm_name: String,
        shell_command: String,
        seq: u64,
        backend_session_id: Option<String>,
    ) -> Self {
        Self {
            host,
            store,
            run_id,
            project,
            backend,
            vm_name,
            shell_command,
            seq,
            turn_id: String::new(),
            backend_session_id,
            files: BTreeMap::new(),
        }
    }

    fn emit(
        &mut self,
        turn_id: &str,
        event_type: &str,
        payload: Value,
        state: &str,
    ) -> Result<(), String> {
        self.turn_id = turn_id.into();
        self.emit_current(event_type, payload, state)
    }

    fn emit_current(
        &mut self,
        event_type: &str,
        mut payload: Value,
        state: &str,
    ) -> Result<(), String> {
        let payload_fields = payload
            .as_object_mut()
            .ok_or_else(|| "run event payload должен быть object".to_string())?;
        payload_fields.insert(
            "projectId".into(),
            Value::String(self.project.project_id.clone()),
        );
        payload_fields.insert(
            "project".into(),
            Value::String(self.project.display_name.clone()),
        );
        payload_fields.insert(
            "cwd".into(),
            Value::String(self.project.canonical_path.to_string_lossy().into_owned()),
        );
        let next_seq = self
            .seq
            .checked_add(1)
            .ok_or_else(|| "run event seq overflow".to_string())?;
        let event = RunEvent {
            run_id: self.run_id.clone(),
            turn_id: self.turn_id.clone(),
            seq: next_seq,
            at: now_ms(),
            event_type: event_type.into(),
            payload,
            backend: self.backend,
            vm: self.vm_name.clone(),
        };
        self.store.append(&event)?;
        self.seq = next_seq;
        let mut attrs = base_attrs(
            &self.project,
            &self.run_id,
            self.backend,
            &self.vm_name,
            &self.shell_command,
            self.backend_session_id.as_deref(),
            false,
        );
        if let Value::Object(fields) = &mut attrs {
            fields.insert("turnId".into(), Value::String(self.turn_id.clone()));
            fields.insert("seq".into(), Value::from(self.seq));
            fields.insert(
                "latestEvent".into(),
                serde_json::to_value(&event)
                    .map_err(|_| "не сериализовать latest run event".to_string())?,
            );
            fields.insert("files".into(), self.files_payload());
        }
        trim_attrs(&mut attrs)?;
        self.host
            .publish_entity("upsert", "agent_run", &self.run_id, state, attrs)
    }

    fn remember_file(&mut self, path: &Path, change: &str) {
        if self.files.len() >= MAX_RESULT_FILES {
            return;
        }
        self.files.insert(display_path(path), change.to_string());
    }

    fn files_payload(&self) -> Value {
        Value::Array(
            self.files
                .iter()
                .map(|(path, change)| json!({"path":path,"change":change}))
                .collect(),
        )
    }
}

fn base_attrs(
    project: &ProjectIdentity,
    run_id: &str,
    backend: Backend,
    vm_name: &str,
    shell_command: &str,
    backend_session_id: Option<&str>,
    queued: bool,
) -> Value {
    json!({
        "runId":run_id,
        "projectId":project.project_id,
        "project":project.display_name,
        "cwd":project.canonical_path,
        "vmName":vm_name,
        "backend":backend,
        "backendSessionId":backend_session_id,
        "shellCommand":shell_command,
        "resumeCommand":backend_session_id.and_then(|id| terminal_resume_command(backend, id)),
        "queued":queued
    })
}

fn runnable_record(snapshot: &RuntimeSnapshot) -> Option<&VmRecord> {
    snapshot
        .vm
        .as_ref()
        .filter(|vm| vm.management == "managed" && vm.state == "running")
        .and_then(|vm| vm.record.as_ref())
}

fn backend_credential_failure(
    snapshot: &RuntimeSnapshot,
    backend: Backend,
) -> Option<(&'static str, &'static str)> {
    let credentials = &snapshot.environment.as_ref()?.credentials;
    let status = match backend {
        Backend::Claude => credentials.claude.as_str(),
        Backend::Codex => credentials.codex.as_str(),
    };
    if status == "ready" {
        return None;
    }
    Some(match (backend, status) {
        (Backend::Claude, _) => (
            "backend_auth_missing",
            "Claude не авторизован для Agent VM. Подключите Claude в настройках Jarvis и повторите запуск",
        ),
        (Backend::Codex, "host-keyring") => (
            "backend_auth_unavailable",
            "Codex авторизован только в Keychain хоста и недоступен Agent VM. Подключите Codex для Agent VM и повторите запуск",
        ),
        (Backend::Codex, _) => (
            "backend_auth_missing",
            "Codex не авторизован для Agent VM. Подключите Codex в настройках Jarvis и повторите запуск",
        ),
    })
}

pub fn terminal_resume_command(backend: Backend, session_id: &str) -> Option<String> {
    validate_backend_session_id(session_id).ok()?;
    Some(match backend {
        Backend::Claude => format!("claude --resume {session_id}"),
        Backend::Codex => format!("codex resume {session_id}"),
    })
}

fn validate_message(message: &str) -> Result<(), String> {
    if message.trim().is_empty()
        || message.len() > crate::run_executor::MAX_PROMPT_BYTES
        || message.contains('\0')
    {
        return Err("message имеет недопустимый размер или bytes".into());
    }
    Ok(())
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

fn short_id(value: &str) -> &str {
    value.get(..value.len().min(20)).unwrap_or(value)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .take(MAX_DISPLAY_PATH_CHARS)
        .collect()
}

fn safe_detail(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if [
        "authorization",
        "api_key",
        "api-key",
        "credential",
        "password",
        "secret",
        "token=",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
        || (lower.contains("://") && lower.contains('@'))
    {
        return "[private detail hidden]".into();
    }
    value.chars().take(2_000).collect()
}

fn trim_attrs(attrs: &mut Value) -> Result<(), String> {
    let mut encoded =
        serde_json::to_vec(attrs).map_err(|_| "не сериализовать agent run attrs".to_string())?;
    if encoded.len() <= MAX_ENTITY_ATTRS_BYTES {
        encoded.zeroize();
        return Ok(());
    }
    encoded.zeroize();
    if let Some(fields) = attrs.as_object_mut() {
        fields.insert("files".into(), json!([]));
    }
    let mut encoded = serde_json::to_vec(attrs)
        .map_err(|_| "не сериализовать trimmed agent run attrs".to_string())?;
    if encoded.len() > MAX_ENTITY_ATTRS_BYTES {
        encoded.zeroize();
        return Err("agent run attrs превышают entity limit".into());
    }
    encoded.zeroize();
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    use serde_json::{json, Value};

    use super::*;
    use crate::guest_bootstrap::BootstrapCredentialStatus;
    use crate::host::{HostApi, PollResponse};
    use crate::inventory::{InventoryVm, VmRecord, VmResources, VmWorkspace};
    use crate::run_event::{BackendEvent, RunEvent};
    use crate::run_executor::{BackendEventSink, ExecutionOutcome, TurnExecution, TurnExecutor};
    use crate::service::{BootstrapStatus, RuntimeService, RuntimeSnapshot};

    type Publications = Arc<(Mutex<Vec<(String, String, Value)>>, Condvar)>;

    #[derive(Clone)]
    struct FakeHost {
        publications: Publications,
        store: RunStore,
    }

    impl FakeHost {
        fn new(store: RunStore) -> Self {
            Self {
                publications: Arc::new((Mutex::new(Vec::new()), Condvar::new())),
                store,
            }
        }

        fn wait_for_state(&self, state: &str) -> Value {
            let (lock, changed) = &*self.publications;
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut values = lock.lock().unwrap();
            loop {
                if let Some((_, _, attrs)) = values
                    .iter()
                    .rev()
                    .find(|(kind, item_state, _)| kind == "agent_run" && item_state == state)
                {
                    return attrs.clone();
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(
                    !remaining.is_zero(),
                    "state {state} not published: {values:?}"
                );
                let (next, _) = changed.wait_timeout(values, remaining).unwrap();
                values = next;
            }
        }
    }

    impl HostApi for FakeHost {
        fn register(&self, _pid: u32) -> Result<(), String> {
            Ok(())
        }

        fn poll(&self, _after: u64) -> Result<PollResponse, String> {
            Err("not used".into())
        }

        fn publish_entity(
            &self,
            _op: &str,
            kind: &str,
            _object_id: &str,
            state: &str,
            attrs: Value,
        ) -> Result<(), String> {
            if kind == "agent_run" {
                if let Some(event) = attrs.get("latestEvent") {
                    let event: RunEvent = serde_json::from_value(event.clone()).unwrap();
                    let persisted = self.store.replay(&event.run_id, event.seq - 1, 1).unwrap();
                    assert_eq!(persisted, vec![event], "journal must win before emit");
                }
            }
            let (lock, changed) = &*self.publications;
            lock.lock()
                .unwrap()
                .push((kind.into(), state.into(), attrs));
            changed.notify_all();
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeExecutorState {
        calls: Vec<(String, Option<String>, String)>,
        block_first: bool,
        first_started: bool,
        release_first: bool,
        cancel_called: bool,
    }

    #[derive(Clone, Default)]
    struct FakeExecutor {
        state: Arc<(Mutex<FakeExecutorState>, Condvar)>,
    }

    impl FakeExecutor {
        fn blocking_first() -> Self {
            let executor = Self::default();
            executor.state.0.lock().unwrap().block_first = true;
            executor
        }

        fn wait_first_started(&self) {
            let (lock, changed) = &*self.state;
            let mut state = lock.lock().unwrap();
            while !state.first_started {
                state = changed.wait(state).unwrap();
            }
        }

        fn release_first(&self) {
            let (lock, changed) = &*self.state;
            lock.lock().unwrap().release_first = true;
            changed.notify_all();
        }

        fn wait_calls(&self, count: usize) {
            let (lock, changed) = &*self.state;
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut state = lock.lock().unwrap();
            while state.calls.len() < count {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(
                    !remaining.is_zero(),
                    "expected {count} calls: {:?}",
                    state.calls
                );
                let (next, _) = changed.wait_timeout(state, remaining).unwrap();
                state = next;
            }
        }
    }

    impl TurnExecutor for FakeExecutor {
        fn execute(
            &self,
            request: TurnExecution,
            sink: &mut dyn BackendEventSink,
        ) -> Result<ExecutionOutcome, String> {
            let (lock, changed) = &*self.state;
            let mut state = lock.lock().unwrap();
            state.calls.push((
                request.run_id.clone(),
                request.backend_session_id.clone(),
                request.prompt.clone(),
            ));
            changed.notify_all();
            let number = state.calls.len();
            if number == 1 {
                state.first_started = true;
                changed.notify_all();
                while state.block_first && !state.release_first && !state.cancel_called {
                    state = changed.wait(state).unwrap();
                }
            }
            let cancelled = state.cancel_called;
            drop(state);
            if cancelled {
                return Ok(ExecutionOutcome {
                    exit_code: -1,
                    ..ExecutionOutcome::default()
                });
            }
            let session = "018f0000-0000-7000-8000-000000000090";
            sink.emit(BackendEvent::Session {
                id: session.into(),
                model: Some("synthetic".into()),
            })?;
            sink.emit(BackendEvent::AssistantDelta {
                text: "Делаю".into(),
            })?;
            sink.emit(BackendEvent::FileChanged {
                guest_path: "/home/dev/synthetic-project/smoke.txt".into(),
                change: "created".into(),
            })?;
            sink.emit(BackendEvent::AssistantMessage {
                text: format!("Готово {number}"),
            })?;
            sink.emit(BackendEvent::TurnCompleted)?;
            Ok(ExecutionOutcome {
                exit_code: 0,
                backend_session_id: Some(session.into()),
                result: None,
                backend_reported_error: false,
                turn_completed: true,
                stderr_bytes: 0,
            })
        }

        fn cancel(&self, _run_id: &str, _vm_name: Option<&str>) -> Result<bool, String> {
            let (lock, changed) = &*self.state;
            let mut state = lock.lock().unwrap();
            state.cancel_called = true;
            changed.notify_all();
            Ok(true)
        }
    }

    #[derive(Clone)]
    struct FakeService {
        project: PathBuf,
        modules: Vec<String>,
        environment: Option<BootstrapStatus>,
    }

    impl FakeService {
        fn snapshot(&self) -> RuntimeSnapshot {
            let canonical = fs::canonicalize(&self.project).unwrap();
            RuntimeSnapshot {
                project_id: ProjectIdentity::from_path(&canonical).unwrap().project_id,
                display_name: "synthetic-project".into(),
                cwd: canonical.to_string_lossy().into_owned(),
                vm_name: "synthetic-project-a1b2c3d4e5f6".into(),
                vm: Some(InventoryVm {
                    name: "synthetic-project-a1b2c3d4e5f6".into(),
                    state: "running".into(),
                    management: "managed".into(),
                    record: Some(VmRecord {
                        name: "synthetic-project-a1b2c3d4e5f6".into(),
                        source: "project".into(),
                        modules: self.modules.clone(),
                        resources: VmResources::default(),
                        user: "dev".into(),
                        workspace: VmWorkspace {
                            mode_name: "mount".into(),
                            guest_path: "/home/dev/synthetic-project".into(),
                            host_path: Some(canonical.to_string_lossy().into_owned()),
                            repo: None,
                            git_ref: None,
                        },
                        mounts: Vec::new(),
                    }),
                }),
                created_spec: false,
                shell_command: "avm shell synthetic-project-a1b2c3d4e5f6".into(),
                environment: self.environment.clone(),
            }
        }
    }

    impl RuntimeService for FakeService {
        fn inventory(&self) -> Result<Vec<InventoryVm>, String> {
            Ok(self.snapshot().vm.into_iter().collect())
        }

        fn status(&self, _cwd: &Path) -> Result<RuntimeSnapshot, String> {
            Ok(self.snapshot())
        }

        fn ensure(&self, _cwd: &Path) -> Result<RuntimeSnapshot, String> {
            Ok(self.snapshot())
        }

        fn stop(&self, _cwd: &Path) -> Result<RuntimeSnapshot, String> {
            Ok(self.snapshot())
        }

        fn restart(&self, _cwd: &Path) -> Result<RuntimeSnapshot, String> {
            Ok(self.snapshot())
        }
    }

    fn fixture(
        tag: &str,
        executor: FakeExecutor,
    ) -> (PathBuf, RunSupervisor<FakeHost>, FakeService) {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-supervisor-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let project = root.join("synthetic-project");
        fs::create_dir_all(&project).unwrap();
        let store = RunStore::new(root.join("private/runs"));
        let host = FakeHost::new(store.clone());
        let supervisor = RunSupervisor::new(host, store, Arc::new(executor));
        let service = FakeService {
            project: project.clone(),
            modules: vec!["claude".into(), "codex".into()],
            environment: None,
        };
        (root, supervisor, service)
    }

    #[test]
    fn run_snapshot_keeps_modules_and_resources_for_the_ui() {
        let (root, supervisor, service) = fixture("snapshot-attrs", FakeExecutor::default());
        let snapshot = service.snapshot();

        supervisor
            .vm_entities()
            .publish_snapshot(&snapshot)
            .unwrap();

        let publications = supervisor.host().publications.0.lock().unwrap();
        let (_, state, attrs) = publications
            .iter()
            .rev()
            .find(|(kind, _, _)| kind == "vm")
            .unwrap();
        assert_eq!(state, "running");
        assert_eq!(attrs["modules"], json!(["claude", "codex"]));
        assert!(attrs["resources"].is_object());
        assert_eq!(attrs["guestWorkspace"], "/home/dev/synthetic-project");
        drop(publications);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn run_events_are_persisted_before_publish_and_resume_command_is_exposed() {
        let executor = FakeExecutor::default();
        let (root, supervisor, service) = fixture("stream", executor);
        let receipt = supervisor
            .submit(
                service,
                SendRequest {
                    cwd: root.join("synthetic-project"),
                    project_id: None,
                    backend: Backend::Claude,
                    run_id: None,
                    message: "сделай smoke".into(),
                },
            )
            .unwrap();

        let attrs = supervisor.host().wait_for_state("completed");
        assert_eq!(attrs["runId"], json!(receipt.run_id));
        assert_eq!(
            attrs["resumeCommand"],
            json!("claude --resume 018f0000-0000-7000-8000-000000000090")
        );
        let events = supervisor.store().replay(&receipt.run_id, 0, 64).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            vec![
                "run.started",
                "user.message",
                "run.started",
                "assistant.delta",
                "file.changed",
                "assistant.message",
                "result.completed",
            ]
        );
        assert_eq!(
            events[4].payload["path"],
            json!(fs::canonicalize(root.join("synthetic-project"))
                .unwrap()
                .join("smoke.txt")
                .to_string_lossy())
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn earliest_failure_event_is_recoverable_with_project_metadata() {
        let executor = FakeExecutor::default();
        let (root, supervisor, _) = fixture("early-failure", executor);
        let project = ProjectIdentity::from_path(&root.join("synthetic-project")).unwrap();
        let run_id = "run-018f000000000091";
        let turn_id = "turn-018f000000000092";
        let mut publisher = RunPublisher::new(
            supervisor.host().clone(),
            supervisor.store().clone(),
            run_id.into(),
            project.clone(),
            Backend::Claude,
            project.vm_name.clone(),
            "avm shell synthetic".into(),
            0,
            None,
        );

        publisher
            .emit(
                turn_id,
                "run.failed",
                json!({"error":"synthetic environment failure"}),
                "failed",
            )
            .unwrap();

        let summary = supervisor.store().summary(run_id).unwrap().unwrap();
        assert_eq!(summary.project_id, project.project_id);
        assert_eq!(Path::new(&summary.cwd), project.canonical_path.as_path());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn absent_backend_module_fails_before_agent_execution() {
        let executor = FakeExecutor::default();
        let (root, supervisor, mut service) = fixture("missing-module", executor.clone());
        service.modules = vec!["claude".into()];

        supervisor
            .submit(
                service,
                SendRequest {
                    cwd: root.join("synthetic-project"),
                    project_id: None,
                    backend: Backend::Codex,
                    run_id: None,
                    message: "не запускать".into(),
                },
            )
            .unwrap();

        let attrs = supervisor.host().wait_for_state("failed");
        assert!(attrs["latestEvent"]["payload"]["error"]
            .as_str()
            .unwrap()
            .contains("Codex"));
        assert!(executor.state.0.lock().unwrap().calls.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_backend_credential_fails_before_agent_execution() {
        let executor = FakeExecutor::default();
        let (root, supervisor, mut service) = fixture("missing-credential", executor.clone());
        service.environment = Some(BootstrapStatus {
            fingerprint: "fixture".into(),
            files: 0,
            skipped: 0,
            credentials: BootstrapCredentialStatus {
                claude: "missing".into(),
                codex: "ready".into(),
            },
            proxy_configured: false,
        });

        supervisor
            .submit(
                service,
                SendRequest {
                    cwd: root.join("synthetic-project"),
                    project_id: None,
                    backend: Backend::Claude,
                    run_id: None,
                    message: "не запускать без авторизации".into(),
                },
            )
            .unwrap();

        let attrs = supervisor.host().wait_for_state("failed");
        let error = attrs["latestEvent"]["payload"]["error"].as_str().unwrap();
        assert!(error.contains("Claude"), "{error}");
        assert!(error.contains("авториз"), "{error}");
        assert!(executor.state.0.lock().unwrap().calls.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn follow_ups_are_bounded_fifo_and_run_with_backend_resume() {
        let executor = FakeExecutor::blocking_first();
        let (root, supervisor, service) = fixture("queue", executor.clone());
        let first = supervisor
            .submit(
                service.clone(),
                SendRequest {
                    cwd: root.join("synthetic-project"),
                    project_id: None,
                    backend: Backend::Codex,
                    run_id: None,
                    message: "первый".into(),
                },
            )
            .unwrap();
        executor.wait_first_started();
        let queued = supervisor
            .submit(
                service.clone(),
                SendRequest {
                    cwd: root.join("synthetic-project"),
                    project_id: None,
                    backend: Backend::Codex,
                    run_id: Some(first.run_id.clone()),
                    message: "второй".into(),
                },
            )
            .unwrap();
        assert!(queued.queued);
        let third = supervisor
            .submit(
                service,
                SendRequest {
                    cwd: root.join("synthetic-project"),
                    project_id: None,
                    backend: Backend::Codex,
                    run_id: Some(first.run_id.clone()),
                    message: "третий".into(),
                },
            )
            .unwrap();
        assert!(third.queued);
        executor.release_first();
        executor.wait_calls(3);
        supervisor.host().wait_for_state("completed");
        let state = executor.state.0.lock().unwrap();
        assert_eq!(state.calls.len(), 3);
        assert_eq!(
            state.calls[1].1.as_deref(),
            Some("018f0000-0000-7000-8000-000000000090")
        );
        assert_eq!(state.calls[1].2, "второй");
        assert_eq!(state.calls[2].2, "третий");
        drop(state);
        let deadline = Instant::now() + Duration::from_secs(3);
        while supervisor.is_active(&first.run_id) {
            assert!(Instant::now() < deadline, "run worker не завершил cleanup");
            std::thread::yield_now();
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn follow_up_queue_applies_backpressure_without_displacing_accepted_turns() {
        let executor = FakeExecutor::blocking_first();
        let (root, supervisor, service) = fixture("queue-limit", executor.clone());
        let first = supervisor
            .submit(
                service.clone(),
                SendRequest {
                    cwd: root.join("synthetic-project"),
                    project_id: None,
                    backend: Backend::Claude,
                    run_id: None,
                    message: "первый".into(),
                },
            )
            .unwrap();
        executor.wait_first_started();
        for index in 0..MAX_QUEUED_TURNS {
            let receipt = supervisor
                .submit(
                    service.clone(),
                    SendRequest {
                        cwd: root.join("synthetic-project"),
                        project_id: None,
                        backend: Backend::Claude,
                        run_id: Some(first.run_id.clone()),
                        message: format!("queued-{index}"),
                    },
                )
                .unwrap();
            assert!(receipt.queued);
        }

        let error = supervisor
            .submit(
                service,
                SendRequest {
                    cwd: root.join("synthetic-project"),
                    project_id: None,
                    backend: Backend::Claude,
                    run_id: Some(first.run_id.clone()),
                    message: "must-not-displace".into(),
                },
            )
            .unwrap_err();

        assert!(error.contains("очередь Agent VM заполнена"));
        assert_eq!(
            supervisor
                .active
                .lock()
                .unwrap()
                .values()
                .next()
                .unwrap()
                .queued
                .len(),
            MAX_QUEUED_TURNS
        );
        assert!(supervisor.cancel(&first.run_id).unwrap());
        supervisor.host().wait_for_state("cancelled");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancel_unblocks_the_worker_clears_queue_and_publishes_cancelled() {
        let executor = FakeExecutor::blocking_first();
        let (root, supervisor, service) = fixture("cancel", executor.clone());
        let receipt = supervisor
            .submit(
                service,
                SendRequest {
                    cwd: root.join("synthetic-project"),
                    project_id: None,
                    backend: Backend::Claude,
                    run_id: None,
                    message: "долго".into(),
                },
            )
            .unwrap();
        executor.wait_first_started();

        assert!(supervisor.cancel(&receipt.run_id).unwrap());

        supervisor.host().wait_for_state("cancelled");
        assert!(!supervisor.is_active(&receipt.run_id));
        assert!(executor.state.0.lock().unwrap().cancel_called);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_marks_only_unfinished_latest_run_interrupted_and_is_idempotent() {
        let executor = FakeExecutor::default();
        let (root, supervisor, _service) = fixture("recovery", executor.clone());
        let project = ProjectIdentity::from_path(&root.join("synthetic-project")).unwrap();
        let run_id = "run-018f000000000077";
        for event in [
            RunEvent {
                run_id: run_id.into(),
                turn_id: "turn-018f000000000078".into(),
                seq: 1,
                at: 1_785_250_000_001,
                event_type: "run.started".into(),
                payload: json!({
                    "projectId":project.project_id,
                    "project":project.display_name,
                    "cwd":project.canonical_path,
                    "backendSessionId":"valid-recovery-session"
                }),
                backend: Backend::Claude,
                vm: "synthetic-project-a1b2c3d4e5f6".into(),
            },
            RunEvent {
                run_id: run_id.into(),
                turn_id: "turn-018f000000000078".into(),
                seq: 2,
                at: 1_785_250_000_002,
                event_type: "assistant.delta".into(),
                payload: json!({"text":"working"}),
                backend: Backend::Claude,
                vm: "synthetic-project-a1b2c3d4e5f6".into(),
            },
        ] {
            supervisor.store().append(&event).unwrap();
        }

        assert_eq!(supervisor.recover().unwrap(), 1);
        assert!(
            executor.state.0.lock().unwrap().cancel_called,
            "recovery must cancel the persisted guest pid before publishing interrupted"
        );
        let summary = supervisor.store().summary(run_id).unwrap().unwrap();
        assert_eq!(summary.state, "interrupted");
        assert_eq!(summary.last_seq, 3);
        let attrs = supervisor.host().wait_for_state("interrupted");
        assert_eq!(attrs["runId"], run_id);
        assert_eq!(attrs["recovered"], true);
        assert_eq!(
            attrs["resumeCommand"],
            "claude --resume valid-recovery-session"
        );
        assert_eq!(attrs["latestEvent"]["payload"]["guestCleanup"], "completed");
        assert!(supervisor.cancel(run_id).unwrap());

        assert_eq!(supervisor.recover().unwrap(), 1);
        assert_eq!(
            supervisor
                .store()
                .summary(run_id)
                .unwrap()
                .unwrap()
                .last_seq,
            3,
            "повторный recovery не дописывает второй interrupted"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn terminal_resume_command_rejects_unsafe_backend_session_identity() {
        assert_eq!(
            terminal_resume_command(Backend::Claude, "valid-session_42"),
            Some("claude --resume valid-session_42".into())
        );
        assert_eq!(
            terminal_resume_command(Backend::Codex, "session;open /tmp/leak"),
            None
        );
    }

    #[test]
    fn file_events_map_only_primary_or_declared_additional_mounts() {
        let record = VmRecord {
            name: "synthetic-project-a1b2c3d4e5f6".into(),
            source: "project".into(),
            modules: vec![],
            resources: VmResources::default(),
            user: "dev".into(),
            workspace: VmWorkspace {
                mode_name: "mount".into(),
                guest_path: "/home/dev/main".into(),
                host_path: Some("/host/main".into()),
                repo: None,
                git_ref: None,
            },
            mounts: vec![crate::inventory::VmMount {
                host_path: "/host/shared".into(),
                guest_path: "/home/dev/shared".into(),
            }],
        };

        assert_eq!(
            map_record_guest_path(&record, Path::new("/home/dev/shared/src/lib.rs")),
            Some((
                PathBuf::from("/host/shared"),
                PathBuf::from("/host/shared/src/lib.rs")
            ))
        );
        assert_eq!(
            map_record_guest_path(&record, Path::new("src/main.rs")),
            Some((
                PathBuf::from("/host/main"),
                PathBuf::from("/host/main/src/main.rs")
            ))
        );
        assert_eq!(
            map_record_guest_path(&record, Path::new("/home/dev/other/private.txt")),
            None
        );
    }
}
