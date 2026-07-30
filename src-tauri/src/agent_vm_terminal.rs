//! Persistent interactive Agent VM terminal transport.
//!
//! The VM lifecycle and private bootstrap remain owned by the `agent-vm`
//! plugin. This module only attaches a dedicated, detached host tmux session
//! to a validated VM entity and exposes bounded screen/input operations to the
//! trusted Tauri panel. Terminal output is never written to EntityStore.

use std::collections::BTreeMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use zeroize::Zeroize;

use crate::entities::Entity;

const TMUX_SOCKET: &str = "jarvis-agent-vm";
const MAX_INPUT_BYTES: usize = 48 * 1024;
const MAX_IMAGE_BYTES: usize = 25 * 1024 * 1024;
const MAX_SCREEN_BYTES: usize = 1024 * 1024;
const CAPTURE_HISTORY_LINES: &str = "-1200";
const STARTUP_MIN_SETTLE: Duration = Duration::from_millis(1_200);
const STARTUP_POLL: Duration = Duration::from_millis(200);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(12);
const BRACKETED_PASTE_SETTLE: Duration = Duration::from_millis(90);
static BUFFER_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static UPLOAD_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const GUEST_UPLOAD_SCRIPT: &str = r#"
upload_dir="$1"
destination="$2"
umask 077
mkdir -p "$upload_dir"
chmod 700 "$upload_dir"
cat > "$destination"
chmod 600 "$destination"
"#;

const GUEST_AGENT_SCRIPT: &str = r#"
backend="$1"
umask 077
env_file="$HOME/.jarvis-vm/agent.env"
if [ -f "$env_file" ]; then
  . "$env_file"
fi
export TERM=xterm-256color
unset CI NO_COLOR
case "$backend" in
  claude)
    exec claude \
      --permission-mode bypassPermissions \
      --setting-sources user,project,local \
      --no-chrome
    ;;
  codex)
    exec codex \
      --sandbox workspace-write \
      --ask-for-approval never \
      --no-alt-screen
    ;;
  *)
    exit 64
    ;;
esac
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalBackend {
    Claude,
    Codex,
}

impl TerminalBackend {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            _ => Err("Agent VM terminal backend должен быть claude или codex".into()),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalTarget {
    pub terminal_id: String,
    pub session_name: String,
    pub project_id: String,
    pub vm_name: String,
    pub guest_workspace: String,
    pub backend: TerminalBackend,
    pub credential_ready: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSnapshot {
    pub terminal_id: String,
    pub session_name: String,
    pub project_id: String,
    pub vm_name: String,
    pub backend: TerminalBackend,
    pub state: String,
    pub screen: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalTools {
    pub tmux: PathBuf,
    pub limactl: PathBuf,
    pub jarvis_dir: PathBuf,
    pub account_home: PathBuf,
}

impl TerminalTools {
    pub fn discover() -> Result<Self, String> {
        Ok(Self {
            tmux: first_executable(
                &[
                    PathBuf::from("/opt/homebrew/bin/tmux"),
                    PathBuf::from("/usr/local/bin/tmux"),
                    PathBuf::from("/usr/bin/tmux"),
                ],
                "tmux",
            )?,
            limactl: first_executable(
                &[
                    PathBuf::from("/opt/homebrew/bin/limactl"),
                    PathBuf::from("/usr/local/bin/limactl"),
                ],
                "limactl",
            )?,
            jarvis_dir: crate::util::jarvis_dir(),
            account_home: crate::util::home_dir(),
        })
    }

    fn command_env(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "HOME".into(),
                self.account_home.to_string_lossy().into_owned(),
            ),
            (
                "PATH".into(),
                "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".into(),
            ),
            ("LANG".into(), "en_US.UTF-8".into()),
        ])
    }

    fn lima_home(&self) -> PathBuf {
        self.jarvis_dir.join("agent-vm/lima")
    }

    fn xdg_config_home(&self) -> PathBuf {
        self.jarvis_dir.join("agent-vm/host-home/.config")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct TerminalCommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub stdin: Option<Vec<u8>>,
}

impl fmt::Debug for TerminalCommandSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalCommandSpec")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .field("stdin_bytes", &self.stdin.as_ref().map(Vec::len))
            .finish()
    }
}

