use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use zeroize::Zeroize;

use crate::inventory::{is_safe_guest_workspace, VmRecord};
use crate::project::is_valid_vm_name;
use crate::run_event::{parse_backend_line, Backend, BackendEvent};
use crate::run_store::validate_run_id;
use crate::runner::{CommandRunner, CommandSpec, SystemRunner};

pub const MAX_BACKEND_LINE_BYTES: usize = 1024 * 1024;
pub const MAX_PROMPT_BYTES: usize = 48 * 1024;
pub const MAX_STDERR_TAIL_BYTES: usize = 64 * 1024;

const GUEST_RUN_SCRIPT: &str = r#"
run_id="$1"
shift
umask 077
run_dir="$HOME/.jarvis-vm/runs"
install -d -m 0700 "$run_dir"
env_file="$HOME/.jarvis-vm/agent.env"
if [ -f "$env_file" ]; then
  . "$env_file"
fi
export CI=1
export NO_COLOR=1
export TERM=dumb
export DISABLE_NON_ESSENTIAL_MODEL_CALLS=1
setsid "$@" <&0 &
agent_pid="$!"
pid_file="$run_dir/$run_id.pid"
printf '%s\n' "$agent_pid" > "$pid_file"
chmod 0600 "$pid_file"
cleanup() {
  rm -f -- "$pid_file"
}
terminate() {
  kill -TERM -- "-$agent_pid" 2>/dev/null || kill -TERM "$agent_pid" 2>/dev/null || true
}
trap terminate HUP INT TERM
trap cleanup EXIT
set +e
wait "$agent_pid"
status="$?"
set -e
exit "$status"
"#;

const GUEST_CANCEL_SCRIPT: &str = r#"
run_id="$1"
pid_file="$HOME/.jarvis-vm/runs/$run_id.pid"
if [ ! -f "$pid_file" ]; then
  exit 0
fi
IFS= read -r agent_pid < "$pid_file"
case "$agent_pid" in
  ''|*[!0-9]*) exit 64 ;;
esac
kill -TERM -- "-$agent_pid" 2>/dev/null || kill -TERM "$agent_pid" 2>/dev/null || true
"#;

pub struct TurnExecution {
    pub run_id: String,
    pub turn_id: String,
    pub backend: Backend,
    pub backend_session_id: Option<String>,
    pub new_claude_session_id: String,
    pub prompt: String,
    pub record: VmRecord,
}

impl fmt::Debug for TurnExecution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TurnExecution")
            .field("run_id", &self.run_id)
            .field("turn_id", &self.turn_id)
            .field("backend", &self.backend)
            .field(
                "backend_session_configured",
                &self.backend_session_id.is_some(),
            )
            .field("prompt_bytes", &self.prompt.len())
            .field("vm", &self.record.name)
            .finish()
    }
}

impl Drop for TurnExecution {
    fn drop(&mut self) {
        self.prompt.zeroize();
    }
}

pub struct StreamCommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub stdin: Option<Vec<u8>>,
}

impl fmt::Debug for StreamCommandSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamCommandSpec")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .field("stdin_bytes", &self.stdin.as_ref().map(Vec::len))
            .finish()
    }
}

