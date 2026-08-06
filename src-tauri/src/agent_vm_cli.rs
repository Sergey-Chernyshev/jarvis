//! Standalone Agent VM terminal control.
//!
//! `jarvis vm ...` is handled before AppKit/Tauri startup, so a shell can
//! inspect and attach to an already-running interactive Agent VM session even
//! when the Jarvis panel is closed. List, inventory and attach are read-only;
//! lifecycle changes happen only through explicit start/stop/restart commands.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::agent_vm_terminal::{self, TerminalSession, TerminalTools};

const MAX_CONTROL_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(100);
const AGENT_VM_OWNER: &str = "plugin:agent-vm";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VmLifecycleAction {
    Status,
    Start,
    Stop,
    Restart,
}

impl VmLifecycleAction {
    fn command(self) -> &'static str {
        match self {
            Self::Status => "runtime.status",
            Self::Start => "runtime.ensure",
            Self::Stop => "runtime.stop",
            Self::Restart => "runtime.restart",
        }
    }

    fn timeout(self) -> Duration {
        match self {
            Self::Status => Duration::from_secs(30),
            Self::Start | Self::Stop | Self::Restart => Duration::from_secs(10 * 60),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum VmCommand {
    Help,
    List {
        all: bool,
    },
    Inventory {
        json: bool,
    },
    Attach {
        target: Option<String>,
        all: bool,
    },
    Lifecycle {
        action: VmLifecycleAction,
        cwd: Option<PathBuf>,
        json: bool,
    },
}

pub fn maybe_run() -> Option<i32> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let (first, rest) = args.split_first()?;
    if first != "vm" {
        return None;
    }
    Some(match parse(rest) {
        Ok(command) => run(command),
        Err(error) => {
            eprintln!("jarvis vm: {error}");
            eprintln!("{}", usage());
            2
        }
    })
}

fn parse(args: &[OsString]) -> Result<VmCommand, String> {
    let args = args
        .iter()
        .map(|value| {
            value
                .to_str()
                .map(str::to_string)
                .ok_or_else(|| "аргументы должны быть UTF-8".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let lifecycle = args.first().and_then(|command| match command.as_str() {
        "status" => Some(VmLifecycleAction::Status),
        "start" => Some(VmLifecycleAction::Start),
        "stop" => Some(VmLifecycleAction::Stop),
        "restart" => Some(VmLifecycleAction::Restart),
        _ => None,
    });
    if let Some(action) = lifecycle {
        let mut rest = args[1..].to_vec();
        let json = take_flag(&mut rest, "--json");
        if rest.len() > 1 || rest.first().is_some_and(|value| value.starts_with('-')) {
            return Err("lifecycle command принимает не более одного project path".into());
        }
        return Ok(VmCommand::Lifecycle {
            action,
            cwd: rest.pop().map(PathBuf::from),
            json,
        });
    }
    match args.as_slice() {
        [] => Ok(VmCommand::Help),
        [value] if value == "help" || value == "-h" || value == "--help" => Ok(VmCommand::Help),
        [command] if command == "list" => Ok(VmCommand::List { all: false }),
        [command, flag] if command == "list" && flag == "--all" => {
            Ok(VmCommand::List { all: true })
        }
        [command] if command == "inventory" => Ok(VmCommand::Inventory { json: false }),
        [command, flag] if command == "inventory" && flag == "--json" => {
            Ok(VmCommand::Inventory { json: true })
        }
        [command] if command == "attach" => Ok(VmCommand::Attach {
            target: None,
            all: false,
        }),
        [command, flag] if command == "attach" && flag == "--all" => Ok(VmCommand::Attach {
            target: None,
            all: true,
        }),
        [command, target] if command == "attach" => Ok(VmCommand::Attach {
            target: Some(target.clone()),
            all: true,
        }),
        _ => Err("неизвестная команда или лишние аргументы".into()),
    }
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let Some(index) = args.iter().position(|value| value == flag) else {
        return false;
    };
    args.remove(index);
    true
}

fn run(command: VmCommand) -> i32 {
    if command == VmCommand::Help {
        println!("{}", usage());
        return 0;
    }
    if let VmCommand::Lifecycle { action, cwd, json } = &command {
        return run_lifecycle(*action, cwd.clone(), *json);
    }
    if let VmCommand::Inventory { json } = &command {
        return run_inventory(*json);
    }
    let tools = match TerminalTools::discover() {
        Ok(tools) => tools,
        Err(error) => {
            eprintln!("jarvis vm: {error}");
            return 1;
        }
    };
    let sessions = match agent_vm_terminal::list_sessions(&tools) {
        Ok(sessions) => sessions,
        Err(error) => {
            eprintln!("jarvis vm: {error}");
            return 1;
        }
    };
    match command {
        VmCommand::Help => 0,
        VmCommand::Inventory { .. } => unreachable!(),
        VmCommand::Lifecycle { .. } => unreachable!(),
        VmCommand::List { all } => {
            let visible = visible_sessions(&sessions, current_project_id().as_deref(), all);
            print_sessions(&visible);
            0
        }
        VmCommand::Attach { target, all } => {
            let visible = visible_sessions(&sessions, current_project_id().as_deref(), all);
            let selected = match choose_session(&sessions, &visible, target.as_deref()) {
                Ok(selected) => selected,
                Err(error) => {
                    eprintln!("jarvis vm: {error}");
                    if !visible.is_empty() {
                        print_sessions(&visible);
                    }
                    return 2;
                }
            };
            match agent_vm_terminal::attach_session(&tools, &selected.session_name) {
                Ok(status) => status,
                Err(error) => {
                    eprintln!("jarvis vm: {error}");
                    1
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct VmInventoryRow {
    runtime: String,
    project: Option<String>,
    project_id: Option<String>,
    cwd: Option<String>,
    status: String,
    session_count: usize,
    management: String,
    stale: bool,
    quarantined: bool,
}

fn run_inventory(json_output: bool) -> i32 {
    let token = crate::capability::tokens::TokenStore::new().ensure_agent_token();
    let result = if token.is_empty() {
        Err("agent token недоступен".to_string())
    } else {
        control_request("GET", "/control/state", &token, None)
            .and_then(|state| vm_inventory_from_control(&state))
            .and_then(|rows| render_vm_inventory(&rows, json_output))
    };
    match result {
        Ok(output) => {
            println!("{output}");
            0
        }
        Err(error) => {
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "ok":false,
                        "error":error
                    }))
                    .unwrap_or_else(|_| "{\"ok\":false}".into())
                );
            } else {
                eprintln!("jarvis vm: {error}");
            }
            1
        }
    }
}

fn control_entities(value: &serde_json::Value) -> Result<&[serde_json::Value], String> {
    let object = value
        .as_object()
        .ok_or_else(|| "Jarvis control state envelope invalid".to_string())?;
    if object.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(object
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Jarvis control state request отклонён")
            .to_owned());
    }
    object
        .get("entities")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| "Jarvis control state не содержит entities array".to_string())
}

fn vm_inventory_from_control(value: &serde_json::Value) -> Result<Vec<VmInventoryRow>, String> {
    let entities = control_entities(value)?;
    let active_runs = entities
        .iter()
        .filter_map(active_run_target)
        .collect::<Vec<_>>();
    let mut runtimes = BTreeSet::new();
    let mut rows = Vec::new();

    for entity in entities {
        if entity.get("owner").and_then(serde_json::Value::as_str) != Some(AGENT_VM_OWNER)
            || entity.get("kind").and_then(serde_json::Value::as_str) != Some("vm")
        {
            continue;
        }
        let id = entity
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "Agent VM entity runtime id отсутствует".to_string())?;
        let runtime = id
            .strip_prefix("vm.")
            .filter(|value| valid_vm_runtime(value))
            .ok_or_else(|| format!("Agent VM entity runtime invalid: {id:?}"))?;
        if !runtimes.insert(runtime.to_owned()) {
            return Err(format!(
                "Agent VM inventory содержит duplicate runtime {runtime}"
            ));
        }
        let status = entity
            .get("state")
            .and_then(serde_json::Value::as_str)
            .filter(|value| safe_token(value, 64))
            .ok_or_else(|| format!("Agent VM {runtime} status invalid"))?
            .to_owned();
        let attrs = entity
            .get("attrs")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| format!("Agent VM {runtime} attrs invalid"))?;
        let project = optional_display_text(attrs.get("project"), "project", 512)?;
        let project_id = optional_display_text(attrs.get("projectId"), "projectId", 128)?;
        if project_id
            .as_deref()
            .is_some_and(|value| !crate::agent_vm::valid_object_id(value))
        {
            return Err(format!("Agent VM {runtime} projectId invalid"));
        }
        let cwd = optional_display_text(attrs.get("cwd"), "cwd", 4_096)?;
        let management = match attrs.get("management") {
            None | Some(serde_json::Value::Null) => "unknown".to_string(),
            Some(serde_json::Value::String(value)) if safe_token(value, 64) => value.clone(),
            _ => return Err(format!("Agent VM {runtime} management invalid")),
        };
        let stale = entity
            .get("stale")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| format!("Agent VM {runtime} stale marker invalid"))?;
        let quarantined_attr = match attrs.get("quarantined") {
            None | Some(serde_json::Value::Null) => false,
            Some(serde_json::Value::Bool(value)) => *value,
            _ => return Err(format!("Agent VM {runtime} quarantined marker invalid")),
        };
        let session_count = active_runs
            .iter()
            .filter(|target| {
                target.vm_name.as_deref() == Some(runtime)
                    || (target.vm_name.is_none()
                        && project_id.is_some()
                        && target.project_id.as_deref() == project_id.as_deref())
            })
            .count();

        rows.push(VmInventoryRow {
            runtime: runtime.to_owned(),
            project,
            project_id,
            cwd,
            status: status.clone(),
            session_count,
            quarantined: quarantined_attr || status == "quarantined" || management == "quarantined",
            management,
            stale,
        });
    }
    rows.sort_by(|left, right| left.runtime.cmp(&right.runtime));
    Ok(rows)
}

