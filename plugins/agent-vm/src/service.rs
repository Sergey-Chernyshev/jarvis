use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CStr;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jarvis_secret_store::{
    migrate_legacy_claude_secret, read_claude_code_credentials, MacKeychainStore, SecretStore,
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::config_mirror::{build_snapshot, build_snapshot_for_project, MirrorRoots};
use crate::guest_bootstrap::{
    bootstrap_spec, build_bundle, guest_credential_probe_spec, load_codex_credential,
    run_bootstrap, run_guest_credential_probe, BootstrapCredentialStatus, LoadedCodexCredential,
};
use crate::inventory::{
    load_records, parse_lima_instances, reconcile, InventoryVm, VmRecord, MAX_ADDITIONAL_MOUNTS,
    MAX_RECORD_BYTES,
};
use crate::project::{ensure_project_link, is_valid_vm_name, ProjectIdentity};
use crate::runner::{CommandRunner, CommandSpec};
use crate::runtime_paths::RuntimePaths;

const PROJECT_SPEC: &str = "\
# Jarvis Agent VM project runtime\n\
modules:\n\
  - node\n\
  - claude\n\
  - codex\n";
const INVENTORY_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_ACTIVE_VMS: usize = 8;
const PROTECTED_ACCOUNT_MOUNT_DIRS: &[&str] = &[
    ".aws",
    ".azure",
    ".claude",
    ".codex",
    ".config",
    ".docker",
    ".gnupg",
    ".jarvis",
    ".jarvis-dev",
    ".kube",
    ".ssh",
    "Library/Keychains",
    ".local/share/keyrings",
];

#[derive(Debug, Deserialize)]
struct ProjectSpec {
    #[serde(default)]
    mounts: Option<Vec<ProjectMountSpec>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ProjectMountSpec {
    Path(String),
    Named {
        path: String,
        #[serde(default)]
        name: String,
    },
}

impl ProjectMountSpec {
    fn path(&self) -> &str {
        match self {
            Self::Path(path) | Self::Named { path, .. } => path,
        }
    }

    fn validate_name(&self) -> Result<(), String> {
        let Self::Named { name, .. } = self else {
            return Ok(());
        };
        if name.is_empty() {
            return Ok(());
        }
        if name == "."
            || name == ".."
            || name.len() > 128
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            return Err("mount name должен быть одним безопасным guest path segment".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toolchain {
    pub avm: PathBuf,
    pub limactl: PathBuf,
}

impl Toolchain {
    pub fn discover() -> Result<Self, String> {
        let account_home = account_home();
        let mut avm_candidates = vec![
            PathBuf::from("/opt/homebrew/bin/avm"),
            PathBuf::from("/usr/local/bin/avm"),
        ];
        if let Some(home) = account_home {
            avm_candidates.insert(0, home.join(".local/bin/avm"));
        }
        Ok(Self {
            avm: first_executable(&avm_candidates, "avm")?,
            limactl: first_executable(
                &[
                    PathBuf::from("/opt/homebrew/bin/limactl"),
                    PathBuf::from("/usr/local/bin/limactl"),
                ],
                "limactl",
            )?,
        })
    }
}

fn account_home() -> Option<PathBuf> {
    // PluginHost очищает окружение, поэтому HOME не является источником истины.
    // getpwuid читает только локальную запись текущего uid и не раскрывает её.
    unsafe {
        let entry = libc::getpwuid(libc::getuid());
        if entry.is_null() || (*entry).pw_dir.is_null() {
            return None;
        }
        CStr::from_ptr((*entry).pw_dir)
            .to_str()
            .ok()
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
    }
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapStatus {
    pub fingerprint: String,
    pub files: usize,
    pub skipped: usize,
    /// guest-пути записей allowlist, которых не было на хосте. Отдельно от
    /// `skipped`: «не нашли что переносить» и «нашли, но отбросили» — разные
    /// диагнозы, а в одном счётчике они неразличимы.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_sources: Vec<String>,
    pub credentials: BootstrapCredentialStatus,
    pub proxy_configured: bool,
}

pub trait ConfigBootstrap: Send + Sync {
    fn apply(&self, record: &crate::inventory::VmRecord) -> Result<BootstrapStatus, String>;
}

#[derive(Clone)]
pub struct SystemConfigBootstrap<R: CommandRunner, S: SecretStore> {
    runner: R,
    paths: RuntimePaths,
    limactl: PathBuf,
    account_home: PathBuf,
    secrets: S,
}

impl<R: CommandRunner, S: SecretStore> SystemConfigBootstrap<R, S> {
    pub fn new(
        runner: R,
        paths: RuntimePaths,
        limactl: PathBuf,
        account_home: PathBuf,
        secrets: S,
    ) -> Self {
        Self {
            runner,
            paths,
            limactl,
            account_home,
            secrets,
        }
    }

    pub fn apply(&self, record: &crate::inventory::VmRecord) -> Result<BootstrapStatus, String> {
        self.apply_inner(record)
    }

    fn apply_inner(&self, record: &crate::inventory::VmRecord) -> Result<BootstrapStatus, String> {
        let probe_spec =
            guest_credential_probe_spec(&self.limactl, &self.paths.command_env(), record)?;
        let guest_credentials = run_guest_credential_probe(&self.runner, &probe_spec)?;
        let migration = migrate_legacy_claude_secret(
            &self.paths.jarvis_dir.join("settings.json"),
            &self.secrets,
        )?;
        let roots = MirrorRoots {
            claude: self.account_home.join(".claude"),
            codex: self.account_home.join(".codex"),
            claude_json: self.account_home.join(".claude.json"),
        };
        let snapshot = match record.workspace.host_path.as_deref() {
            Some(host_path) => {
                let canonical_host = fs::canonicalize(host_path).map_err(|_| {
                    "не canonicalize project workspace для Claude memory".to_string()
                })?;
                build_snapshot_for_project(&roots, &canonical_host, &record.workspace.guest_path)?
            }
            None => build_snapshot(&roots)?,
        };
        let codex_credential = if guest_credentials.codex == "ready" {
            LoadedCodexCredential::ready_without_copy()
        } else {
            load_codex_credential(&snapshot, &self.account_home.join(".codex"))?
        };
        let preserve_guest_claude = guest_credentials.claude == "ready" && migration.kind.is_none();
        let host_claude_login = if !preserve_guest_claude
            && account_home().as_deref() == Some(self.account_home.as_path())
            && migration.kind.is_none()
        {
            read_claude_code_credentials()?
        } else {
            None
        };
        let mut private_env =
            read_private_runtime_env(&self.paths.jarvis_dir.join("settings.json"))?;
        let proxy_configured = private_env.contains_key("HTTPS_PROXY");
        let bundle = build_bundle(
            &snapshot,
            &self.secrets,
            migration.kind,
            host_claude_login.as_ref(),
            &codex_credential,
            &private_env,
        );
        for value in private_env.values_mut() {
            value.zeroize();
        }
        let mut bundle = bundle?;
        if preserve_guest_claude {
            bundle.credential_status.claude = "ready".into();
        }
        let credentials = bundle.credential_status.clone();
        let spec = bootstrap_spec(&self.limactl, &self.paths.command_env(), record, bundle)?;
        run_bootstrap(&self.runner, &spec)?;
        Ok(BootstrapStatus {
            fingerprint: snapshot.fingerprint,
            files: snapshot.files.len(),
            skipped: snapshot.diagnostics.skipped_symlinks
                + snapshot.diagnostics.skipped_non_regular
                + snapshot.diagnostics.skipped_oversize
                + snapshot.diagnostics.removed_host_commands,
            missing_sources: snapshot.diagnostics.missing_sources.clone(),
            credentials,
            proxy_configured,
            ..Default::default()
        })
    }
}

impl<R: CommandRunner, S: SecretStore> ConfigBootstrap for SystemConfigBootstrap<R, S> {
    fn apply(&self, record: &crate::inventory::VmRecord) -> Result<BootstrapStatus, String> {
        self.apply_inner(record)
    }
}

fn read_private_runtime_env(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeMap::new());
        }
        Err(_) => return Err("не проверить private Jarvis settings".into()),
    };
    if !metadata.file_type().is_file() || metadata.len() > 4 * 1024 * 1024 {
        return Err("private Jarvis settings имеют unsafe type или размер".into());
    }
    let mut bytes =
        fs::read(path).map_err(|_| "не прочитать private Jarvis settings".to_string())?;
    let parsed = serde_json::from_slice(&bytes)
        .map_err(|_| "private Jarvis settings содержат invalid JSON".to_string());
    bytes.zeroize();
    let mut value: serde_json::Value = parsed?;
    let proxy = value
        .get_mut("service")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|service| service.remove("proxy"))
        .or_else(|| value.as_object_mut().and_then(|root| root.remove("proxy")))
        .and_then(|value| value.as_str().map(str::to_string));
    let Some(mut proxy) = proxy else {
        return Ok(BTreeMap::new());
    };
    let trimmed = proxy.trim().to_string();
    proxy.zeroize();
    if trimmed.is_empty() {
        return Ok(BTreeMap::new());
    }
    if trimmed.len() > 64 * 1024
        || trimmed
            .bytes()
            .any(|byte| matches!(byte, 0 | b'\n' | b'\r'))
    {
        return Err("private proxy setting имеет unsafe format".into());
    }
    Ok(BTreeMap::from([
        ("HTTP_PROXY".into(), trimmed.clone()),
        ("HTTPS_PROXY".into(), trimmed),
    ]))
}

fn first_executable(candidates: &[PathBuf], name: &str) -> Result<PathBuf, String> {
    candidates
        .iter()
        .find(|path| {
            fs::metadata(path)
                .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
        .cloned()
        .ok_or_else(|| format!("{name} не найден в поддерживаемых install paths"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnsureAction {
    Create,
    Recreate,
    Start,
    Ready,
    Wait,
}

pub fn plan_ensure(current: Option<(&str, &str)>) -> EnsureAction {
    match current {
        None => EnsureAction::Create,
        Some(("orphaned", _)) => EnsureAction::Wait,
        Some(("managed", "stopped")) => EnsureAction::Start,
        Some(("managed", "running")) => EnsureAction::Ready,
        Some(_) => EnsureAction::Wait,
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSnapshot {
    pub project_id: String,
    pub display_name: String,
    pub cwd: String,
    pub vm_name: String,
    pub vm: Option<InventoryVm>,
    pub created_spec: bool,
    pub shell_command: String,
    pub environment: Option<BootstrapStatus>,
}

#[derive(Clone)]
pub struct AgentVmService<R: CommandRunner> {
    runner: R,
    pub paths: RuntimePaths,
    tools: Toolchain,
    config_bootstrap: Option<Arc<dyn ConfigBootstrap>>,
    trust_source: Option<PathBuf>,
    ensure_admission: Arc<Mutex<()>>,
}

impl<R: CommandRunner> AgentVmService<R> {
    pub fn new(runner: R, paths: RuntimePaths, tools: Toolchain) -> Self {
        Self {
            runner,
            paths,
            tools,
            config_bootstrap: None,
            trust_source: None,
            ensure_admission: Arc::new(Mutex::new(())),
        }
    }

    pub fn with_system_bootstrap(
        runner: R,
        paths: RuntimePaths,
        tools: Toolchain,
    ) -> Result<Self, String> {
        let home = account_home()
            .ok_or_else(|| "не определить account home для config mirror".to_string())?;
        let trust_source = home.join(".config/agent-vm");
        paths.sync_trust_from(&trust_source)?;
        let bootstrap = SystemConfigBootstrap::new(
            runner.clone(),
            paths.clone(),
            tools.limactl.clone(),
            home,
            MacKeychainStore,
        );
        Ok(Self {
            runner,
            paths,
            tools,
            config_bootstrap: Some(Arc::new(bootstrap)),
            trust_source: Some(trust_source),
            ensure_admission: Arc::new(Mutex::new(())),
        })
    }

    pub fn inventory(&self) -> Result<Vec<InventoryVm>, String> {
        let result = self
            .runner
            .run_with_timeout(
                &CommandSpec {
                    program: self.tools.limactl.clone(),
                    args: vec![
                        "list".into(),
                        "--format".into(),
                        "{{.Name}}\t{{.Status}}".into(),
                    ],
                    cwd: None,
                    env: self.paths.command_env(),
                    stdin: None,
                },
                INVENTORY_COMMAND_TIMEOUT,
            )?
            .success_or_error("limactl list")?;
        let output = result.stdout_text("limactl list")?;
        let records = load_records(&self.paths.registry_root)?;
        Ok(reconcile(records, parse_lima_instances(output)?))
    }

    pub fn status(&self, cwd: &Path) -> Result<RuntimeSnapshot, String> {
        let project = ProjectIdentity::from_path(cwd)?;
        self.snapshot(&project, self.inventory()?, false)
    }

    pub fn ensure(&self, cwd: &Path) -> Result<RuntimeSnapshot, String> {
        let _admission = self
            .ensure_admission
            .lock()
            .map_err(|_| "Agent VM admission lock poisoned".to_string())?;
        self.paths.create_private_dirs()?;
        if let Some(source) = &self.trust_source {
            self.paths.sync_trust_from(source)?;
        }
        let project = ProjectIdentity::from_path(cwd)?;
        let home = account_home()
            .ok_or_else(|| "не определить account home для mount policy".to_string())?;
        validate_project_spec(&project, &self.paths, &home)?;
        let binding = ensure_project_link(&self.paths.project_links, &project)?;
        let before = self.inventory()?;
        let current = find_project_vm(&before, &project);
        if let Some(vm) = current.filter(|vm| vm.management == "managed") {
            let record = vm
                .record
                .as_ref()
                .ok_or_else(|| "managed VM требует valid Agent VM Record".to_string())?;
            validate_record_mounts(record, &project, &self.paths, &home)?;
        }
        let action = plan_ensure(current.map(|vm| (vm.management.as_str(), vm.state.as_str())));
        let expected_vm_name = current
            .map(|vm| vm.name.clone())
            .unwrap_or_else(|| project.vm_name.clone());
        if matches!(action, EnsureAction::Create | EnsureAction::Start) {
            let active = before
                .iter()
                .filter(|vm| !matches!(vm.state.as_str(), "stopped" | "missing"))
                .count();
            if active >= MAX_ACTIVE_VMS {
                return Err(format!(
                    "достигнут безопасный лимит {MAX_ACTIVE_VMS} active Agent VM; останови ненужную VM перед запуском новой"
                ));
            }
        }
        let mut created_spec = false;
        let changed_runtime = matches!(
            action,
            EnsureAction::Create | EnsureAction::Recreate | EnsureAction::Start
        );
        let inventory = match action {
            EnsureAction::Create => {
                created_spec = ensure_project_spec(&project.canonical_path)?;
                validate_project_spec(&project, &self.paths, &home)?;
                self.run_avm(
                    "create",
                    vec!["create".into(), binding.to_string_lossy().into_owned()],
                )?;
                self.inventory()
            }
            EnsureAction::Recreate => {
                let name = current
                    .map(|vm| vm.name.as_str())
                    .unwrap_or(&project.vm_name);
                let record = current
                    .and_then(|vm| vm.record.as_ref())
                    .ok_or_else(|| "recreate требует valid Agent VM Record".to_string())?;
                validate_record_mounts(record, &project, &self.paths, &home)?;
                self.run_lifecycle("recreate", name)?;
                self.inventory()
            }
            EnsureAction::Start => {
                let name = current
                    .map(|vm| vm.name.as_str())
                    .unwrap_or(&project.vm_name);
                let record = current
                    .and_then(|vm| vm.record.as_ref())
                    .ok_or_else(|| "start требует valid Agent VM Record".to_string())?;
                validate_record_mounts(record, &project, &self.paths, &home)?;
                self.run_lifecycle("start", name)?;
                self.inventory()
            }
            EnsureAction::Ready | EnsureAction::Wait => Ok(before),
        };
        let inventory = match inventory {
            Ok(inventory) => inventory,
            Err(error) if changed_runtime => {
                return Err(self.contain_post_action_failure(&expected_vm_name, error));
            }
            Err(error) => return Err(error),
        };
        if action != EnsureAction::Wait {
            if let Err(error) = validate_expected_managed_vm(
                &inventory,
                &expected_vm_name,
                &project,
                &self.paths,
                &home,
            ) {
                if changed_runtime {
                    return Err(self.contain_post_action_failure(&expected_vm_name, error));
                }
                return Err(error);
            }
        }
        let snapshot = self.snapshot(&project, inventory, created_spec)?;
        self.bootstrap_if_ready(snapshot)
    }

    pub fn stop(&self, cwd: &Path) -> Result<RuntimeSnapshot, String> {
        self.lifecycle(cwd, "stop")
    }

    pub fn restart(&self, cwd: &Path) -> Result<RuntimeSnapshot, String> {
        self.lifecycle(cwd, "restart")
    }

    fn lifecycle(&self, cwd: &Path, action: &str) -> Result<RuntimeSnapshot, String> {
        let _admission = self
            .ensure_admission
            .lock()
            .map_err(|_| "Agent VM admission lock poisoned".to_string())?;
        let project = ProjectIdentity::from_path(cwd)?;
        let before = self.inventory()?;
        let vm = find_project_vm(&before, &project)
            .ok_or_else(|| "для project не найдена managed Agent VM".to_string())?;
        if vm.management != "managed" {
            return Err(format!("VM {} имеет состояние {}", vm.name, vm.management));
        }
        if action == "restart" {
            let home = account_home()
                .ok_or_else(|| "не определить account home для mount policy".to_string())?;
            let record = vm
                .record
                .as_ref()
                .ok_or_else(|| "restart требует valid Agent VM Record".to_string())?;
            validate_record_mounts(record, &project, &self.paths, &home)?;
            if matches!(vm.state.as_str(), "stopped" | "missing") {
                let active = before
                    .iter()
                    .filter(|item| !matches!(item.state.as_str(), "stopped" | "missing"))
                    .count();
                if active >= MAX_ACTIVE_VMS {
                    return Err(format!(
                        "достигнут безопасный лимит {MAX_ACTIVE_VMS} active Agent VM; останови ненужную VM перед restart"
                    ));
                }
            }
        }
        let vm_name = vm.name.clone();
        self.run_lifecycle(action, &vm_name)?;
        let inventory = match self.inventory() {
            Ok(inventory) => inventory,
            Err(error) if action == "restart" => {
                return Err(self.contain_post_action_failure(&vm_name, error));
            }
            Err(error) => return Err(error),
        };
        if action == "restart" {
            let home = account_home()
                .ok_or_else(|| "не определить account home для mount policy".to_string())?;
            if let Err(error) =
                validate_expected_managed_vm(&inventory, &vm_name, &project, &self.paths, &home)
            {
                return Err(self.contain_post_action_failure(&vm_name, error));
            }
        }
        let snapshot = self.snapshot(&project, inventory, false)?;
        if action == "restart" {
            self.bootstrap_if_ready(snapshot)
        } else {
            Ok(snapshot)
        }
    }

    fn run_lifecycle(&self, action: &str, name: &str) -> Result<(), String> {
        let spec = lifecycle_spec(&self.tools.avm, &self.paths.command_env(), action, name)?;
        self.runner
            .run(&spec)?
            .success_or_error(&format!("avm {action}"))?;
        Ok(())
    }

    fn run_avm(&self, operation: &str, args: Vec<String>) -> Result<(), String> {
        self.runner
            .run(&CommandSpec {
                program: self.tools.avm.clone(),
                args,
                cwd: None,
                env: self.paths.command_env(),
                stdin: None,
            })?
            .success_or_error(&format!("avm {operation}"))?;
        Ok(())
    }

    fn contain_post_action_failure(&self, vm_name: &str, cause: String) -> String {
        let stop = self.run_lifecycle("stop", vm_name);
        let inventory = self.inventory();
        let state = inventory.as_ref().ok().and_then(|inventory| {
            inventory
                .iter()
                .find(|vm| vm.name == vm_name)
                .map(|vm| vm.state.clone())
        });
        let contained = matches!(state.as_deref(), None | Some("stopped") | Some("missing"));
        match (stop, inventory, contained) {
            (_, _, true) => {
                format!("{cause}; rejected Agent VM {vm_name} остановлена и сверена")
            }
            (Err(stop_error), Err(inventory_error), false) => format!(
                "{cause}; не удалось остановить rejected Agent VM {vm_name}: {stop_error}; не удалось сверить состояние: {inventory_error}"
            ),
            (Err(stop_error), _, false) => format!(
                "{cause}; не удалось остановить rejected Agent VM {vm_name}: {stop_error}; observed state={}",
                state.as_deref().unwrap_or("unknown")
            ),
            (_, Err(inventory_error), false) => format!(
                "{cause}; stop rejected Agent VM {vm_name} выполнен, но состояние не сверено: {inventory_error}"
            ),
            (_, _, false) => format!(
                "{cause}; rejected Agent VM {vm_name} осталась в unsafe state={}",
                state.as_deref().unwrap_or("unknown")
            ),
        }
    }

    fn snapshot(
        &self,
        project: &ProjectIdentity,
        inventory: Vec<InventoryVm>,
        created_spec: bool,
    ) -> Result<RuntimeSnapshot, String> {
        let vm = find_project_vm(&inventory, project).cloned();
        let vm_name = vm
            .as_ref()
            .map(|item| item.name.clone())
            .unwrap_or_else(|| project.vm_name.clone());
        Ok(RuntimeSnapshot {
            project_id: project.project_id.clone(),
            display_name: project.display_name.clone(),
            cwd: project.canonical_path.to_string_lossy().into_owned(),
            vm_name: vm_name.clone(),
            vm,
            created_spec,
            shell_command: self.paths.shell_command(&vm_name, true),
            environment: None,
        })
    }

    fn bootstrap_if_ready(&self, mut snapshot: RuntimeSnapshot) -> Result<RuntimeSnapshot, String> {
        let Some(bootstrap) = &self.config_bootstrap else {
            return Ok(snapshot);
        };
        let record = snapshot
            .vm
            .as_ref()
            .filter(|vm| vm.management == "managed" && vm.state == "running")
            .and_then(|vm| vm.record.as_ref());
        if let Some(record) = record {
            snapshot.environment = Some(bootstrap.apply(record)?);
        }
        Ok(snapshot)
    }
}

pub trait RuntimeService: Clone + Send + Sync + 'static {
    fn inventory(&self) -> Result<Vec<InventoryVm>, String>;
    fn status(&self, cwd: &Path) -> Result<RuntimeSnapshot, String>;
    fn ensure(&self, cwd: &Path) -> Result<RuntimeSnapshot, String>;
    fn stop(&self, cwd: &Path) -> Result<RuntimeSnapshot, String>;
    fn restart(&self, cwd: &Path) -> Result<RuntimeSnapshot, String>;
    fn shell_command(&self, vm_name: &str, managed: bool) -> String {
        let executable = if managed { "avm" } else { "limactl" };
        format!("{executable} shell {vm_name}")
    }
}

impl<R: CommandRunner> RuntimeService for AgentVmService<R> {
    fn inventory(&self) -> Result<Vec<InventoryVm>, String> {
        AgentVmService::inventory(self)
    }

    fn status(&self, cwd: &Path) -> Result<RuntimeSnapshot, String> {
        AgentVmService::status(self, cwd)
    }

    fn ensure(&self, cwd: &Path) -> Result<RuntimeSnapshot, String> {
        AgentVmService::ensure(self, cwd)
    }

    fn stop(&self, cwd: &Path) -> Result<RuntimeSnapshot, String> {
        AgentVmService::stop(self, cwd)
    }

    fn restart(&self, cwd: &Path) -> Result<RuntimeSnapshot, String> {
        AgentVmService::restart(self, cwd)
    }

    fn shell_command(&self, vm_name: &str, managed: bool) -> String {
        self.paths.shell_command(vm_name, managed)
    }
}

fn find_project_vm<'a>(
    inventory: &'a [InventoryVm],
    project: &ProjectIdentity,
) -> Option<&'a InventoryVm> {
    inventory.iter().find(|vm| {
        vm.record
            .as_ref()
            .and_then(|record| record.workspace.host_path.as_deref())
            .and_then(|path| fs::canonicalize(path).ok())
            .is_some_and(|path| path == project.canonical_path)
    })
}

fn validate_expected_managed_vm(
    inventory: &[InventoryVm],
    expected_vm_name: &str,
    project: &ProjectIdentity,
    paths: &RuntimePaths,
    account_home: &Path,
) -> Result<(), String> {
    let vm = inventory
        .iter()
        .find(|vm| vm.name == expected_vm_name)
        .ok_or_else(|| {
            format!("Agent VM {expected_vm_name} отсутствует в fresh runtime inventory")
        })?;
    if vm.management != "managed" {
        return Err(format!(
            "Agent VM {expected_vm_name} имеет недоверенное состояние {}",
            vm.management
        ));
    }
    let record = vm
        .record
        .as_ref()
        .ok_or_else(|| format!("Agent VM {expected_vm_name} не содержит valid Record"))?;
    validate_record_mounts(record, project, paths, account_home)
}

pub fn validate_project_id(
    project: &ProjectIdentity,
    supplied: Option<&str>,
) -> Result<(), String> {
    if supplied.is_some_and(|id| id != project.project_id) {
        return Err("projectId не соответствует canonical cwd".into());
    }
    Ok(())
}

pub fn lifecycle_spec(
    avm: &Path,
    env: &BTreeMap<String, String>,
    action: &str,
    vm_name: &str,
) -> Result<CommandSpec, String> {
    if !matches!(action, "start" | "stop" | "restart" | "recreate") {
        return Err(format!("неподдерживаемый lifecycle action {action}"));
    }
    if !is_valid_vm_name(vm_name) {
        return Err("недопустимое VM name".into());
    }
    Ok(CommandSpec {
        program: avm.to_path_buf(),
        args: vec![action.into(), vm_name.into()],
        cwd: None,
        env: env.clone(),
        stdin: None,
    })
}

fn validate_project_spec(
    project: &ProjectIdentity,
    paths: &RuntimePaths,
    account_home: &Path,
) -> Result<(), String> {
    reject_protected_mount(&project.canonical_path, paths, account_home)?;
    let spec_path = project.canonical_path.join(".agent-vm.yaml");
    let metadata = match fs::symlink_metadata(&spec_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "не проверить project config {}: {error}",
                spec_path.display()
            ));
        }
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_RECORD_BYTES {
        return Err("project config должен быть bounded regular file".into());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&spec_path)
        .map_err(|_| "не открыть project config без symlink follow".to_string())?;
    let opened = file
        .metadata()
        .map_err(|_| "не проверить открытый project config".to_string())?;
    if !opened.file_type().is_file() || opened.len() > MAX_RECORD_BYTES {
        return Err("project config изменился или имеет unsafe type/size".into());
    }
    let mut data = Vec::with_capacity(opened.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|_| "не прочитать project config".to_string())?;
    if data.len() as u64 > MAX_RECORD_BYTES {
        return Err("project config превышает size limit".into());
    }
    let spec = serde_yaml::from_slice::<Option<ProjectSpec>>(&data)
        .map_err(|error| format!("не разобрать project config: {error}"))?
        .unwrap_or(ProjectSpec { mounts: None });
    let mounts = spec.mounts.unwrap_or_default();
    if mounts.len() > MAX_ADDITIONAL_MOUNTS {
        return Err(format!(
            "project config содержит больше {MAX_ADDITIONAL_MOUNTS} mounts"
        ));
    }

    let mut seen = BTreeSet::new();
    for mount in mounts {
        mount.validate_name()?;
        let raw = mount.path();
        if raw.is_empty() || raw.len() > 4096 || raw.as_bytes().contains(&0) {
            return Err("mount path имеет unsafe format".into());
        }
        let raw_path = Path::new(raw);
        let candidate = if raw_path.is_absolute() {
            raw_path.to_path_buf()
        } else {
            project.canonical_path.join(raw_path)
        };
        reject_protected_mount(&candidate, paths, account_home)?;
        let canonical = canonical_mount_directory(&candidate)?;
        reject_protected_mount(&canonical, paths, account_home)?;
        if canonical == project.canonical_path {
            return Err("additional mount дублирует primary project mount".into());
        }
        if !canonical.starts_with(&project.canonical_path) {
            return Err(
                "mount path находится вне granted canonical project root; нужен explicit grant"
                    .into(),
            );
        }
        if !seen.insert(canonical) {
            return Err("project config содержит duplicate canonical mount".into());
        }
    }
    Ok(())
}

fn validate_record_mounts(
    record: &VmRecord,
    project: &ProjectIdentity,
    paths: &RuntimePaths,
    account_home: &Path,
) -> Result<(), String> {
    reject_protected_mount(&project.canonical_path, paths, account_home)?;
    if record.workspace.mode_name != "mount" {
        return Err("Agent VM Record не описывает project mount".into());
    }
    let primary = record
        .workspace
        .host_path
        .as_deref()
        .ok_or_else(|| "Agent VM Record не содержит primary host mount".to_string())?;
    let primary = canonical_mount_directory(Path::new(primary))?;
    reject_protected_mount(&primary, paths, account_home)?;
    if primary != project.canonical_path {
        return Err("Agent VM Record primary mount не совпадает с canonical project grant".into());
    }

    let mut seen = BTreeSet::from([primary]);
    for mount in &record.mounts {
        let candidate = Path::new(&mount.host_path);
        reject_protected_mount(candidate, paths, account_home)?;
        let canonical = canonical_mount_directory(candidate)?;
        reject_protected_mount(&canonical, paths, account_home)?;
        if !canonical.starts_with(&project.canonical_path) {
            return Err(
                "Agent VM Record mount находится вне granted canonical project root".into(),
            );
        }
        if !seen.insert(canonical) {
            return Err("Agent VM Record содержит duplicate canonical mount".into());
        }
    }
    Ok(())
}

fn canonical_mount_directory(path: &Path) -> Result<PathBuf, String> {
    let canonical = fs::canonicalize(path)
        .map_err(|_| "mount source недоступен для canonicalize".to_string())?;
    if !canonical.is_dir() {
        return Err("mount source должен быть каталогом".into());
    }
    Ok(canonical)
}

fn reject_protected_mount(
    path: &Path,
    paths: &RuntimePaths,
    account_home: &Path,
) -> Result<(), String> {
    if path == Path::new("/") {
        return Err("mount source не может быть filesystem root".into());
    }
    if path.starts_with("/Library/Keychains") || path.starts_with("/System/Library/Keychains") {
        return Err("mount source указывает на protected system Keychain state".into());
    }
    let canonical_home =
        fs::canonicalize(account_home).unwrap_or_else(|_| account_home.to_path_buf());
    for home in [account_home, canonical_home.as_path()] {
        if path == home {
            return Err("mount source не может быть account home root".into());
        }
        if PROTECTED_ACCOUNT_MOUNT_DIRS
            .iter()
            .map(|relative| home.join(relative))
            .any(|protected| path.starts_with(protected))
        {
            return Err("mount source указывает на protected auth/account state".into());
        }
    }

    let canonical_jarvis =
        fs::canonicalize(&paths.jarvis_dir).unwrap_or_else(|_| paths.jarvis_dir.clone());
    if path.starts_with(&paths.jarvis_dir) || path.starts_with(canonical_jarvis) {
        return Err("mount source указывает на protected Jarvis/plugin/runtime state".into());
    }
    Ok(())
}

pub fn ensure_project_spec(project: &Path) -> Result<bool, String> {
    let path = project.join(".agent-vm.yaml");
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(err) => {
            return Err(format!(
                "не создать project config {}: {err}",
                path.display()
            ));
        }
    };
    file.write_all(PROJECT_SPEC.as_bytes())
        .map_err(|err| format!("не записать project config {}: {err}", path.display()))?;
    file.sync_all()
        .map_err(|err| format!("не сохранить project config {}: {err}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;

    use super::*;
    use crate::runner::{CommandResult, CommandRunner, CommandSpec};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    #[derive(Clone, Default)]
    struct FakeRunner {
        calls: Arc<Mutex<Vec<CommandSpec>>>,
        outputs: Arc<Mutex<VecDeque<CommandResult>>>,
        timeouts: Arc<Mutex<Vec<Duration>>>,
    }

    impl FakeRunner {
        fn with_outputs(outputs: Vec<CommandResult>) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                outputs: Arc::new(Mutex::new(outputs.into())),
                timeouts: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, spec: &CommandSpec) -> Result<CommandResult, String> {
            self.calls.lock().unwrap().push(spec.clone());
            self.outputs
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "fake output exhausted".into())
        }

        fn run_with_timeout(
            &self,
            spec: &CommandSpec,
            timeout: Duration,
        ) -> Result<CommandResult, String> {
            self.timeouts.lock().unwrap().push(timeout);
            self.run(spec)
        }
    }

    #[derive(Clone)]
    struct BlockingBootstrap {
        state: Arc<(Mutex<(bool, bool)>, Condvar)>,
    }

    impl BlockingBootstrap {
        fn new() -> Self {
            Self {
                state: Arc::new((Mutex::new((false, false)), Condvar::new())),
            }
        }

        fn wait_until_started(&self) {
            let (state, changed) = &*self.state;
            let mut state = state.lock().unwrap();
            while !state.0 {
                state = changed.wait(state).unwrap();
            }
        }

        fn release(&self) {
            let (state, changed) = &*self.state;
            state.lock().unwrap().1 = true;
            changed.notify_all();
        }
    }

    impl ConfigBootstrap for BlockingBootstrap {
        fn apply(&self, _record: &VmRecord) -> Result<BootstrapStatus, String> {
            let (state, changed) = &*self.state;
            let mut state = state.lock().unwrap();
            state.0 = true;
            changed.notify_all();
            while !state.1 {
                state = changed.wait(state).unwrap();
            }
            Ok(BootstrapStatus {
                fingerprint: "a".repeat(64),
                files: 0,
                skipped: 0,
                credentials: BootstrapCredentialStatus {
                    claude: "ready".into(),
                    codex: "ready".into(),
                },
                proxy_configured: false,
                ..Default::default()
            })
        }
    }

    #[derive(Clone)]
    struct CorruptingRunner {
        calls: Arc<Mutex<Vec<CommandSpec>>>,
        outputs: Arc<Mutex<VecDeque<CommandResult>>>,
        paths: RuntimePaths,
        vm_name: String,
        binding: PathBuf,
        outside: PathBuf,
    }

    impl CommandRunner for CorruptingRunner {
        fn run(&self, spec: &CommandSpec) -> Result<CommandResult, String> {
            self.calls.lock().unwrap().push(spec.clone());
            if spec.args.first().map(String::as_str) == Some("start") {
                write_record_with_mount(&self.paths, &self.vm_name, &self.binding, &self.outside);
            }
            self.outputs
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "fake output exhausted".into())
        }
    }

    #[derive(Clone)]
    struct ResultRunner {
        calls: Arc<Mutex<Vec<CommandSpec>>>,
        outputs: Arc<Mutex<VecDeque<Result<CommandResult, String>>>>,
    }

    impl CommandRunner for ResultRunner {
        fn run(&self, spec: &CommandSpec) -> Result<CommandResult, String> {
            self.calls.lock().unwrap().push(spec.clone());
            self.outputs
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "fake output exhausted".to_string())?
        }
    }

    fn ok(stdout: &str) -> CommandResult {
        CommandResult {
            status: 0,
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    fn fixture(tag: &str) -> (PathBuf, PathBuf, RuntimePaths) {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-service-{tag}-{}-{id}",
            std::process::id()
        ));
        let project = root.join("work/synthetic-project");
        fs::create_dir_all(&project).unwrap();
        let profile = root.join("profile");
        fs::create_dir_all(&profile).unwrap();
        let paths = RuntimePaths::from_socket(&profile.join("run.sock")).unwrap();
        paths.create_private_dirs().unwrap();
        (root, project, paths)
    }

    fn write_record(paths: &RuntimePaths, vm_name: &str, host_path: &Path) {
        fs::create_dir_all(paths.registry_root.join("vms")).unwrap();
        fs::write(
            paths.registry_root.join(format!("vms/{vm_name}.yaml")),
            format!(
                "name: {vm_name}\nsource: project\nmodules: [node, claude, codex]\nresources: {{cpus: 4, memory: 4GiB, disk: 120GiB}}\nuser: dev\nworkspace:\n  mode: mount\n  guestPath: /home/dev/{vm_name}\n  hostPath: {}\n",
                host_path.display()
            ),
        )
        .unwrap();
    }

    fn write_record_with_mount(
        paths: &RuntimePaths,
        vm_name: &str,
        host_path: &Path,
        mount_path: &Path,
    ) {
        fs::create_dir_all(paths.registry_root.join("vms")).unwrap();
        fs::write(
            paths.registry_root.join(format!("vms/{vm_name}.yaml")),
            format!(
                "name: {vm_name}\nsource: project\nmodules: [node, claude, codex]\nresources: {{cpus: 4, memory: 4GiB, disk: 120GiB}}\nuser: dev\nworkspace:\n  mode: mount\n  guestPath: /home/dev/{vm_name}\n  hostPath: {}\nmounts:\n  - hostPath: {}\n    guestPath: /home/dev/additional\n",
                host_path.display(),
                mount_path.display()
            ),
        )
        .unwrap();
    }

    fn service(runner: FakeRunner, paths: RuntimePaths) -> AgentVmService<FakeRunner> {
        AgentVmService::new(
            runner,
            paths,
            Toolchain {
                avm: PathBuf::from("/synthetic/bin/avm"),
                limactl: PathBuf::from("/synthetic/bin/limactl"),
            },
        )
    }

    #[test]
    fn ensure_plan_never_implicitly_recreates_an_orphaned_vm() {
        assert_eq!(plan_ensure(None), EnsureAction::Create);
        assert_eq!(
            plan_ensure(Some(("orphaned", "missing"))),
            EnsureAction::Wait
        );
        assert_eq!(
            plan_ensure(Some(("managed", "stopped"))),
            EnsureAction::Start
        );
        assert_eq!(
            plan_ensure(Some(("managed", "running"))),
            EnsureAction::Ready
        );
    }

    #[test]
    fn inventory_uses_a_bounded_runtime_tool_deadline() {
        let (root, _project, paths) = fixture("inventory-timeout");
        let runner = FakeRunner::with_outputs(vec![ok("")]);
        let api = service(runner.clone(), paths);

        let inventory = api.inventory().unwrap();

        assert!(inventory.is_empty());
        assert_eq!(
            runner.timeouts.lock().unwrap().as_slice(),
            [INVENTORY_COMMAND_TIMEOUT]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ensure_leaves_an_orphaned_record_untouched_without_a_lifecycle_command() {
        let (root, project, paths) = fixture("orphan-no-recreate");
        let identity = ProjectIdentity::from_path(&project).unwrap();
        let binding = ensure_project_link(&paths.project_links, &identity).unwrap();
        write_record(&paths, &identity.vm_name, &binding);
        let runner = FakeRunner::with_outputs(vec![ok("")]);
        let api = service(runner.clone(), paths);

        let snapshot = api.ensure(&project).unwrap();

        let vm = snapshot
            .vm
            .expect("orphan remains visible for explicit repair");
        assert_eq!(vm.management, "orphaned");
        assert_eq!(vm.state, "missing");
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "ensure must only inspect inventory");
        assert_eq!(calls[0].program, Path::new("/synthetic/bin/limactl"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn existing_spec_rejects_ungranted_and_symlink_escape_mounts_before_runner() {
        use std::os::unix::fs::symlink;

        for tag in ["absolute", "symlink", "home", "auth", "jarvis"] {
            let (root, project, paths) = fixture(&format!("unsafe-spec-{tag}"));
            let outside = root.join("outside");
            fs::create_dir_all(&outside).unwrap();
            let (mount, object_form) = match tag {
                "absolute" => (outside.to_string_lossy().into_owned(), false),
                "symlink" => {
                    let link = project.join("escape");
                    symlink(&outside, &link).unwrap();
                    ("escape".to_string(), true)
                }
                "home" => (
                    account_home()
                        .expect("test account home")
                        .to_string_lossy()
                        .into_owned(),
                    false,
                ),
                "auth" => (
                    account_home()
                        .expect("test account home")
                        .join(".ssh")
                        .to_string_lossy()
                        .into_owned(),
                    false,
                ),
                "jarvis" => (paths.state_root.to_string_lossy().into_owned(), false),
                _ => unreachable!(),
            };
            let mount_yaml = if object_form {
                format!("  - path: {mount}\n    name: escaped\n")
            } else {
                format!("  - {mount}\n")
            };
            fs::write(
                project.join(".agent-vm.yaml"),
                format!("modules: [node]\nmounts:\n{mount_yaml}"),
            )
            .unwrap();
            let runner = FakeRunner::with_outputs(Vec::new());
            let api = service(runner.clone(), paths);

            let error = api.ensure(&project).unwrap_err();

            assert!(
                error.contains("mount") || error.contains("grant"),
                "unsafe project spec must have an actionable error: {error}"
            );
            assert!(
                runner.calls.lock().unwrap().is_empty(),
                "unsafe spec reached a runner command"
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn ensure_rejects_an_ungranted_record_mount_before_lifecycle() {
        let (root, project, paths) = fixture("unsafe-record-mount");
        let identity = ProjectIdentity::from_path(&project).unwrap();
        let binding = ensure_project_link(&paths.project_links, &identity).unwrap();
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        write_record_with_mount(&paths, &identity.vm_name, &binding, &outside);
        let runner =
            FakeRunner::with_outputs(vec![ok(&format!("{}\tStopped\n", identity.vm_name))]);
        let api = service(runner.clone(), paths);

        let error = api.ensure(&project).unwrap_err();

        assert!(
            error.contains("mount") || error.contains("grant"),
            "unsafe Record must have an actionable error: {error}"
        );
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "unsafe Record reached avm lifecycle");
        assert_eq!(calls[0].program, Path::new("/synthetic/bin/limactl"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ensure_rejects_an_ungranted_record_mount_for_an_already_running_vm() {
        let (root, project, paths) = fixture("unsafe-running-record-mount");
        let identity = ProjectIdentity::from_path(&project).unwrap();
        let binding = ensure_project_link(&paths.project_links, &identity).unwrap();
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        write_record_with_mount(&paths, &identity.vm_name, &binding, &outside);
        let runner =
            FakeRunner::with_outputs(vec![ok(&format!("{}\tRunning\n", identity.vm_name))]);
        let api = service(runner.clone(), paths);

        let error = api.ensure(&project).unwrap_err();

        assert!(
            error.contains("mount") || error.contains("grant"),
            "unsafe running Record must have an actionable error: {error}"
        );
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            1,
            "unsafe running Record reached bootstrap or lifecycle"
        );
        assert_eq!(calls[0].program, Path::new("/synthetic/bin/limactl"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn post_create_validation_selects_the_expected_vm_name_before_trusting_its_mount() {
        let (root, project, paths) = fixture("unsafe-created-primary");
        let identity = ProjectIdentity::from_path(&project).unwrap();
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        write_record(&paths, &identity.vm_name, &outside);
        let record = load_records(&paths.registry_root).unwrap().remove(0);
        let inventory = vec![InventoryVm {
            name: identity.vm_name.clone(),
            management: "managed".into(),
            state: "running".into(),
            record: Some(record),
        }];

        assert!(
            find_project_vm(&inventory, &identity).is_none(),
            "the old selector demonstrates why validation could be skipped"
        );
        let error = validate_expected_managed_vm(
            &inventory,
            &identity.vm_name,
            &identity,
            &paths,
            &account_home().expect("test account home"),
        )
        .unwrap_err();

        assert!(error.contains("primary mount") || error.contains("grant"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stop_waits_until_same_vm_bootstrap_restores_guest_home() {
        let (root, project, paths) = fixture("serialized-bootstrap-stop");
        let identity = ProjectIdentity::from_path(&project).unwrap();
        let binding = ensure_project_link(&paths.project_links, &identity).unwrap();
        write_record(&paths, &identity.vm_name, &binding);
        let runner = FakeRunner::with_outputs(vec![
            ok(&format!("{}\tRunning\n", identity.vm_name)),
            ok(&format!("{}\tRunning\n", identity.vm_name)),
            ok("stopped\n"),
            ok(&format!("{}\tStopped\n", identity.vm_name)),
        ]);
        let bootstrap = BlockingBootstrap::new();
        let mut api = service(runner.clone(), paths);
        api.config_bootstrap = Some(Arc::new(bootstrap.clone()));

        let ensure_api = api.clone();
        let ensure_project = project.clone();
        let ensure = thread::spawn(move || ensure_api.ensure(&ensure_project));
        bootstrap.wait_until_started();

        let stop_api = api.clone();
        let stop_project = project.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let stop = thread::spawn(move || {
            let result = stop_api.stop(&stop_project);
            done_tx.send(()).unwrap();
            result
        });

        assert!(
            done_rx.recv_timeout(Duration::from_millis(150)).is_err(),
            "stop raced through a bootstrap that still owns guest home"
        );
        assert_eq!(
            runner.calls.lock().unwrap().len(),
            1,
            "stop reached inventory before bootstrap released its ownership lock"
        );

        bootstrap.release();
        ensure.join().unwrap().unwrap();
        stop.join().unwrap().unwrap();
        assert_eq!(runner.calls.lock().unwrap().len(), 4);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejected_post_start_record_is_stopped_by_exact_vm_name() {
        let (root, project, paths) = fixture("post-start-cleanup");
        let identity = ProjectIdentity::from_path(&project).unwrap();
        let binding = ensure_project_link(&paths.project_links, &identity).unwrap();
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        write_record(&paths, &identity.vm_name, &binding);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let runner = CorruptingRunner {
            calls: calls.clone(),
            outputs: Arc::new(Mutex::new(
                vec![
                    ok(&format!("{}\tStopped\n", identity.vm_name)),
                    ok("started\n"),
                    ok(&format!("{}\tRunning\n", identity.vm_name)),
                    ok("stopped\n"),
                    ok(&format!("{}\tStopped\n", identity.vm_name)),
                ]
                .into(),
            )),
            paths: paths.clone(),
            vm_name: identity.vm_name.clone(),
            binding,
            outside,
        };
        let api = AgentVmService::new(
            runner,
            paths,
            Toolchain {
                avm: PathBuf::from("/synthetic/bin/avm"),
                limactl: PathBuf::from("/synthetic/bin/limactl"),
            },
        );

        let error = api.ensure(&project).unwrap_err();

        assert!(
            error.contains("mount") || error.contains("grant"),
            "{error}"
        );
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 5, "rejected running VM was not reconciled");
        assert_eq!(
            calls[3].args,
            ["stop", identity.vm_name.as_str()],
            "cleanup must stop only the exact VM that passed the pre-action checks"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn post_create_inventory_failure_stops_and_reconciles_exact_vm() {
        let (root, project, paths) = fixture("post-create-inventory-cleanup");
        let identity = ProjectIdentity::from_path(&project).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let runner = ResultRunner {
            calls: calls.clone(),
            outputs: Arc::new(Mutex::new(
                vec![
                    Ok(ok("")),
                    Ok(ok("created\n")),
                    Err("synthetic post-create inventory failure".into()),
                    Ok(ok("stopped\n")),
                    Ok(ok(&format!("{}\tStopped\n", identity.vm_name))),
                ]
                .into(),
            )),
        };
        let api = AgentVmService::new(
            runner,
            paths,
            Toolchain {
                avm: PathBuf::from("/synthetic/bin/avm"),
                limactl: PathBuf::from("/synthetic/bin/limactl"),
            },
        );

        let error = api.ensure(&project).unwrap_err();

        assert!(
            error.contains("synthetic post-create inventory failure"),
            "{error}"
        );
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            5,
            "failed create was not stopped and reconciled"
        );
        assert_eq!(calls[3].args, ["stop", identity.vm_name.as_str()]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_spec_accepts_relative_and_absolute_mounts_inside_the_project_grant() {
        let (root, project, paths) = fixture("safe-spec-mounts");
        let relative = project.join("relative");
        let absolute = project.join("absolute");
        fs::create_dir_all(&relative).unwrap();
        fs::create_dir_all(&absolute).unwrap();
        fs::write(
            project.join(".agent-vm.yaml"),
            format!(
                "modules: [node]\nmounts:\n  - relative\n  - path: {}\n    name: absolute-source\n",
                absolute.display()
            ),
        )
        .unwrap();
        let identity = ProjectIdentity::from_path(&project).unwrap();
        let home = account_home().expect("test account home");

        validate_project_spec(&identity, &paths, &home).unwrap();

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mount_policy_denies_home_auth_and_jarvis_state_as_primary_projects() {
        let (root, _project, paths) = fixture("protected-primary");
        let account_home = root.join("account-home");
        let auth = account_home.join(".ssh");
        fs::create_dir_all(&auth).unwrap();
        for project_path in [&account_home, &auth, &paths.state_root] {
            fs::write(project_path.join(".agent-vm.yaml"), "modules: [node]\n").unwrap();
            let identity = ProjectIdentity::from_path(project_path).unwrap();

            let error = validate_project_spec(&identity, &paths, &account_home).unwrap_err();

            assert!(
                error.contains("home") || error.contains("auth") || error.contains("Jarvis"),
                "protected primary mount has an actionable error: {error}"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ensure_starts_stopped_vm_with_sanitized_argv_and_returns_shell_command() {
        let (root, project, paths) = fixture("start");
        let identity = ProjectIdentity::from_path(&project).unwrap();
        let binding = ensure_project_link(&paths.project_links, &identity).unwrap();
        let additional = project.join("additional");
        fs::create_dir_all(&additional).unwrap();
        write_record_with_mount(&paths, &identity.vm_name, &binding, &additional);
        let runner = FakeRunner::with_outputs(vec![
            ok(&format!("{}\tStopped\n", identity.vm_name)),
            ok("start: synthetic\n"),
            ok(&format!("{}\tRunning\n", identity.vm_name)),
        ]);
        let expected_shell = paths.shell_command(&identity.vm_name, true);
        let api = service(runner.clone(), paths);

        let snapshot = api.ensure(&project).unwrap();

        assert_eq!(snapshot.vm.unwrap().state, "running");
        assert_eq!(snapshot.shell_command, expected_shell);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[1].program, Path::new("/synthetic/bin/avm"));
        assert_eq!(calls[1].args, ["start", identity.vm_name.as_str()]);
        assert_eq!(
            calls[1].env.keys().map(String::as_str).collect::<Vec<_>>(),
            ["HOME", "LANG", "LIMA_HOME", "PATH", "XDG_CONFIG_HOME"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ensure_denies_new_vm_when_the_global_active_budget_is_full() {
        let (root, project, paths) = fixture("active-budget");
        let inventory = (0..MAX_ACTIVE_VMS)
            .map(|index| format!("foreign-{index}\tRunning"))
            .collect::<Vec<_>>()
            .join("\n");
        let runner = FakeRunner::with_outputs(vec![ok(&inventory)]);
        let api = service(runner.clone(), paths);

        let error = api.ensure(&project).unwrap_err();

        assert!(error.contains("лимит"));
        assert!(error.contains(&MAX_ACTIVE_VMS.to_string()));
        assert_eq!(
            runner.calls.lock().unwrap().len(),
            1,
            "budget rejection must not invoke avm create"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restart_of_a_stopped_vm_obeys_the_same_global_active_budget() {
        let (root, project, paths) = fixture("restart-active-budget");
        let identity = ProjectIdentity::from_path(&project).unwrap();
        let binding = ensure_project_link(&paths.project_links, &identity).unwrap();
        write_record(&paths, &identity.vm_name, &binding);
        let mut inventory = vec![format!("{}\tStopped", identity.vm_name)];
        inventory.extend((0..MAX_ACTIVE_VMS).map(|index| format!("foreign-{index}\tRunning")));
        let runner = FakeRunner::with_outputs(vec![ok(&inventory.join("\n"))]);
        let api = service(runner.clone(), paths);

        let error = api.restart(&project).unwrap_err();

        assert!(error.contains("лимит"));
        assert_eq!(
            runner.calls.lock().unwrap().len(),
            1,
            "budget rejection must not invoke avm restart"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn new_project_spec_is_minimal_and_existing_spec_is_never_overwritten() {
        let (root, project, _paths) = fixture("spec");

        assert!(ensure_project_spec(&project).unwrap());
        let generated = fs::read_to_string(project.join(".agent-vm.yaml")).unwrap();
        assert!(generated.contains("node"));
        assert!(generated.contains("claude"));
        assert!(generated.contains("codex"));
        for forbidden in ["proxy", "token", "credential", "api_key", "certificate"] {
            assert!(
                !generated.to_ascii_lowercase().contains(forbidden),
                "generated project spec must stay public-safe"
            );
        }

        fs::write(project.join(".agent-vm.yaml"), "modules: []\n").unwrap();
        assert!(!ensure_project_spec(&project).unwrap());
        assert_eq!(
            fs::read_to_string(project.join(".agent-vm.yaml")).unwrap(),
            "modules: []\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_id_must_match_canonical_cwd() {
        let (_root, project, _paths) = fixture("project-id");
        let identity = ProjectIdentity::from_path(&project).unwrap();
        assert!(validate_project_id(&identity, Some(&identity.project_id)).is_ok());
        let err = validate_project_id(&identity, Some("project-deadbeef")).unwrap_err();
        assert!(err.contains("projectId"));
    }

    #[test]
    fn command_args_are_arrays_not_shell_fragments() {
        let spec = lifecycle_spec(
            Path::new("/synthetic/bin/avm"),
            &BTreeMap::new(),
            "restart",
            "safe-vm-123",
        )
        .unwrap();
        assert_eq!(spec.args, ["restart", "safe-vm-123"]);
        assert!(lifecycle_spec(
            Path::new("/synthetic/bin/avm"),
            &BTreeMap::new(),
            "restart",
            "unsafe;rm"
        )
        .is_err());
    }

    #[test]
    fn system_config_bootstrap_migrates_and_delivers_without_visible_secret_bytes() {
        use jarvis_secret_store::MemorySecretStore;

        let (root, project, paths) = fixture("config-bootstrap");
        let account_home = root.join("account-home");
        fs::create_dir_all(account_home.join(".claude")).unwrap();
        fs::create_dir_all(account_home.join(".codex")).unwrap();
        fs::write(
            account_home.join(".claude/settings.json"),
            br#"{"model":"synthetic"}"#,
        )
        .unwrap();
        fs::write(
            account_home.join(".codex/config.toml"),
            "model = \"synthetic\"\n",
        )
        .unwrap();
        let codex_auth = account_home.join(".codex/auth.json");
        fs::write(&codex_auth, br#"{"auth_mode":"synthetic"}"#).unwrap();
        fs::set_permissions(&codex_auth, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(
            paths.jarvis_dir.join("settings.json"),
            br#"{"service":{"claudeAuthMode":"subscription","claudeSecret":"SYNTHETIC_PRIVATE_VALUE"}}"#,
        )
        .unwrap();
        let identity = ProjectIdentity::from_path(&project).unwrap();
        let record = crate::inventory::VmRecord {
            name: identity.vm_name,
            source: "project".into(),
            modules: vec!["claude".into(), "codex".into()],
            resources: crate::inventory::VmResources::default(),
            user: "dev".into(),
            workspace: crate::inventory::VmWorkspace {
                mode_name: "mount".into(),
                guest_path: "/home/dev/synthetic-project".into(),
                host_path: Some(project.to_string_lossy().into_owned()),
                repo: None,
                git_ref: None,
            },
            mounts: Vec::new(),
        };
        let runner = FakeRunner::with_outputs(vec![ok("claude=missing\ncodex=missing\n"), ok("")]);
        let bootstrap = SystemConfigBootstrap::new(
            runner.clone(),
            paths.clone(),
            PathBuf::from("/synthetic/bin/limactl"),
            account_home,
            MemorySecretStore::default(),
        );

        let status = bootstrap.apply(&record).unwrap();

        assert_eq!(status.credentials.claude, "ready");
        assert_eq!(status.credentials.codex, "ready");
        assert_eq!(status.files, 2);
        assert_eq!(status.fingerprint.len(), 64);
        let settings = fs::read(paths.jarvis_dir.join("settings.json")).unwrap();
        assert!(!settings
            .windows(b"SYNTHETIC_PRIVATE_VALUE".len())
            .any(|part| part == b"SYNTHETIC_PRIVATE_VALUE"));
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].stdin.is_none());
        let visible = format!("{:?}{:?}", calls[1].args, calls[1].env);
        assert!(!visible.contains("SYNTHETIC_PRIVATE_VALUE"));
        assert!(calls[1]
            .stdin
            .as_ref()
            .unwrap()
            .windows(b"SYNTHETIC_PRIVATE_VALUE".len())
            .any(|part| part == b"SYNTHETIC_PRIVATE_VALUE"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn system_config_bootstrap_preserves_ready_guest_credentials_without_recopying_auth() {
        use jarvis_secret_store::MemorySecretStore;

        let (root, project, paths) = fixture("config-bootstrap-preserve");
        let account_home = root.join("account-home");
        fs::create_dir_all(account_home.join(".claude")).unwrap();
        fs::create_dir_all(account_home.join(".codex")).unwrap();
        fs::write(
            account_home.join(".claude/settings.json"),
            br#"{"model":"synthetic"}"#,
        )
        .unwrap();
        fs::write(
            account_home.join(".codex/config.toml"),
            "model = \"synthetic\"\n",
        )
        .unwrap();
        let codex_auth = account_home.join(".codex/auth.json");
        fs::write(
            &codex_auth,
            br#"{"private":"SYNTHETIC_HOST_CODEX_CREDENTIAL"}"#,
        )
        .unwrap();
        fs::set_permissions(&codex_auth, fs::Permissions::from_mode(0o600)).unwrap();
        let identity = ProjectIdentity::from_path(&project).unwrap();
        let record = crate::inventory::VmRecord {
            name: identity.vm_name,
            source: "project".into(),
            modules: vec!["claude".into(), "codex".into()],
            resources: crate::inventory::VmResources::default(),
            user: "dev".into(),
            workspace: crate::inventory::VmWorkspace {
                mode_name: "mount".into(),
                guest_path: "/home/dev/synthetic-project".into(),
                host_path: Some(project.to_string_lossy().into_owned()),
                repo: None,
                git_ref: None,
            },
            mounts: Vec::new(),
        };
        let runner = FakeRunner::with_outputs(vec![ok("claude=ready\ncodex=ready\n"), ok("")]);
        let bootstrap = SystemConfigBootstrap::new(
            runner.clone(),
            paths,
            PathBuf::from("/synthetic/bin/limactl"),
            account_home,
            MemorySecretStore::default(),
        );

        let status = bootstrap.apply(&record).unwrap();

        assert_eq!(
            status.credentials,
            BootstrapCredentialStatus {
                claude: "ready".into(),
                codex: "ready".into(),
            }
        );
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].stdin.is_none());
        let archive = calls[1].stdin.as_ref().unwrap();
        assert!(!archive
            .windows(b"SYNTHETIC_HOST_CODEX_CREDENTIAL".len())
            .any(|part| part == b"SYNTHETIC_HOST_CODEX_CREDENTIAL"));
        let entries = {
            let mut archive = tar::Archive::new(archive.as_slice());
            archive
                .entries()
                .unwrap()
                .map(|entry| {
                    entry
                        .unwrap()
                        .path()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect::<Vec<_>>()
        };
        assert!(!entries.iter().any(|path| {
            matches!(
                path.as_str(),
                ".claude/.credentials.json" | ".codex/auth.json"
            )
        }));
        fs::remove_dir_all(root).unwrap();
    }
}
