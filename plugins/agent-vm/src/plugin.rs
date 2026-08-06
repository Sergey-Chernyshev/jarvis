use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use serde_json::{json, Value};
use zeroize::Zeroize;

use crate::host::{HostApi, HostEvent};
use crate::project::ProjectIdentity;
use crate::run_event::Backend;
use crate::run_supervisor::{RunSupervisor, SendRequest};
use crate::service::{validate_project_id, RuntimeService, RuntimeSnapshot};
use crate::vm_entity::VmEntityPublisher;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_PUBLIC_ERROR_CHARS: usize = 400;

pub struct PluginEnvironment {
    pub socket: PathBuf,
    pub plugin_id: String,
    pub token: String,
    pub protocol_version: u32,
}

impl PluginEnvironment {
    pub fn from_current() -> Result<Self, String> {
        let values = std::env::vars().collect::<BTreeMap<_, _>>();
        Self::from_values(&values)
    }

    pub fn from_values(values: &BTreeMap<String, String>) -> Result<Self, String> {
        let socket = values
            .get("JARVIS_SOCKET")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| "JARVIS_SOCKET отсутствует".to_string())?;
        crate::runtime_paths::RuntimePaths::from_socket(&socket)?;
        let plugin_id = values
            .get("JARVIS_PLUGIN_ID")
            .filter(|value| value.as_str() == "agent-vm")
            .cloned()
            .ok_or_else(|| "JARVIS_PLUGIN_ID не соответствует agent-vm".to_string())?;
        let token = values
            .get("JARVIS_PLUGIN_TOKEN")
            .filter(|value| {
                value.len() == 64
                    && value
                        .bytes()
                        .all(|ch| ch.is_ascii_digit() || (b'a'..=b'f').contains(&ch))
            })
            .cloned()
            .ok_or_else(|| "JARVIS_PLUGIN_TOKEN имеет некорректный формат".to_string())?;
        let protocol_version = values
            .get("JARVIS_PLUGIN_PROTOCOL")
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|version| *version == PROTOCOL_VERSION)
            .ok_or_else(|| "JARVIS_PLUGIN_PROTOCOL несовместим".to_string())?;
        Ok(Self {
            socket,
            plugin_id,
            token,
            protocol_version,
        })
    }
}

pub fn public_error(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if [
        "authorization",
        "bearer",
        "credential",
        "api_key",
        "api-key",
        "token",
        "proxy",
        "password",
        "secret",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return "Agent VM operation failed; sensitive details withheld".into();
    }
    error
        .replace(['\r', '\n'], " ")
        .chars()
        .take(MAX_PUBLIC_ERROR_CHARS)
        .collect()
}

pub fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == b'-' || ch == b'_')
}

pub struct Dispatcher<S: RuntimeService, H: HostApi> {
    service: S,
    host: H,
    vm_entities: VmEntityPublisher<H>,
    inventory_in_flight: Arc<AtomicBool>,
    supervisor: Option<RunSupervisor<H>>,
}

impl<S: RuntimeService, H: HostApi> Dispatcher<S, H> {
    pub fn new(service: S, host: H) -> Self {
        let vm_entities = VmEntityPublisher::new(host.clone());
        Self {
            service,
            host,
            vm_entities,
            inventory_in_flight: Arc::new(AtomicBool::new(false)),
            supervisor: None,
        }
    }

    pub fn with_supervisor(service: S, host: H, supervisor: RunSupervisor<H>) -> Self {
        let vm_entities = supervisor.vm_entities().clone();
        Self {
            service,
            host,
            vm_entities,
            inventory_in_flight: Arc::new(AtomicBool::new(false)),
            supervisor: Some(supervisor),
        }
    }

    pub fn schedule_inventory_reconcile(&self) -> bool {
        if self
            .inventory_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        let service = self.service.clone();
        let vm_entities = self.vm_entities.clone();
        let inventory_in_flight = self.inventory_in_flight.clone();
        let spawned = thread::Builder::new()
            .name("agent-vm-inventory".into())
            .spawn(move || {
                let _guard = InventoryJobGuard(inventory_in_flight);
                let checkpoint = vm_entities.checkpoint();
                if let Ok(inventory) = service.inventory() {
                    let _ = vm_entities.reconcile_inventory(checkpoint, &service, inventory);
                }
            });
        if spawned.is_err() {
            self.inventory_in_flight.store(false, Ordering::Release);
            return false;
        }
        true
    }

    pub fn poll_and_reconcile(&mut self, after: u64) -> Result<u64, String> {
        let batch = self.host.poll(after)?;
        let next_seq = after.max(batch.next_seq);
        let heartbeat = batch.events.is_empty();
        for event in batch.events {
            self.process(event)?;
        }
        if heartbeat {
            self.schedule_inventory_reconcile();
        }
        Ok(next_seq)
    }

    pub fn process(&mut self, event: HostEvent) -> Result<(), String> {
        if event.kind != "command" {
            return Ok(());
        }
        let request_id = event.payload.request_id;
        if !valid_request_id(&request_id) {
            return Err("Jarvis прислал некорректный requestId".into());
        }
        let name = event.payload.name;
        let context = operation_context(&event.payload.args);
        self.publish_operation(&request_id, &name, "started", context.clone())?;

        if name == "runtime.inventory" {
            let scheduled = self.schedule_inventory_reconcile();
            return self.publish_operation(
                &request_id,
                &name,
                "done",
                operation_attrs(&context, json!({"scheduled":scheduled})),
            );
        }

        // Освобождение кэша образов не привязано к проекту: кэш общий для всех
        // VM. Чистим только по явной просьбе — существующие VM не пострадают,
        // но следующая скачает образ заново.
        if name == "runtime.releaseCache" {
            return match self.service.release_image_cache() {
                Ok(freed) => {
                    let disk = self.service.disk_usage();
                    self.publish_operation(
                        &request_id,
                        &name,
                        "done",
                        operation_attrs(&context, json!({"freedBytes":freed,"disk":disk})),
                    )
                }
                Err(error) => self.publish_operation(
                    &request_id,
                    &name,
                    "error",
                    operation_attrs(&context, json!({"error": public_error(&error)})),
                ),
            };
        }

        if let Some(result) = self.dispatch_supervisor_command(&name, &event.payload.args) {
            return match result {
                Ok(attrs) => self.publish_operation(
                    &request_id,
                    &name,
                    "done",
                    operation_attrs(&context, attrs),
                ),
                Err(error) => self.publish_operation(
                    &request_id,
                    &name,
                    "error",
                    operation_attrs(&context, json!({"error": public_error(&error)})),
                ),
            };
        }

        let result = self.dispatch_project_command(&name, &event.payload.args);
        match result {
            Ok(snapshot) => {
                self.publish_snapshot(&snapshot)?;
                self.publish_operation(
                    &request_id,
                    &name,
                    "done",
                    operation_attrs(&context, json!({
                        "projectId": snapshot.project_id,
                        "vmName": snapshot.vm_name,
                        "state": snapshot.vm.as_ref().map(|vm| vm.state.as_str()).unwrap_or("absent"),
                        "shellCommand": snapshot.shell_command,
                        "createdSpec": snapshot.created_spec,
                        // занятое место отдаём ответом операции, а не сущностью:
                        // размер меняется постоянно и ломал бы дедупликацию
                        // публикаций vm.* (см. same_runtime_binding)
                        "disk": snapshot.disk,
                        "environment": snapshot.environment
                    })),
                )
            }
            Err(error) => {
                self.publish_residual_project_snapshot(&name, &event.payload.args);
                self.publish_operation(
                    &request_id,
                    &name,
                    "error",
                    operation_attrs(&context, json!({"error": public_error(&error)})),
                )
            }
        }
    }

