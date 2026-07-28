use std::collections::BTreeMap;
use std::ffi::CStr;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use jarvis_secret_store::{migrate_legacy_claude_secret, MacKeychainStore, SecretStore};
use serde::Serialize;
use zeroize::Zeroize;

use crate::config_mirror::{build_snapshot, MirrorRoots};
use crate::guest_bootstrap::{
    bootstrap_spec, build_bundle, load_codex_credential, run_bootstrap, BootstrapCredentialStatus,
};
use crate::inventory::{load_records, parse_lima_instances, reconcile, InventoryVm};
use crate::project::{ensure_project_link, is_valid_vm_name, ProjectIdentity};
use crate::runner::{CommandRunner, CommandSpec};
use crate::runtime_paths::RuntimePaths;

const PROJECT_SPEC: &str = "\
# Jarvis Agent VM project runtime\n\
modules:\n\
  - node\n\
  - claude\n\
  - codex\n";

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

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapStatus {
    pub fingerprint: String,
    pub files: usize,
    pub skipped: usize,
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
        let migration = migrate_legacy_claude_secret(
            &self.paths.jarvis_dir.join("settings.json"),
            &self.secrets,
        )?;
        let snapshot = build_snapshot(&MirrorRoots {
            claude: self.account_home.join(".claude"),
            codex: self.account_home.join(".codex"),
        })?;
        let codex_credential = load_codex_credential(&snapshot, &self.account_home.join(".codex"))?;
        let mut private_env =
            read_private_runtime_env(&self.paths.jarvis_dir.join("settings.json"))?;
        let proxy_configured = private_env.contains_key("HTTPS_PROXY");
        let bundle = build_bundle(
            &snapshot,
            &self.secrets,
            migration.kind,
            &codex_credential,
            &private_env,
        );
        for value in private_env.values_mut() {
            value.zeroize();
        }
        let bundle = bundle?;
        let credentials = bundle.credential_status.clone();
        let spec = bootstrap_spec(&self.limactl, &self.paths.command_env(), record, bundle)?;
        run_bootstrap(&self.runner, &spec)?;
        Ok(BootstrapStatus {
            fingerprint: snapshot.fingerprint,
            files: snapshot.files.len(),
            skipped: snapshot.diagnostics.skipped_symlinks
                + snapshot.diagnostics.skipped_non_regular
                + snapshot.diagnostics.skipped_oversize,
            credentials,
            proxy_configured,
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
        Some(("orphaned", _)) => EnsureAction::Recreate,
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
}

impl<R: CommandRunner> AgentVmService<R> {
    pub fn new(runner: R, paths: RuntimePaths, tools: Toolchain) -> Self {
        Self {
            runner,
            paths,
            tools,
            config_bootstrap: None,
        }
    }

    pub fn with_system_bootstrap(
        runner: R,
        paths: RuntimePaths,
        tools: Toolchain,
    ) -> Result<Self, String> {
        let home = account_home()
            .ok_or_else(|| "не определить account home для config mirror".to_string())?;
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
        })
    }

    pub fn inventory(&self) -> Result<Vec<InventoryVm>, String> {
        let result = self
            .runner
            .run(&CommandSpec {
                program: self.tools.limactl.clone(),
                args: vec![
                    "list".into(),
                    "--format".into(),
                    "{{.Name}}\t{{.Status}}".into(),
                ],
                cwd: None,
                env: self.paths.command_env(),
                stdin: None,
            })?
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
        self.paths.create_private_dirs()?;
        let project = ProjectIdentity::from_path(cwd)?;
        let binding = ensure_project_link(&self.paths.project_links, &project)?;
        let before = self.inventory()?;
        let current = find_project_vm(&before, &project);
        let action = plan_ensure(current.map(|vm| (vm.management.as_str(), vm.state.as_str())));
        let mut created_spec = false;
        let inventory = match action {
            EnsureAction::Create => {
                created_spec = ensure_project_spec(&project.canonical_path)?;
                self.run_avm(
                    "create",
                    vec!["create".into(), binding.to_string_lossy().into_owned()],
                )?;
                self.inventory()?
            }
            EnsureAction::Recreate => {
                let name = current
                    .map(|vm| vm.name.as_str())
                    .unwrap_or(&project.vm_name);
                self.run_lifecycle("recreate", name)?;
                self.inventory()?
            }
            EnsureAction::Start => {
                let name = current
                    .map(|vm| vm.name.as_str())
                    .unwrap_or(&project.vm_name);
                self.run_lifecycle("start", name)?;
                self.inventory()?
            }
            EnsureAction::Ready | EnsureAction::Wait => before,
        };
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
        let project = ProjectIdentity::from_path(cwd)?;
        let before = self.inventory()?;
        let vm = find_project_vm(&before, &project)
            .ok_or_else(|| "для project не найдена managed Agent VM".to_string())?;
        if vm.management != "managed" {
            return Err(format!("VM {} имеет состояние {}", vm.name, vm.management));
        }
        self.run_lifecycle(action, &vm.name)?;
        let snapshot = self.snapshot(&project, self.inventory()?, false)?;
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
            shell_command: format!("avm shell {vm_name}"),
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
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::runner::{CommandResult, CommandRunner, CommandSpec};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    #[derive(Clone, Default)]
    struct FakeRunner {
        calls: Arc<Mutex<Vec<CommandSpec>>>,
        outputs: Arc<Mutex<VecDeque<CommandResult>>>,
    }

    impl FakeRunner {
        fn with_outputs(outputs: Vec<CommandResult>) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                outputs: Arc::new(Mutex::new(outputs.into())),
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
    fn ensure_plan_never_recreates_a_stopped_or_running_vm() {
        assert_eq!(plan_ensure(None), EnsureAction::Create);
        assert_eq!(
            plan_ensure(Some(("orphaned", "missing"))),
            EnsureAction::Recreate
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
    fn ensure_starts_stopped_vm_with_sanitized_argv_and_returns_shell_command() {
        let (root, project, paths) = fixture("start");
        let identity = ProjectIdentity::from_path(&project).unwrap();
        let binding = ensure_project_link(&paths.project_links, &identity).unwrap();
        write_record(&paths, &identity.vm_name, &binding);
        let runner = FakeRunner::with_outputs(vec![
            ok(&format!("{}\tStopped\n", identity.vm_name)),
            ok("start: synthetic\n"),
            ok(&format!("{}\tRunning\n", identity.vm_name)),
        ]);
        let api = service(runner.clone(), paths);

        let snapshot = api.ensure(&project).unwrap();

        assert_eq!(snapshot.vm.unwrap().state, "running");
        assert_eq!(
            snapshot.shell_command,
            format!("avm shell {}", identity.vm_name)
        );
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
        };
        let runner = FakeRunner::with_outputs(vec![ok("")]);
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
        assert_eq!(calls.len(), 1);
        let visible = format!("{:?}{:?}", calls[0].args, calls[0].env);
        assert!(!visible.contains("SYNTHETIC_PRIVATE_VALUE"));
        assert!(calls[0]
            .stdin
            .as_ref()
            .unwrap()
            .windows(b"SYNTHETIC_PRIVATE_VALUE".len())
            .any(|part| part == b"SYNTHETIC_PRIVATE_VALUE"));
        fs::remove_dir_all(root).unwrap();
    }
}
