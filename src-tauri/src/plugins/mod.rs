use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

pub mod install;
pub mod manifest;
pub mod manifest_v2;
pub mod package;
#[allow(dead_code)] // A5 storage primitives are wired into manager transactions in A6.
pub mod package_manager;
pub mod protocol;
pub mod supervisor;
pub mod trust;

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

    fn reset_events(&mut self) {
        self.events = EventQueue::default();
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
        for error in &found.errors {
            crate::log::line(&format!(
                "[plugins] invalid package {}: {}",
                error.path.display(),
                error.message
            ));
        }
        *self.discovery_errors.lock().unwrap() = found.errors;
    }

    pub fn init(&self, d: &Arc<crate::daemon::Daemon>) {
        self.discover();
        emit_statuses(d);
    }

    pub fn tick(&self, d: &Arc<crate::daemon::Daemon>) {
        let settings = d.settings.load();
        let effects = self.tick_with(
            crate::util::now_ms(),
            &|id| enabled_from_settings(&settings, id),
            &d.tokens,
            &crate::util::sock_path(),
        );
        apply_host_effects(d, effects, true);
    }

    pub fn dispose(&self, d: &Arc<crate::daemon::Daemon>) {
        apply_host_effects(d, self.dispose_with(), false);
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

    pub async fn poll_events(
        &self,
        id: &str,
        after: u64,
        limit: usize,
        wait_ms: u64,
    ) -> Result<Vec<PluginEvent>, String> {
        let limit = limit.clamp(1, protocol::MAX_POLL_EVENTS);
        let wait_ms = wait_ms.min(protocol::MAX_WAIT_MS);
        let mut notified = {
            let slots = self.slots.lock().unwrap();
            let slot = slots
                .get(id)
                .ok_or_else(|| "плагин не найден".to_string())?;
            let mut notified = Box::pin(slot.events.notifier().notified_owned());
            notified.as_mut().enable();
            let events = slot.events.read_after(after, limit);
            if !events.is_empty() || wait_ms == 0 {
                return Ok(events);
            }
            notified
        };

        let _ =
            tokio::time::timeout(std::time::Duration::from_millis(wait_ms), &mut notified).await;
        self.events_after(id, after, limit)
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
        slot.reset_events();
        slot.runtime.disable();
        slot.enabled = true;
        Ok(vec![
            HostEffect::MarkOwnerStale(format!("plugin:{id}")),
            HostEffect::Changed,
        ])
    }

    pub fn command(
        &self,
        d: &Arc<crate::daemon::Daemon>,
        id: &str,
        name: &str,
        args: Value,
    ) -> Value {
        if !self.contains(id) {
            return json!({ "ok": false, "error": "плагин не найден" });
        }
        if name == "_enable" {
            let on = args.get("on").and_then(Value::as_bool).unwrap_or(false);
            let mut patch = serde_json::Map::new();
            patch.insert("enabled".into(), Value::Bool(on));
            d.settings.set_plugin(id, patch);
            self.tick(d);
            return json!({ "ok": true });
        }
        if name == "_restart" {
            return match self.restart(id) {
                Ok(effects) => {
                    apply_host_effects(d, effects, true);
                    self.tick(d);
                    json!({ "ok": true })
                }
                Err(error) => json!({ "ok": false, "error": error }),
            };
        }
        match self.enqueue_command(id, name, args) {
            Ok(value) => value,
            Err(error) => json!({ "ok": false, "error": error }),
        }
    }

    fn dispose_with(&self) -> Vec<HostEffect> {
        let mut effects = Vec::new();
        let mut slots = self.slots.lock().unwrap();
        for (id, slot) in slots.iter_mut() {
            let was_active = slot.child.is_some() || slot.runtime.lifecycle != Lifecycle::Stopped;
            match slot.stop_child() {
                Ok(()) => {
                    slot.reset_events();
                    slot.runtime.disable();
                }
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
                        "startedAt": slot.runtime.started_at_ms,
                        "registeredAt": slot.runtime.registered_at_ms,
                        "handshakeDeadline": slot.runtime.handshake_deadline_ms,
                        "retryAt": slot.runtime.retry_at_ms,
                        "restartAttempt": slot.runtime.restart_attempt,
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
                    "startedAt": Value::Null,
                    "registeredAt": Value::Null,
                    "handshakeDeadline": Value::Null,
                    "retryAt": Value::Null,
                    "restartAttempt": 0,
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
                    Ok(()) => {
                        slot.reset_events();
                        slot.runtime.disable();
                    }
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
                        slot.reset_events();
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
                    slot.reset_events();
                    slot.runtime
                        .on_failure(now_ms, format!("plugin process exited with code {code}"));
                    effects.push(HostEffect::MarkOwnerStale(format!("plugin:{id}")));
                    effects.push(HostEffect::Changed);
                    continue;
                }
                Some(Err(err)) => {
                    match slot.stop_child() {
                        Ok(()) => {
                            slot.reset_events();
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
                            slot.reset_events();
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
                    slot.reset_events();
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
                    slot.runtime.on_error(format!("token issue failed: {err}"));
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

fn enabled_from_settings(settings: &Value, id: &str) -> bool {
    settings
        .pointer(&format!("/plugins/{id}/enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(id == "agent-vm")
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

pub fn combine_status_values(builtins: Value, external: Value) -> Value {
    let mut combined = builtins.as_array().cloned().unwrap_or_default();
    combined.extend(external.as_array().cloned().unwrap_or_default());
    Value::Array(combined)
}

pub fn combined_statuses(d: &Arc<crate::daemon::Daemon>) -> Value {
    combine_status_values(
        d.power.statuses(d),
        d.plugins.statuses(crate::util::now_ms()),
    )
}

pub fn emit_statuses(d: &Arc<crate::daemon::Daemon>) {
    crate::windows::emit_to_panel(&d.app, "plugins", &combined_statuses(d));
}

fn apply_host_effects(d: &Arc<crate::daemon::Daemon>, effects: Vec<HostEffect>, emit: bool) {
    let mut changed = false;
    for effect in effects {
        match effect {
            HostEffect::MarkOwnerStale(owner) => {
                d.entities.mark_stale(&owner);
            }
            HostEffect::Changed => changed = true,
        }
    }
    if changed && emit {
        emit_statuses(d);
    }
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
    fn first_party_agent_vm_is_enabled_by_default_but_explicit_false_wins() {
        assert!(enabled_from_settings(&json!({}), "agent-vm"));
        assert!(!enabled_from_settings(&json!({}), "third-party"));
        assert!(!enabled_from_settings(
            &json!({"plugins": {"agent-vm": {"enabled": false}}}),
            "agent-vm"
        ));
        assert!(enabled_from_settings(
            &json!({"plugins": {"third-party": {"enabled": true}}}),
            "third-party"
        ));
    }

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

    #[test]
    fn combined_statuses_keep_builtins_and_append_external_plugins() {
        let combined = combine_status_values(
            json!([
                { "id": "keep-awake", "enabled": true },
                { "id": "clamshell", "enabled": false }
            ]),
            json!([
                { "id": "agent-vm", "external": true }
            ]),
        );

        let ids = combined
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["keep-awake", "clamshell", "agent-vm"]);
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
        assert_eq!(statuses[0]["status"]["startedAt"], 1_000);
        assert_eq!(statuses[0]["status"]["handshakeDeadline"], 11_000);
        assert_eq!(statuses[0]["status"]["restartAttempt"], 0);
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
        assert_eq!(statuses[0]["status"]["retryAt"], 3_000);
        assert_eq!(statuses[0]["status"]["restartAttempt"], 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn child_exit_discards_commands_before_a_fresh_sidecar_can_poll_from_zero() {
        let root = temp_plugin_root("crash-command-queue");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
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
        host.enqueue_command(
            "agent-vm",
            "runtime.send",
            json!({"message":"synthetic prompt"}),
        )
        .unwrap();
        assert_eq!(host.events_after("agent-vm", 0, 64).unwrap().len(), 1);
        fake.state.lock().unwrap().exit = Some(1);

        host.tick_with(2_000, &|_| true, &tokens, &socket);

        assert!(
            host.events_after("agent-vm", 0, 64).unwrap().is_empty(),
            "новый sidecar не должен повторно получить команду старого процесса"
        );
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
    fn token_issue_failure_is_visible_as_error_and_never_spawns() {
        let root = temp_plugin_root("token-error");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let invalid_token_path = TokenStore::at(root.clone());
        let socket = root.join("run.sock");

        host.tick_with(1_000, &|_| true, &invalid_token_path, &socket);

        assert!(fake.specs.lock().unwrap().is_empty());
        let status = host.statuses(1_000);
        assert_eq!(status[0]["status"]["state"], "error");
        assert!(status[0]["status"]["error"]
            .as_str()
            .unwrap()
            .contains("token issue failed"));
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
        assert_eq!(host.statuses(1_100)[0]["status"]["registeredAt"], 1_100);
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

    #[tokio::test]
    async fn poll_events_wakes_when_a_command_arrives_after_waiting_starts() {
        let root = temp_plugin_root("poll-wake");
        let fake = FakeSpawner::new(4242);
        let host = Arc::new(PluginHost::with_spawner(vec![root.clone()], Arc::new(fake)));
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
        let waiter = {
            let host = host.clone();
            tokio::spawn(async move { host.poll_events("agent-vm", 0, 64, 1_000).await })
        };
        tokio::task::yield_now().await;

        host.enqueue_command("agent-vm", "runtime.ensure", json!({"projectId": "sup"}))
            .unwrap();
        let events = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("long-poll разбужен")
            .unwrap()
            .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["name"], "runtime.ensure");
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

    #[tokio::test]
    async fn fake_plugin_sends_authenticated_versioned_registration_over_unix_socket() {
        use std::process::Stdio;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);
        let socket = PathBuf::from("/tmp").join(format!(
            "jarvis-fake-plugin-{}-{}.sock",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_file(&socket);
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/plugin-host/fake-plugin/fake-plugin.sh");
        let token = "fixture-plugin-token";
        let mut command = tokio::process::Command::new(&script);
        command
            .env("JARVIS_SOCKET", &socket)
            .env("JARVIS_PLUGIN_ID", "fake-plugin")
            .env("JARVIS_PLUGIN_TOKEN", token)
            .env(
                "JARVIS_PLUGIN_PROTOCOL",
                manifest::PROTOCOL_VERSION.to_string(),
            )
            .env("JARVIS_FAKE_ONESHOT", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = command.spawn().unwrap();
        let child_pid = child.id().unwrap();

        let (mut stream, _) =
            tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                .await
                .expect("fake plugin подключился к UDS")
                .unwrap();
        let mut request = Vec::new();
        let (header_end, content_length) = loop {
            let mut chunk = [0_u8; 4096];
            let read =
                tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut chunk))
                    .await
                    .expect("HTTP request прочитан")
                    .unwrap();
            assert!(read > 0, "curl не закрыл request до полного body");
            request.extend_from_slice(&chunk[..read]);
            let Some(header_end) = request.windows(4).position(|it| it == b"\r\n\r\n") else {
                continue;
            };
            let header_end = header_end + 4;
            let headers = std::str::from_utf8(&request[..header_end]).unwrap();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if request.len() >= header_end + content_length {
                break (header_end, content_length);
            }
        };

        let headers = std::str::from_utf8(&request[..header_end]).unwrap();
        assert!(headers.starts_with("POST /plugin/register HTTP/1.1\r\n"));
        assert!(headers.lines().any(|line| {
            line.split_once(':').is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("x-jarvis-token") && value.trim() == token
            })
        }));
        let body: Value =
            serde_json::from_slice(&request[header_end..header_end + content_length]).unwrap();
        assert_eq!(body["protocolVersion"], manifest::PROTOCOL_VERSION);
        assert_eq!(body["pid"], child_pid);

        let response_body = "{\"ok\":true}";
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_body}",
            response_body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        drop(stream);
        drop(listener);

        let output =
            tokio::time::timeout(std::time::Duration::from_secs(5), child.wait_with_output())
                .await
                .expect("fake plugin завершился после oneshot")
                .unwrap();
        assert!(
            output.status.success(),
            "fake plugin stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        fs::remove_file(socket).unwrap();
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