impl Drop for StreamCommandSpec {
    fn drop(&mut self) {
        if let Some(stdin) = &mut self.stdin {
            stdin.zeroize();
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecutionOutcome {
    pub exit_code: i32,
    pub backend_session_id: Option<String>,
    pub result: Option<String>,
    pub backend_reported_error: bool,
    pub turn_completed: bool,
    pub stderr_bytes: usize,
}

pub trait BackendEventSink {
    fn emit(&mut self, event: BackendEvent) -> Result<(), String>;
}

impl<F> BackendEventSink for F
where
    F: FnMut(BackendEvent) -> Result<(), String>,
{
    fn emit(&mut self, event: BackendEvent) -> Result<(), String> {
        self(event)
    }
}

pub trait TurnExecutor: Send + Sync + 'static {
    fn execute(
        &self,
        request: TurnExecution,
        sink: &mut dyn BackendEventSink,
    ) -> Result<ExecutionOutcome, String>;

    fn cancel(&self, run_id: &str, vm_name: Option<&str>) -> Result<bool, String>;
}

#[derive(Clone)]
pub struct SystemTurnExecutor {
    limactl: PathBuf,
    env: BTreeMap<String, String>,
    processes: Arc<Mutex<HashMap<String, Arc<Mutex<Child>>>>>,
}

impl SystemTurnExecutor {
    pub fn new(limactl: PathBuf, env: BTreeMap<String, String>) -> Self {
        Self {
            limactl,
            env,
            processes: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl TurnExecutor for SystemTurnExecutor {
    fn execute(
        &self,
        request: TurnExecution,
        sink: &mut dyn BackendEventSink,
    ) -> Result<ExecutionOutcome, String> {
        let spec = build_turn_spec(&self.limactl, &self.env, &request)?;
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .env_clear()
            .envs(&spec.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|_| "не запустить headless Agent VM invocation".to_string())?;
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("headless Agent VM stdout недоступен".into());
        };
        let Some(stderr) = child.stderr.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("headless Agent VM stderr недоступен".into());
        };
        let Some(mut stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("headless Agent VM stdin недоступен".into());
        };
        let child = Arc::new(Mutex::new(child));
        self.processes
            .lock()
            .unwrap()
            .insert(request.run_id.clone(), child.clone());
        if let Some(input) = &spec.stdin {
            if stdin.write_all(input).is_err() {
                let _ = child.lock().unwrap().kill();
                let _ = child.lock().unwrap().wait();
                self.processes.lock().unwrap().remove(&request.run_id);
                return Err("не передать prompt в headless Agent VM stdin".into());
            }
        }
        drop(stdin);

        let stderr_thread = thread::spawn(move || read_stderr_tail(stderr));

        let mut outcome = ExecutionOutcome::default();
        let mut reader = BufReader::new(stdout);
        let mut stream_error = None;
        loop {
            let line = match read_backend_line(&mut reader) {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    stream_error = Some(error);
                    break;
                }
            };
            let parsed = std::str::from_utf8(&line)
                .map_err(|_| "backend JSONL line имеет non-UTF-8 bytes".to_string())
                .and_then(|text| {
                    serde_json::from_str::<serde_json::Value>(text)
                        .map_err(|_| "backend stdout содержит invalid JSONL".to_string())?;
                    Ok(parse_backend_line(request.backend, text))
                });
            let mut line = line;
            line.zeroize();
            let events = match parsed {
                Ok(events) => events,
                Err(error) => {
                    stream_error = Some(error);
                    break;
                }
            };
            for event in events {
                match &event {
                    BackendEvent::Session { id, .. } => {
                        if validate_backend_session_id(id).is_err() {
                            stream_error = Some("backend вернул unsafe session identity".into());
                            break;
                        }
                        outcome.backend_session_id = Some(id.clone());
                    }
                    BackendEvent::Result {
                        text,
                        is_error,
                        session_id,
                    } => {
                        outcome.result = Some(text.clone());
                        outcome.backend_reported_error |= *is_error;
                        if session_id.as_ref().is_some_and(|id| !id.is_empty()) {
                            if session_id
                                .as_deref()
                                .is_some_and(|id| validate_backend_session_id(id).is_err())
                            {
                                stream_error =
                                    Some("backend вернул unsafe result session identity".into());
                                break;
                            }
                            outcome.backend_session_id = session_id.clone();
                        }
                    }
                    BackendEvent::Failure { .. } => {
                        outcome.backend_reported_error = true;
                    }
                    BackendEvent::TurnCompleted => outcome.turn_completed = true,
                    _ => {}
                }
                if let Err(error) = sink.emit(event) {
                    stream_error = Some(error);
                    break;
                }
            }
            if stream_error.is_some() {
                break;
            }
        }
        if stream_error.is_some() {
            let _ = child.lock().unwrap().kill();
        }
        let status = child.lock().unwrap().wait();
        self.processes.lock().unwrap().remove(&request.run_id);
        let status = status.map_err(|_| "не дождаться headless Agent VM invocation".to_string())?;
        outcome.exit_code = status.code().unwrap_or(-1);
        let mut stderr_tail = stderr_thread.join().unwrap_or_default();
        outcome.stderr_bytes = stderr_tail.len();
        stderr_tail.zeroize();
        if let Some(error) = stream_error {
            return Err(error);
        }
        Ok(outcome)
    }

    fn cancel(&self, run_id: &str, vm_name: Option<&str>) -> Result<bool, String> {
        validate_run_id(run_id)?;
        let child = self.processes.lock().unwrap().get(run_id).cloned();
        let mut cancelled = false;
        if let Some(child) = child {
            child
                .lock()
                .unwrap()
                .kill()
                .map_err(|_| "не остановить local Agent VM transport".to_string())?;
            cancelled = true;
        }
        if let Some(vm_name) = vm_name {
            let spec = build_cancel_spec(&self.limactl, &self.env, vm_name, run_id)?;
            thread::Builder::new()
                .name(format!("agent-vm-cancel-{}", short_id(run_id)))
                .spawn(move || {
                    let _ = SystemRunner
                        .run(&spec)
                        .and_then(|result| result.success_or_error("Agent VM remote cancel"));
                })
                .map_err(|_| "не запустить Agent VM remote cancel worker".to_string())?;
            cancelled = true;
        }
        Ok(cancelled)
    }
}

fn short_id(value: &str) -> &str {
    value.get(..value.len().min(20)).unwrap_or(value)
}

pub fn build_turn_spec(
    limactl: &Path,
    env: &BTreeMap<String, String>,
    request: &TurnExecution,
) -> Result<StreamCommandSpec, String> {
    validate_execution(limactl, request)?;
    let mut backend_args = match request.backend {
        Backend::Claude => vec![
            "claude".into(),
            "-p".into(),
            "--verbose".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--input-format".into(),
            "text".into(),
            "--include-partial-messages".into(),
            "--include-hook-events".into(),
            "--permission-mode".into(),
            "bypassPermissions".into(),
            "--setting-sources".into(),
            "user,project,local".into(),
            "--no-chrome".into(),
        ],
        Backend::Codex => vec!["codex".into(), "exec".into()],
    };
    match (request.backend, request.backend_session_id.as_deref()) {
        (Backend::Claude, Some(session_id)) => {
            backend_args.extend(["--resume".into(), session_id.into()]);
        }
        (Backend::Claude, None) => {
            backend_args.extend(["--session-id".into(), request.new_claude_session_id.clone()]);
        }
        (Backend::Codex, Some(session_id)) => {
            backend_args.extend([
                "resume".into(),
                "--json".into(),
                "--color".into(),
                "never".into(),
                session_id.into(),
                "-".into(),
            ]);
        }
        (Backend::Codex, None) => {
            backend_args.extend([
                "--json".into(),
                "--color".into(),
                "never".into(),
                "--sandbox".into(),
                "workspace-write".into(),
                "-".into(),
            ]);
        }
    }
    let mut args = vec![
        "shell".into(),
        "--tty=false".into(),
        "--workdir".into(),
        request.record.workspace.guest_path.clone(),
        request.record.name.clone(),
        "--".into(),
        "/bin/bash".into(),
        "-ceu".into(),
        GUEST_RUN_SCRIPT.into(),
        "jarvis-run".into(),
        request.run_id.clone(),
    ];
    args.extend(backend_args);
    Ok(StreamCommandSpec {
        program: limactl.to_path_buf(),
        args,
        env: env.clone(),
        stdin: Some(request.prompt.as_bytes().to_vec()),
    })
}

pub fn build_cancel_spec(
    limactl: &Path,
    env: &BTreeMap<String, String>,
    vm_name: &str,
    run_id: &str,
) -> Result<CommandSpec, String> {
    if !limactl.is_absolute() {
        return Err("limactl path должен быть absolute".into());
    }
    if !is_valid_vm_name(vm_name) {
        return Err("cancel содержит unsafe VM name".into());
    }
    validate_run_id(run_id)?;
    Ok(CommandSpec {
        program: limactl.to_path_buf(),
        args: vec![
            "shell".into(),
            "--tty=false".into(),
            "--workdir".into(),
            "/".into(),
            vm_name.into(),
            "--".into(),
            "/bin/bash".into(),
            "-ceu".into(),
            GUEST_CANCEL_SCRIPT.into(),
            "jarvis-cancel".into(),
            run_id.into(),
        ],
        cwd: None,
        env: env.clone(),
        stdin: None,
    })
}

fn validate_execution(limactl: &Path, request: &TurnExecution) -> Result<(), String> {
    if !limactl.is_absolute() {
        return Err("limactl path должен быть absolute".into());
    }
    validate_run_id(&request.run_id)?;
    validate_run_id(&request.turn_id)?;
    if request.prompt.is_empty()
        || request.prompt.len() > MAX_PROMPT_BYTES
        || request.prompt.contains('\0')
    {
        return Err("prompt имеет недопустимый размер или bytes".into());
    }
    if !is_valid_vm_name(&request.record.name)
        || !valid_guest_user(&request.record.user)
        || request.record.workspace.mode_name != "mount"
    {
        return Err("run record содержит unsafe VM identity".into());
    }
    if !is_safe_guest_workspace(&request.record.user, &request.record.workspace.guest_path) {
        return Err("run record содержит unsafe guest workspace".into());
    }
    if !request
        .record
        .modules
        .iter()
        .any(|module| module == request.backend.as_str())
    {
        return Err("requested backend отсутствует в VM modules".into());
    }
    if let Some(session_id) = &request.backend_session_id {
        validate_backend_session_id(session_id)?;
    }
    if request.backend == Backend::Claude
        && request.backend_session_id.is_none()
        && uuid::Uuid::parse_str(&request.new_claude_session_id).is_err()
    {
        return Err("new Claude session id должен быть UUID".into());
    }
    Ok(())
}

pub fn validate_backend_session_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("backend session id имеет unsafe format".into());
    }
    Ok(())
}