struct ActiveRunTarget {
    vm_name: Option<String>,
    project_id: Option<String>,
}

fn active_run_target(entity: &serde_json::Value) -> Option<ActiveRunTarget> {
    if entity.get("owner").and_then(serde_json::Value::as_str) != Some(AGENT_VM_OWNER)
        || entity.get("kind").and_then(serde_json::Value::as_str) != Some("agent_run")
        || entity
            .get("stale")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true)
        || !matches!(
            entity.get("state").and_then(serde_json::Value::as_str),
            Some("starting" | "working" | "waiting" | "queued")
        )
    {
        return None;
    }
    let attrs = entity.get("attrs")?.as_object()?;
    let vm_name = attrs
        .get("vmName")
        .and_then(serde_json::Value::as_str)
        .filter(|value| valid_vm_runtime(value))
        .map(str::to_owned);
    let project_id = attrs
        .get("projectId")
        .and_then(serde_json::Value::as_str)
        .filter(|value| crate::agent_vm::valid_object_id(value))
        .map(str::to_owned);
    (vm_name.is_some() || project_id.is_some()).then_some(ActiveRunTarget {
        vm_name,
        project_id,
    })
}

fn optional_display_text(
    value: Option<&serde_json::Value>,
    field: &str,
    max_bytes: usize,
) -> Result<Option<String>, String> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value))
            if !value.is_empty()
                && value.len() <= max_bytes
                && !value.chars().any(char::is_control) =>
        {
            Ok(Some(value.clone()))
        }
        Some(serde_json::Value::String(value)) if value.is_empty() => Ok(None),
        _ => Err(format!("Agent VM entity {field} invalid")),
    }
}

