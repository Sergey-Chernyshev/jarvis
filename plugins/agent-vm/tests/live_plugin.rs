#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use jarvis_agent_vm_plugin::host::{HostApi, PollResponse};
use jarvis_agent_vm_plugin::run_event::Backend;
use jarvis_agent_vm_plugin::run_executor::SystemTurnExecutor;
use jarvis_agent_vm_plugin::run_store::RunStore;
use jarvis_agent_vm_plugin::run_supervisor::{RunSupervisor, SendRequest};
use jarvis_agent_vm_plugin::runner::{CommandRunner, CommandSpec, SystemRunner};
use jarvis_agent_vm_plugin::runtime_paths::RuntimePaths;
use jarvis_agent_vm_plugin::service::{AgentVmService, Toolchain};
use serde_json::{json, Value};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

fn temp_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "jarvis-agent-vm-live-plugin-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ))
}

#[derive(Clone, Debug, Default)]
struct LiveRunObservation {
    states: BTreeSet<String>,
    event_types: BTreeSet<String>,
    terminal: Option<String>,
    vm_running: bool,
    has_backend_session: bool,
    has_resume_command: bool,
}

#[derive(Clone, Default)]
struct CapturingHost {
    shared: Arc<(Mutex<LiveRunObservation>, Condvar)>,
}

impl CapturingHost {
    fn wait_for_terminal(&self, timeout: Duration) -> Result<LiveRunObservation, String> {
        let deadline = Instant::now() + timeout;
        let (lock, changed) = &*self.shared;
        let mut observation = lock.lock().unwrap();
        while observation.terminal.is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("live Agent VM run did not reach a terminal state".into());
            }
            let (next, timeout) = changed.wait_timeout(observation, remaining).unwrap();
            observation = next;
            if timeout.timed_out() && observation.terminal.is_none() {
                return Err("live Agent VM run timed out".into());
            }
        }
        Ok(observation.clone())
    }
}

impl HostApi for CapturingHost {
    fn register(&self, _pid: u32) -> Result<(), String> {
        Ok(())
    }

    fn poll(&self, after: u64) -> Result<PollResponse, String> {
        Ok(PollResponse {
            ok: true,
            events: Vec::new(),
            next_seq: after,
        })
    }

    fn publish_entity(
        &self,
        _op: &str,
        kind: &str,
        _object_id: &str,
        state: &str,
        attrs: Value,
    ) -> Result<(), String> {
        let (lock, changed) = &*self.shared;
        let mut observation = lock.lock().unwrap();
        if kind == "vm" && state == "running" {
            observation.vm_running = true;
        }
        if kind == "agent_run" {
            observation.states.insert(state.to_string());
            if let Some(event_type) = attrs
                .get("latestEvent")
                .and_then(|event| event.get("type"))
                .and_then(Value::as_str)
            {
                observation.event_types.insert(event_type.to_string());
            }
            if matches!(state, "completed" | "failed" | "cancelled") {
                observation.terminal = Some(state.to_string());
                observation.has_backend_session = attrs
                    .get("backendSessionId")
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.is_empty());
                observation.has_resume_command = attrs
                    .get("resumeCommand")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.starts_with("claude --resume "));
            }
        }
        changed.notify_all();
        Ok(())
    }
}

fn read_request(stream: &UnixStream) -> (String, Value) {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut headers = String::new();
    let mut content_length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .and_then(|value| value.trim().parse::<usize>().ok())
        {
            content_length = value;
        }
        headers.push_str(&line);
    }
    let mut bytes = vec![0; content_length];
    reader.read_exact(&mut bytes).unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (headers, body)
}

fn respond(mut stream: UnixStream, body: Value) {
    let body = serde_json::to_vec(&body).unwrap();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
}