fn valid_guest_user(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'-' | b'_'))
        })
}

pub fn read_backend_line<R: BufRead>(reader: &mut R) -> Result<Option<Vec<u8>>, String> {
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|_| "не прочитать backend JSONL".to_string())?;
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
        if line.len().saturating_add(take) > MAX_BACKEND_LINE_BYTES + 1 {
            line.zeroize();
            return Err("backend JSONL line превышает limit".into());
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if line.last() == Some(&b'\n') {
            line.pop();
            return Ok(Some(line));
        }
    }
}

fn read_stderr_tail<R: Read>(mut reader: R) -> Vec<u8> {
    let mut tail = VecDeque::with_capacity(MAX_STDERR_TAIL_BYTES);
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        for byte in &buffer[..read] {
            if tail.len() == MAX_STDERR_TAIL_BYTES {
                tail.pop_front();
            }
            tail.push_back(*byte);
        }
        buffer[..read].zeroize();
    }
    tail.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::inventory::{VmRecord, VmResources, VmWorkspace};
    use crate::run_event::Backend;

    fn record() -> VmRecord {
        VmRecord {
            name: "synthetic-project-a1b2c3d4e5f6".into(),
            source: "project".into(),
            modules: vec!["claude".into(), "codex".into()],
            resources: VmResources::default(),
            user: "dev".into(),
            workspace: VmWorkspace {
                mode_name: "mount".into(),
                guest_path: "/home/dev/synthetic-project".into(),
                host_path: Some("/host/synthetic-project".into()),
                repo: None,
                git_ref: None,
            },
        }
    }

    fn request(backend: Backend, resume: Option<&str>) -> TurnExecution {
        TurnExecution {
            run_id: "run-018f000000000001".into(),
            turn_id: "turn-018f000000000002".into(),
            backend,
            backend_session_id: resume.map(str::to_string),
            new_claude_session_id: "018f0000-0000-7000-8000-000000000003".into(),
            prompt: "SYNTHETIC_PRIVATE_PROMPT".into(),
            record: record(),
        }
    }

    #[test]
    fn claude_new_and_resume_specs_keep_prompt_out_of_argv_and_enable_jsonl() {
        let env = BTreeMap::from([("HOME".into(), "/private/host-home".into())]);
        let fresh = build_turn_spec(
            Path::new("/synthetic/bin/limactl"),
            &env,
            &request(Backend::Claude, None),
        )
        .unwrap();
        let visible = format!("{:?}{:?}", fresh.args, fresh.env);
        assert!(!visible.contains("SYNTHETIC_PRIVATE_PROMPT"));
        assert_eq!(
            fresh.stdin.as_deref(),
            Some("SYNTHETIC_PRIVATE_PROMPT".as_bytes())
        );
        assert!(fresh
            .args
            .windows(2)
            .any(|pair| pair == ["--output-format", "stream-json"]));
        assert!(fresh
            .args
            .windows(2)
            .any(|pair| pair == ["--session-id", "018f0000-0000-7000-8000-000000000003"]));
        assert!(fresh
            .args
            .windows(2)
            .any(|pair| pair == ["--permission-mode", "bypassPermissions"]));
        assert!(fresh
            .args
            .iter()
            .any(|arg| arg.contains(r#"setsid "$@" <&0 &"#)));

        let resumed = build_turn_spec(
            Path::new("/synthetic/bin/limactl"),
            &env,
            &request(
                Backend::Claude,
                Some("018f0000-0000-7000-8000-000000000004"),
            ),
        )
        .unwrap();
        assert!(resumed
            .args
            .windows(2)
            .any(|pair| pair == ["--resume", "018f0000-0000-7000-8000-000000000004"]));
        assert!(!resumed.args.iter().any(|arg| arg == "--session-id"));
    }

    #[test]
    fn codex_specs_use_workspace_write_then_structured_resume() {
        let fresh = build_turn_spec(
            Path::new("/synthetic/bin/limactl"),
            &BTreeMap::new(),
            &request(Backend::Codex, None),
        )
        .unwrap();
        assert!(fresh
            .args
            .windows(2)
            .any(|pair| pair == ["--sandbox", "workspace-write"]));
        assert!(fresh.args.iter().any(|arg| arg == "--json"));
        assert_eq!(fresh.args.last().map(String::as_str), Some("-"));

        let resumed = build_turn_spec(
            Path::new("/synthetic/bin/limactl"),
            &BTreeMap::new(),
            &request(Backend::Codex, Some("019f0000-0000-7000-8000-000000000005")),
        )
        .unwrap();
        let marker = resumed
            .args
            .windows(3)
            .any(|triple| triple == ["resume", "--json", "--color"]);
        assert!(marker, "{:?}", resumed.args);
        assert!(resumed
            .args
            .iter()
            .any(|arg| arg == "019f0000-0000-7000-8000-000000000005"));
    }

    #[test]
    fn command_debug_and_cancel_spec_never_contain_stdin_or_shell_fragments() {
        let spec = build_turn_spec(
            Path::new("/synthetic/bin/limactl"),
            &BTreeMap::from([("PRIVATE".into(), "SYNTHETIC_PRIVATE_ENV".into())]),
            &request(Backend::Claude, None),
        )
        .unwrap();
        let debug = format!("{spec:?}");
        assert!(debug.contains("stdin_bytes"));
        assert!(!debug.contains("SYNTHETIC_PRIVATE_PROMPT"));
        assert!(!debug.contains("SYNTHETIC_PRIVATE_ENV"));

        let cancel = build_cancel_spec(
            Path::new("/synthetic/bin/limactl"),
            &BTreeMap::new(),
            "synthetic-project-a1b2c3d4e5f6",
            "run-018f000000000001",
        )
        .unwrap();
        assert_eq!(cancel.stdin, None);
        assert!(build_cancel_spec(
            Path::new("/synthetic/bin/limactl"),
            &BTreeMap::new(),
            "unsafe;vm",
            "../run",
        )
        .is_err());
    }

    #[test]
    fn oversized_jsonl_line_is_rejected_without_unbounded_allocation() {
        let bytes = vec![b'x'; MAX_BACKEND_LINE_BYTES + 2];
        let mut reader = std::io::BufReader::new(bytes.as_slice());
        assert!(read_backend_line(&mut reader).is_err());
    }

    #[test]
    fn invalid_workspace_or_session_identity_is_rejected_before_spawn() {
        let mut unsafe_request = request(Backend::Claude, Some("bad session;id"));
        unsafe_request.record.workspace.guest_path = "/tmp/outside".into();
        assert!(build_turn_spec(
            &PathBuf::from("/synthetic/bin/limactl"),
            &BTreeMap::new(),
            &unsafe_request,
        )
        .is_err());
    }

    #[test]
    fn execution_accepts_agent_vm_guest_mount_home() {
        let mut mounted = request(Backend::Claude, None);
        mounted.record.workspace.guest_path =
            "/home/dev.guest/synthetic-project-a1b2c3d4e5f6".into();

        let spec = build_turn_spec(
            Path::new("/synthetic/bin/limactl"),
            &BTreeMap::new(),
            &mounted,
        )
        .unwrap();

        assert!(spec
            .args
            .iter()
            .any(|value| value == "/home/dev.guest/synthetic-project-a1b2c3d4e5f6"));
    }
}