fn valid_vm_runtime(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn safe_token(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn render_vm_inventory(rows: &[VmInventoryRow], json_output: bool) -> Result<String, String> {
    if json_output {
        return serde_json::to_string(&serde_json::json!({"ok":true,"vms":rows}))
            .map_err(|_| "не сериализовать Agent VM inventory".to_string());
    }
    if rows.is_empty() {
        return Ok("Нет сохранённых Agent VM runtimes.".into());
    }
    let mut lines = vec!["RUNTIME\tPROJECT\tSTATUS\tSESSIONS\tMANAGEMENT".to_string()];
    lines.extend(rows.iter().map(|row| {
        let project = row
            .project
            .as_deref()
            .or(row.project_id.as_deref())
            .unwrap_or("-");
        let status = if row.stale {
            format!("stale/{}", row.status)
        } else {
            row.status.clone()
        };
        let management = if row.quarantined {
            "quarantined"
        } else {
            row.management.as_str()
        };
        format!(
            "{}\t{project}\t{status}\t{}\t{management}",
            row.runtime, row.session_count
        )
    }));
    Ok(lines.join("\n"))
}

fn run_lifecycle(action: VmLifecycleAction, cwd: Option<PathBuf>, json_output: bool) -> i32 {
    let cwd = cwd
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| "не определить current project directory".to_string())
        .and_then(|path| {
            std::fs::canonicalize(&path)
                .map_err(|_| format!("project path недоступен: {}", path.display()))
        });
    let cwd = match cwd {
        Ok(cwd) => cwd,
        Err(error) => {
            eprintln!("jarvis vm: {error}");
            return 1;
        }
    };
    let token = crate::capability::tokens::TokenStore::new().ensure_agent_token();
    let accepted = control_request(
        "POST",
        "/control/agent-vm",
        &token,
        Some(&serde_json::json!({
            "command":action.command(),
            "args":{"cwd":cwd}
        })),
    );
    let accepted = match accepted {
        Ok(value) if value.get("ok").and_then(serde_json::Value::as_bool) == Some(true) => value,
        Ok(value) => {
            eprintln!(
                "jarvis vm: {}",
                value
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("daemon отклонил Agent VM command")
            );
            return 1;
        }
        Err(error) => {
            eprintln!("jarvis vm: {error}");
            return 1;
        }
    };
    let Some(request_id) = accepted
        .get("requestId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
    else {
        eprintln!("jarvis vm: daemon не вернул operation requestId");
        return 1;
    };
    let result = wait_for_operation(&token, &request_id, action.timeout());
    let _ = control_request(
        "POST",
        "/control/agent-vm/ack",
        &token,
        Some(&serde_json::json!({"requestId":request_id})),
    );
    match result {
        Ok(attrs) => {
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({"ok":true,"value":attrs}))
                        .unwrap_or_else(|_| "{\"ok\":true}".into())
                );
            } else {
                print_lifecycle_result(action, &attrs);
            }
            0
        }
        Err(error) => {
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({"ok":false,"error":error}))
                        .unwrap_or_else(|_| "{\"ok\":false}".into())
                );
            } else {
                eprintln!("jarvis vm: {error}");
            }
            1
        }
    }
}

