use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

pub mod manifest;
pub mod protocol;
pub mod supervisor;

use manifest::{LoadError, PluginPackage};
use protocol::{EventQueue, PluginEvent, RegisterRequest};
use supervisor::{
    Lifecycle, ManagedChild, ProcessSpawner, RegistrationError, Runtime, SpawnSpec, SystemSpawner,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostEffect {
    MarkOwnerStale(String),
    Changed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostRegistrationError {
    NotFound,
    Runtime(RegistrationError),
}

impl HostRegistrationError {
    pub fn code(&self) -> &'static str {
        match self {
            HostRegistrationError::NotFound => "plugin_not_found",
            HostRegistrationError::Runtime(err) => err.code(),
        }
    }
}

impl std::fmt::Display for HostRegistrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostRegistrationError::NotFound => f.write_str("плагин не найден"),
            HostRegistrationError::Runtime(err) => err.fmt(f),
        }
    }
}

struct PluginSlot {
    package: PluginPackage,
    enabled: bool,
    runtime: Runtime,
    child: Option<Box<dyn ManagedChild>>,
    events: EventQueue,
}

impl PluginSlot {
    fn stop_child(&mut self) -> Result<(), String> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        if let Err(err) = child.kill() {
            self.child = Some(child);
            return Err(err);
        }
        Ok(())
    }
}

pub struct PluginHost {
    roots: Vec<PathBuf>,
    slots: Mutex<BTreeMap<String, PluginSlot>>,
    discovery_errors: Mutex<Vec<LoadError>>,
    spawner: Arc<dyn ProcessSpawner>,
    next_event_seq: AtomicU64,
}