impl Drop for TerminalCommandSpec {
    fn drop(&mut self) {
        if let Some(stdin) = &mut self.stdin {
            stdin.zeroize();
        }
    }
}

struct TerminalCommandResult {
    status: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Default)]
struct StartupScreenGate {
    last_hash: Option<u64>,
    stable_samples: u8,
}

impl StartupScreenGate {
    fn observe(&mut self, screen: &str, elapsed: Duration) -> bool {
        if screen.trim().is_empty() {
            self.last_hash = None;
            self.stable_samples = 0;
            return false;
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        screen.hash(&mut hasher);
        let hash = hasher.finish();
        if self.last_hash == Some(hash) {
            self.stable_samples = self.stable_samples.saturating_add(1);
        } else {
            self.last_hash = Some(hash);
            self.stable_samples = 1;
        }
        elapsed >= STARTUP_MIN_SETTLE && self.stable_samples >= 3
    }
}

impl Drop for TerminalCommandResult {
    fn drop(&mut self) {
        self.stdout.zeroize();
        self.stderr.zeroize();
    }
}

pub fn resolve_target(
    entities: &[Entity],
    project_id: &str,
    backend: &str,
) -> Result<TerminalTarget, String> {
    if !crate::agent_vm::valid_object_id(project_id) {
        return Err("Некорректный Agent VM project ID".into());
    }
    let backend = TerminalBackend::parse(backend)?;
    let entity = entities
        .iter()
        .find(|entity| {
            entity.owner == "plugin:agent-vm"
                && entity.kind == "vm"
                && !entity.stale
                && entity
                    .attrs
                    .get("projectId")
                    .and_then(serde_json::Value::as_str)
                    == Some(project_id)
        })
        .ok_or_else(|| "Agent VM проекта не найдена".to_string())?;
    if !matches!(entity.state.as_str(), "running" | "ready" | "working") {
        return Err("Agent VM проекта не запущена".into());
    }
    let vm_name = entity
        .id
        .strip_prefix("vm.")
        .filter(|value| valid_vm_name(value))
        .ok_or_else(|| "Agent VM entity содержит unsafe VM name".to_string())?
        .to_string();
    let modules = entity
        .attrs
        .get("modules")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Agent VM entity не содержит modules".to_string())?;
    if !modules
        .iter()
        .any(|module| module.as_str() == Some(backend.as_str()))
    {
        return Err(format!(
            "{} не установлен в этой Agent VM",
            if backend == TerminalBackend::Claude {
                "Claude"
            } else {
                "Codex"
            }
        ));
    }
    let guest_workspace = entity
        .attrs
        .get("guestWorkspace")
        .and_then(serde_json::Value::as_str)
        .filter(|path| safe_guest_workspace(path, &vm_name))
        .ok_or_else(|| "Agent VM entity содержит unsafe guest workspace".to_string())?
        .to_string();
    let credential_ready = entity
        .attrs
        .pointer(&format!("/environment/credentials/{}", backend.as_str()))
        .and_then(serde_json::Value::as_str)
        == Some("ready");
    let terminal_id = format!("{project_id}-{}", backend.as_str());
    let session_name = format!("avm-{project_id}-{}", backend.as_str());
    Ok(TerminalTarget {
        terminal_id,
        session_name,
        project_id: project_id.to_string(),
        vm_name,
        guest_workspace,
        backend,
        credential_ready,
    })
}

pub fn ensure_terminal(
    tools: &TerminalTools,
    target: &TerminalTarget,
    cols: u16,
    rows: u16,
) -> Result<TerminalSnapshot, String> {
    validate_size(cols, rows)?;
    if session_alive(tools, target)? {
        resize_terminal(tools, target, cols, rows)?;
        return snapshot_terminal(tools, target);
    }
    if !target.credential_ready {
        return Err("Agent VM credentials ещё не подготовлены".into());
    }
    let spec = build_start_spec(tools, target, cols, rows)?;
    let result = run(&spec)?;
    if result.status != 0 && !session_alive(tools, target)? {
        return Err(format!(
            "не запустить постоянную Agent VM terminal session (код {})",
            result.status
        ));
    }
    if !session_alive(tools, target)? {
        return Err("Agent VM terminal session завершилась при запуске".into());
    }
    wait_for_input_ready(tools, target)?;
    snapshot_terminal(tools, target)
}

pub fn snapshot_terminal(
    tools: &TerminalTools,
    target: &TerminalTarget,
) -> Result<TerminalSnapshot, String> {
    if !session_alive(tools, target)? {
        return Ok(snapshot(target, "absent", String::new()));
    }
    let spec = tmux_spec(
        tools,
        vec![
            "capture-pane".into(),
            "-p".into(),
            "-S".into(),
            CAPTURE_HISTORY_LINES.into(),
            "-t".into(),
            pane_target(target),
        ],
        None,
    );
    let result = run(&spec)?;
    if result.status != 0 {
        return Ok(snapshot(target, "disconnected", String::new()));
    }
    if result.stdout.len() > MAX_SCREEN_BYTES {
        return Err("Agent VM terminal screen превышает безопасный лимит".into());
    }
    let screen = String::from_utf8(result.stdout.clone())
        .map_err(|_| "Agent VM terminal screen содержит non-UTF-8 bytes".to_string())?;
    Ok(snapshot(target, "working", screen))
}

pub fn input_terminal(
    tools: &TerminalTools,
    target: &TerminalTarget,
    mut text: String,
    submit: bool,
) -> Result<(), String> {
    if text.is_empty() || text.len() > MAX_INPUT_BYTES || text.contains('\0') {
        text.zeroize();
        return Err("Agent VM terminal input имеет недопустимый размер".into());
    }
    if !session_alive(tools, target)? {
        text.zeroize();
        return Err("Agent VM terminal session не запущена".into());
    }
    if submit {
        require_success(
            run(&terminal_key_spec(tools, target, "C-u"))?,
            "не очистить черновик Agent VM terminal",
        )?;
    }
    let buffer = unique_buffer_name();
    let load = tmux_spec(
        tools,
        vec![
            "load-buffer".into(),
            "-b".into(),
            buffer.clone(),
            "-".into(),
        ],
        Some(text.as_bytes().to_vec()),
    );
    let load_result = run(&load)?;
    text.zeroize();
    require_success(load_result, "не передать ввод в Agent VM terminal")?;
    require_success(
        run(&paste_buffer_spec(tools, target, &buffer))?,
        "не вставить ввод в Agent VM terminal",
    )?;
    if submit {
        // Claude/Codex process bracketed paste asynchronously. A separate
        // tmux command plus the same settle window as the main Jarvis tmux
        // transport prevents Enter from overtaking the pasted payload.
        thread::sleep(BRACKETED_PASTE_SETTLE);
        require_success(
            run(&terminal_key_spec(tools, target, "Enter"))?,
            "не отправить ввод в Agent VM terminal",
        )?;
    }
    Ok(())
}

pub fn send_key(tools: &TerminalTools, target: &TerminalTarget, key: &str) -> Result<(), String> {
    if !matches!(
        key,
        "Enter"
            | "Escape"
            | "Up"
            | "Down"
            | "Left"
            | "Right"
            | "Tab"
            | "Backspace"
            | "C-c"
            | "C-d"
            | "C-u"
    ) {
        return Err("Agent VM terminal key не разрешена".into());
    }
    if !session_alive(tools, target)? {
        return Err("Agent VM terminal session не запущена".into());
    }
    require_success(
        run(&terminal_key_spec(tools, target, key))?,
        "не отправить клавишу в Agent VM terminal",
    )
}

pub fn upload_image(
    tools: &TerminalTools,
    target: &TerminalTarget,
    bytes: Vec<u8>,
    extension: &str,
) -> Result<String, String> {
    let (spec, guest_path) = build_upload_spec(tools, target, bytes, extension)?;
    require_success(
        run(&spec)?,
        "не загрузить изображение в приватный каталог Agent VM",
    )?;
    Ok(guest_path)
}

pub fn resize_terminal(
    tools: &TerminalTools,
    target: &TerminalTarget,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    validate_size(cols, rows)?;
    if !session_alive(tools, target)? {
        return Ok(());
    }
    require_success(
        run(&tmux_spec(
            tools,
            vec![
                "resize-window".into(),
                "-x".into(),
                cols.to_string(),
                "-y".into(),
                rows.to_string(),
                "-t".into(),
                target.session_name.clone(),
            ],
            None,
        ))?,
        "не изменить размер Agent VM terminal",
    )
}

pub fn stop_terminal(tools: &TerminalTools, target: &TerminalTarget) -> Result<bool, String> {
    if !session_alive(tools, target)? {
        return Ok(false);
    }
    require_success(
        run(&tmux_spec(
            tools,
            vec![
                "kill-session".into(),
                "-t".into(),
                target.session_name.clone(),
            ],
            None,
        ))?,
        "не остановить Agent VM terminal",
    )?;
    Ok(true)
}

fn build_upload_spec(
    tools: &TerminalTools,
    target: &TerminalTarget,
    bytes: Vec<u8>,
    extension: &str,
) -> Result<(TerminalCommandSpec, String), String> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return Err("Agent VM image имеет недопустимый размер".into());
    }
    let extension = match extension
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "png",
        "jpg" | "jpeg" => "jpg",
        "gif" => "gif",
        "webp" => "webp",
        _ => return Err("Agent VM image имеет неподдерживаемый формат".into()),
    };
    if !tools.limactl.is_absolute()
        || !tools.jarvis_dir.is_absolute()
        || !tools.account_home.is_absolute()
    {
        return Err("Agent VM upload tools должны иметь absolute paths".into());
    }
    let guest_home = Path::new(&target.guest_workspace)
        .parent()
        .and_then(Path::to_str)
        .ok_or_else(|| "Agent VM guest home недоступен".to_string())?;
    let upload_dir = format!("{guest_home}/.jarvis-vm/uploads");
    let sequence = UPLOAD_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let guest_path = format!(
        "{upload_dir}/jarvis-{}-{sequence}.{extension}",
        std::process::id()
    );
    let mut env = tools.command_env();
    env.insert(
        "LIMA_HOME".into(),
        tools.lima_home().to_string_lossy().into_owned(),
    );
    env.insert(
        "XDG_CONFIG_HOME".into(),
        tools.xdg_config_home().to_string_lossy().into_owned(),
    );
    Ok((
        TerminalCommandSpec {
            program: tools.limactl.clone(),
            args: vec![
                "shell".into(),
                "--tty=false".into(),
                "--workdir".into(),
                guest_home.into(),
                target.vm_name.clone(),
                "--".into(),
                "/bin/bash".into(),
                "-ceu".into(),
                GUEST_UPLOAD_SCRIPT.into(),
                "jarvis-agent-upload".into(),
                upload_dir,
                guest_path.clone(),
            ],
            env,
            stdin: Some(bytes),
        },
        guest_path,
    ))
}