/// Local-only real-process smoke. It uses installed `avm`/`limactl`, but an
/// empty synthetic HOME/LIMA_HOME derived by the plugin from this temp profile.
#[test]
#[ignore = "requires locally installed avm and limactl"]
fn real_sidecar_handshakes_polls_and_publishes_inventory_operation() {
    let root = temp_root();
    fs::create_dir_all(&root).unwrap();
    let socket = root.join("run.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let token = "a".repeat(64);
    let (published_tx, published_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();

    let server = std::thread::spawn(move || {
        let mut publications = Vec::new();
        for step in 0..4 {
            let (stream, _) = listener.accept().unwrap();
            let (headers, body) = read_request(&stream);
            assert!(headers.contains("x-jarvis-token: "));
            match step {
                0 => {
                    assert!(headers.starts_with("POST /plugin/register HTTP/1.1"));
                    assert_eq!(body["protocolVersion"], 1);
                    respond(stream, json!({"ok": true}));
                }
                1 => {
                    assert!(headers.starts_with("GET /plugin/events?"));
                    respond(
                        stream,
                        json!({
                            "ok": true,
                            "events": [{
                                "seq": 1,
                                "kind": "command",
                                "payload": {
                                    "requestId": "agent-vm-1",
                                    "name": "runtime.inventory",
                                    "args": {}
                                }
                            }],
                            "nextSeq": 1
                        }),
                    );
                }
                _ => {
                    assert!(headers.starts_with("POST /capability HTTP/1.1"));
                    publications.push(body);
                    respond(stream, json!({"ok": true, "value": {}}));
                }
            }
        }
        published_tx.send(publications).unwrap();
        let _ = stop_rx.recv();
    });

    let binary = env!("CARGO_BIN_EXE_jarvis-agent-vm-plugin");
    let mut child = Command::new(binary)
        .env_clear()
        .env("JARVIS_SOCKET", &socket)
        .env("JARVIS_PLUGIN_ID", "agent-vm")
        .env("JARVIS_PLUGIN_TOKEN", token)
        .env("JARVIS_PLUGIN_PROTOCOL", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let publications = published_rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap();
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    let _ = stop_tx.send(());
    server.join().unwrap();

    assert_eq!(publications.len(), 2);
    assert_eq!(publications[0]["id"], "entities.publish");
    assert_eq!(publications[0]["args"]["kind"], "operation");
    assert_eq!(publications[0]["args"]["state"], "started");
    assert_eq!(publications[1]["args"]["state"], "done");
    assert!(
        String::from_utf8_lossy(&output.stderr).is_empty(),
        "healthy sidecar stays silent"
    );
    fs::remove_dir_all(root).unwrap();
}

/// Local-only credential smoke against an already managed project VM. The
/// assertions inspect only status, type, mode and forbidden key names; secret
/// values never leave the private bootstrap pipe or guest credential file.
#[test]
#[ignore = "requires JARVIS_AGENT_VM_LIVE_SOCKET, JARVIS_AGENT_VM_LIVE_CWD and a running managed VM"]
fn real_existing_vm_bootstraps_standard_claude_login_privately() {
    let socket = std::env::var_os("JARVIS_AGENT_VM_LIVE_SOCKET")
        .map(PathBuf::from)
        .expect("JARVIS_AGENT_VM_LIVE_SOCKET is required");
    let cwd = std::env::var_os("JARVIS_AGENT_VM_LIVE_CWD")
        .map(PathBuf::from)
        .expect("JARVIS_AGENT_VM_LIVE_CWD is required");
    let paths = RuntimePaths::from_socket(&socket).unwrap();
    let tools = Toolchain::discover().unwrap();
    let service =
        AgentVmService::with_system_bootstrap(SystemRunner, paths.clone(), tools.clone()).unwrap();

    let snapshot = service.ensure(&cwd).unwrap();
    let environment = snapshot.environment.as_ref().expect("bootstrap status");
    assert_eq!(environment.credentials.claude, "ready");
    let record = snapshot
        .vm
        .as_ref()
        .and_then(|vm| vm.record.as_ref())
        .expect("managed VM record");
    let guest_home = PathBuf::from(&record.workspace.guest_path)
        .parent()
        .expect("guest workspace parent")
        .to_path_buf();
    let result = SystemRunner
        .run(&CommandSpec {
            program: tools.limactl,
            args: vec![
                "shell".into(),
                "--tty=false".into(),
                "--workdir".into(),
                "/".into(),
                record.name.clone(),
                "--".into(),
                "/bin/bash".into(),
                "-ceu".into(),
                r#"
credential="$1/.claude/.credentials.json"
test -f "$credential"
test ! -L "$credential"
test "$(stat -c '%a' "$credential")" = "600"
! grep -q '"mcpOAuth"' "$credential"
"#
                .into(),
                "jarvis-live-smoke".into(),
                guest_home.to_string_lossy().into_owned(),
            ],
            cwd: None,
            env: paths.command_env(),
            stdin: None,
        })
        .unwrap()
        .success_or_error("private Claude credential validation")
        .unwrap();
    assert_eq!(result.status, 0);
}

/// Local-only full run smoke. It exercises the real setup, Claude JSONL
/// parser, run journal and entity projection while retaining only event types
/// and booleans in the test observer.
#[test]
#[ignore = "requires JARVIS_AGENT_VM_LIVE_SOCKET, JARVIS_AGENT_VM_LIVE_CWD and a running Claude VM"]
fn real_run_supervisor_streams_claude_to_terminal_entity() {
    let socket = std::env::var_os("JARVIS_AGENT_VM_LIVE_SOCKET")
        .map(PathBuf::from)
        .expect("JARVIS_AGENT_VM_LIVE_SOCKET is required");
    let cwd = std::env::var_os("JARVIS_AGENT_VM_LIVE_CWD")
        .map(PathBuf::from)
        .expect("JARVIS_AGENT_VM_LIVE_CWD is required");
    let paths = RuntimePaths::from_socket(&socket).unwrap();
    let tools = Toolchain::discover().unwrap();
    let service =
        AgentVmService::with_system_bootstrap(SystemRunner, paths.clone(), tools.clone()).unwrap();
    let journal_root = temp_root().join("runs");
    let store = RunStore::new(journal_root.clone());
    let host = CapturingHost::default();
    let supervisor = RunSupervisor::new(
        host.clone(),
        store.clone(),
        Arc::new(SystemTurnExecutor::new(tools.limactl, paths.command_env())),
    )
    .with_runtime_paths(paths);

    let receipt = supervisor
        .submit(
            service,
            SendRequest {
                cwd,
                project_id: None,
                backend: Backend::Claude,
                run_id: None,
                message: "Reply with exactly JARVIS_SUPERVISOR_OK. Do not inspect or modify files."
                    .into(),
            },
        )
        .unwrap();
    let observation = host.wait_for_terminal(Duration::from_secs(180)).unwrap();

    assert_eq!(observation.terminal.as_deref(), Some("completed"));
    assert!(observation.vm_running);
    assert!(observation.states.contains("starting"));
    assert!(observation.states.contains("working"));
    assert!(observation.event_types.contains("run.started"));
    assert!(observation.event_types.contains("assistant.message"));
    assert!(observation.event_types.contains("usage.updated"));
    assert!(observation.event_types.contains("result.completed"));
    assert!(observation.has_backend_session);
    assert!(observation.has_resume_command);
    let summary = store
        .summary(&receipt.run_id)
        .unwrap()
        .expect("run summary");
    assert_eq!(summary.state, "completed");
    assert_eq!(summary.backend, Backend::Claude);
    let journal = journal_root.join(format!("{}.jsonl", receipt.run_id));
    assert_eq!(
        fs::metadata(&journal).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::remove_dir_all(journal_root.parent().unwrap()).unwrap();
}