impl PluginHost {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self::with_spawner(roots, Arc::new(SystemSpawner))
    }

    fn with_spawner(roots: Vec<PathBuf>, spawner: Arc<dyn ProcessSpawner>) -> Self {
        Self {
            roots,
            slots: Mutex::new(BTreeMap::new()),
            discovery_errors: Mutex::new(Vec::new()),
            spawner,
            next_event_seq: AtomicU64::new(0),
        }
    }

    pub fn discover(&self) {
        let found = manifest::discover(&self.roots);
        let mut slots = self.slots.lock().unwrap();
        if slots.values().any(|slot| slot.child.is_some()) {
            crate::log::line("[plugins] discovery skipped while processes are running");
            return;
        }
        slots.clear();
        for package in found.packages {
            slots.insert(
                package.manifest.id.clone(),
                PluginSlot {
                    package,
                    enabled: false,
                    runtime: Runtime::default(),
                    child: None,
                    events: EventQueue::default(),
                },
            );
        }
        *self.discovery_errors.lock().unwrap() = found.errors;
    }

    pub fn contains(&self, id: &str) -> bool {
        self.slots.lock().unwrap().contains_key(id)
    }

    pub fn register(
        &self,
        id: &str,
        request: &RegisterRequest,
        now_ms: i64,
    ) -> Result<(), HostRegistrationError> {
        let mut slots = self.slots.lock().unwrap();
        let slot = slots.get_mut(id).ok_or(HostRegistrationError::NotFound)?;
        slot.runtime
            .register(request, now_ms)
            .map_err(HostRegistrationError::Runtime)
    }

    pub fn enqueue_command(&self, id: &str, name: &str, args: Value) -> Result<Value, String> {
        if name.trim().is_empty() {
            return Err("plugin command name обязателен".into());
        }
        let mut slots = self.slots.lock().unwrap();
        let slot = slots
            .get_mut(id)
            .ok_or_else(|| "плагин не найден".to_string())?;
        if slot.runtime.lifecycle != Lifecycle::Running {
            return Err(format!(
                "плагин не готов: {}",
                slot.runtime.lifecycle.as_str()
            ));
        }
        let seq = self.next_event_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let request_id = format!("{id}-{seq}");
        slot.events.push(
            seq,
            "command",
            json!({
                "requestId": request_id,
                "name": name,
                "args": args,
            }),
        )?;
        Ok(json!({
            "ok": true,
            "accepted": true,
            "requestId": request_id,
        }))
    }

    pub fn events_after(
        &self,
        id: &str,
        after: u64,
        limit: usize,
    ) -> Result<Vec<PluginEvent>, String> {
        let slots = self.slots.lock().unwrap();
        let slot = slots
            .get(id)
            .ok_or_else(|| "плагин не найден".to_string())?;
        Ok(slot.events.read_after(after, limit))
    }

    pub fn restart(&self, id: &str) -> Result<Vec<HostEffect>, String> {
        let mut slots = self.slots.lock().unwrap();
        let slot = slots
            .get_mut(id)
            .ok_or_else(|| "плагин не найден".to_string())?;
        if !slot.enabled {
            return Err("плагин выключен".into());
        }
        slot.stop_child()?;
        slot.runtime.disable();
        slot.enabled = true;
        Ok(vec![
            HostEffect::MarkOwnerStale(format!("plugin:{id}")),
            HostEffect::Changed,
        ])
    }

    fn dispose_with(&self) -> Vec<HostEffect> {
        let mut effects = Vec::new();
        let mut slots = self.slots.lock().unwrap();
        for (id, slot) in slots.iter_mut() {
            let was_active = slot.child.is_some() || slot.runtime.lifecycle != Lifecycle::Stopped;
            match slot.stop_child() {
                Ok(()) => slot.runtime.disable(),
                Err(err) => {
                    crate::log::line(&format!("[plugin:{id}] dispose stop failed: {err}"));
                    slot.runtime.last_error = Some(format!("dispose stop failed: {err}"));
                }
            }
            if was_active {
                effects.push(HostEffect::MarkOwnerStale(format!("plugin:{id}")));
                effects.push(HostEffect::Changed);
            }
        }
        effects
    }

    pub fn statuses(&self, now_ms: i64) -> Value {
        let slots = self.slots.lock().unwrap();
        let mut out = slots
            .values()
            .map(|slot| {
                let retry_in_ms = slot
                    .runtime
                    .retry_at_ms
                    .map(|retry_at| (retry_at - now_ms).max(0));
                json!({
                    "id": slot.package.manifest.id,
                    "name": slot.package.manifest.name,
                    "version": slot.package.manifest.version,
                    "external": true,
                    "enabled": slot.enabled,
                    "projectRuntimes": slot.package.manifest.project_runtimes,
                    "status": {
                        "state": slot.runtime.lifecycle.as_str(),
                        "pid": slot.runtime.pid,
                        "protocolVersion": slot.package.manifest.protocol_version,
                        "retryInMs": retry_in_ms,
                        "error": slot.runtime.last_error,
                    }
                })
            })
            .collect::<Vec<_>>();
        drop(slots);

        for (index, err) in self.discovery_errors.lock().unwrap().iter().enumerate() {
            let safe_key = err
                .key
                .chars()
                .map(|ch| {
                    if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' {
                        ch
                    } else {
                        '-'
                    }
                })
                .collect::<String>();
            out.push(json!({
                "id": format!("invalid-{safe_key}-{index}"),
                "name": err.key,
                "external": true,
                "enabled": false,
                "status": {
                    "state": if err.incompatible { "incompatible" } else { "error" },
                    "pid": Value::Null,
                    "protocolVersion": manifest::PROTOCOL_VERSION,
                    "retryInMs": Value::Null,
                    "error": err.message,
                }
            }));
        }
        Value::Array(out)
    }

    fn tick_with(
        &self,
        now_ms: i64,
        is_enabled: &dyn Fn(&str) -> bool,
        tokens: &crate::capability::tokens::TokenStore,
        socket: &std::path::Path,
    ) -> Vec<HostEffect> {
        let mut effects = Vec::new();
        let mut slots = self.slots.lock().unwrap();

        for (id, slot) in slots.iter_mut() {
            let desired = is_enabled(id);
            if !desired {
                let was_active = slot.enabled
                    || slot.child.is_some()
                    || slot.runtime.lifecycle != Lifecycle::Stopped;
                let stop_result = slot.stop_child();
                if let Err(err) = tokens.revoke_plugin(id) {
                    crate::log::line(&format!("[plugin:{id}] token revoke failed: {err}"));
                }
                slot.enabled = false;
                match stop_result {
                    Ok(()) => slot.runtime.disable(),
                    Err(err) => {
                        crate::log::line(&format!("[plugin:{id}] stop failed: {err}"));
                        slot.runtime.last_error = Some(format!("stop failed: {err}"));
                    }
                }
                if was_active {
                    effects.push(HostEffect::MarkOwnerStale(format!("plugin:{id}")));
                    effects.push(HostEffect::Changed);
                }
                continue;
            }

            if !slot.enabled {
                slot.enabled = true;
                effects.push(HostEffect::Changed);
            }

            if slot.runtime.lifecycle == Lifecycle::Incompatible && slot.child.is_some() {
                match slot.stop_child() {
                    Ok(()) => {
                        slot.runtime.pid = None;
                        slot.runtime.started_at_ms = None;
                        slot.runtime.handshake_deadline_ms = None;
                    }
                    Err(err) => {
                        crate::log::line(&format!("[plugin:{id}] incompatible stop failed: {err}"));
                        slot.runtime.last_error =
                            Some(format!("incompatible plugin stop failed: {err}"));
                    }
                }
                effects.push(HostEffect::MarkOwnerStale(format!("plugin:{id}")));
                effects.push(HostEffect::Changed);
                continue;
            }

            let observation = slot.child.as_mut().map(|child| child.try_wait());
            match observation {
                Some(Ok(Some(code))) => {
                    slot.child.take();
                    slot.runtime
                        .on_failure(now_ms, format!("plugin process exited with code {code}"));
                    effects.push(HostEffect::MarkOwnerStale(format!("plugin:{id}")));
                    effects.push(HostEffect::Changed);
                    continue;
                }
                Some(Err(err)) => {
                    match slot.stop_child() {
                        Ok(()) => {
                            slot.runtime.on_failure(now_ms, err);
                        }
                        Err(stop_err) => {
                            slot.runtime.last_error =
                                Some(format!("{err}; process stop failed: {stop_err}"));
                        }
                    }
                    effects.push(HostEffect::MarkOwnerStale(format!("plugin:{id}")));
                    effects.push(HostEffect::Changed);
                    continue;
                }
                Some(Ok(None)) if slot.runtime.handshake_timed_out(now_ms) => {
                    match slot.stop_child() {
                        Ok(()) => {
                            slot.runtime.on_failure(now_ms, "handshake timeout");
                        }
                        Err(err) => {
                            crate::log::line(&format!(
                                "[plugin:{id}] handshake timeout stop failed: {err}"
                            ));
                            slot.runtime.last_error =
                                Some(format!("handshake timeout; process stop failed: {err}"));
                        }
                    }
                    effects.push(HostEffect::MarkOwnerStale(format!("plugin:{id}")));
                    effects.push(HostEffect::Changed);
                    continue;
                }
                Some(Ok(None)) => continue,
                None => {}
            }

            let should_spawn = match slot.runtime.lifecycle {
                Lifecycle::Stopped | Lifecycle::Error => true,
                Lifecycle::Backoff => slot.runtime.retry_due(now_ms),
                Lifecycle::Starting | Lifecycle::Running => {
                    slot.runtime
                        .on_failure(now_ms, "plugin process handle lost");
                    effects.push(HostEffect::MarkOwnerStale(format!("plugin:{id}")));
                    effects.push(HostEffect::Changed);
                    false
                }
                Lifecycle::Incompatible => false,
            };
            if !should_spawn {
                continue;
            }

            let token = match tokens.ensure_plugin_token(id, &slot.package.manifest.capabilities) {
                Ok(token) => token,
                Err(err) => {
                    slot.runtime
                        .on_failure(now_ms, format!("token issue failed: {err}"));
                    effects.push(HostEffect::Changed);
                    continue;
                }
            };
            let spec = SpawnSpec {
                plugin_id: id.clone(),
                executable: slot.package.executable.clone(),
                args: slot.package.manifest.entry.args.clone(),
                cwd: slot.package.root.clone(),
                socket: socket.to_path_buf(),
                token,
                protocol_version: manifest::PROTOCOL_VERSION,
            };
            match self.spawner.spawn(&spec) {
                Ok(child) => {
                    let pid = child.id();
                    slot.runtime.on_spawned(pid, now_ms);
                    slot.child = Some(child);
                    effects.push(HostEffect::Changed);
                }
                Err(err) => {
                    slot.runtime.on_failure(now_ms, err);
                    effects.push(HostEffect::Changed);
                }
            }
        }
        effects
    }
}

