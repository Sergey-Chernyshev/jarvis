//! Agent VM inventory and explicit lifecycle actions. Registry schemas are from
//! MikD1/agent-vm v0.1–v0.3/current; live state comes from Lima JSON, never tables.

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;
use tokio::process::Command;

const MAX_RECORD_BYTES: u64 = 256 * 1024;
static ACTIVE_ACTIONS: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Workspace {
    #[serde(default)]
    mode: String,
    #[serde(default)]
    host_path: String,
    #[serde(default)]
    guest_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Mount {
    host_path: String,
    guest_path: String,
}

// Deliberately omit files, scripts, environment and provisioning credentials.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    name: String,
    #[serde(default)]
    config_dir: String,
    #[serde(default)]
    workspace: Option<Workspace>,
    #[serde(default)]
    mounts: Vec<Mount>,
}

#[derive(Clone, Debug, Default)]
struct Cli {
    path: Option<PathBuf>,
    version: Option<String>,
    generation: &'static str,
    start: bool,
    stop: bool,
}

#[derive(Debug)]
struct Inventory {
    cli: Cli,
    vms: Vec<Value>,
    errors: Vec<String>,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn executable_paths() -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::env::var_os("PATH")
        .map(|v| {
            std::env::split_paths(&v)
                .filter(|p| p.is_absolute())
                .collect()
        })
        .unwrap_or_default();
    paths.extend([
        crate::util::home_dir().join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ]);
    paths
}

fn executable(name: &str) -> Option<PathBuf> {
    executable_paths()
        .into_iter()
        .map(|p| p.join(name))
        .find(|p| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                p.metadata()
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            }
            #[cfg(not(unix))]
            p.is_file()
        })
}

async fn run(path: &Path, args: &[&str], seconds: u64) -> Result<String, String> {
    let mut command = Command::new(path);
    command.args(args).stdin(Stdio::null()).kill_on_drop(true);
    // Finder-launched macOS apps often lack Homebrew in PATH; avm itself calls Lima.
    if let Ok(path) = std::env::join_paths(executable_paths()) {
        command.env("PATH", path);
    }
    let output = tokio::time::timeout(Duration::from_secs(seconds), command.output())
        .await
        .map_err(|_| {
            "Команда не завершилась вовремя. Обновите состояние перед повтором.".to_string()
        })?
        .map_err(|e| format!("Не удалось запустить команду: {e}"))?;
    if !output.status.success() {
        // Provisioning output may contain credentials: never forward it into UI.
        return Err(format!(
            "Команда завершилась с ошибкой ({}). Подробности доступны в avm или limactl.",
            output.status
        ));
    }
    if output.stdout.len() > 8 * 1024 * 1024 {
        return Err("Ответ команды слишком большой.".into());
    }
    String::from_utf8(output.stdout).map_err(|_| "Команда вернула некорректный UTF-8.".into())
}

fn cli_from_help(path: PathBuf, help: &str, version: Option<String>) -> Cli {
    let commands: HashSet<_> = help
        .lines()
        .skip_while(|line| line.trim() != "Available Commands:")
        .skip(1)
        .take_while(|line| line.trim() != "Flags:")
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    let recognized = help.contains("Lima")
        && help.contains("avm")
        && ["list", "create", "shell"]
            .iter()
            .all(|c| commands.contains(c));
    let generation = if recognized && commands.contains("mount") && commands.contains("unmount") {
        "modern"
    } else if recognized && help.contains("one per project") {
        "legacy"
    } else {
        "unknown"
    };
    Cli {
        path: Some(path),
        version,
        generation,
        start: generation != "unknown" && commands.contains("start"),
        stop: generation != "unknown" && commands.contains("stop"),
    }
}

async fn discover_cli() -> (Cli, Option<String>) {
    let Some(path) = executable("avm") else {
        return (
            Cli {
                generation: "unknown",
                ..Cli::default()
            },
            None,
        );
    };
    let (help, version) = tokio::join!(run(&path, &["--help"], 8), run(&path, &["--version"], 8));
    match help {
        Ok(help) => {
            let version = version.ok().map(|v| {
                v.trim()
                    .strip_prefix("avm version ")
                    .unwrap_or(v.trim())
                    .to_string()
            });
            (cli_from_help(path, &help, version), None)
        }
        Err(error) => (
            Cli {
                path: Some(path),
                generation: "unknown",
                ..Cli::default()
            },
            Some(error),
        ),
    }
}