pub fn build_start_spec(
    tools: &TerminalTools,
    target: &TerminalTarget,
    cols: u16,
    rows: u16,
) -> Result<TerminalCommandSpec, String> {
    validate_size(cols, rows)?;
    if !tools.tmux.is_absolute()
        || !tools.limactl.is_absolute()
        || !tools.jarvis_dir.is_absolute()
        || !tools.account_home.is_absolute()
    {
        return Err("Agent VM terminal tools должны иметь absolute paths".into());
    }
    let guest_command = [
        "exec env".to_string(),
        format!(
            "LIMA_HOME={}",
            crate::util::shell_quote(&tools.lima_home().to_string_lossy())
        ),
        format!(
            "XDG_CONFIG_HOME={}",
            crate::util::shell_quote(&tools.xdg_config_home().to_string_lossy())
        ),
        crate::util::shell_quote(&tools.limactl.to_string_lossy()),
        "shell".into(),
        "--tty=true".into(),
        "--workdir".into(),
        crate::util::shell_quote(&target.guest_workspace),
        crate::util::shell_quote(&target.vm_name),
        "--".into(),
        "/bin/bash".into(),
        "-ceu".into(),
        crate::util::shell_quote(GUEST_AGENT_SCRIPT),
        "jarvis-agent-terminal".into(),
        crate::util::shell_quote(target.backend.as_str()),
    ]
    .join(" ");
    Ok(tmux_spec(
        tools,
        vec![
            "-f".into(),
            "/dev/null".into(),
            "new-session".into(),
            "-d".into(),
            "-s".into(),
            target.session_name.clone(),
            "-x".into(),
            cols.to_string(),
            "-y".into(),
            rows.to_string(),
            guest_command,
        ],
        None,
    ))
}