fn wait_for_operation(
    token: &str,
    request_id: &str,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| "operation timeout имеет unsafe значение".to_string())?;
    loop {
        let state = control_request("GET", "/control/state", token, None)?;
        let entities = control_entities(&state)?;
        if let Some(result) = operation_result(entities, request_id) {
            return result;
        }
        if Instant::now() >= deadline {
            return Err("Agent VM operation не завершилась вовремя".into());
        }
        std::thread::sleep(CONTROL_POLL_INTERVAL);
    }
}

fn operation_result(
    entities: &[serde_json::Value],
    request_id: &str,
) -> Option<Result<serde_json::Value, String>> {
    let entity = entities.iter().find(|entity| {
        entity.get("owner").and_then(serde_json::Value::as_str) == Some("plugin:agent-vm")
            && entity.get("kind").and_then(serde_json::Value::as_str) == Some("operation")
            && entity
                .pointer("/attrs/requestId")
                .and_then(serde_json::Value::as_str)
                == Some(request_id)
    })?;
    match entity.get("state").and_then(serde_json::Value::as_str) {
        Some("done") => Some(Ok(entity
            .get("attrs")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})))),
        Some("error") => Some(Err(entity
            .pointer("/attrs/error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Agent VM operation failed")
            .to_owned())),
        _ => None,
    }
}

fn print_lifecycle_result(action: VmLifecycleAction, attrs: &serde_json::Value) {
    let state = attrs
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let vm = attrs
        .get("vmName")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("-");
    let cwd = attrs
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("-");
    println!("{action:?}\t{state}\t{vm}\t{cwd}");
    if let Some(command) = attrs
        .get("shellCommand")
        .and_then(serde_json::Value::as_str)
    {
        println!("Shell: {command}");
    }
}