fn registry_root() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::util::home_dir().join(".config"))
        .join("agent-vm")
}

fn parse_record(content: &[u8], file_name: &str) -> Result<Record, String> {
    let record: Record = serde_yaml::from_slice(content)
        .map_err(|_| format!("Некорректная запись VM: {file_name}"))?;
    if !valid_name(&record.name) || file_name != format!("{}.yaml", record.name) {
        return Err(format!("Имя VM не совпадает с записью: {file_name}"));
    }
    Ok(record)
}

fn read_records(root: &Path) -> (Vec<Record>, Vec<String>) {
    let entries = match std::fs::read_dir(root.join("vms")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (vec![], vec![]),
        Err(_) => return (vec![], vec!["Не удалось прочитать реестр Agent VM.".into()]),
    };
    let mut records = vec![];
    let mut errors = vec![];
    for (index, entry) in entries.take(513).enumerate() {
        if index >= 512 {
            errors.push("Реестр Agent VM превышает лимит 512 записей.".into());
            break;
        }
        let Ok(entry) = entry else {
            errors.push("Не удалось прочитать запись реестра Agent VM.".into());
            continue;
        };
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("yaml") {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type().is_ok_and(|t| t.is_file())
            || !entry.metadata().is_ok_and(|m| m.len() <= MAX_RECORD_BYTES)
        {
            errors.push(format!(
                "Запись Agent VM недоступна или слишком велика: {name}"
            ));
            continue;
        }
        match std::fs::read(path)
            .map_err(|_| format!("Не удалось прочитать запись VM: {name}"))
            .and_then(|data| parse_record(&data, &name))
        {
            Ok(record) => records.push(record),
            Err(error) => errors.push(error),
        }
    }
    (records, errors)
}

fn parse_runtime(text: &str) -> Result<BTreeMap<String, Value>, String> {
    let values: Vec<Value> = if text.trim_start().starts_with('[') {
        serde_json::from_str(text).map_err(|_| "Некорректный JSON от Lima.".to_string())?
    } else {
        serde_json::Deserializer::from_str(text)
            .into_iter::<Value>()
            .collect::<Result<_, _>>()
            .map_err(|_| "Некорректный JSON от Lima.".to_string())?
    };
    let mut runtime = BTreeMap::new();
    for value in values {
        let name = value
            .get("name")
            .and_then(Value::as_str)
            // Other Lima clients may use names outside avm's DNS-label subset.
            // They remain read-only and must not invalidate managed VM state.
            .filter(|n| !n.is_empty() && n.len() <= 255 && !n.chars().any(char::is_control))
            .ok_or("В ответе Lima отсутствует корректное имя VM.")?;
        if runtime.contains_key(name) {
            return Err("В ответе Lima повторяется имя VM.".into());
        }
        runtime.insert(name.to_string(), value);
    }
    Ok(runtime)
}

async fn runtime() -> Result<BTreeMap<String, Value>, String> {
    let path = executable("limactl").ok_or("Lima не найдена. Состояние VM недоступно.")?;
    parse_runtime(&run(&path, &["list", "--json"], 10).await?)
}

fn lifecycle_status(value: Option<&Value>, runtime_known: bool) -> &'static str {
    match value
        .and_then(|v| v.get("status"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("running") => "running",
        Some("stopped") => "stopped",
        _ if value.is_none() && runtime_known => "missing",
        _ => "unknown",
    }
}

fn project(host: &str, guest: &str) -> Value {
    json!({"name": crate::util::basename(if guest.is_empty() { host } else { guest }),
        "path": host, "guestPath": guest})
}

fn config_details(record: &Record) -> (Option<String>, Option<String>) {
    let directory = if !record.config_dir.is_empty() {
        Some(PathBuf::from(&record.config_dir))
    } else {
        record
            .workspace
            .as_ref()
            .filter(|w| w.mode == "mount" && !w.host_path.is_empty())
            .map(|w| PathBuf::from(&w.host_path))
    }
    .filter(|p| p.is_absolute());
    let config = directory
        .as_ref()
        .map(|p| {
            p.join(if record.config_dir.is_empty() {
                ".agent-vm.yaml"
            } else {
                "agent-vm.yaml"
            })
        })
        .filter(|p| p.is_file());
    (
        directory.map(|p| p.to_string_lossy().into_owned()),
        config.map(|p| p.to_string_lossy().into_owned()),
    )
}

/// Lima already writes the exact OpenSSH configuration (including vsock or
/// ProxyCommand). Reuse it; never guess that a VM hostname is plain SSH.
fn connection_metadata(name: &str, live: Option<&Value>) -> Option<Value> {
    let live = live?;
    let config = live.get("sshConfigFile")?.as_str()?;
    if !PathBuf::from(config).is_absolute() || config.chars().any(char::is_control) { return None; }
    let host = live.get("hostname")?.as_str()?;
    if host.is_empty() || host.starts_with('-') || host.chars().any(|c| c.is_control() || c.is_whitespace()) { return None; }
    Some(json!({"name":format!("lima-{name}"),"transport":"ssh","sshHost":host,
        "sshConfigFile":config,"jarvisDir":"~/.jarvis","origin":"agent-vm",
        "vmName":name,"requiresNodeInstall":true,"canConnect":lifecycle_status(Some(live),true)=="running"}))
}

fn merge_inventory(
    cli: &Cli,
    records: Vec<Record>,
    runtime: &BTreeMap<String, Value>,
    runtime_known: bool,
) -> Vec<Value> {
    let mut rows = BTreeMap::new();
    for record in records {
        let live = runtime.get(&record.name);
        let state = lifecycle_status(live, runtime_known);
        let (directory, config) = config_details(&record);
        let projects: Vec<_> = if let Some(workspace) = &record.workspace {
            vec![project(&workspace.host_path, &workspace.guest_path)]
        } else {
            record
                .mounts
                .iter()
                .map(|m| project(&m.host_path, &m.guest_path))
                .collect()
        };
        let compatible_record = match cli.generation {
            "legacy" => record.workspace.is_some() && record.config_dir.is_empty(),
            "modern" => !record.config_dir.is_empty() && record.workspace.is_none(),
            _ => false,
        };
        rows.insert(record.name.clone(), json!({"name": record.name, "status": state,
            "registryStatus": if state == "missing" { "orphaned" } else { "managed" },
            "directory": directory, "configPath": config, "projects": projects,
            "connection": connection_metadata(&record.name, live),
            "capabilities": {"start": compatible_record && cli.start && state == "stopped",
                "stop": compatible_record && cli.stop && state == "running", "openConfig": config.is_some()}}));
    }
    for (name, live) in runtime {
        rows.entry(name.clone()).or_insert_with(|| json!({"name": name,
            "status": lifecycle_status(Some(live), true), "registryStatus": "unmanaged",
            "directory": live.get("dir").and_then(Value::as_str), "configPath": null, "projects": [],
            "capabilities": {"start": false, "stop": false, "openConfig": false}}));
    }
    rows.into_values().collect()
}

async fn inspect() -> Inventory {
    let ((cli, cli_error), runtime, records) = tokio::join!(
        discover_cli(),
        runtime(),
        tokio::task::spawn_blocking(|| read_records(&registry_root()))
    );
    let (records, mut errors) =
        records.unwrap_or_else(|_| (vec![], vec!["Не удалось прочитать реестр Agent VM.".into()]));
    if let Some(error) = cli_error {
        errors.push(error);
    }
    let runtime_known = runtime.is_ok();
    let runtime = runtime.unwrap_or_else(|error| {
        errors.push(error);
        BTreeMap::new()
    });
    let vms = merge_inventory(&cli, records, &runtime, runtime_known);
    Inventory { cli, vms, errors }
}

pub async fn status() -> Value {
    let snapshot = inspect().await;
    // A failed live query still yields useful registry inventory with unknown
    // states. Keep partial results visible instead of making IPC clients throw.
    json!({"ok": true, "partial": !snapshot.errors.is_empty(), "available": snapshot.cli.path.is_some(),
        "version": snapshot.cli.version, "generation": snapshot.cli.generation,
        "capabilities": {"start": snapshot.cli.start, "stop": snapshot.cli.stop},
        "vms": snapshot.vms, "error": if snapshot.errors.is_empty() { None } else { Some(snapshot.errors.join(" ")) }})
}

pub async fn connection(name: &str) -> Result<Value, String> {
    if !valid_name(name) { return Err("Некорректное имя VM".into()); }
    let inventory = inspect().await;
    let row = inventory.vms.iter().find(|row| row["name"] == name).ok_or("VM не найдена")?;
    if row["registryStatus"] != "managed" { return Err("VM не зарегистрирована в Agent VM".into()); }
    row.get("connection").filter(|connection| connection.is_object()).cloned()
        .ok_or_else(|| "Lima не вернула SSH config для VM".into())
}

struct ActionGuard(String);

impl ActionGuard {
    fn acquire(name: &str) -> Result<Self, String> {
        let mut active = ACTIVE_ACTIONS
            .lock()
            .map_err(|_| "Блокировка действий VM недоступна.")?;
        if active.iter().any(|n| n == name) {
            return Err("Для этой VM уже выполняется действие.".into());
        }
        active.push(name.to_string());
        Ok(Self(name.to_string()))
    }
}

impl Drop for ActionGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = ACTIVE_ACTIONS.lock() {
            active.retain(|name| name != &self.0);
        }
    }
}