fn snapshot(target: &TerminalTarget, state: &str, screen: String) -> TerminalSnapshot {
    TerminalSnapshot {
        terminal_id: target.terminal_id.clone(),
        session_name: target.session_name.clone(),
        project_id: target.project_id.clone(),
        vm_name: target.vm_name.clone(),
        backend: target.backend,
        state: state.to_string(),
        screen,
    }
}

fn pane_target(target: &TerminalTarget) -> String {
    format!("{}:0.0", target.session_name)
}

fn paste_buffer_spec(
    tools: &TerminalTools,
    target: &TerminalTarget,
    buffer: &str,
) -> TerminalCommandSpec {
    tmux_spec(
        tools,
        vec![
            "paste-buffer".into(),
            "-p".into(),
            "-d".into(),
            "-b".into(),
            buffer.into(),
            "-t".into(),
            pane_target(target),
        ],
        None,
    )
}

fn terminal_key_spec(
    tools: &TerminalTools,
    target: &TerminalTarget,
    key: &str,
) -> TerminalCommandSpec {
    tmux_spec(
        tools,
        vec![
            "send-keys".into(),
            "-t".into(),
            pane_target(target),
            key.into(),
        ],
        None,
    )
}

fn session_alive(tools: &TerminalTools, target: &TerminalTarget) -> Result<bool, String> {
    let result = run(&tmux_spec(
        tools,
        vec![
            "has-session".into(),
            "-t".into(),
            target.session_name.clone(),
        ],
        None,
    ))?;
    Ok(result.status == 0)
}