    fn dispatch_supervisor_command(
        &self,
        name: &str,
        args: &Value,
    ) -> Option<Result<Value, String>> {
        if name == "runtime.commands" && args.get("runId").and_then(Value::as_str).is_none() {
            return None;
        }
        if !matches!(
            name,
            "runtime.send"
                | "runtime.cancel"
                | "runtime.replay"
                | "runtime.commands"
                | "runtime.runs"
        ) {
            return None;
        }
        let Some(supervisor) = &self.supervisor else {
            return Some(Err("headless Agent VM supervisor недоступен".into()));
        };
        Some(match name {
            "runtime.send" => {
                let cwd = required_string(args, "cwd").map(PathBuf::from);
                let backend = required_string(args, "agent")
                    .or_else(|_| required_string(args, "backend"))
                    .and_then(|value| Backend::parse(&value));
                let message = required_string(args, "message");
                match (cwd, backend, message) {
                    (Ok(cwd), Ok(backend), Ok(message)) => supervisor
                        .submit(
                            self.service.clone(),
                            SendRequest {
                                cwd,
                                project_id: optional_string(args, "projectId"),
                                backend,
                                run_id: optional_string(args, "runId"),
                                message,
                            },
                        )
                        .map(|receipt| {
                            json!({
                                "runId":receipt.run_id,
                                "turnId":receipt.turn_id,
                                "queued":receipt.queued
                            })
                        }),
                    (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => Err(error),
                }
            }
            "runtime.cancel" => required_string(args, "runId")
                .and_then(|run_id| supervisor.cancel(&run_id))
                .map(|cancelled| json!({"cancelled":cancelled})),
            "runtime.replay" => required_string(args, "runId").and_then(|run_id| {
                let after_seq = args.get("afterSeq").and_then(Value::as_u64).unwrap_or(0);
                let limit: usize = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(64)
                    .try_into()
                    .map_err(|_| "replay limit имеет invalid type".to_string())?;
                if limit == 0 || limit >= crate::run_store::MAX_REPLAY_EVENTS {
                    return Err("replay limit должен быть от 1 до 255".into());
                }
                let mut events = supervisor.replay(&run_id, after_seq, limit + 1)?;
                let store_has_more = events.len() > limit;
                events.truncate(limit);
                let (events, payload_has_more) = fit_replay_events(events)?;
                let has_more = store_has_more || payload_has_more;
                let next_seq = events.last().map(|event| event.seq).unwrap_or(after_seq);
                Ok(json!({
                    "runId":run_id,
                    "events":events,
                    "nextSeq":next_seq,
                    "hasMore":has_more
                }))
            }),
            "runtime.commands" => {
                required_string(args, "runId").and_then(|run_id| supervisor.commands(&run_id))
            }
            // Список прогонов проекта — источник для экрана «чаты проекта».
            // projectId выводим из canonical cwd, а не принимаем от UI: так же,
            // как остальные runtime.* (спека §16 — путь канонизирует backend).
            "runtime.runs" => (|| {
                let cwd = required_string(args, "cwd")?;
                let project = crate::project::ProjectIdentity::from_path(Path::new(&cwd))?;
                crate::service::validate_project_id(
                    &project,
                    args.get("projectId").and_then(Value::as_str),
                )?;
                let limit: usize = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(50)
                    .clamp(1, 200) as usize;
                supervisor.project_runs(&project.project_id, limit)
            })(),
            _ => unreachable!(),
        })
    }

    fn dispatch_project_command(
        &self,
        name: &str,
        args: &Value,
    ) -> Result<RuntimeSnapshot, String> {
        let cwd = args
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|cwd| !cwd.is_empty())
            .ok_or_else(|| "cwd обязателен".to_string())?;
        let cwd = Path::new(cwd);
        let project = ProjectIdentity::from_path(cwd)?;
        validate_project_id(&project, args.get("projectId").and_then(Value::as_str))?;
        match name {
            "runtime.status" | "runtime.commands" => self.service.status(cwd),
            "runtime.ensure" => self.service.ensure(cwd),
            "runtime.stop" => self.service.stop(cwd),
            "runtime.restart" => self.service.restart(cwd),
            _ => Err(format!("неподдерживаемая Agent VM command {name}")),
        }
    }

    fn publish_snapshot(&mut self, snapshot: &RuntimeSnapshot) -> Result<(), String> {
        self.vm_entities.publish_snapshot(snapshot)
    }

    fn publish_residual_project_snapshot(&mut self, name: &str, args: &Value) {
        if !matches!(name, "runtime.ensure" | "runtime.stop" | "runtime.restart") {
            return;
        }
        let Some(cwd) = args
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|cwd| !cwd.is_empty())
        else {
            return;
        };
        let Ok(project) = ProjectIdentity::from_path(Path::new(cwd)) else {
            return;
        };
        if validate_project_id(&project, args.get("projectId").and_then(Value::as_str)).is_err() {
            return;
        }
        if let Ok(snapshot) = self.service.status(&project.canonical_path) {
            let _ = self.publish_snapshot(&snapshot);
        }
    }

    fn publish_operation(
        &self,
        request_id: &str,
        name: &str,
        state: &str,
        attrs: Value,
    ) -> Result<(), String> {
        let mut payload = serde_json::Map::from_iter([
            ("requestId".into(), Value::String(request_id.into())),
            ("command".into(), Value::String(name.into())),
        ]);
        if let Value::Object(extra) = attrs {
            payload.extend(extra);
        }
        self.host.publish_entity(
            "upsert",
            "operation",
            request_id,
            state,
            Value::Object(payload),
        )
    }
}

struct InventoryJobGuard(Arc<AtomicBool>);

impl Drop for InventoryJobGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn required_string(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{key} обязателен"))
}

fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn operation_context(args: &Value) -> Value {
    let Some(cwd) = args.get("cwd").and_then(Value::as_str) else {
        return json!({});
    };
    let Ok(identity) = ProjectIdentity::from_path(Path::new(cwd)) else {
        return json!({});
    };
    let mut context = serde_json::Map::from_iter([
        ("projectId".into(), Value::String(identity.project_id)),
        ("project".into(), Value::String(identity.display_name)),
        (
            "cwd".into(),
            Value::String(identity.canonical_path.to_string_lossy().into_owned()),
        ),
    ]);
    if let Some(run_id) = args
        .get("runId")
        .and_then(Value::as_str)
        .filter(|value| valid_request_id(value))
    {
        context.insert("runId".into(), Value::String(run_id.into()));
    }
    Value::Object(context)
}

fn operation_attrs(context: &Value, attrs: Value) -> Value {
    let mut merged = context.as_object().cloned().unwrap_or_default();
    if let Value::Object(extra) = attrs {
        merged.extend(extra);
    }
    Value::Object(merged)
}