fn control_request(
    method: &str,
    path: &str,
    token: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let socket = crate::util::sock_path();
    validate_control_socket(&socket)?;
    let mut stream = UnixStream::connect(&socket)
        .map_err(|_| "Jarvis daemon недоступен; запустите Jarvis".to_string())?;
    validate_control_peer(&stream)?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(10))))
        .map_err(|_| "не настроить Jarvis control timeout".to_string())?;
    let body = body
        .map(serde_json::to_vec)
        .transpose()
        .map_err(|_| "не сериализовать Agent VM control request".to_string())?
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nX-Jarvis-Token: {token}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .and_then(|()| stream.write_all(&body))
        .map_err(|_| "не отправить Agent VM control request".to_string())?;
    let mut response = Vec::new();
    Read::by_ref(&mut stream)
        .take(MAX_CONTROL_RESPONSE_BYTES + 1)
        .read_to_end(&mut response)
        .map_err(|_| "не прочитать Agent VM control response".to_string())?;
    if response.len() as u64 > MAX_CONTROL_RESPONSE_BYTES {
        return Err("Agent VM control response превышает limit".into());
    }
    parse_http_json(&response)
}

fn validate_control_socket(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| "Jarvis daemon socket не найден; запустите Jarvis".to_string())?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err("Jarvis daemon socket имеет unsafe owner/type/mode".into());
    }
    Ok(())
}

fn validate_control_peer(stream: &UnixStream) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let mut uid = 0;
        let mut gid = 0;
        let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
        if result != 0 || uid != unsafe { libc::geteuid() } {
            return Err("Jarvis daemon peer identity не подтверждена".into());
        }
    }
    Ok(())
}

fn parse_http_json(response: &[u8]) -> Result<serde_json::Value, String> {
    let separator = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "Jarvis control вернул invalid HTTP".to_string())?;
    let headers = std::str::from_utf8(&response[..separator])
        .map_err(|_| "Jarvis control headers не UTF-8".to_string())?;
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| "Jarvis control status invalid".to_string())?;
    let value = serde_json::from_slice::<serde_json::Value>(&response[separator + 4..])
        .map_err(|_| "Jarvis control вернул invalid JSON".to_string())?;
    if !(200..300).contains(&status) {
        return Err(value
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Jarvis control request отклонён")
            .to_owned());
    }
    Ok(value)
}

fn current_project_id() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    crate::agent_vm::identity_for_path(Path::new(&cwd))
        .ok()
        .map(|identity| identity.project_id)
}

fn visible_sessions<'a>(
    sessions: &'a [TerminalSession],
    project_id: Option<&str>,
    all: bool,
) -> Vec<&'a TerminalSession> {
    if all {
        return sessions.iter().collect();
    }
    let local = project_id
        .map(|project_id| {
            sessions
                .iter()
                .filter(|session| session.project_id == project_id)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if local.is_empty() {
        sessions.iter().collect()
    } else {
        local
    }
}

fn choose_session<'a>(
    all_sessions: &'a [TerminalSession],
    visible: &[&'a TerminalSession],
    target: Option<&str>,
) -> Result<&'a TerminalSession, String> {
    if let Some(target) = target {
        return all_sessions
            .iter()
            .find(|session| session.session_name == target)
            .ok_or_else(|| "указанная Agent VM session не найдена".to_string());
    }
    match visible {
        [] => Err("нет активных Agent VM terminal sessions".into()),
        [only] => Ok(*only),
        many if !io::stdin().is_terminal() => Err(format!(
            "найдено {} sessions; укажите точное имя: jarvis vm attach <session>",
            many.len()
        )),
        many => {
            eprintln!("Выберите Agent VM session:");
            for (index, session) in many.iter().enumerate() {
                eprintln!(
                    "  {}) {} [{}]{}",
                    index + 1,
                    session.session_name,
                    session.backend.as_str(),
                    if session.attached {
                        " — уже подключена"
                    } else {
                        ""
                    }
                );
            }
            eprint!("> ");
            let _ = io::stderr().flush();
            let mut input = String::new();
            io::stdin()
                .lock()
                .read_line(&mut input)
                .map_err(|_| "не прочитать выбор session".to_string())?;
            let selected = input
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|index| (1..=many.len()).contains(index))
                .ok_or_else(|| "некорректный номер session".to_string())?;
            Ok(many[selected - 1])
        }
    }
}