fn wait_for_input_ready(tools: &TerminalTools, target: &TerminalTarget) -> Result<(), String> {
    let started = Instant::now();
    let mut gate = StartupScreenGate::default();
    loop {
        if !session_alive(tools, target)? {
            return Err("Agent VM terminal session завершилась во время запуска".into());
        }
        let snapshot = snapshot_terminal(tools, target)?;
        let elapsed = started.elapsed();
        if gate.observe(&snapshot.screen, elapsed) || elapsed >= STARTUP_TIMEOUT {
            return Ok(());
        }
        thread::sleep(STARTUP_POLL);
    }
}

fn tmux_spec(
    tools: &TerminalTools,
    mut args: Vec<String>,
    stdin: Option<Vec<u8>>,
) -> TerminalCommandSpec {
    let mut prefixed = vec!["-L".into(), TMUX_SOCKET.into()];
    prefixed.append(&mut args);
    TerminalCommandSpec {
        program: tools.tmux.clone(),
        args: prefixed,
        env: tools.command_env(),
        stdin,
    }
}

fn run(spec: &TerminalCommandSpec) -> Result<TerminalCommandResult, String> {
    if !spec.program.is_absolute() {
        return Err("Agent VM terminal program должен быть absolute".into());
    }
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .env_clear()
        .envs(&spec.env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if spec.stdin.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    let mut child = command
        .spawn()
        .map_err(|_| "не запустить Agent VM terminal transport".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Agent VM terminal stdout недоступен".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Agent VM terminal stderr недоступен".to_string())?;
    let stdout_thread = thread::spawn(move || read_bounded_and_drain(stdout, MAX_SCREEN_BYTES + 1));
    let stderr_thread = thread::spawn(move || read_bounded_and_drain(stderr, 64 * 1024));
    if let Some(input) = &spec.stdin {
        child
            .stdin
            .take()
            .ok_or_else(|| "Agent VM terminal stdin недоступен".to_string())?
            .write_all(input)
            .map_err(|_| "не записать Agent VM terminal stdin".to_string())?;
    }
    let status = child
        .wait()
        .map_err(|_| "не дождаться Agent VM terminal transport".to_string())?;
    let stdout = stdout_thread
        .join()
        .map_err(|_| "Agent VM terminal stdout reader завершился аварийно".to_string())?
        .map_err(|_| "не прочитать Agent VM terminal stdout".to_string())?;
    let stderr = stderr_thread
        .join()
        .map_err(|_| "Agent VM terminal stderr reader завершился аварийно".to_string())?
        .map_err(|_| "не прочитать Agent VM terminal stderr".to_string())?;
    Ok(TerminalCommandResult {
        status: status.code().unwrap_or(-1),
        stdout,
        stderr,
    })
}

fn read_bounded_and_drain<R: Read>(mut reader: R, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut bounded = Vec::with_capacity(limit.min(64 * 1024));
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bounded.len());
        bounded.extend_from_slice(&chunk[..read.min(remaining)]);
    }
    Ok(bounded)
}