fn fit_replay_events(
    events: Vec<crate::run_event::RunEvent>,
) -> Result<(Vec<crate::run_event::RunEvent>, bool), String> {
    let original_len = events.len();
    let mut fitted = Vec::new();
    for event in events {
        fitted.push(event);
        let mut bytes = serde_json::to_vec(&fitted)
            .map_err(|_| "не сериализовать replay events".to_string())?;
        if bytes.len() > 48 * 1024 {
            bytes.zeroize();
            fitted.pop();
            break;
        }
        bytes.zeroize();
    }
    if fitted.is_empty() && original_len > 0 {
        return Err("один replay event превышает entity payload limit".into());
    }
    let has_more = fitted.len() < original_len;
    Ok((fitted, has_more))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::path::Path;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    use serde_json::{json, Value};

    use super::*;
    use crate::guest_bootstrap::BootstrapCredentialStatus;
    use crate::host::{HostApi, PollResponse};
    use crate::inventory::{InventoryVm, VmRecord, VmResources, VmWorkspace};
    use crate::run_event::{Backend, BackendEvent, RunEvent};
    use crate::run_executor::{BackendEventSink, ExecutionOutcome, TurnExecution, TurnExecutor};
    use crate::run_store::RunStore;
    use crate::run_supervisor::RunSupervisor;
    use crate::service::{BootstrapStatus, RuntimeService, RuntimeSnapshot};

    struct Publication {
        op: String,
        kind: String,
        object_id: String,
        state: String,
        attrs: Value,
    }

    #[derive(Clone, Default)]
    struct FakeHost {
        publications: Arc<Mutex<Vec<Publication>>>,
        polls: Arc<Mutex<VecDeque<PollResponse>>>,
        poll_after: Arc<Mutex<Vec<u64>>>,
        publish_control: Arc<(Mutex<PublishControl>, Condvar)>,
        persisted_vm_ids: Arc<Mutex<Vec<String>>>,
    }

    #[derive(Default)]
    struct PublishControl {
        blocked_state: Option<String>,
        started: bool,
        released: bool,
    }

    impl FakeHost {
        fn push_poll(&self, response: PollResponse) {
            self.polls.lock().unwrap().push_back(response);
        }

        fn wait_for_publication(&self, kind: &str, state: &str, count: usize) {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let found = self
                    .publications
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|item| item.kind == kind && item.state == state)
                    .count();
                if found >= count {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "missing {kind}/{state} publication #{count}"
                );
                std::thread::yield_now();
            }
        }

        fn block_vm_publication(&self, state: &str) {
            let mut control = self.publish_control.0.lock().unwrap();
            control.blocked_state = Some(state.into());
            control.started = false;
            control.released = false;
        }

        fn wait_for_blocked_publication(&self) {
            let (lock, changed) = &*self.publish_control;
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut control = lock.lock().unwrap();
            while !control.started {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "blocked publication did not start");
                let (next, _) = changed.wait_timeout(control, remaining).unwrap();
                control = next;
            }
        }

        fn release_vm_publication(&self) {
            let (lock, changed) = &*self.publish_control;
            lock.lock().unwrap().released = true;
            changed.notify_all();
        }

        fn seed_persisted_vm(&self, vm_name: &str) {
            self.persisted_vm_ids
                .lock()
                .unwrap()
                .push(format!("vm.{vm_name}"));
            self.publications.lock().unwrap().push(Publication {
                op: "upsert".into(),
                kind: "vm".into(),
                object_id: vm_name.into(),
                state: "running".into(),
                attrs: json!({"persisted":true}),
            });
        }
    }

    impl HostApi for FakeHost {
        fn register(&self, _pid: u32) -> Result<(), String> {
            Ok(())
        }

        fn poll(&self, after: u64) -> Result<PollResponse, String> {
            self.poll_after.lock().unwrap().push(after);
            self.polls
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "unexpected poll".into())
        }

        fn publish_entity(
            &self,
            op: &str,
            kind: &str,
            object_id: &str,
            state: &str,
            attrs: Value,
        ) -> Result<(), String> {
            assert!(matches!(op, "upsert" | "remove"));
            if kind == "vm" {
                let (lock, changed) = &*self.publish_control;
                let mut control = lock.lock().unwrap();
                if control.blocked_state.as_deref() == Some(state) && !control.released {
                    control.started = true;
                    changed.notify_all();
                    while !control.released {
                        control = changed.wait(control).unwrap();
                    }
                }
            }
            self.publications.lock().unwrap().push(Publication {
                op: op.into(),
                kind: kind.into(),
                object_id: object_id.into(),
                state: state.into(),
                attrs,
            });
            Ok(())
        }

        fn query_vm_entity_ids(&self) -> Result<Vec<String>, String> {
            Ok(self
                .persisted_vm_ids
                .lock()
                .unwrap()
                .iter()
                .filter_map(|id| id.strip_prefix("vm."))
                .map(str::to_owned)
                .collect())
        }
    }

    #[derive(Default)]
    struct InventoryControl {
        calls: usize,
        block: bool,
        released: bool,
        error: Option<String>,
        override_vms: Option<Vec<InventoryVm>>,
    }

    #[derive(Clone)]
    struct FakeService {
        result: Arc<Mutex<Result<RuntimeSnapshot, String>>>,
        calls: Arc<Mutex<Vec<String>>>,
        inventory: Arc<(Mutex<InventoryControl>, Condvar)>,
        paths: Arc<Mutex<Option<crate::runtime_paths::RuntimePaths>>>,
    }

    impl FakeService {
        fn new(result: Result<RuntimeSnapshot, String>) -> Self {
            Self {
                result: Arc::new(Mutex::new(result)),
                calls: Arc::new(Mutex::new(Vec::new())),
                inventory: Arc::new((Mutex::new(InventoryControl::default()), Condvar::new())),
                paths: Arc::new(Mutex::new(None)),
            }
        }

        fn with_paths(self, paths: crate::runtime_paths::RuntimePaths) -> Self {
            *self.paths.lock().unwrap() = Some(paths);
            self
        }

        fn set_inventory_error(&self, error: &str) {
            self.inventory.0.lock().unwrap().error = Some(error.into());
        }

        fn clear_inventory_error(&self) {
            self.inventory.0.lock().unwrap().error = None;
        }

        fn set_inventory_override(&self, vms: Vec<InventoryVm>) {
            self.inventory.0.lock().unwrap().override_vms = Some(vms);
        }

        fn block_inventory(&self) {
            let mut control = self.inventory.0.lock().unwrap();
            control.block = true;
            control.released = false;
        }

        fn release_inventory(&self) {
            let (lock, changed) = &*self.inventory;
            lock.lock().unwrap().released = true;
            changed.notify_all();
        }

        fn wait_for_inventory_calls(&self, count: usize) {
            let (lock, changed) = &*self.inventory;
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut control = lock.lock().unwrap();
            while control.calls < count {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "inventory call #{count} not observed");
                let (next, _) = changed.wait_timeout(control, remaining).unwrap();
                control = next;
            }
        }

        fn inventory_calls(&self) -> usize {
            self.inventory.0.lock().unwrap().calls
        }
    }

    impl RuntimeService for FakeService {
        fn inventory(&self) -> Result<Vec<InventoryVm>, String> {
            let (lock, changed) = &*self.inventory;
            let mut control = lock.lock().unwrap();
            control.calls += 1;
            changed.notify_all();
            while control.block && !control.released {
                control = changed.wait(control).unwrap();
            }
            if let Some(error) = &control.error {
                return Err(error.clone());
            }
            if let Some(vms) = &control.override_vms {
                return Ok(vms.clone());
            }
            drop(control);
            Ok(self
                .result
                .lock()
                .unwrap()
                .as_ref()
                .map(|snapshot| snapshot.vm.clone().into_iter().collect())
                .unwrap_or_default())
        }

        fn status(&self, _cwd: &Path) -> Result<RuntimeSnapshot, String> {
            self.calls.lock().unwrap().push("status".into());
            self.result.lock().unwrap().clone()
        }

        fn ensure(&self, _cwd: &Path) -> Result<RuntimeSnapshot, String> {
            self.calls.lock().unwrap().push("ensure".into());
            self.result.lock().unwrap().clone()
        }

        fn stop(&self, _cwd: &Path) -> Result<RuntimeSnapshot, String> {
            self.calls.lock().unwrap().push("stop".into());
            self.result.lock().unwrap().clone()
        }

        fn restart(&self, _cwd: &Path) -> Result<RuntimeSnapshot, String> {
            self.calls.lock().unwrap().push("restart".into());
            self.result.lock().unwrap().clone()
        }

        // Кэш освобождается на настоящем каталоге, если он задан: иначе тест
        // проверял бы заглушку, а не реальное освобождение места.
        fn release_image_cache(&self) -> Result<u64, String> {
            self.calls.lock().unwrap().push("releaseCache".into());
            match self.paths.lock().unwrap().as_ref() {
                Some(paths) => paths.release_image_cache(),
                None => Ok(0),
            }
        }

        fn disk_usage(&self) -> crate::service::DiskUsage {
            match self.paths.lock().unwrap().as_ref() {
                Some(paths) => paths.disk_usage(),
                None => crate::service::DiskUsage::default(),
            }
        }
    }

    #[derive(Clone)]
    struct FailedLifecycleService {
        residual: RuntimeSnapshot,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl RuntimeService for FailedLifecycleService {
        fn inventory(&self) -> Result<Vec<InventoryVm>, String> {
            Ok(self.residual.vm.clone().into_iter().collect())
        }

        fn status(&self, _cwd: &Path) -> Result<RuntimeSnapshot, String> {
            self.calls.lock().unwrap().push("status".into());
            Ok(self.residual.clone())
        }

        fn ensure(&self, _cwd: &Path) -> Result<RuntimeSnapshot, String> {
            self.calls.lock().unwrap().push("ensure".into());
            Err("synthetic rejected runtime".into())
        }

        fn stop(&self, _cwd: &Path) -> Result<RuntimeSnapshot, String> {
            Err("unexpected stop".into())
        }

        fn restart(&self, _cwd: &Path) -> Result<RuntimeSnapshot, String> {
            Err("unexpected restart".into())
        }
    }

    fn snapshot(root: &Path) -> RuntimeSnapshot {
        let vm_name = "synthetic-project-a1b2c3d4e5f6";
        let identity = crate::project::ProjectIdentity::from_path(root).unwrap();
        RuntimeSnapshot {
            project_id: identity.project_id,
            display_name: identity.display_name,
            cwd: identity.canonical_path.to_string_lossy().into_owned(),
            vm_name: vm_name.into(),
            vm: Some(InventoryVm {
                name: vm_name.into(),
                management: "managed".into(),
                state: "running".into(),
                record: Some(VmRecord {
                    name: vm_name.into(),
                    source: "project".into(),
                    modules: vec!["node".into(), "claude".into(), "codex".into()],
                    resources: VmResources::default(),
                    user: "dev".into(),
                    workspace: VmWorkspace {
                        mode_name: "mount".into(),
                        guest_path: "/home/dev/synthetic".into(),
                        host_path: Some(root.to_string_lossy().into_owned()),
                        repo: None,
                        git_ref: None,
                    },
                    mounts: Vec::new(),
                }),
            }),
            created_spec: false,
            shell_command: format!("avm shell {vm_name}"),
            environment: None,
            disk: Default::default(),
        }
    }

    fn command(name: &str, cwd: &Path) -> crate::host::HostEvent {
        let identity = crate::project::ProjectIdentity::from_path(cwd).unwrap();
        crate::host::HostEvent {
            seq: 7,
            kind: "command".into(),
            payload: crate::host::CommandPayload {
                request_id: "agent-vm-7".into(),
                name: name.into(),
                args: json!({
                    "cwd": cwd,
                    "projectId": identity.project_id
                }),
            },
        }
    }

    fn wait_for_inventory_idle<S: RuntimeService, H: HostApi>(dispatcher: &Dispatcher<S, H>) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while dispatcher.inventory_in_flight.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "inventory reconcile did not become idle"
            );
            std::thread::yield_now();
        }
    }

    #[derive(Clone, Default)]
    struct NoopExecutor;

    impl TurnExecutor for NoopExecutor {
        fn execute(
            &self,
            _request: TurnExecution,
            sink: &mut dyn BackendEventSink,
        ) -> Result<ExecutionOutcome, String> {
            sink.emit(BackendEvent::Session {
                id: "018f0000-0000-7000-8000-000000000099".into(),
                model: None,
            })?;
            sink.emit(BackendEvent::AssistantMessage {
                text: "готово".into(),
            })?;
            Ok(ExecutionOutcome {
                exit_code: 0,
                backend_session_id: Some("018f0000-0000-7000-8000-000000000099".into()),
                turn_completed: true,
                ..ExecutionOutcome::default()
            })
        }

        fn cancel(&self, _run_id: &str, _vm_name: Option<&str>) -> Result<bool, String> {
            Ok(true)
        }
    }

    #[test]
    fn public_error_redacts_sensitive_categories_and_bounds_text() {
        assert_eq!(
            public_error("request failed: Authorization Bearer synthetic"),
            "Agent VM operation failed; sensitive details withheld"
        );
        let long = "ordinary failure ".repeat(100);
        assert!(public_error(&long).len() <= MAX_PUBLIC_ERROR_CHARS);
    }

    #[test]
    fn request_ids_are_strict_entity_object_ids() {
        assert!(valid_request_id("agent-vm-42"));
        assert!(!valid_request_id(""));
        assert!(!valid_request_id("../escape"));
        assert!(!valid_request_id(&"a".repeat(129)));
    }

    #[test]
    fn plugin_environment_accepts_only_versioned_agent_vm_identity_contract() {
        let values = BTreeMap::from([
            (
                "JARVIS_SOCKET".into(),
                "/tmp/synthetic-profile/run.sock".into(),
            ),
            ("JARVIS_PLUGIN_ID".into(), "agent-vm".into()),
            ("JARVIS_PLUGIN_TOKEN".into(), "a".repeat(64)),
            ("JARVIS_PLUGIN_PROTOCOL".into(), "1".into()),
        ]);

        let env = PluginEnvironment::from_values(&values).unwrap();

        assert_eq!(env.socket, Path::new("/tmp/synthetic-profile/run.sock"));
        assert_eq!(env.protocol_version, 1);
        assert_eq!(env.plugin_id, "agent-vm");

        let mut wrong = values;
        wrong.insert("JARVIS_PLUGIN_ID".into(), "other-plugin".into());
        assert!(PluginEnvironment::from_values(&wrong).is_err());
    }

    #[test]
    fn unchanged_inventory_does_not_republish_vm_entity() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-inventory-stable-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let service = FakeService::new(Ok(snapshot(&root)));
        let host = FakeHost::default();
        let mut dispatcher = Dispatcher::new(service, host.clone());
        host.push_poll(PollResponse {
            ok: true,
            events: Vec::new(),
            next_seq: 0,
        });
        host.push_poll(PollResponse {
            ok: true,
            events: Vec::new(),
            next_seq: 0,
        });

        dispatcher.poll_and_reconcile(0).unwrap();
        wait_for_inventory_idle(&dispatcher);
        dispatcher.poll_and_reconcile(0).unwrap();
        wait_for_inventory_idle(&dispatcher);

        let publications = host.publications.lock().unwrap();
        assert_eq!(
            publications.iter().filter(|item| item.kind == "vm").count(),
            1
        );
        drop(publications);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disappeared_inventory_vm_is_removed_once_without_churn() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-inventory-remove-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let service = FakeService::new(Ok(snapshot(&root)));
        let host = FakeHost::default();
        let mut dispatcher = Dispatcher::new(service.clone(), host.clone());
        for next_seq in 1..=3 {
            host.push_poll(PollResponse {
                ok: true,
                events: Vec::new(),
                next_seq,
            });
        }

        dispatcher.poll_and_reconcile(0).unwrap();
        wait_for_inventory_idle(&dispatcher);
        service.set_inventory_override(Vec::new());
        dispatcher.poll_and_reconcile(1).unwrap();
        wait_for_inventory_idle(&dispatcher);
        dispatcher.poll_and_reconcile(2).unwrap();
        wait_for_inventory_idle(&dispatcher);

        let publications = host.publications.lock().unwrap();
        let vm_publications = publications
            .iter()
            .filter(|item| item.kind == "vm")
            .collect::<Vec<_>>();
        assert_eq!(
            vm_publications
                .iter()
                .map(|item| item.op.as_str())
                .collect::<Vec<_>>(),
            ["upsert", "remove"]
        );
        assert_eq!(
            vm_publications
                .iter()
                .map(|item| item.object_id.as_str())
                .collect::<Vec<_>>(),
            [
                "synthetic-project-a1b2c3d4e5f6",
                "synthetic-project-a1b2c3d4e5f6"
            ]
        );
        drop(publications);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn poll_heartbeat_reconciles_external_running_to_stopped_transition() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-inventory-heartbeat-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let service = FakeService::new(Ok(snapshot(&root)));
        let host = FakeHost::default();
        let mut dispatcher = Dispatcher::new(service.clone(), host.clone());
        host.push_poll(PollResponse {
            ok: true,
            events: Vec::new(),
            next_seq: 0,
        });
        dispatcher.poll_and_reconcile(0).unwrap();
        wait_for_inventory_idle(&dispatcher);
        service
            .result
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .vm
            .as_mut()
            .unwrap()
            .state = "stopped".into();
        host.push_poll(PollResponse {
            ok: true,
            events: Vec::new(),
            next_seq: 0,
        });

        let next_seq = dispatcher.poll_and_reconcile(0).unwrap();
        wait_for_inventory_idle(&dispatcher);

        assert_eq!(next_seq, 0);
        let publications = host.publications.lock().unwrap();
        assert_eq!(
            publications
                .iter()
                .filter(|item| item.kind == "vm")
                .map(|item| item.state.as_str())
                .collect::<Vec<_>>(),
            ["running", "stopped"]
        );
        drop(publications);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn poll_command_processing_advances_next_sequence() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-poll-command-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let service = FakeService::new(Ok(snapshot(&root)));
        let host = FakeHost::default();
        host.push_poll(PollResponse {
            ok: true,
            events: vec![command("runtime.status", &root)],
            next_seq: 9,
        });
        let mut dispatcher = Dispatcher::new(service.clone(), host.clone());

        let next_seq = dispatcher.poll_and_reconcile(4).unwrap();

        assert_eq!(next_seq, 9);
        assert_eq!(*host.poll_after.lock().unwrap(), [4]);
        assert_eq!(*service.calls.lock().unwrap(), ["status"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ensure_poll_keeps_bootstrap_credentials_for_terminal_contract() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-poll-credentials-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut ready = snapshot(&root);
        ready.created_spec = true;
        ready.environment = Some(BootstrapStatus {
            fingerprint: "synthetic-ready".into(),
            files: 3,
            skipped: 0,
            credentials: BootstrapCredentialStatus {
                claude: "ready".into(),
                codex: "ready".into(),
            },
            proxy_configured: true,
            ..Default::default()
        });
        let service = FakeService::new(Ok(ready));
        let host = FakeHost::default();
        host.push_poll(PollResponse {
            ok: true,
            events: vec![command("runtime.ensure", &root)],
            next_seq: 8,
        });
        let mut dispatcher = Dispatcher::new(service.clone(), host.clone());

        assert_eq!(dispatcher.poll_and_reconcile(0).unwrap(), 8);
        assert_eq!(service.inventory_calls(), 0);
        host.push_poll(PollResponse {
            ok: true,
            events: Vec::new(),
            next_seq: 8,
        });
        assert_eq!(dispatcher.poll_and_reconcile(8).unwrap(), 8);
        wait_for_inventory_idle(&dispatcher);

        let publications = host.publications.lock().unwrap();
        let vm = publications
            .iter()
            .rev()
            .find(|item| item.kind == "vm")
            .unwrap();
        assert_eq!(vm.state, "running");
        assert_eq!(
            vm.attrs.pointer("/environment/credentials/claude"),
            Some(&json!("ready"))
        );
        assert_eq!(vm.attrs["createdSpec"], json!(true));
        assert_eq!(vm.attrs["guestWorkspace"], "/home/dev/synthetic");
        assert!(vm.attrs["modules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|module| module == "claude"));
        assert_eq!(service.inventory_calls(), 1);
        assert_eq!(
            publications.iter().filter(|item| item.kind == "vm").count(),
            1
        );
        drop(publications);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn running_status_snapshot_does_not_clear_ready_bootstrap_environment() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-status-credentials-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut ready = snapshot(&root);
        ready.created_spec = true;
        ready.environment = Some(BootstrapStatus {
            fingerprint: "synthetic-ready".into(),
            files: 3,
            skipped: 0,
            credentials: BootstrapCredentialStatus {
                claude: "ready".into(),
                codex: "ready".into(),
            },
            proxy_configured: true,
            ..Default::default()
        });
        let service = FakeService::new(Ok(ready));
        let host = FakeHost::default();
        let mut dispatcher = Dispatcher::new(service.clone(), host.clone());
        dispatcher
            .process(command("runtime.ensure", &root))
            .unwrap();
        {
            let mut current = service.result.lock().unwrap();
            let snapshot = current.as_mut().unwrap();
            snapshot.created_spec = false;
            snapshot.environment = None;
        }

        dispatcher
            .process(command("runtime.status", &root))
            .unwrap();

        let publications = host.publications.lock().unwrap();
        let vm = publications
            .iter()
            .rev()
            .find(|item| item.kind == "vm")
            .unwrap();
        assert_eq!(
            vm.attrs.pointer("/environment/credentials/claude"),
            Some(&json!("ready"))
        );
        assert_eq!(vm.attrs["createdSpec"], json!(true));
        assert_eq!(
            publications.iter().filter(|item| item.kind == "vm").count(),
            1
        );
        drop(publications);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn newer_bootstrap_snapshot_replaces_previous_environment_status() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-bootstrap-refresh-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut initial = snapshot(&root);
        initial.created_spec = true;
        initial.environment = Some(BootstrapStatus {
            fingerprint: "synthetic-old".into(),
            files: 1,
            skipped: 0,
            credentials: BootstrapCredentialStatus {
                claude: "missing".into(),
                codex: "ready".into(),
            },
            proxy_configured: false,
            ..Default::default()
        });
        let service = FakeService::new(Ok(initial));
        let host = FakeHost::default();
        let mut dispatcher = Dispatcher::new(service.clone(), host.clone());
        dispatcher
            .process(command("runtime.ensure", &root))
            .unwrap();
        {
            let mut current = service.result.lock().unwrap();
            let snapshot = current.as_mut().unwrap();
            snapshot.created_spec = false;
            snapshot.environment = Some(BootstrapStatus {
                fingerprint: "synthetic-new".into(),
                files: 4,
                skipped: 0,
                credentials: BootstrapCredentialStatus {
                    claude: "ready".into(),
                    codex: "ready".into(),
                },
                proxy_configured: true,
                ..Default::default()
            });
        }

        dispatcher
            .process(command("runtime.ensure", &root))
            .unwrap();

        let publications = host.publications.lock().unwrap();
        let vm = publications
            .iter()
            .rev()
            .find(|item| item.kind == "vm")
            .unwrap();
        assert_eq!(
            vm.attrs.pointer("/environment/credentials/claude"),
            Some(&json!("ready"))
        );
        assert_eq!(
            vm.attrs.pointer("/environment/fingerprint"),
            Some(&json!("synthetic-new"))
        );
        assert_eq!(vm.attrs["createdSpec"], json!(false));
        assert_eq!(
            publications.iter().filter(|item| item.kind == "vm").count(),
            2
        );
        drop(publications);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stopped_or_rebound_vm_does_not_keep_previous_bootstrap_environment() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-bootstrap-clear-{}",
            uuid::Uuid::new_v4()
        ));
        let replacement = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-bootstrap-rebound-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&replacement).unwrap();
        let mut ready = snapshot(&root);
        ready.created_spec = true;
        ready.environment = Some(BootstrapStatus {
            fingerprint: "synthetic-ready".into(),
            files: 3,
            skipped: 0,
            credentials: BootstrapCredentialStatus {
                claude: "ready".into(),
                codex: "ready".into(),
            },
            proxy_configured: true,
            ..Default::default()
        });
        let service = FakeService::new(Ok(ready));
        let host = FakeHost::default();
        let mut dispatcher = Dispatcher::new(service.clone(), host.clone());
        dispatcher
            .process(command("runtime.ensure", &root))
            .unwrap();
        {
            let mut current = service.result.lock().unwrap();
            let snapshot = current.as_mut().unwrap();
            snapshot.vm.as_mut().unwrap().state = "stopped".into();
            snapshot.created_spec = false;
            snapshot.environment = None;
        }
        dispatcher
            .process(command("runtime.status", &root))
            .unwrap();
        {
            let mut current = service.result.lock().unwrap();
            *current = Ok(snapshot(&replacement));
        }
        dispatcher
            .process(command("runtime.status", &replacement))
            .unwrap();

        let publications = host.publications.lock().unwrap();
        let vm_publications = publications
            .iter()
            .filter(|item| item.kind == "vm")
            .collect::<Vec<_>>();
        assert_eq!(
            vm_publications
                .iter()
                .map(|item| item.state.as_str())
                .collect::<Vec<_>>(),
            ["running", "stopped", "running"]
        );
        for vm in &vm_publications[1..] {
            assert_eq!(vm.attrs["environment"], Value::Null);
            assert_eq!(vm.attrs["createdSpec"], json!(false));
        }
        assert_ne!(
            vm_publications[0].attrs["projectId"],
            vm_publications[2].attrs["projectId"]
        );
        drop(publications);
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(replacement).unwrap();
    }

    #[test]
    fn transient_inventory_error_does_not_fail_poll_or_remove_last_vm() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-poll-inventory-error-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let service = FakeService::new(Ok(snapshot(&root)));
        service.set_inventory_error("synthetic limactl timeout");
        let host = FakeHost::default();
        host.push_poll(PollResponse {
            ok: true,
            events: Vec::new(),
            next_seq: 3,
        });
        let mut dispatcher = Dispatcher::new(service.clone(), host.clone());
        dispatcher
            .process(command("runtime.status", &root))
            .unwrap();

        let result = dispatcher.poll_and_reconcile(1);
        service.wait_for_inventory_calls(1);
        wait_for_inventory_idle(&dispatcher);
        service.clear_inventory_error();
        host.push_poll(PollResponse {
            ok: true,
            events: Vec::new(),
            next_seq: 4,
        });
        let retry = dispatcher.poll_and_reconcile(3);
        service.wait_for_inventory_calls(2);
        wait_for_inventory_idle(&dispatcher);

        assert_eq!(result.unwrap(), 3);
        assert_eq!(retry.unwrap(), 4);
        assert_eq!(service.inventory_calls(), 2);
        let publications = host.publications.lock().unwrap();
        assert_eq!(
            publications
                .iter()
                .filter(|item| item.kind == "vm")
                .map(|item| item.state.as_str())
                .collect::<Vec<_>>(),
            ["running"]
        );
        drop(publications);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn blocked_reconcile_does_not_block_next_command_or_spawn_another_job() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-poll-blocked-inventory-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let service = FakeService::new(Ok(snapshot(&root)));
        service.set_inventory_override(Vec::new());
        service.block_inventory();
        let host = FakeHost::default();
        host.push_poll(PollResponse {
            ok: true,
            events: Vec::new(),
            next_seq: 2,
        });
        host.push_poll(PollResponse {
            ok: true,
            events: vec![command("runtime.status", &root)],
            next_seq: 5,
        });
        host.push_poll(PollResponse {
            ok: true,
            events: Vec::new(),
            next_seq: 5,
        });
        let dispatcher = Dispatcher::new(service.clone(), host.clone());
        let (sent, received) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut dispatcher = dispatcher;
            let result = dispatcher.poll_and_reconcile(0);
            sent.send((dispatcher, result)).unwrap();
        });

        let first = received.recv_timeout(Duration::from_millis(250));
        let (mut dispatcher, first_result) = match first {
            Ok(value) => value,
            Err(error) => {
                service.release_inventory();
                worker.join().unwrap();
                panic!("heartbeat blocked on inventory: {error}");
            }
        };
        worker.join().unwrap();
        service.wait_for_inventory_calls(1);
        let command_next = dispatcher.poll_and_reconcile(2);
        let heartbeat_next = dispatcher.poll_and_reconcile(5);
        let inventory_calls = service.inventory_calls();
        let runtime_calls = service.calls.lock().unwrap().clone();
        let poll_after = host.poll_after.lock().unwrap().clone();
        service.release_inventory();
        wait_for_inventory_idle(&dispatcher);

        assert_eq!(first_result.unwrap(), 2);
        assert_eq!(command_next.unwrap(), 5);
        assert_eq!(heartbeat_next.unwrap(), 5);
        assert_eq!(inventory_calls, 1);
        assert_eq!(runtime_calls, ["status"]);
        assert_eq!(poll_after, [0, 2, 5]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn identical_authoritative_snapshot_fences_older_inventory_sample() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-authoritative-fence-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let running = snapshot(&root);
        let mut stopped = running.vm.clone().unwrap();
        stopped.state = "stopped".into();
        let service = FakeService::new(Ok(running));
        service.set_inventory_override(vec![stopped]);
        service.block_inventory();
        let host = FakeHost::default();
        let mut dispatcher = Dispatcher::new(service.clone(), host.clone());
        dispatcher
            .process(command("runtime.status", &root))
            .unwrap();
        host.push_poll(PollResponse {
            ok: true,
            events: Vec::new(),
            next_seq: 1,
        });
        dispatcher.poll_and_reconcile(0).unwrap();
        service.wait_for_inventory_calls(1);

        dispatcher
            .process(command("runtime.status", &root))
            .unwrap();
        service.release_inventory();
        wait_for_inventory_idle(&dispatcher);

        let publications = host.publications.lock().unwrap();
        assert_eq!(
            publications
                .iter()
                .filter(|item| item.kind == "vm")
                .map(|item| item.state.as_str())
                .collect::<Vec<_>>(),
            ["running"]
        );
        drop(publications);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn slow_inventory_publish_does_not_block_newer_vm_command_and_repairs_final_state() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-slow-publish-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let running = snapshot(&root);
        let mut stopped = running.vm.clone().unwrap();
        stopped.state = "stopped".into();
        let service = FakeService::new(Ok(running));
        service.set_inventory_override(vec![stopped]);
        let host = FakeHost::default();
        let mut dispatcher = Dispatcher::new(service.clone(), host.clone());
        dispatcher
            .process(command("runtime.status", &root))
            .unwrap();
        host.block_vm_publication("stopped");
        host.push_poll(PollResponse {
            ok: true,
            events: Vec::new(),
            next_seq: 1,
        });
        dispatcher.poll_and_reconcile(0).unwrap();
        host.wait_for_blocked_publication();
        let (sent, received) = std::sync::mpsc::channel();
        let command_root = root.clone();
        let worker = std::thread::spawn(move || {
            let result = dispatcher.process(command("runtime.status", &command_root));
            sent.send((dispatcher, result)).unwrap();
        });

        let response = received.recv_timeout(Duration::from_millis(250));
        let (dispatcher, command_result) = match response {
            Ok(value) => value,
            Err(error) => {
                host.release_vm_publication();
                worker.join().unwrap();
                panic!("VM command blocked behind inventory publication: {error}");
            }
        };
        worker.join().unwrap();
        host.release_vm_publication();
        wait_for_inventory_idle(&dispatcher);

        command_result.unwrap();
        let publications = host.publications.lock().unwrap();
        let states = publications
            .iter()
            .filter(|item| item.kind == "vm")
            .map(|item| item.state.as_str())
            .collect::<Vec<_>>();
        assert_eq!(states.first(), Some(&"running"));
        assert_eq!(states.last(), Some(&"running"));
        drop(publications);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_reconcile_removes_stale_host_vm_absent_from_inventory() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-restart-remove-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let service = FakeService::new(Ok(snapshot(&root)));
        service.set_inventory_override(Vec::new());
        let host = FakeHost::default();
        host.seed_persisted_vm("legacy_VM.v1");
        host.push_poll(PollResponse {
            ok: true,
            events: Vec::new(),
            next_seq: 1,
        });
        let mut dispatcher = Dispatcher::new(service, host.clone());

        dispatcher.poll_and_reconcile(0).unwrap();
        wait_for_inventory_idle(&dispatcher);

        let publications = host.publications.lock().unwrap();
        let stale = publications
            .iter()
            .filter(|item| item.object_id == "legacy_VM.v1")
            .collect::<Vec<_>>();
        assert_eq!(
            stale
                .iter()
                .map(|item| item.op.as_str())
                .collect::<Vec<_>>(),
            ["upsert", "remove"]
        );
        drop(publications);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn supervisor_vm_publication_updates_cache_before_external_stop() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-supervisor-shared-cache-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let running = snapshot(&root);
        let mut stopped_vm = running.vm.clone().unwrap();
        stopped_vm.state = "stopped".into();
        let service = FakeService::new(Ok(running));
        service.set_inventory_override(vec![stopped_vm]);
        let host = FakeHost::default();
        host.push_poll(PollResponse {
            ok: true,
            events: Vec::new(),
            next_seq: 0,
        });
        host.push_poll(PollResponse {
            ok: true,
            events: Vec::new(),
            next_seq: 0,
        });
        let store = RunStore::new(root.join("private/runs"));
        let supervisor = RunSupervisor::new(host.clone(), store, Arc::new(NoopExecutor));
        let mut dispatcher = Dispatcher::with_supervisor(service.clone(), host.clone(), supervisor);

        dispatcher.poll_and_reconcile(0).unwrap();
        service.wait_for_inventory_calls(1);
        wait_for_inventory_idle(&dispatcher);
        host.wait_for_publication("vm", "stopped", 1);
        let identity = ProjectIdentity::from_path(&root).unwrap();
        let mut send = command("runtime.send", &root);
        send.payload.args = json!({
            "cwd":root,
            "projectId":identity.project_id,
            "agent":"claude",
            "message":"сделай"
        });
        dispatcher.process(send).unwrap();
        host.wait_for_publication("vm", "running", 1);
        host.wait_for_publication("agent_run", "completed", 1);

        dispatcher.poll_and_reconcile(0).unwrap();
        service.wait_for_inventory_calls(2);
        wait_for_inventory_idle(&dispatcher);
        host.wait_for_publication("vm", "stopped", 2);

        let publications = host.publications.lock().unwrap();
        assert_eq!(
            publications
                .iter()
                .filter(|item| item.kind == "vm")
                .map(|item| item.state.as_str())
                .collect::<Vec<_>>(),
            ["stopped", "running", "stopped"]
        );
        drop(publications);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dispatcher_publishes_started_vm_snapshot_and_done_for_ensure() {
        let root =
            std::env::temp_dir().join(format!("jarvis-agent-vm-dispatch-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let service = FakeService::new(Ok(snapshot(&root)));
        let host = FakeHost::default();
        let mut dispatcher = Dispatcher::new(service.clone(), host.clone());

        dispatcher
            .process(command("runtime.ensure", &root))
            .unwrap();

        assert_eq!(*service.calls.lock().unwrap(), ["ensure"]);
        let publications = host.publications.lock().unwrap();
        assert_eq!(
            publications
                .iter()
                .map(|item| (item.kind.as_str(), item.state.as_str()))
                .collect::<Vec<_>>(),
            [
                ("operation", "started"),
                ("vm", "running"),
                ("operation", "done"),
            ]
        );
        assert_eq!(publications[1].object_id, "synthetic-project-a1b2c3d4e5f6");
        assert_eq!(
            publications[2].attrs["shellCommand"],
            "avm shell synthetic-project-a1b2c3d4e5f6"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dispatcher_reports_runtime_failure_without_sensitive_text() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-dispatch-error-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let service = FakeService::new(Err("proxy Authorization credential synthetic".into()));
        let host = FakeHost::default();
        let mut dispatcher = Dispatcher::new(service, host.clone());
        let mut event = command("runtime.restart", &root);
        event.payload.args = json!({"cwd": root});

        dispatcher.process(event).unwrap();

        let publications = host.publications.lock().unwrap();
        assert_eq!(publications.last().unwrap().state, "error");
        assert_eq!(
            publications.last().unwrap().attrs["error"],
            "Agent VM operation failed; sensitive details withheld"
        );
        assert_eq!(
            publications.last().unwrap().attrs["project"],
            root.file_name().unwrap().to_string_lossy().as_ref()
        );
        assert_eq!(
            publications.last().unwrap().attrs["cwd"],
            root.canonicalize().unwrap().to_string_lossy().as_ref()
        );
        assert!(publications.last().unwrap().attrs["projectId"]
            .as_str()
            .is_some_and(|value| value.starts_with("project-")));
        assert!(
            !publications
                .last()
                .unwrap()
                .attrs
                .to_string()
                .contains("proxy"),
            "operation context не копирует чувствительные args/error"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dispatcher_publishes_reconciled_vm_before_lifecycle_error() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-dispatch-residual-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut residual = snapshot(&root);
        residual.vm.as_mut().unwrap().state = "stopped".into();
        residual.environment = None;
        let calls = Arc::new(Mutex::new(Vec::new()));
        let service = FailedLifecycleService {
            residual,
            calls: calls.clone(),
        };
        let host = FakeHost::default();
        let mut dispatcher = Dispatcher::new(service, host.clone());

        dispatcher
            .process(command("runtime.ensure", &root))
            .unwrap();

        assert_eq!(*calls.lock().unwrap(), ["ensure", "status"]);
        let publications = host.publications.lock().unwrap();
        assert_eq!(
            publications
                .iter()
                .map(|item| (item.kind.as_str(), item.state.as_str()))
                .collect::<Vec<_>>(),
            [
                ("operation", "started"),
                ("vm", "stopped"),
                ("operation", "error"),
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dispatcher_routes_headless_send_and_replay_without_requiring_cwd_for_replay() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-dispatch-run-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let service = FakeService::new(Ok(snapshot(&root)));
        let host = FakeHost::default();
        let store = RunStore::new(root.join("private/runs"));
        let supervisor = RunSupervisor::new(host.clone(), store.clone(), Arc::new(NoopExecutor));
        let mut dispatcher = Dispatcher::with_supervisor(service, host.clone(), supervisor);
        let identity = ProjectIdentity::from_path(&root).unwrap();
        let mut send = command("runtime.send", &root);
        send.payload.args = json!({
            "cwd":root,
            "projectId":identity.project_id,
            "agent":"claude",
            "message":"сделай"
        });

        dispatcher.process(send).unwrap();

        let run_id = {
            let publications = host.publications.lock().unwrap();
            let operation = publications
                .iter()
                .rev()
                .find(|item| item.kind == "operation" && item.state == "done")
                .unwrap();
            assert_eq!(operation.attrs["queued"], json!(false));
            operation.attrs["runId"].as_str().unwrap().to_string()
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while store.replay(&run_id, 0, 64).unwrap().is_empty() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        let mut replay = command("runtime.replay", &root);
        replay.payload.args = json!({"runId":run_id,"afterSeq":0,"limit":64});

        dispatcher.process(replay).unwrap();

        let publications = host.publications.lock().unwrap();
        let operation = publications
            .iter()
            .rev()
            .find(|item| {
                item.kind == "operation"
                    && item.state == "done"
                    && item.attrs["command"] == "runtime.replay"
            })
            .unwrap();
        let events: Vec<RunEvent> =
            serde_json::from_value(operation.attrs["events"].clone()).unwrap();
        assert!(!events.is_empty());
        assert!(events.iter().all(|event| event.backend == Backend::Claude));
        drop(publications);
        while store
            .summary(&run_id)
            .unwrap()
            .map(|summary| summary.state != "completed")
            .unwrap_or(true)
        {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }

        let mut commands = command("runtime.commands", &root);
        commands.payload.args = json!({"runId":run_id});
        dispatcher.process(commands).unwrap();

        let publications = host.publications.lock().unwrap();
        let operation = publications
            .iter()
            .rev()
            .find(|item| {
                item.kind == "operation"
                    && item.state == "done"
                    && item.attrs["command"] == "runtime.commands"
            })
            .unwrap();
        assert_eq!(
            operation.attrs["shellCommand"],
            "avm shell synthetic-project-a1b2c3d4e5f6"
        );
        assert_eq!(
            operation.attrs["resumeCommand"],
            "claude --resume 018f0000-0000-7000-8000-000000000099"
        );
        drop(publications);

        // Список прогонов проекта — источник экрана «чаты проекта».
        let mut runs = command("runtime.runs", &root);
        runs.payload.args = json!({"cwd":root,"projectId":identity.project_id});
        dispatcher.process(runs).unwrap();

        let publications = host.publications.lock().unwrap();
        let operation = publications
            .iter()
            .rev()
            .find(|item| {
                item.kind == "operation"
                    && item.state == "done"
                    && item.attrs["command"] == "runtime.runs"
            })
            .unwrap();
        let listed = operation.attrs["runs"].as_array().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["runId"], json!(run_id));
        assert_eq!(listed[0]["projectId"], json!(identity.project_id));
        assert_eq!(listed[0]["backend"], json!("claude"));
        assert_eq!(listed[0]["state"], json!("completed"));
        // transport-идентичность прогона наружу не уходит (спека v2 §13.3)
        assert!(listed[0].get("backendSessionId").is_none());
        assert!(listed[0].get("resumeCommand").is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn releasing_image_cache_frees_space_and_reports_new_usage() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-release-cache-{}",
            uuid::Uuid::new_v4()
        ));
        let profile = root.join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let paths =
            crate::runtime_paths::RuntimePaths::from_socket(&profile.join("run.sock")).unwrap();
        paths.create_private_dirs().unwrap();
        // Кэш образа и образ существующей VM: чистка должна тронуть только кэш.
        let cache = paths.image_cache().join("by-url-sha256/abc");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("data"), vec![1u8; 4096]).unwrap();
        let vm_dir = paths.lima_home.join("proj-1");
        std::fs::create_dir_all(&vm_dir).unwrap();
        std::fs::write(vm_dir.join("disk.img"), vec![7u8; 2048]).unwrap();

        let service = FakeService::new(Ok(snapshot(&root))).with_paths(paths);
        let host = FakeHost::default();
        let mut dispatcher = Dispatcher::new(service, host.clone());
        dispatcher
            .process(command("runtime.releaseCache", &root))
            .unwrap();

        let publications = host.publications.lock().unwrap();
        let operation = publications
            .iter()
            .rev()
            .find(|item| {
                item.kind == "operation"
                    && item.state == "done"
                    && item.attrs["command"] == "runtime.releaseCache"
            })
            .expect("освобождение кэша должно завершиться успешно");
        assert_eq!(operation.attrs["freedBytes"], json!(4096));
        assert_eq!(operation.attrs["disk"]["cacheBytes"], json!(0));
        assert_eq!(
            operation.attrs["disk"]["imagesBytes"],
            json!(2048),
            "образы существующих VM остаются на месте"
        );
        drop(publications);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn runs_listing_rejects_project_id_that_does_not_match_cwd() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-runs-guard-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let service = FakeService::new(Ok(snapshot(&root)));
        let host = FakeHost::default();
        let store = RunStore::new(root.join("private/runs"));
        let supervisor = RunSupervisor::new(host.clone(), store, Arc::new(NoopExecutor));
        let mut dispatcher = Dispatcher::with_supervisor(service, host.clone(), supervisor);

        // UI не может подсунуть чужой projectId: он сверяется с каноническим cwd.
        let mut runs = command("runtime.runs", &root);
        runs.payload.args = json!({"cwd":root,"projectId":"someone-else-0123456789ab"});
        dispatcher.process(runs).unwrap();

        let publications = host.publications.lock().unwrap();
        assert!(publications.iter().any(|item| item.kind == "operation"
            && item.state == "error"
            && item.attrs["command"] == "runtime.runs"));
        assert!(!publications.iter().any(|item| item.kind == "operation"
            && item.state == "done"
            && item.attrs["command"] == "runtime.runs"));
        drop(publications);
        std::fs::remove_dir_all(root).unwrap();
    }
}