fn allowed_action<'a>(
    snapshot: &'a Inventory,
    name: &str,
    action: &str,
) -> Result<&'a Value, String> {
    let capability = match action {
        "start" => "start",
        "stop" => "stop",
        "open-config" => "openConfig",
        _ => return Err("Это действие Agent VM не поддерживается.".into()),
    };
    let vm = snapshot
        .vms
        .iter()
        .find(|vm| vm["name"] == name)
        .ok_or("VM отсутствует в текущем реестре.")?;
    if vm["capabilities"][capability] != true {
        return Err(
            "Действие недоступно для текущей версии или состояния VM. Обновите список.".into(),
        );
    }
    Ok(vm)
}

async fn perform_action(name: &str, action: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err("Некорректное имя VM.".into());
    }
    if !["start", "stop", "open-config"].contains(&action) {
        return Err("Это действие Agent VM не поддерживается.".into());
    }
    let _guard = ActionGuard::acquire(name)?;
    let snapshot = inspect().await;
    let vm = allowed_action(&snapshot, name, action)?;
    if action == "open-config" {
        let path = vm["configPath"]
            .as_str()
            .ok_or("Конфигурация VM не найдена.")?;
        if !Path::new(path).is_absolute() || !Path::new(path).is_file() {
            return Err("Файл конфигурации VM больше не доступен.".into());
        }
        #[cfg(target_os = "macos")]
        {
            run(Path::new("/usr/bin/open"), &["-t", path], 10).await?;
        }
        #[cfg(not(target_os = "macos"))]
        {
            let opener = executable("xdg-open")
                .ok_or("Не найден текстовый редактор для открытия конфигурации.")?;
            run(&opener, &[path], 10).await?;
        }
    } else {
        let path = snapshot
            .cli
            .path
            .as_deref()
            .ok_or("Agent VM не установлен.")?;
        run(path, &[action, name], 120).await?;
    }
    Ok(())
}

