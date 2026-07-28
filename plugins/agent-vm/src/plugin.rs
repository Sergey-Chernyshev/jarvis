use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use zeroize::Zeroize;

use crate::host::{HostApi, HostEvent};
use crate::inventory::InventoryVm;
use crate::project::ProjectIdentity;
use crate::run_event::Backend;
use crate::run_supervisor::{RunSupervisor, SendRequest};
use crate::service::{validate_project_id, RuntimeService, RuntimeSnapshot};

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
    published_vms: BTreeSet<String>,
    supervisor: Option<RunSupervisor<H>>,
}

impl<S: RuntimeService, H: HostApi> Dispatcher<S, H> {
    pub fn new(service: S, host: H) -> Self {
        Self {
            service,
            host,
            published_vms: BTreeSet::new(),
            supervisor: None,
        }
    }

    pub fn with_supervisor(service: S, host: H, supervisor: RunSupervisor<H>) -> Self {
        Self {
            service,
            host,
            published_vms: BTreeSet::new(),
            supervisor: Some(supervisor),
        }
    }

    pub fn refresh_inventory(&mut self) -> Result<(), String> {
        let inventory = self.service.inventory()?;
        let current = inventory
            .iter()
            .map(|vm| vm.name.clone())
            .collect::<BTreeSet<_>>();
        for vm in &inventory {
            self.publish_inventory_vm(vm)?;
        }
        for removed in self.published_vms.difference(&current) {
            self.host
                .publish_entity("remove", "vm", removed, "", json!({}))?;
        }
        self.published_vms = current;
        Ok(())
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
            return match self.refresh_inventory() {
                Ok(()) => self.publish_operation(
                    &request_id,
                    &name,
                    "done",
                    operation_attrs(&context, json!({})),
                ),
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
                        "environment": snapshot.environment
                    })),
                )
            }
            Err(error) => self.publish_operation(
                &request_id,
                &name,
                "error",
                operation_attrs(&context, json!({"error": public_error(&error)})),
            ),
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
            "runtime.send" | "runtime.cancel" | "runtime.replay" | "runtime.commands"
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
        let state = snapshot
            .vm
            .as_ref()
            .map(|vm| vm.state.as_str())
            .unwrap_or("absent");
        let (management, guest_workspace, modules, resources) = snapshot
            .vm
            .as_ref()
            .and_then(|vm| vm.record.as_ref().map(|record| (vm, record)))
            .map(|(vm, record)| {
                (
                    vm.management.as_str(),
                    record.workspace.guest_path.as_str(),
                    record.modules.clone(),
                    serde_json::to_value(&record.resources).unwrap_or(Value::Null),
                )
            })
            .unwrap_or(("missing", "", Vec::new(), Value::Null));
        self.host.publish_entity(
            "upsert",
            "vm",
            &snapshot.vm_name,
            state,
            json!({
                "projectId": snapshot.project_id,
                "project": snapshot.display_name,
                "cwd": snapshot.cwd,
                "management": management,
                "guestWorkspace": guest_workspace,
                "modules": modules,
                "resources": resources,
                "shellCommand": snapshot.shell_command,
                "createdSpec": snapshot.created_spec,
                "environment": snapshot.environment
            }),
        )?;
        self.published_vms.insert(snapshot.vm_name.clone());
        Ok(())
    }

    fn publish_inventory_vm(&self, vm: &InventoryVm) -> Result<(), String> {
        let attrs = match &vm.record {
            Some(record) => {
                let project = record
                    .workspace
                    .host_path
                    .as_deref()
                    .and_then(|path| ProjectIdentity::from_path(Path::new(path)).ok());
                json!({
                    "management": vm.management,
                    "projectId": project.as_ref().map(|item| item.project_id.as_str()),
                    "project": project.as_ref().map(|item| item.display_name.as_str()),
                    "cwd": project.as_ref().map(|item| item.canonical_path.to_string_lossy().into_owned()),
                    "guestWorkspace": record.workspace.guest_path,
                    "modules": record.modules,
                    "resources": record.resources,
                    "shellCommand": format!("avm shell {}", vm.name)
                })
            }
            None => json!({
                "management": vm.management,
                "shellCommand": format!("limactl shell {}", vm.name)
            }),
        };
        self.host
            .publish_entity("upsert", "vm", &vm.name, &vm.state, attrs)
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
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use serde_json::{json, Value};

    use super::*;
    use crate::host::{HostApi, PollResponse};
    use crate::inventory::{InventoryVm, VmRecord, VmResources, VmWorkspace};
    use crate::run_event::{Backend, BackendEvent, RunEvent};
    use crate::run_executor::{BackendEventSink, ExecutionOutcome, TurnExecution, TurnExecutor};
    use crate::run_store::RunStore;
    use crate::run_supervisor::RunSupervisor;
    use crate::service::{RuntimeService, RuntimeSnapshot};

    struct Publication {
        kind: String,
        object_id: String,
        state: String,
        attrs: Value,
    }

    #[derive(Clone, Default)]
    struct FakeHost {
        publications: Arc<Mutex<Vec<Publication>>>,
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
            op: &str,
            kind: &str,
            object_id: &str,
            state: &str,
            attrs: Value,
        ) -> Result<(), String> {
            assert!(matches!(op, "upsert" | "remove"));
            self.publications.lock().unwrap().push(Publication {
                kind: kind.into(),
                object_id: object_id.into(),
                state: state.into(),
                attrs,
            });
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FakeService {
        result: Arc<Mutex<Result<RuntimeSnapshot, String>>>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl RuntimeService for FakeService {
        fn inventory(&self) -> Result<Vec<InventoryVm>, String> {
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
                }),
            }),
            created_spec: false,
            shell_command: format!("avm shell {vm_name}"),
            environment: None,
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
    fn dispatcher_publishes_started_vm_snapshot_and_done_for_ensure() {
        let root =
            std::env::temp_dir().join(format!("jarvis-agent-vm-dispatch-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let service = FakeService {
            result: Arc::new(Mutex::new(Ok(snapshot(&root)))),
            calls: Arc::new(Mutex::new(Vec::new())),
        };
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
        let service = FakeService {
            result: Arc::new(Mutex::new(Err(
                "proxy Authorization credential synthetic".into()
            ))),
            calls: Arc::new(Mutex::new(Vec::new())),
        };
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
    fn dispatcher_routes_headless_send_and_replay_without_requiring_cwd_for_replay() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-dispatch-run-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let service = FakeService {
            result: Arc::new(Mutex::new(Ok(snapshot(&root)))),
            calls: Arc::new(Mutex::new(Vec::new())),
        };
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
        std::fs::remove_dir_all(root).unwrap();
    }
}