fn print_sessions(sessions: &[&TerminalSession]) {
    if sessions.is_empty() {
        println!("Нет активных Agent VM terminal sessions.");
        return;
    }
    println!("SESSION\tPROJECT\tBACKEND\tATTACHED");
    for session in sessions {
        println!(
            "{}\t{}\t{}\t{}",
            session.session_name,
            session.project_id,
            session.backend.as_str(),
            if session.attached { "yes" } else { "no" }
        );
    }
}

fn usage() -> &'static str {
    "Использование:
  jarvis vm list [--all]
  jarvis vm attach [--all | <session>]
  jarvis vm inventory [--json]
  jarvis vm status [project-path] [--json]
  jarvis vm start [project-path] [--json]
  jarvis vm stop [project-path] [--json]
  jarvis vm restart [project-path] [--json]

Без --all сначала выбираются terminal sessions текущей project-папки.
Inventory только читает VM entities через owner-only socket запущенного daemon.
Lifecycle-команды используют текущую папку по умолчанию и управляют VM через
уже запущенный Jarvis daemon. Attach не создаёт и не пересоздаёт VM."
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_vm_terminal::TerminalBackend;

    fn session(name: &str, project: &str, backend: TerminalBackend) -> TerminalSession {
        TerminalSession {
            session_name: name.into(),
            project_id: project.into(),
            backend,
            attached: false,
            activity: 42,
        }
    }

    #[test]
    fn parses_list_attach_and_help_without_starting_tauri() {
        assert_eq!(parse(&[]).unwrap(), VmCommand::Help);
        assert_eq!(
            parse(&[OsString::from("list")]).unwrap(),
            VmCommand::List { all: false }
        );
        assert_eq!(
            parse(&[OsString::from("list"), OsString::from("--all")]).unwrap(),
            VmCommand::List { all: true }
        );
        assert_eq!(
            parse(&[OsString::from("inventory")]).unwrap(),
            VmCommand::Inventory { json: false }
        );
        assert_eq!(
            parse(&[OsString::from("inventory"), OsString::from("--json")]).unwrap(),
            VmCommand::Inventory { json: true }
        );
        assert!(parse(&[OsString::from("inventory"), OsString::from("--all")]).is_err());
        assert_eq!(
            parse(&[OsString::from("attach"), OsString::from("--all")]).unwrap(),
            VmCommand::Attach {
                target: None,
                all: true
            }
        );
        assert_eq!(
            parse(&[
                OsString::from("attach"),
                OsString::from("avm-project-0123456789abcdef-claude")
            ])
            .unwrap(),
            VmCommand::Attach {
                target: Some("avm-project-0123456789abcdef-claude".into()),
                all: true
            }
        );
        assert!(parse(&[OsString::from("destroy")]).is_err());
        assert_eq!(
            parse(&[
                OsString::from("restart"),
                OsString::from("/synthetic/project"),
                OsString::from("--json")
            ])
            .unwrap(),
            VmCommand::Lifecycle {
                action: VmLifecycleAction::Restart,
                cwd: Some(PathBuf::from("/synthetic/project")),
                json: true,
            }
        );
        assert_eq!(
            parse(&[OsString::from("status")]).unwrap(),
            VmCommand::Lifecycle {
                action: VmLifecycleAction::Status,
                cwd: None,
                json: false,
            }
        );
        assert!(parse(&[
            OsString::from("start"),
            OsString::from("/one"),
            OsString::from("/two")
        ])
        .is_err());
    }

    #[test]
    fn control_http_and_operation_result_are_strict() {
        let response = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"ok\":true}";
        assert_eq!(parse_http_json(response).unwrap()["ok"], true);
        let denied = b"HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\n\r\n{\"ok\":false,\"error\":\"denied\"}";
        assert_eq!(parse_http_json(denied).unwrap_err(), "denied");
        let state = serde_json::json!({
            "ok":true,
            "entities":[
                {
                    "owner":"plugin:foreign",
                    "kind":"operation",
                    "state":"done",
                    "attrs":{"requestId":"agent-vm-7","state":"foreign"}
                },
                {
                    "owner":"plugin:agent-vm",
                    "kind":"operation",
                    "state":"done",
                    "attrs":{"requestId":"agent-vm-7","state":"running"}
                }
            ]
        });
        let entities = control_entities(&state).unwrap();
        assert_eq!(
            operation_result(entities, "agent-vm-7").unwrap().unwrap()["state"],
            "running"
        );
        assert!(operation_result(entities, "agent-vm-other").is_none());
        assert_eq!(
            control_entities(&serde_json::json!({"ok":false,"error":"denied"})).unwrap_err(),
            "denied"
        );
        assert!(control_entities(&serde_json::json!({"ok":true,"entities":{}})).is_err());
    }

    #[test]
    fn inventory_distinguishes_runtime_management_staleness_and_active_sessions() {
        let state = serde_json::json!({
            "ok":true,
            "entities":[
                {
                    "id":"vm.beta-vm",
                    "owner":"plugin:agent-vm",
                    "kind":"vm",
                    "state":"running",
                    "attrs":{
                        "projectId":"project-beta",
                        "project":"Beta Project",
                        "cwd":"/work/beta",
                        "management":"managed"
                    },
                    "updatedAt":20,
                    "stale":false
                },
                {
                    "id":"vm.alpha-vm",
                    "owner":"plugin:agent-vm",
                    "kind":"vm",
                    "state":"stopped",
                    "attrs":{"management":"unmanaged"},
                    "updatedAt":10,
                    "stale":false
                },
                {
                    "id":"vm.quarantine-vm",
                    "owner":"plugin:agent-vm",
                    "kind":"vm",
                    "state":"quarantined",
                    "attrs":{
                        "project":"Quarantine",
                        "management":"quarantined"
                    },
                    "updatedAt":30,
                    "stale":true
                },
                {
                    "id":"agent_run.active",
                    "owner":"plugin:agent-vm",
                    "kind":"agent_run",
                    "state":"working",
                    "attrs":{"vmName":"beta-vm","projectId":"project-beta"},
                    "updatedAt":21,
                    "stale":false
                },
                {
                    "id":"agent_run.completed",
                    "owner":"plugin:agent-vm",
                    "kind":"agent_run",
                    "state":"completed",
                    "attrs":{"vmName":"beta-vm","projectId":"project-beta"},
                    "updatedAt":22,
                    "stale":false
                },
                {
                    "id":"agent_run.stale",
                    "owner":"plugin:agent-vm",
                    "kind":"agent_run",
                    "state":"waiting",
                    "attrs":{"vmName":"beta-vm","projectId":"project-beta"},
                    "updatedAt":23,
                    "stale":true
                },
                {
                    "id":"vm.foreign",
                    "owner":"plugin:foreign",
                    "kind":"vm",
                    "state":"running",
                    "attrs":{"management":"managed"},
                    "updatedAt":99,
                    "stale":false
                }
            ]
        });

        let rows = vm_inventory_from_control(&state).unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| row.runtime.as_str())
                .collect::<Vec<_>>(),
            ["alpha-vm", "beta-vm", "quarantine-vm"]
        );
        assert_eq!(rows[0].management, "unmanaged");
        assert_eq!(rows[1].project.as_deref(), Some("Beta Project"));
        assert_eq!(rows[1].session_count, 1);
        assert!(rows[2].stale);
        assert!(rows[2].quarantined);

        assert_eq!(
            render_vm_inventory(&rows, false).unwrap(),
            concat!(
                "RUNTIME\tPROJECT\tSTATUS\tSESSIONS\tMANAGEMENT\n",
                "alpha-vm\t-\tstopped\t0\tunmanaged\n",
                "beta-vm\tBeta Project\trunning\t1\tmanaged\n",
                "quarantine-vm\tQuarantine\tstale/quarantined\t0\tquarantined"
            )
        );
        let json =
            serde_json::from_str::<serde_json::Value>(&render_vm_inventory(&rows, true).unwrap())
                .unwrap();
        assert_eq!(json["ok"], true);
        assert_eq!(json["vms"][1]["runtime"], "beta-vm");
        assert_eq!(json["vms"][1]["sessionCount"], 1);
        assert_eq!(json["vms"][2]["quarantined"], true);
    }

    #[test]
    fn inventory_rejects_malformed_owned_entities_without_terminal_injection() {
        let unsafe_runtime = serde_json::json!({
            "ok":true,
            "entities":[{
                "id":"vm.bad\tname",
                "owner":"plugin:agent-vm",
                "kind":"vm",
                "state":"running",
                "attrs":{"management":"managed"},
                "updatedAt":1,
                "stale":false
            }]
        });
        assert!(vm_inventory_from_control(&unsafe_runtime)
            .unwrap_err()
            .contains("runtime"));

        let unsafe_project = serde_json::json!({
            "ok":true,
            "entities":[{
                "id":"vm.safe-name",
                "owner":"plugin:agent-vm",
                "kind":"vm",
                "state":"running",
                "attrs":{"project":"line one\nline two","management":"managed"},
                "updatedAt":1,
                "stale":false
            }]
        });
        assert!(vm_inventory_from_control(&unsafe_project)
            .unwrap_err()
            .contains("project"));

        let duplicate = serde_json::json!({
            "ok":true,
            "entities":[
                {
                    "id":"vm.safe-name",
                    "owner":"plugin:agent-vm",
                    "kind":"vm",
                    "state":"running",
                    "attrs":{"management":"managed"},
                    "updatedAt":1,
                    "stale":false
                },
                {
                    "id":"vm.safe-name",
                    "owner":"plugin:agent-vm",
                    "kind":"vm",
                    "state":"stopped",
                    "attrs":{"management":"managed"},
                    "updatedAt":2,
                    "stale":false
                }
            ]
        });
        assert!(vm_inventory_from_control(&duplicate)
            .unwrap_err()
            .contains("duplicate"));
    }

    #[test]
    fn empty_inventory_has_stable_human_and_json_outputs() {
        let rows = vm_inventory_from_control(&serde_json::json!({
            "ok":true,
            "entities":[{
                "id":"vm.foreign",
                "owner":"plugin:foreign",
                "kind":"vm",
                "state":"running",
                "attrs":{},
                "updatedAt":1,
                "stale":false
            }]
        }))
        .unwrap();
        assert!(rows.is_empty());
        assert_eq!(
            render_vm_inventory(&rows, false).unwrap(),
            "Нет сохранённых Agent VM runtimes."
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&render_vm_inventory(&rows, true).unwrap())
                .unwrap(),
            serde_json::json!({"ok":true,"vms":[]})
        );
    }

    #[test]
    fn current_directory_sessions_are_preferred_but_all_remain_discoverable() {
        let sessions = vec![
            session(
                "avm-project-0123456789abcdef-claude",
                "project-0123456789abcdef",
                TerminalBackend::Claude,
            ),
            session(
                "avm-project-fedcba9876543210-codex",
                "project-fedcba9876543210",
                TerminalBackend::Codex,
            ),
        ];

        let local = visible_sessions(&sessions, Some("project-fedcba9876543210"), false);
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].backend, TerminalBackend::Codex);
        assert_eq!(visible_sessions(&sessions, None, false).len(), 2);
        assert_eq!(
            visible_sessions(&sessions, Some("project-fedcba9876543210"), true).len(),
            2
        );
    }

    #[test]
    fn explicit_attach_only_selects_an_observed_exact_session() {
        let sessions = vec![session(
            "avm-project-0123456789abcdef-claude",
            "project-0123456789abcdef",
            TerminalBackend::Claude,
        )];
        let visible = sessions.iter().collect::<Vec<_>>();

        assert_eq!(
            choose_session(
                &sessions,
                &visible,
                Some("avm-project-0123456789abcdef-claude")
            )
            .unwrap()
            .session_name,
            "avm-project-0123456789abcdef-claude"
        );
        assert!(choose_session(&sessions, &visible, Some("$(touch /tmp/no)")).is_err());
    }
}