pub async fn action(name: &str, action: &str) -> Value {
    match perform_action(name, action).await {
        Ok(()) => json!({"ok": true}),
        Err(error) => json!({"ok": false, "error": error}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(generation: &'static str) -> Cli {
        Cli {
            path: Some("/test/avm".into()),
            generation,
            start: true,
            stop: true,
            version: Some("dev".into()),
        }
    }

    fn legacy() -> Record {
        parse_record(b"name: work\nworkspace:\n  mode: mount\n  hostPath: /host/api\n  guestPath: /home/dev/api\n", "work.yaml").unwrap()
    }

    #[test]
    fn distinguishes_cli_generations_even_with_dev_version() {
        let help = "Isolated Lima dev VMs, one per project\nUsage:\n  avm [command]\nAvailable Commands:\n  create Create VM\n  list List VMs\n  shell Open shell\n  start Start VM\n  stop Stop VM\nFlags:\n  --version\n";
        let old = cli_from_help("/avm".into(), help, Some("dev".into()));
        assert_eq!(old.generation, "legacy");
        assert!(old.start && old.stop);
        let modern = help
            .replace("one per project", "one per domain of work")
            .replace(
                "Flags:",
                "  mount Mount project\n  unmount Remove project\nFlags:",
            );
        assert_eq!(
            cli_from_help("/avm".into(), &modern, None).generation,
            "modern"
        );
        let unknown = cli_from_help("/avm".into(), "Other avm with start stop mount", None);
        assert_eq!(unknown.generation, "unknown");
        assert!(!unknown.start);
    }

    #[test]
    fn modern_record_preserves_multiple_guest_project_names() {
        let record = parse_record(b"name: work\nconfigDir: /vm/work\nmounts:\n  - hostPath: /team/api\n    guestPath: /home/dev/team-api\n  - hostPath: /personal/api\n    guestPath: /home/dev/personal-api\nfiles:\n  - anything: ignored\n", "work.yaml").unwrap();
        let runtime = parse_runtime(r#"{"name":"work","status":"Stopped"}"#).unwrap();
        let vms = merge_inventory(&cli("modern"), vec![record], &runtime, true);
        assert_eq!(vms[0]["projects"][0]["name"], "team-api");
        assert_eq!(vms[0]["projects"][1]["path"], "/personal/api");
        assert_eq!(vms[0]["capabilities"]["start"], true);
        assert!(vms[0].get("files").is_none());
    }

    #[test]
    fn jsonl_and_array_lima_outputs_are_supported_but_corruption_is_unknown() {
        let jsonl = "{\"name\":\"one\",\"status\":\"Running\"}\n{\"name\":\"two\",\"status\":\"Stopped\"}\n";
        assert_eq!(parse_runtime(jsonl).unwrap().len(), 2);
        assert_eq!(
            parse_runtime(r#"[{"name":"one","status":"Running"}]"#)
                .unwrap()
                .len(),
            1
        );
        assert!(parse_runtime("warning\n{}").is_err());
        assert!(parse_runtime(r#"{"name":"one"} {"name":"one"}"#).is_err());
        assert!(parse_runtime("").unwrap().is_empty());
    }

    #[test]
    fn missing_runtime_query_is_not_a_missing_vm() {
        let unknown = merge_inventory(&cli("legacy"), vec![legacy()], &BTreeMap::new(), false);
        assert_eq!(unknown[0]["status"], "unknown");
        assert_eq!(unknown[0]["registryStatus"], "managed");
        assert_eq!(unknown[0]["capabilities"]["start"], false);
        let absent = merge_inventory(&cli("legacy"), vec![legacy()], &BTreeMap::new(), true);
        assert_eq!(absent[0]["status"], "missing");
        assert_eq!(absent[0]["registryStatus"], "orphaned");
    }

    #[test]
    fn unrelated_lima_and_incompatible_registry_are_read_only() {
        let runtime = parse_runtime(
            r#"{"name":"work","status":"Stopped"} {"name":"docker_profile","status":"Running"}"#,
        )
        .unwrap();
        let rows = merge_inventory(&cli("modern"), vec![legacy()], &runtime, true);
        assert!(rows
            .iter()
            .all(|r| r["capabilities"]["start"] == false && r["capabilities"]["stop"] == false));
        assert_eq!(rows[0]["registryStatus"], "unmanaged");
    }

    #[test]
    fn refuses_ambiguous_targets_and_unsupported_or_stale_actions() {
        for name in ["", "--all", "../work", "Work", "work\nstop", "work-", "a_b"] {
            assert!(!valid_name(name), "{name}");
        }
        assert!(valid_name("team-123"));
        assert!(parse_record(b"name: other", "work.yaml").is_err());
        let runtime = parse_runtime(r#"{"name":"work","status":"Stopped"}"#).unwrap();
        let cli = cli("legacy");
        let vms = merge_inventory(&cli, vec![legacy()], &runtime, true);
        let snapshot = Inventory {
            cli,
            vms,
            errors: vec![],
        };
        assert!(allowed_action(&snapshot, "work", "start").is_ok());
        assert!(allowed_action(&snapshot, "work", "stop").is_err());
        assert!(allowed_action(&snapshot, "work", "recreate").is_err());
        assert!(allowed_action(&snapshot, "absent", "start").is_err());
    }

    #[test]
    fn duplicate_actions_remain_blocked_until_guard_is_dropped() {
        let guard = ActionGuard::acquire("test-unique-vm").unwrap();
        assert!(ActionGuard::acquire("test-unique-vm").is_err());
        drop(guard);
        assert!(ActionGuard::acquire("test-unique-vm").is_ok());
    }
}