fn roots_from_sources(
    settings: &Value,
    env_override: Option<&str>,
    installed: PathBuf,
) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut push = |raw: &str| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        let path = PathBuf::from(trimmed);
        if !roots.contains(&path) {
            roots.push(path);
        }
    };
    if let Some(raw) = env_override {
        push(raw);
    }
    if let Some(raw) = settings.get("pluginsDevDir").and_then(Value::as_str) {
        push(raw);
    }
    if !roots.contains(&installed) {
        roots.push(installed);
    }
    roots
}

pub fn roots_from_settings(settings: &Value) -> Vec<PathBuf> {
    let env_override = std::env::var("JARVIS_PLUGIN_DEV_DIR").ok();
    roots_from_sources(
        settings,
        env_override.as_deref(),
        crate::util::jarvis_dir().join("plugins"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::tokens::TokenStore;
    use serde_json::json;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    #[test]
    fn roots_prefer_env_then_settings_then_installed_and_dedupe() {
        let roots = roots_from_sources(
            &json!({ "pluginsDevDir": "/settings/plugins" }),
            Some("/env/plugins"),
            PathBuf::from("/installed/plugins"),
        );
        assert_eq!(
            roots,
            [
                PathBuf::from("/env/plugins"),
                PathBuf::from("/settings/plugins"),
                PathBuf::from("/installed/plugins"),
            ]
        );

        let deduped = roots_from_sources(
            &json!({ "pluginsDevDir": "/env/plugins" }),
            Some("/env/plugins"),
            PathBuf::from("/installed/plugins"),
        );
        assert_eq!(
            deduped,
            [
                PathBuf::from("/env/plugins"),
                PathBuf::from("/installed/plugins"),
            ]
        );
    }

    #[test]
    fn roots_ignore_blank_or_non_string_dev_values() {
        for settings in [
            json!({ "pluginsDevDir": "" }),
            json!({ "pluginsDevDir": 42 }),
            json!({}),
        ] {
            assert_eq!(
                roots_from_sources(&settings, Some("  "), PathBuf::from("/installed/plugins"),),
                [PathBuf::from("/installed/plugins")]
            );
        }
    }

    #[derive(Default)]
    struct FakeProcessState {
        exit: Option<i32>,
        wait_error: bool,
        killed: bool,
        kill_error: bool,
        kill_attempts: usize,
    }

    struct FakeChild {
        pid: u32,
        state: Arc<Mutex<FakeProcessState>>,
    }

    impl ManagedChild for FakeChild {
        fn id(&self) -> u32 {
            self.pid
        }

        fn try_wait(&mut self) -> Result<Option<i32>, String> {
            let state = self.state.lock().unwrap();
            if state.wait_error {
                return Err("fixture wait failed".into());
            }
            Ok(state.exit)
        }

        fn kill(&mut self) -> Result<(), String> {
            let mut state = self.state.lock().unwrap();
            state.kill_attempts += 1;
            if state.kill_error {
                return Err("fixture kill failed".into());
            }
            state.killed = true;
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FakeSpawner {
        pid: u32,
        state: Arc<Mutex<FakeProcessState>>,
        specs: Arc<Mutex<Vec<SpawnSpec>>>,
    }

    impl FakeSpawner {
        fn new(pid: u32) -> Self {
            Self {
                pid,
                state: Arc::new(Mutex::new(FakeProcessState::default())),
                specs: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl ProcessSpawner for FakeSpawner {
        fn spawn(&self, spec: &SpawnSpec) -> Result<Box<dyn ManagedChild>, String> {
            self.specs.lock().unwrap().push(spec.clone());
            Ok(Box::new(FakeChild {
                pid: self.pid,
                state: self.state.clone(),
            }))
        }
    }

    fn temp_plugin_root(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "jarvis-plugin-host-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let package = root.join("agent-vm");
        fs::create_dir_all(&package).unwrap();
        let executable = package.join("plugin");
        fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            package.join("manifest.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "agent-vm",
                "name": "Agent VM",
                "version": "0.1.0",
                "protocolVersion": manifest::PROTOCOL_VERSION,
                "entry": {
                    "type": "binary",
                    "path": "plugin",
                    "args": ["--serve"]
                },
                "capabilities": ["read", "control"],
                "projectRuntimes": []
            }))
            .unwrap(),
        )
        .unwrap();
        root
    }

    fn token_store(root: &Path) -> TokenStore {
        TokenStore::at(root.join("tokens.json"))
    }

    #[test]
    fn enabled_discovered_plugin_spawns_with_expected_identity_env() {
        let root = temp_plugin_root("spawn");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");

        host.tick_with(1_000, &|id| id == "agent-vm", &tokens, &socket);

        let specs = fake.specs.lock().unwrap();
        assert_eq!(specs.len(), 1);
        let spec = &specs[0];
        assert_eq!(spec.plugin_id, "agent-vm");
        assert_eq!(spec.args, ["--serve"]);
        assert_eq!(spec.cwd, root.join("agent-vm").canonicalize().unwrap());
        assert_eq!(spec.socket, socket);
        assert_eq!(spec.protocol_version, manifest::PROTOCOL_VERSION);
        assert_eq!(spec.token.len(), 64);
        let statuses = host.statuses(1_000);
        assert_eq!(statuses[0]["status"]["state"], "starting");
        assert_eq!(statuses[0]["status"]["pid"], 4242);
        assert!(
            !statuses.to_string().contains(&spec.token),
            "token не попадает в status"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn child_exit_marks_owner_stale_and_enters_backoff() {
        let root = temp_plugin_root("crash");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");
        host.tick_with(1_000, &|_| true, &tokens, &socket);
        fake.state.lock().unwrap().exit = Some(1);

        let effects = host.tick_with(2_000, &|_| true, &tokens, &socket);

        assert!(effects.contains(&HostEffect::MarkOwnerStale("plugin:agent-vm".into())));
        let statuses = host.statuses(2_000);
        assert_eq!(statuses[0]["status"]["state"], "backoff");
        assert_eq!(statuses[0]["status"]["retryInMs"], 1_000);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disabled_plugin_is_killed_and_token_revoked() {
        let root = temp_plugin_root("disable");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");
        host.tick_with(1_000, &|_| true, &tokens, &socket);
        let token = fake.specs.lock().unwrap()[0].token.clone();

        let effects = host.tick_with(2_000, &|_| false, &tokens, &socket);

        assert!(fake.state.lock().unwrap().killed);
        assert!(tokens.resolve(&token).is_none());
        assert!(effects.contains(&HostEffect::MarkOwnerStale("plugin:agent-vm".into())));
        assert_eq!(host.statuses(2_000)[0]["status"]["state"], "stopped");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_disable_kill_keeps_child_until_a_later_tick_can_stop_it() {
        let root = temp_plugin_root("disable-kill-failure");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");
        host.tick_with(1_000, &|_| true, &tokens, &socket);
        let token = fake.specs.lock().unwrap()[0].token.clone();
        fake.state.lock().unwrap().kill_error = true;

        host.tick_with(2_000, &|_| false, &tokens, &socket);

        assert!(tokens.resolve(&token).is_none(), "token отозван сразу");
        assert_eq!(fake.specs.lock().unwrap().len(), 1);
        assert_eq!(fake.state.lock().unwrap().kill_attempts, 1);
        assert_eq!(
            host.statuses(2_000)[0]["status"]["state"],
            "starting",
            "живой child остаётся под supervision"
        );

        fake.state.lock().unwrap().kill_error = false;
        host.tick_with(3_000, &|_| false, &tokens, &socket);

        let state = fake.state.lock().unwrap();
        assert!(state.killed);
        assert_eq!(state.kill_attempts, 2);
        drop(state);
        assert_eq!(fake.specs.lock().unwrap().len(), 1);
        assert_eq!(host.statuses(3_000)[0]["status"]["state"], "stopped");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tick_does_not_spawn_before_retry_deadline() {
        let root = temp_plugin_root("retry");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");
        host.tick_with(1_000, &|_| true, &tokens, &socket);
        fake.state.lock().unwrap().exit = Some(1);
        host.tick_with(2_000, &|_| true, &tokens, &socket);
        fake.state.lock().unwrap().exit = None;

        host.tick_with(2_999, &|_| true, &tokens, &socket);
        assert_eq!(fake.specs.lock().unwrap().len(), 1);
        host.tick_with(3_000, &|_| true, &tokens, &socket);
        assert_eq!(fake.specs.lock().unwrap().len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn register_and_command_event_use_live_runtime_and_replayable_queue() {
        let root = temp_plugin_root("register-command");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake));
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");
        host.tick_with(1_000, &|_| true, &tokens, &socket);

        host.register(
            "agent-vm",
            &protocol::RegisterRequest {
                protocol_version: manifest::PROTOCOL_VERSION,
                pid: 4242,
            },
            1_100,
        )
        .unwrap();
        let accepted = host
            .enqueue_command("agent-vm", "runtime.ensure", json!({"projectId": "sup"}))
            .unwrap();
        let events = host.events_after("agent-vm", 0, 64).unwrap();

        assert_eq!(host.statuses(1_100)[0]["status"]["state"], "running");
        assert_eq!(accepted["ok"], true);
        assert_eq!(accepted["accepted"], true);
        assert_eq!(accepted["requestId"], "agent-vm-1");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[0].kind, "command");
        assert_eq!(events[0].payload["requestId"], "agent-vm-1");
        assert_eq!(events[0].payload["name"], "runtime.ensure");
        assert_eq!(events[0].payload["args"]["projectId"], "sup");
        assert!(host.events_after("agent-vm", 1, 64).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restart_kills_current_process_and_allows_immediate_spawn() {
        let root = temp_plugin_root("restart");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");
        host.tick_with(1_000, &|_| true, &tokens, &socket);

        let effects = host.restart("agent-vm").unwrap();
        assert!(fake.state.lock().unwrap().killed);
        assert!(effects.contains(&HostEffect::MarkOwnerStale("plugin:agent-vm".into())));
        host.tick_with(2_000, &|_| true, &tokens, &socket);

        assert_eq!(fake.specs.lock().unwrap().len(), 2);
        assert_eq!(host.statuses(2_000)[0]["status"]["state"], "starting");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_restart_kill_keeps_process_supervised_and_does_not_double_spawn() {
        let root = temp_plugin_root("restart-kill-failure");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");
        host.tick_with(1_000, &|_| true, &tokens, &socket);
        fake.state.lock().unwrap().kill_error = true;

        let err = host.restart("agent-vm").unwrap_err();
        host.tick_with(2_000, &|_| true, &tokens, &socket);

        assert!(err.contains("fixture kill failed"));
        assert_eq!(fake.specs.lock().unwrap().len(), 1);
        assert_eq!(
            host.statuses(2_000)[0]["status"]["state"],
            "starting",
            "старый child handle остаётся под supervision"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn handshake_timeout_kills_child_and_enters_backoff() {
        let root = temp_plugin_root("handshake-timeout");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");
        host.tick_with(1_000, &|_| true, &tokens, &socket);

        let effects = host.tick_with(
            1_000 + supervisor::HANDSHAKE_TIMEOUT_MS,
            &|_| true,
            &tokens,
            &socket,
        );

        assert!(fake.state.lock().unwrap().killed);
        assert!(effects.contains(&HostEffect::MarkOwnerStale("plugin:agent-vm".into())));
        assert_eq!(
            host.statuses(1_000 + supervisor::HANDSHAKE_TIMEOUT_MS)[0]["status"]["state"],
            "backoff"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn handshake_timeout_does_not_drop_child_when_kill_fails() {
        let root = temp_plugin_root("handshake-timeout-kill-failure");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");
        host.tick_with(1_000, &|_| true, &tokens, &socket);
        fake.state.lock().unwrap().kill_error = true;
        let deadline = 1_000 + supervisor::HANDSHAKE_TIMEOUT_MS;

        host.tick_with(deadline, &|_| true, &tokens, &socket);

        assert_eq!(fake.specs.lock().unwrap().len(), 1);
        assert_eq!(host.statuses(deadline)[0]["status"]["state"], "starting");
        assert_eq!(fake.state.lock().unwrap().kill_attempts, 1);

        fake.state.lock().unwrap().kill_error = false;
        host.tick_with(deadline + 1, &|_| true, &tokens, &socket);

        assert_eq!(fake.state.lock().unwrap().kill_attempts, 2);
        assert_eq!(host.statuses(deadline + 1)[0]["status"]["state"], "backoff");
        assert_eq!(fake.specs.lock().unwrap().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn observation_and_kill_errors_do_not_lose_the_supervised_child() {
        let root = temp_plugin_root("observe-kill-failure");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");
        host.tick_with(1_000, &|_| true, &tokens, &socket);
        {
            let mut state = fake.state.lock().unwrap();
            state.wait_error = true;
            state.kill_error = true;
        }

        host.tick_with(2_000, &|_| true, &tokens, &socket);

        assert_eq!(fake.specs.lock().unwrap().len(), 1);
        assert_eq!(host.statuses(2_000)[0]["status"]["state"], "starting");

        {
            let mut state = fake.state.lock().unwrap();
            state.wait_error = false;
            state.kill_error = false;
        }
        host.tick_with(3_000, &|_| true, &tokens, &socket);

        assert_eq!(fake.specs.lock().unwrap().len(), 1);
        assert_eq!(host.statuses(3_000)[0]["status"]["state"], "starting");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dispose_stops_children_and_marks_stale_without_revoking_tokens() {
        let root = temp_plugin_root("dispose");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");
        host.tick_with(1_000, &|_| true, &tokens, &socket);
        let token = fake.specs.lock().unwrap()[0].token.clone();

        let effects = host.dispose_with();

        assert!(fake.state.lock().unwrap().killed);
        assert!(
            tokens.resolve(&token).is_some(),
            "shutdown не отзывает token"
        );
        assert!(effects.contains(&HostEffect::MarkOwnerStale("plugin:agent-vm".into())));
        let status = host.statuses(2_000);
        assert_eq!(status[0]["enabled"], true, "настройка enable сохраняется");
        assert_eq!(status[0]["status"]["state"], "stopped");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn incompatible_manifest_is_visible_but_never_spawnable() {
        let root = temp_plugin_root("incompatible");
        let path = root.join("agent-vm/manifest.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest["protocolVersion"] = json!(manifest::PROTOCOL_VERSION + 1);
        fs::write(&path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));

        host.discover();

        let statuses = host.statuses(1_000);
        assert_eq!(statuses[0]["status"]["state"], "incompatible");
        assert!(!host.contains("agent-vm"));
        assert!(fake.specs.lock().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