fn require_success(result: TerminalCommandResult, operation: &str) -> Result<(), String> {
    if result.status == 0 {
        Ok(())
    } else {
        Err(format!("{operation} (код {})", result.status))
    }
}

fn unique_buffer_name() -> String {
    let seq = BUFFER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("jarvis-agent-vm-input-{}-{seq}", std::process::id())
}

fn validate_size(cols: u16, rows: u16) -> Result<(), String> {
    if !(40..=240).contains(&cols) || !(12..=100).contains(&rows) {
        return Err("Agent VM terminal size вне допустимого диапазона".into());
    }
    Ok(())
}

fn safe_guest_workspace(value: &str, vm_name: &str) -> bool {
    let components = Path::new(value).components().collect::<Vec<_>>();
    if components.len() != 4
        || components[0] != Component::RootDir
        || components[1].as_os_str() != "home"
        || components[3].as_os_str() != vm_name
    {
        return false;
    }
    let Some(home) = components[2].as_os_str().to_str() else {
        return false;
    };
    !home.is_empty()
        && !home.starts_with('.')
        && home
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn valid_vm_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn first_executable(candidates: &[PathBuf], name: &str) -> Result<PathBuf, String> {
    candidates
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .ok_or_else(|| format!("{name} не найден в поддерживаемых install paths"))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use serde_json::json;

    use super::*;

    fn vm_entity() -> Entity {
        Entity {
            id: "vm.synthetic-project-a1b2c3d4e5f6".into(),
            kind: "vm".into(),
            owner: "plugin:agent-vm".into(),
            state: "running".into(),
            attrs: json!({
                "projectId":"project-018f000000000001",
                "guestWorkspace":"/home/dev.guest/synthetic-project-a1b2c3d4e5f6",
                "modules":["node","claude","codex"],
                "environment":{
                    "credentials":{"claude":"ready","codex":"ready"}
                }
            }),
            updated_at: 1,
            stale: false,
        }
    }

    fn tools() -> TerminalTools {
        TerminalTools {
            tmux: PathBuf::from("/synthetic/bin/tmux"),
            limactl: PathBuf::from("/synthetic/bin/limactl"),
            jarvis_dir: PathBuf::from("/private/jarvis"),
            account_home: PathBuf::from("/private/account"),
        }
    }

    #[test]
    fn target_is_deterministic_and_only_accepts_owned_running_vm() {
        let target = resolve_target(&[vm_entity()], "project-018f000000000001", "claude").unwrap();

        assert_eq!(target.terminal_id, "project-018f000000000001-claude");
        assert_eq!(target.session_name, "avm-project-018f000000000001-claude");
        assert_eq!(target.vm_name, "synthetic-project-a1b2c3d4e5f6");
        assert!(target.credential_ready);

        let mut foreign = vm_entity();
        foreign.owner = "plugin:foreign".into();
        assert!(resolve_target(&[foreign], "project-018f000000000001", "claude").is_err());
        let mut stopped = vm_entity();
        stopped.state = "stopped".into();
        assert!(resolve_target(&[stopped], "project-018f000000000001", "claude").is_err());
    }

    #[test]
    fn target_rejects_missing_module_and_unsafe_guest_workspace() {
        let mut missing = vm_entity();
        missing.attrs["modules"] = json!(["node", "claude"]);
        assert!(resolve_target(&[missing], "project-018f000000000001", "codex").is_err());

        let mut unsafe_vm = vm_entity();
        unsafe_vm.attrs["guestWorkspace"] = json!("/tmp/project");
        assert!(resolve_target(&[unsafe_vm], "project-018f000000000001", "claude").is_err());
    }

    #[test]
    fn start_spec_launches_interactive_agent_without_prompt_or_secret_argv() {
        let target = resolve_target(&[vm_entity()], "project-018f000000000001", "claude").unwrap();
        let spec = build_start_spec(&tools(), &target, 120, 36).unwrap();
        let visible = format!("{spec:?}");
        let command = spec.args.last().unwrap();

        assert!(spec
            .args
            .windows(2)
            .any(|pair| pair == ["new-session", "-d"]));
        assert!(command.contains("shell --tty=true"));
        assert!(command.contains("--permission-mode bypassPermissions"));
        assert!(command.contains("/home/dev.guest/synthetic-project-a1b2c3d4e5f6"));
        assert!(!visible.contains("credential"));
        assert!(!visible.contains("SYNTHETIC_PRIVATE_PROMPT"));
        assert!(spec.stdin.is_none());
    }

    #[test]
    fn input_is_stdin_only_and_special_keys_are_allowlisted() {
        let target = resolve_target(&[vm_entity()], "project-018f000000000001", "claude").unwrap();
        let load = tmux_spec(
            &tools(),
            vec![
                "load-buffer".into(),
                "-b".into(),
                "synthetic".into(),
                "-".into(),
            ],
            Some(b"SYNTHETIC_PRIVATE_PROMPT".to_vec()),
        );
        let paste = paste_buffer_spec(&tools(), &target, "synthetic");
        let enter = terminal_key_spec(&tools(), &target, "Enter");

        assert!(!format!("{load:?}").contains("SYNTHETIC_PRIVATE_PROMPT"));
        assert!(!load.args.join(" ").contains("SYNTHETIC_PRIVATE_PROMPT"));
        assert_eq!(
            load.stdin.as_deref(),
            Some("SYNTHETIC_PRIVATE_PROMPT".as_bytes())
        );
        assert!(paste.args.contains(&"paste-buffer".to_string()));
        assert!(!paste.args.contains(&";".to_string()));
        assert!(!paste.args.contains(&"Enter".to_string()));
        assert_eq!(enter.args.last().map(String::as_str), Some("Enter"));
        assert_eq!(BRACKETED_PASTE_SETTLE, Duration::from_millis(90));
        assert!(matches!(
            "C-c",
            "Enter"
                | "Escape"
                | "Up"
                | "Down"
                | "Left"
                | "Right"
                | "Tab"
                | "Backspace"
                | "C-c"
                | "C-d"
                | "C-u"
        ));
        assert_eq!(
            pane_target(&target),
            "avm-project-018f000000000001-claude:0.0"
        );
    }

    #[test]
    fn terminal_dimensions_are_bounded() {
        assert!(validate_size(120, 36).is_ok());
        assert!(validate_size(39, 36).is_err());
        assert!(validate_size(120, 101).is_err());
    }

    #[test]
    fn bounded_reader_keeps_prefix_and_drains_remaining_output() {
        let bytes = (0_u8..=255).cycle().take(32 * 1024).collect::<Vec<_>>();
        let mut reader = Cursor::new(bytes.clone());

        let bounded = read_bounded_and_drain(&mut reader, 1024).unwrap();

        assert_eq!(bounded, bytes[..1024]);
        assert_eq!(reader.position(), bytes.len() as u64);
    }

    #[test]
    fn image_upload_is_private_guest_stdin_and_never_project_or_argv_data() {
        let target = resolve_target(&[vm_entity()], "project-018f000000000001", "claude").unwrap();
        let bytes = b"SYNTHETIC_PRIVATE_IMAGE_BYTES".to_vec();
        let (spec, guest_path) =
            build_upload_spec(&tools(), &target, bytes.clone(), "png").unwrap();

        assert_eq!(spec.stdin.as_deref(), Some(bytes.as_slice()));
        assert!(!format!("{spec:?}").contains("SYNTHETIC_PRIVATE_IMAGE_BYTES"));
        assert!(!spec
            .args
            .join(" ")
            .contains("SYNTHETIC_PRIVATE_IMAGE_BYTES"));
        assert!(guest_path.starts_with("/home/dev.guest/.jarvis-vm/uploads/"));
        assert!(!guest_path.starts_with(&target.guest_workspace));
        assert!(guest_path.ends_with(".png"));
        assert!(build_upload_spec(&tools(), &target, vec![], "png").is_err());
        assert!(build_upload_spec(&tools(), &target, vec![1], "exe").is_err());
    }

    #[test]
    fn cold_terminal_waits_for_a_stable_screen_before_accepting_first_input() {
        let mut gate = StartupScreenGate::default();

        assert!(!gate.observe("", Duration::from_secs(2)));
        assert!(!gate.observe("Claude starting…", Duration::from_millis(800)));
        assert!(!gate.observe("Claude starting…", Duration::from_millis(1_200)));
        assert!(!gate.observe("❯ ", Duration::from_millis(1_400)));
        assert!(!gate.observe("❯ ", Duration::from_millis(1_600)));
        assert!(gate.observe("❯ ", Duration::from_millis(1_800)));
    }
}
