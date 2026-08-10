use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use jarvis_plugin_protocol::manifest::{PluginId, RuntimeKind, PLUGIN_API_VERSION};
use jarvis_plugin_protocol::package::PackageTarget;
use jarvis_plugin_protocol::receipt::GrantedPermission;
use serde_json::{json, Value};

#[cfg(test)]
use jarvis_plugin_protocol::manifest::PermissionId;

pub mod developer;
pub mod install;
pub mod manifest;
pub mod manifest_v2;
pub mod package;
#[allow(dead_code)] // A5 storage primitives are wired into manager transactions in A6.
pub mod package_manager;
pub mod protocol;
pub mod resolver;
pub mod supervisor;
pub mod trust;

use manifest::LoadError;
use protocol::{EventQueue, PluginEvent, RegisterRequest};
#[cfg(test)]
use resolver::UnavailableReceiptTrust;
use resolver::{
    ActivationSource, CurrentReceiptTrust, PluginActivationResolver, PluginResolver,
    ResolutionPolicy, ResolvedPlugin,
};
use supervisor::{
    Lifecycle, ManagedChild, ProcessSpawner, RegistrationError, Runtime, SpawnExecutable,
    SpawnSpec, SystemSpawner,
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
    id: String,
    name: String,
    version: String,
    protocol_version: u32,
    project_runtimes: Vec<Value>,
    activation_source: Option<ActivationSource>,
    receipt_generation: Option<u64>,
    granted_permissions: Vec<GrantedPermission>,
    enabled: bool,
    runtime: Runtime,
    child: Option<Box<dyn ManagedChild>>,
    events: EventQueue,
}

impl PluginSlot {
    fn candidate(plugin_id: &PluginId) -> Self {
        Self {
            id: plugin_id.as_str().to_owned(),
            name: plugin_id.as_str().to_owned(),
            version: String::new(),
            protocol_version: jarvis_plugin_protocol::manifest::MANIFEST_PROCESS_PROTOCOL,
            project_runtimes: Vec::new(),
            activation_source: None,
            receipt_generation: None,
            granted_permissions: Vec::new(),
            enabled: false,
            runtime: Runtime::default(),
            child: None,
            events: EventQueue::default(),
        }
    }

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

fn revoke_runtime_token(
    slot: &mut PluginSlot,
    tokens: &crate::capability::tokens::TokenStore,
    id: &str,
    context: &str,
) {
    if let Err(error) = tokens.revoke_plugin(id) {
        crate::log::line(&format!(
            "[plugin:{id}] {context} token revoke failed: {error}"
        ));
        let previous = slot.runtime.last_error.take();
        slot.runtime.last_error = Some(match previous {
            Some(previous) if !previous.is_empty() => {
                format!("{previous}; token revoke failed: {error}")
            }
            _ => format!("{context}; token revoke failed: {error}"),
        });
    }
}

pub struct PluginHost {
    discovery: PluginDiscoveryConfig,
    slots: Mutex<BTreeMap<String, PluginSlot>>,
    manager_blocks: Mutex<BTreeSet<String>>,
    discovery_errors: Mutex<Vec<LoadError>>,
    spawner: Arc<dyn ProcessSpawner>,
    resolver: Arc<dyn PluginActivationResolver>,
    next_event_seq: AtomicU64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginDiscoveryConfig {
    roots: Vec<manifest::DiscoveryRoot>,
    policy: manifest::DiscoveryPolicy,
}

impl From<Vec<PathBuf>> for PluginDiscoveryConfig {
    fn from(roots: Vec<PathBuf>) -> Self {
        Self {
            roots: roots
                .into_iter()
                .map(manifest::DiscoveryRoot::production)
                .collect(),
            policy: manifest::DiscoveryPolicy::default(),
        }
    }
}

impl PluginHost {
    pub fn new(
        roots: impl Into<PluginDiscoveryConfig>,
        receipt_trust: Arc<dyn CurrentReceiptTrust>,
    ) -> Self {
        let resolver = Arc::new(PluginResolver::new(
            package_manager::paths::PluginPaths::new(crate::util::jarvis_dir()),
            manifest_v2::HostCompatibility::parse(env!("CARGO_PKG_VERSION"), PLUGIN_API_VERSION)
                .expect("crate version is valid semver"),
            current_package_target(),
            receipt_trust,
        ));
        Self::with_components(roots.into(), Arc::new(SystemSpawner), resolver)
    }

    pub fn with_activation_resolver(
        roots: impl Into<PluginDiscoveryConfig>,
        resolver: Arc<dyn PluginActivationResolver>,
    ) -> Self {
        Self::with_components(roots.into(), Arc::new(SystemSpawner), resolver)
    }

    #[cfg(test)]
    fn with_spawner(
        roots: impl Into<PluginDiscoveryConfig>,
        spawner: Arc<dyn ProcessSpawner>,
    ) -> Self {
        let discovery = roots.into();
        let profile = discovery
            .roots
            .iter()
            .find_map(|root| {
                (root.path.file_name().and_then(|name| name.to_str()) == Some("plugins"))
                    .then(|| root.path.parent().map(PathBuf::from))
                    .flatten()
            })
            .unwrap_or_else(|| crate::util::jarvis_dir());
        let resolver = Arc::new(PluginResolver::new(
            package_manager::paths::PluginPaths::new(profile),
            manifest_v2::HostCompatibility::parse("0.4.0", PLUGIN_API_VERSION)
                .expect("test host version is valid"),
            current_package_target(),
            Arc::new(UnavailableReceiptTrust),
        ));
        Self::with_components(discovery, spawner, resolver)
    }

    fn with_components(
        discovery: impl Into<PluginDiscoveryConfig>,
        spawner: Arc<dyn ProcessSpawner>,
        resolver: Arc<dyn PluginActivationResolver>,
    ) -> Self {
        let discovery = discovery.into();
        Self {
            discovery,
            slots: Mutex::new(BTreeMap::new()),
            manager_blocks: Mutex::new(BTreeSet::new()),
            discovery_errors: Mutex::new(Vec::new()),
            spawner,
            resolver,
            next_event_seq: AtomicU64::new(0),
        }
    }

    pub fn discover(&self) {
        let found = manifest::discover_roots(&self.discovery.roots, self.discovery.policy);
        let mut slots = self.slots.lock().unwrap();
        if slots.values().any(|slot| slot.child.is_some()) {
            crate::log::line("[plugins] discovery skipped while processes are running");
            return;
        }
        slots.clear();
        for plugin_id in self.resolver.candidate_ids() {
            slots.insert(
                plugin_id.as_str().to_owned(),
                PluginSlot::candidate(&plugin_id),
            );
        }
        let mut errors = found.errors;
        for package in found.packages {
            if package.manifest.id != "agent-vm" {
                errors.push(LoadError {
                    key: package.manifest.id.clone(),
                    path: package.root.join("manifest.json"),
                    message: "legacy v1 manifest запрещён; требуется verified v2 receipt".into(),
                    incompatible: false,
                });
                continue;
            }
            let plugin_id =
                PluginId::new(package.manifest.id.clone()).expect("validated legacy Agent VM ID");
            let slot = slots
                .entry(package.manifest.id.clone())
                .or_insert_with(|| PluginSlot::candidate(&plugin_id));
            slot.name = package.manifest.name;
            slot.version = package.manifest.version;
            slot.protocol_version = package.manifest.protocol_version;
            slot.project_runtimes = package.manifest.project_runtimes;
        }
        for error in &errors {
            slots.remove(&error.key);
            crate::log::line(&format!(
                "[plugins] invalid package {}: {}",
                error.path.display(),
                error.message
            ));
        }
        *self.discovery_errors.lock().unwrap() = errors;
    }

    pub fn init(&self, d: &Arc<crate::daemon::Daemon>) {
        self.discover();
        emit_statuses(d);
    }

    pub fn tick(&self, d: &Arc<crate::daemon::Daemon>) {
        let settings = d.settings.load();
        let effects = self.tick_with_policy(
            crate::util::now_ms(),
            &|id| enabled_from_settings(&settings, id),
            settings
                .get("pluginDeveloperMode")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            &d.tokens,
            &crate::util::sock_path(),
        );
        apply_host_effects(d, effects, true);
    }

    pub fn dispose(&self, d: &Arc<crate::daemon::Daemon>) -> Result<(), String> {
        let (effects, errors) = self.dispose_attempt(&d.tokens);
        apply_host_effects(d, effects, false);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    pub fn contains(&self, id: &str) -> bool {
        self.slots.lock().unwrap().contains_key(id)
    }

    /// Synchronous manager boundary: the child is stopped and its socket
    /// identity is revoked before the slot is marked disabled. Any failed step
    /// leaves an observable live state so receipt/settings mutation can abort.
    pub fn teardown_for_manager(
        &self,
        id: &str,
        tokens: &crate::capability::tokens::TokenStore,
    ) -> Result<(), String> {
        self.manager_blocks.lock().unwrap().insert(id.to_owned());
        let mut slots = self.slots.lock().unwrap();
        let stop_result = match slots.get_mut(id) {
            Some(slot) => slot.stop_child(),
            None => Ok(()),
        };
        let revoke_result = tokens.revoke_plugin(id);

        if let Err(stop_error) = stop_result {
            return match revoke_result {
                Ok(_) => Err(format!("plugin child teardown failed: {stop_error}")),
                Err(revoke_error) => Err(format!(
                    "plugin child teardown failed: {stop_error}; token revoke failed: {revoke_error}"
                )),
            };
        }
        revoke_result.map_err(|error| format!("plugin token revoke failed: {error}"))?;

        if let Some(slot) = slots.get_mut(id) {
            slot.enabled = false;
            slot.reset_events();
            slot.runtime.disable();
        }
        Ok(())
    }

    pub fn resume_after_manager(&self, id: &str) {
        self.manager_blocks.lock().unwrap().remove(id);
    }

    /// A slot may still own a process even after its token has been revoked,
    /// while a token may survive without an in-memory slot after a core crash.
    /// Both stores therefore participate in the lifecycle decision.
    pub fn has_live_resources(
        &self,
        id: &str,
        tokens: &crate::capability::tokens::TokenStore,
    ) -> Result<bool, String> {
        let slot_live = self.slots.lock().unwrap().get(id).is_some_and(|slot| {
            slot.enabled || slot.child.is_some() || slot.runtime.lifecycle != Lifecycle::Stopped
        });
        let token_live = tokens.plugin_token_present(id)?;
        Ok(slot_live || token_live)
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
            .register(slot.protocol_version, request, now_ms)
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

    pub fn restart(
        &self,
        id: &str,
        tokens: &crate::capability::tokens::TokenStore,
    ) -> Result<Vec<HostEffect>, String> {
        let mut slots = self.slots.lock().unwrap();
        let slot = slots
            .get_mut(id)
            .ok_or_else(|| "плагин не найден".to_string())?;
        if !slot.enabled {
            return Err("плагин выключен".into());
        }
        let stop_result = slot.stop_child();
        let revoke_result = tokens.revoke_plugin(id);
        if stop_result.is_ok() {
            slot.reset_events();
            slot.runtime.disable();
            slot.enabled = true;
        }
        match (stop_result, revoke_result) {
            (Ok(()), Ok(_)) => {}
            (Err(stop_error), Ok(_)) => {
                slot.runtime.last_error = Some(format!("restart stop failed: {stop_error}"));
                return Err(format!("plugin restart stop failed: {stop_error}"));
            }
            (Ok(()), Err(revoke_error)) => {
                slot.runtime.last_error =
                    Some(format!("restart token revoke failed: {revoke_error}"));
                return Err(format!(
                    "plugin restart token revoke failed: {revoke_error}"
                ));
            }
            (Err(stop_error), Err(revoke_error)) => {
                slot.runtime.last_error = Some(format!(
                    "restart stop failed: {stop_error}; token revoke failed: {revoke_error}"
                ));
                return Err(format!(
                    "plugin restart stop failed: {stop_error}; token revoke failed: {revoke_error}"
                ));
            }
        }
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
            return match self.restart(id, &d.tokens) {
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

    fn dispose_attempt(
        &self,
        tokens: &crate::capability::tokens::TokenStore,
    ) -> (Vec<HostEffect>, Vec<String>) {
        let mut effects = Vec::new();
        let mut errors = Vec::new();
        let mut slots = self.slots.lock().unwrap();
        for (id, slot) in slots.iter_mut() {
            let was_active = slot.child.is_some() || slot.runtime.lifecycle != Lifecycle::Stopped;
            let mut slot_errors = Vec::new();
            if let Err(error) = slot.stop_child() {
                crate::log::line(&format!("[plugin:{id}] dispose stop failed: {error}"));
                slot_errors.push(format!("child stop failed: {error}"));
            } else {
                slot.reset_events();
                slot.runtime.disable();
            }
            if let Err(error) = tokens.revoke_plugin(id) {
                crate::log::line(&format!(
                    "[plugin:{id}] dispose token revoke failed: {error}"
                ));
                slot_errors.push(format!("token revoke failed: {error}"));
            }
            if !slot_errors.is_empty() {
                let detail = slot_errors.join("; ");
                slot.runtime.last_error = Some(format!("dispose {detail}"));
                errors.push(format!("plugin {id} dispose failed: {detail}"));
            }
            if was_active {
                effects.push(HostEffect::MarkOwnerStale(format!("plugin:{id}")));
                effects.push(HostEffect::Changed);
            }
        }
        (effects, errors)
    }

    #[cfg(test)]
    fn dispose_with(
        &self,
        tokens: &crate::capability::tokens::TokenStore,
    ) -> Result<Vec<HostEffect>, String> {
        let (effects, errors) = self.dispose_attempt(tokens);
        if errors.is_empty() {
            Ok(effects)
        } else {
            Err(errors.join("; "))
        }
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
                    "id": slot.id,
                    "name": slot.name,
                    "version": slot.version,
                    "external": true,
                    "enabled": slot.enabled,
                    "projectRuntimes": slot.project_runtimes,
                    "activationSource": slot.activation_source.map(activation_source_name),
                    "receiptGeneration": slot.receipt_generation,
                    "grantedPermissions": slot.granted_permissions,
                    "status": {
                        "state": slot.runtime.lifecycle.as_str(),
                        "pid": slot.runtime.pid,
                        "protocolVersion": slot.protocol_version,
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

    #[cfg(test)]
    fn tick_with(
        &self,
        now_ms: i64,
        is_enabled: &dyn Fn(&str) -> bool,
        tokens: &crate::capability::tokens::TokenStore,
        socket: &std::path::Path,
    ) -> Vec<HostEffect> {
        self.tick_with_policy(now_ms, is_enabled, false, tokens, socket)
    }

    fn tick_with_policy(
        &self,
        now_ms: i64,
        is_enabled: &dyn Fn(&str) -> bool,
        developer_mode: bool,
        tokens: &crate::capability::tokens::TokenStore,
        socket: &std::path::Path,
    ) -> Vec<HostEffect> {
        let mut effects = Vec::new();
        let manager_blocks = self.manager_blocks.lock().unwrap().clone();
        let mut slots = self.slots.lock().unwrap();

        for (id, slot) in slots.iter_mut() {
            let desired = is_enabled(id) && !manager_blocks.contains(id);
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
                revoke_runtime_token(slot, tokens, id, "incompatible runtime");
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
                    revoke_runtime_token(slot, tokens, id, "plugin process exit");
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
                    revoke_runtime_token(slot, tokens, id, "process observation failure");
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
                    revoke_runtime_token(slot, tokens, id, "handshake timeout");
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
                    revoke_runtime_token(slot, tokens, id, "plugin process handle lost");
                    effects.push(HostEffect::MarkOwnerStale(format!("plugin:{id}")));
                    effects.push(HostEffect::Changed);
                    false
                }
                Lifecycle::Incompatible => false,
            };
            if !should_spawn {
                continue;
            }

            let plugin_id = match PluginId::new(id.clone()) {
                Ok(plugin_id) => plugin_id,
                Err(_) => {
                    slot.runtime.on_error("activation candidate ID невалиден");
                    revoke_runtime_token(slot, tokens, id, "invalid activation candidate");
                    effects.push(HostEffect::Changed);
                    continue;
                }
            };
            let resolved = match self.resolver.resolve(
                &plugin_id,
                ResolutionPolicy {
                    developer_mode,
                    legacy_agent_vm_enabled: desired,
                },
            ) {
                Ok(resolved) => resolved,
                Err(error) => {
                    if let Err(revoke_error) = tokens.revoke_plugin(id) {
                        crate::log::line(&format!(
                            "[plugin:{id}] stale token revoke failed: {revoke_error}"
                        ));
                    }
                    slot.runtime.on_error(format!(
                        "activation blocked [{}]: {}",
                        error.code(),
                        error.cause()
                    ));
                    effects.push(HostEffect::Changed);
                    continue;
                }
            };
            let plan = match ActivationPlan::from_resolved(resolved) {
                Ok(plan) => plan,
                Err(error) => {
                    if let Err(revoke_error) = tokens.revoke_plugin(id) {
                        crate::log::line(&format!(
                            "[plugin:{id}] invalid activation token revoke failed: {revoke_error}"
                        ));
                    }
                    slot.runtime.on_error(error);
                    effects.push(HostEffect::Changed);
                    continue;
                }
            };
            slot.name = plan.name;
            slot.version = plan.version;
            slot.protocol_version = plan.protocol_version;
            slot.project_runtimes = plan.project_runtimes;
            slot.activation_source = Some(plan.source);
            slot.receipt_generation = plan.receipt_generation;
            slot.granted_permissions = plan.granted_permissions;

            let Some(executable) = plan.executable else {
                if let Err(revoke_error) = tokens.revoke_plugin(id) {
                    crate::log::line(&format!(
                        "[plugin:{id}] ui-only token revoke failed: {revoke_error}"
                    ));
                }
                slot.runtime.disable();
                effects.push(HostEffect::Changed);
                continue;
            };
            let token = match tokens.rotate_plugin_token(id, &plan.token_classes) {
                Ok(token) => token,
                Err(err) => {
                    slot.runtime.on_error(format!("token issue failed: {err}"));
                    effects.push(HostEffect::Changed);
                    continue;
                }
            };
            let spec = SpawnSpec {
                plugin_id: id.clone(),
                executable,
                args: plan.args,
                cwd: plan.cwd,
                socket: socket.to_path_buf(),
                token,
                protocol_version: plan.protocol_version,
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
                    revoke_runtime_token(slot, tokens, id, "plugin spawn failure");
                    effects.push(HostEffect::Changed);
                }
            }
        }
        effects
    }
}

struct ActivationPlan {
    name: String,
    version: String,
    protocol_version: u32,
    project_runtimes: Vec<Value>,
    source: ActivationSource,
    receipt_generation: Option<u64>,
    granted_permissions: Vec<GrantedPermission>,
    token_classes: Vec<crate::capability::contract::RiskClass>,
    executable: Option<SpawnExecutable>,
    args: Vec<String>,
    cwd: PathBuf,
}

impl ActivationPlan {
    fn from_resolved(resolved: ResolvedPlugin) -> Result<Self, String> {
        match resolved {
            ResolvedPlugin::VerifiedReceipt(plugin) => {
                let project_runtimes = plugin
                    .manifest
                    .contributes
                    .project_runtimes
                    .iter()
                    .map(serde_json::to_value)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| {
                        format!("verified project runtime metadata не сериализуется: {error}")
                    })?;
                let token_classes =
                    legacy_token_classes_for_exact_v2_receipt(&plugin.grants).to_vec();
                let executable = match plugin.manifest.runtime.kind {
                    RuntimeKind::UiOnly => None,
                    RuntimeKind::VerifiedNative => Some(SpawnExecutable::VerifiedReceipt(
                        plugin.bridge_executable.ok_or_else(|| {
                            "verified native activation не содержит bridge descriptor".to_owned()
                        })?,
                    )),
                };
                Ok(Self {
                    name: plugin.manifest.name,
                    version: plugin.manifest.version.to_string(),
                    protocol_version: plugin.manifest.runtime.protocol,
                    project_runtimes,
                    source: plugin.source.activation_source(),
                    receipt_generation: Some(plugin.generation),
                    granted_permissions: plugin.grants,
                    token_classes,
                    executable,
                    args: Vec::new(),
                    cwd: plugin.root,
                })
            }
            ResolvedPlugin::LegacyAgentVm(plugin) => Ok(Self {
                name: plugin.package.manifest.name,
                version: plugin.package.manifest.version,
                protocol_version: plugin.package.manifest.protocol_version,
                project_runtimes: plugin.package.manifest.project_runtimes,
                source: ActivationSource::LegacyBundledV1,
                receipt_generation: None,
                granted_permissions: Vec::new(),
                token_classes: plugin.package.manifest.capabilities,
                executable: Some(SpawnExecutable::LegacyAgentVm(plugin.package.executable)),
                args: plugin.package.manifest.entry.args,
                cwd: plugin.package.root,
            }),
        }
    }
}

fn legacy_token_classes_for_exact_v2_receipt(
    _grants: &[GrantedPermission],
) -> &'static [crate::capability::contract::RiskClass] {
    // Exact v2 permission IDs cannot be represented by the legacy class-only
    // capability gate. Mapping even one permission to Read/Control would grant
    // every legacy capability in that class. Keep the socket identity usable,
    // but deny all legacy capabilities until a 1:1 permission-aware adapter
    // exists. Legacy Agent VM v1 bypasses this function and keeps its manifest
    // classes in ActivationPlan::from_resolved.
    &[]
}

fn activation_source_name(source: ActivationSource) -> &'static str {
    match source {
        ActivationSource::ReceiptV2 => "receipt-v2",
        ActivationSource::DeveloperSnapshot => "developer-snapshot",
        ActivationSource::LegacyBundledV1 => "legacy-agent-vm",
    }
}

pub(crate) fn current_package_target() -> PackageTarget {
    #[cfg(target_arch = "aarch64")]
    {
        PackageTarget::DarwinArm64
    }
    #[cfg(target_arch = "x86_64")]
    {
        PackageTarget::DarwinAmd64
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
) -> PluginDiscoveryConfig {
    let mut roots = Vec::new();
    let mut push = |raw: &str, source: manifest::DiscoverySource| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        let path = PathBuf::from(trimmed);
        if !roots
            .iter()
            .any(|root: &manifest::DiscoveryRoot| root.path == path)
        {
            roots.push(manifest::DiscoveryRoot { path, source });
        }
    };
    let developer_mode = settings
        .get("pluginDeveloperMode")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if developer_mode {
        if let Some(raw) = env_override {
            push(raw, manifest::DiscoverySource::Developer);
        }
        if let Some(raw) = settings.get("pluginsDevDir").and_then(Value::as_str) {
            push(raw, manifest::DiscoverySource::Developer);
        }
    }
    if !roots.iter().any(|root| root.path == installed) {
        roots.push(manifest::DiscoveryRoot::production(installed));
    }
    PluginDiscoveryConfig {
        roots,
        policy: manifest::DiscoveryPolicy { developer_mode },
    }
}

pub fn roots_from_settings(settings: &Value) -> PluginDiscoveryConfig {
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
    use crate::capability::contract::RiskClass;
    use crate::capability::tokens::TokenStore;
    use serde_json::json;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Mutex};

    fn discovery_paths(config: &PluginDiscoveryConfig) -> Vec<PathBuf> {
        config.roots.iter().map(|root| root.path.clone()).collect()
    }

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
    fn roots_exclude_developer_sources_unless_developer_mode_is_enabled() {
        let production = roots_from_sources(
            &json!({ "pluginsDevDir": "/settings/plugins" }),
            Some("/env/plugins"),
            PathBuf::from("/installed/plugins"),
        );
        assert_eq!(
            discovery_paths(&production),
            [PathBuf::from("/installed/plugins")]
        );

        let development = roots_from_sources(
            &json!({
                "pluginDeveloperMode": true,
                "pluginsDevDir": "/settings/plugins"
            }),
            Some("/env/plugins"),
            PathBuf::from("/installed/plugins"),
        );
        assert_eq!(
            discovery_paths(&development),
            [
                PathBuf::from("/env/plugins"),
                PathBuf::from("/settings/plugins"),
                PathBuf::from("/installed/plugins"),
            ]
        );
        assert_eq!(
            development
                .roots
                .iter()
                .map(|root| root.source)
                .collect::<Vec<_>>(),
            [
                manifest::DiscoverySource::Developer,
                manifest::DiscoverySource::Developer,
                manifest::DiscoverySource::Production,
            ]
        );

        let deduped = roots_from_sources(
            &json!({
                "pluginDeveloperMode": true,
                "pluginsDevDir": "/env/plugins"
            }),
            Some("/env/plugins"),
            PathBuf::from("/installed/plugins"),
        );
        assert_eq!(
            discovery_paths(&deduped),
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
                discovery_paths(&roots_from_sources(
                    &settings,
                    Some("  "),
                    PathBuf::from("/installed/plugins"),
                )),
                [PathBuf::from("/installed/plugins")]
            );
        }
    }

    #[test]
    fn developer_root_overrides_installed_root_in_host_discovery() {
        let developer = temp_plugin_root("developer-override");
        let installed = temp_plugin_root("installed-override");
        let developer_manifest = developer.join("agent-vm/manifest.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&developer_manifest).unwrap()).unwrap();
        manifest["name"] = Value::String("Developer Agent VM".into());
        fs::write(
            &developer_manifest,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let roots = roots_from_sources(
            &json!({ "pluginDeveloperMode": true }),
            developer.to_str(),
            installed.clone(),
        );
        let host = PluginHost::with_spawner(roots, Arc::new(FakeSpawner::new(4242)));

        host.discover();

        let slots = host.slots.lock().unwrap();
        let slot = slots
            .get("agent-vm")
            .expect("explicit Developer Mode override remains discoverable");
        assert_eq!(slot.name, "Developer Agent VM");
        assert!(
            host.discovery_errors.lock().unwrap().is_empty(),
            "intentional dev override is not a duplicate-id error"
        );
        drop(slots);
        fs::remove_dir_all(developer.parent().unwrap()).unwrap();
        fs::remove_dir_all(installed.parent().unwrap()).unwrap();
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
        spawn_error: Arc<Mutex<Option<String>>>,
    }

    impl FakeSpawner {
        fn new(pid: u32) -> Self {
            Self {
                pid,
                state: Arc::new(Mutex::new(FakeProcessState::default())),
                specs: Arc::new(Mutex::new(Vec::new())),
                spawn_error: Arc::new(Mutex::new(None)),
            }
        }
    }

    impl ProcessSpawner for FakeSpawner {
        fn spawn(&self, spec: &SpawnSpec) -> Result<Box<dyn ManagedChild>, String> {
            self.specs.lock().unwrap().push(spec.clone());
            if let Some(error) = self.spawn_error.lock().unwrap().clone() {
                return Err(error);
            }
            Ok(Box::new(FakeChild {
                pid: self.pid,
                state: self.state.clone(),
            }))
        }
    }

    fn temp_plugin_root(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let profile = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "jarvis-plugin-host-{tag}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let root = profile.join("plugins");
        let package = root.join("agent-vm");
        let bin = package.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::set_permissions(&profile, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&package, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
        let executable = bin.join("agent-vm-plugin");
        fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let manifest_path = package.join("manifest.json");
        fs::write(
            &manifest_path,
            include_bytes!("../../../plugins/agent-vm/manifest.json"),
        )
        .unwrap();
        fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o600)).unwrap();
        root
    }

    fn token_store(root: &Path) -> TokenStore {
        TokenStore::at(root.join("tokens.json"))
    }

    #[test]
    fn v2_notifications_publish_grant_does_not_expand_to_legacy_control_capabilities() {
        let root = temp_plugin_root("v2-notifications-token");
        let tokens = token_store(&root);
        let classes = legacy_token_classes_for_exact_v2_receipt(&[GrantedPermission {
            id: PermissionId::NotificationsPublish,
            scope: None,
            modes: None,
        }]);
        let token = tokens
            .ensure_plugin_token("dev.example.notifications", classes)
            .unwrap();
        let consumer = tokens.resolve(&token).unwrap();

        assert!(
            !consumer.grant.allows_id(
                "stt.transcribe",
                crate::capability::contract::RiskClass::Control
            ),
            "точечный notifications.publish не должен открывать микрофон"
        );
        assert!(
            !consumer.grant.allows_id(
                "sessions.control",
                crate::capability::contract::RiskClass::Control
            ),
            "точечный notifications.publish не должен открывать управление сессиями"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn v2_projects_read_grant_does_not_expand_to_other_legacy_read_capabilities() {
        let root = temp_plugin_root("v2-projects-token");
        let tokens = token_store(&root);
        let classes = legacy_token_classes_for_exact_v2_receipt(&[GrantedPermission {
            id: PermissionId::ProjectsRead,
            scope: None,
            modes: None,
        }]);
        let token = tokens
            .ensure_plugin_token("dev.example.projects", classes)
            .unwrap();
        let consumer = tokens.resolve(&token).unwrap();

        assert!(
            !consumer.grant.allows_id(
                "sessions.list",
                crate::capability::contract::RiskClass::Read
            ),
            "точечный projects.read не должен открывать прочие legacy Read capability"
        );
        fs::remove_dir_all(root).unwrap();
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
        assert_eq!(
            specs.len(),
            1,
            "unexpected plugin status: {}",
            host.statuses(1_000)
        );
        let spec = &specs[0];
        assert_eq!(spec.plugin_id, "agent-vm");
        assert!(spec.args.is_empty());
        assert_eq!(spec.cwd, root.join("agent-vm").canonicalize().unwrap());
        assert!(matches!(
            &spec.executable,
            SpawnExecutable::LegacyAgentVm(_)
        ));
        assert_eq!(spec.socket, socket);
        assert_eq!(spec.protocol_version, manifest::PROTOCOL_VERSION);
        assert_eq!(spec.token.len(), 64);
        let legacy_consumer = tokens.resolve(&spec.token).unwrap();
        assert!(
            legacy_consumer
                .grant
                .allows(crate::capability::contract::RiskClass::Read),
            "legacy Agent VM v1 сохраняет manifest Read"
        );
        assert!(
            legacy_consumer
                .grant
                .allows(crate::capability::contract::RiskClass::Control),
            "legacy Agent VM v1 сохраняет manifest Control"
        );
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
        let stale_token = fake.specs.lock().unwrap()[0].token.clone();
        fake.state.lock().unwrap().exit = Some(1);

        let effects = host.tick_with(2_000, &|_| true, &tokens, &socket);

        assert!(effects.contains(&HostEffect::MarkOwnerStale("plugin:agent-vm".into())));
        assert!(
            tokens.resolve(&stale_token).is_none(),
            "crashed child bearer must be revoked before retry"
        );
        let statuses = host.statuses(2_000);
        assert_eq!(statuses[0]["status"]["state"], "backoff");
        assert_eq!(statuses[0]["status"]["retryInMs"], 1_000);
        assert_eq!(statuses[0]["status"]["retryAt"], 3_000);
        assert_eq!(statuses[0]["status"]["restartAttempt"], 1);

        fake.state.lock().unwrap().exit = None;
        host.tick_with(3_000, &|_| true, &tokens, &socket);
        let fresh_token = fake.specs.lock().unwrap()[1].token.clone();
        assert_ne!(
            stale_token, fresh_token,
            "a replacement process must receive a rotated bearer"
        );
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
    fn manager_teardown_kills_child_and_revokes_token_before_reporting_complete() {
        let root = temp_plugin_root("manager-teardown");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");
        host.tick_with(1_000, &|_| true, &tokens, &socket);
        let token = fake.specs.lock().unwrap()[0].token.clone();

        host.teardown_for_manager("agent-vm", &tokens).unwrap();

        assert!(fake.state.lock().unwrap().killed);
        assert!(tokens.resolve(&token).is_none());
        assert!(!host.has_live_resources("agent-vm", &tokens).unwrap());
        assert_eq!(host.statuses(2_000)[0]["status"]["state"], "stopped");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manager_teardown_blocks_settings_driven_respawn_until_commit_finishes() {
        let root = temp_plugin_root("manager-teardown-block");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");
        host.tick_with(1_000, &|_| true, &tokens, &socket);
        let old_token = fake.specs.lock().unwrap()[0].token.clone();

        host.teardown_for_manager("agent-vm", &tokens).unwrap();
        host.tick_with(2_000, &|_| true, &tokens, &socket);

        assert_eq!(
            fake.specs.lock().unwrap().len(),
            1,
            "manager barrier must prevent the old activation from respawning"
        );
        host.resume_after_manager("agent-vm");
        host.tick_with(3_000, &|_| true, &tokens, &socket);
        let specs = fake.specs.lock().unwrap();
        assert_eq!(specs.len(), 2);
        assert_ne!(specs[1].token, old_token);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manager_teardown_failure_revokes_token_but_keeps_child_supervised_and_fails_closed() {
        let root = temp_plugin_root("manager-teardown-failure");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");
        host.tick_with(1_000, &|_| true, &tokens, &socket);
        let token = fake.specs.lock().unwrap()[0].token.clone();
        fake.state.lock().unwrap().kill_error = true;

        let error = host
            .teardown_for_manager("agent-vm", &tokens)
            .expect_err("failed child termination must block receipt mutation");

        assert!(error.contains("fixture kill failed"));
        assert!(
            tokens.resolve(&token).is_none(),
            "token is revoked immediately"
        );
        assert!(host.has_live_resources("agent-vm", &tokens).unwrap());
        assert_eq!(fake.state.lock().unwrap().kill_attempts, 1);
        assert_eq!(
            host.statuses(2_000)[0]["status"]["state"],
            "starting",
            "failed kill keeps the child under host supervision"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manager_live_resource_check_fails_closed_on_corrupt_token_state() {
        let root = temp_plugin_root("manager-corrupt-token");
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(FakeSpawner::new(4242)));
        host.discover();
        let tokens = token_store(&root);
        fs::write(root.join("tokens.json"), b"{not-json").unwrap();

        assert!(
            host.has_live_resources("agent-vm", &tokens).is_err(),
            "corrupt token state must not be interpreted as no live resources"
        );
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

    struct CountingLegacyResolver {
        package: manifest::PluginPackage,
        calls: AtomicUsize,
    }

    impl PluginActivationResolver for CountingLegacyResolver {
        fn candidate_ids(&self) -> Vec<PluginId> {
            vec![PluginId::new("agent-vm").unwrap()]
        }

        fn resolve(
            &self,
            _plugin_id: &PluginId,
            _policy: ResolutionPolicy,
        ) -> Result<ResolvedPlugin, resolver::ResolverError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ResolvedPlugin::LegacyAgentVm(
                resolver::LegacyAgentVmPlugin {
                    package: self.package.clone(),
                    status: resolver::CompatibilityStatus {
                        migration_available: true,
                    },
                },
            ))
        }
    }

    #[test]
    fn every_spawn_attempt_re_resolves_the_exact_activation_lease() {
        let root = temp_plugin_root("resolve-per-spawn");
        let package = manifest::load_package(&root.join("agent-vm/manifest.json")).unwrap();
        let resolver = Arc::new(CountingLegacyResolver {
            package,
            calls: AtomicUsize::new(0),
        });
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_components(
            vec![root.clone()],
            Arc::new(fake.clone()),
            resolver.clone(),
        );
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");

        host.tick_with(1_000, &|_| true, &tokens, &socket);
        fake.state.lock().unwrap().exit = Some(1);
        host.tick_with(2_000, &|_| true, &tokens, &socket);
        fake.state.lock().unwrap().exit = None;
        host.tick_with(3_000, &|_| true, &tokens, &socket);

        assert_eq!(resolver.calls.load(Ordering::SeqCst), 2);
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
    fn spawn_failure_revokes_the_issued_token() {
        let root = temp_plugin_root("spawn-failure");
        let fake = FakeSpawner::new(4242);
        *fake.spawn_error.lock().unwrap() = Some("fixture spawn failed".into());
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");

        host.tick_with(1_000, &|_| true, &tokens, &socket);

        let issued_token = fake.specs.lock().unwrap()[0].token.clone();
        assert!(
            tokens.resolve(&issued_token).is_none(),
            "failed spawn must not leave a usable bearer"
        );
        assert_eq!(host.statuses(1_000)[0]["status"]["state"], "backoff");
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
        let old_token = fake.specs.lock().unwrap()[0].token.clone();

        let effects = host.restart("agent-vm", &tokens).unwrap();
        assert!(fake.state.lock().unwrap().killed);
        assert!(
            tokens.resolve(&old_token).is_none(),
            "restart must revoke the previous process bearer"
        );
        assert!(effects.contains(&HostEffect::MarkOwnerStale("plugin:agent-vm".into())));
        host.tick_with(2_000, &|_| true, &tokens, &socket);

        let specs = fake.specs.lock().unwrap();
        assert_eq!(specs.len(), 2);
        assert_ne!(
            specs[1].token, old_token,
            "replacement process must receive a fresh bearer"
        );
        drop(specs);
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
        let old_token = fake.specs.lock().unwrap()[0].token.clone();
        fake.state.lock().unwrap().kill_error = true;

        let err = host.restart("agent-vm", &tokens).unwrap_err();
        host.tick_with(2_000, &|_| true, &tokens, &socket);

        assert!(err.contains("fixture kill failed"));
        assert!(
            tokens.resolve(&old_token).is_none(),
            "failed restart must still deny the old bearer"
        );
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
        let stale_token = fake.specs.lock().unwrap()[0].token.clone();

        let effects = host.tick_with(
            1_000 + supervisor::HANDSHAKE_TIMEOUT_MS,
            &|_| true,
            &tokens,
            &socket,
        );

        assert!(fake.state.lock().unwrap().killed);
        assert!(
            tokens.resolve(&stale_token).is_none(),
            "timed-out child bearer must be revoked"
        );
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
        let stale_token = fake.specs.lock().unwrap()[0].token.clone();
        fake.state.lock().unwrap().kill_error = true;
        let deadline = 1_000 + supervisor::HANDSHAKE_TIMEOUT_MS;

        host.tick_with(deadline, &|_| true, &tokens, &socket);

        assert_eq!(fake.specs.lock().unwrap().len(), 1);
        assert!(
            tokens.resolve(&stale_token).is_none(),
            "timeout revokes bearer even when process termination must retry"
        );
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
    fn dispose_stops_children_revokes_tokens_and_marks_stale() {
        let root = temp_plugin_root("dispose");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");
        host.tick_with(1_000, &|_| true, &tokens, &socket);
        let token = fake.specs.lock().unwrap()[0].token.clone();

        let effects = host.dispose_with(&tokens).unwrap();

        assert!(fake.state.lock().unwrap().killed);
        assert!(
            tokens.resolve(&token).is_none(),
            "shutdown must revoke the stopped process bearer"
        );
        assert!(effects.contains(&HostEffect::MarkOwnerStale("plugin:agent-vm".into())));
        let status = host.statuses(2_000);
        assert_eq!(status[0]["enabled"], true, "настройка enable сохраняется");
        assert_eq!(status[0]["status"]["state"], "stopped");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dispose_reports_stop_failure_and_retries_without_reusing_the_bearer() {
        let root = temp_plugin_root("dispose-retry");
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let tokens = token_store(&root);
        let socket = root.join("run.sock");
        host.tick_with(1_000, &|_| true, &tokens, &socket);
        let old_token = fake.specs.lock().unwrap()[0].token.clone();
        fake.state.lock().unwrap().kill_error = true;

        let error = host
            .dispose_with(&tokens)
            .expect_err("failed child stop must keep shutdown phase retryable");

        assert!(error.contains("fixture kill failed"));
        assert!(tokens.resolve(&old_token).is_none());
        assert_eq!(fake.state.lock().unwrap().kill_attempts, 1);
        assert_eq!(host.statuses(2_000)[0]["status"]["state"], "starting");

        fake.state.lock().unwrap().kill_error = false;
        host.dispose_with(&tokens).unwrap();

        assert_eq!(fake.state.lock().unwrap().kill_attempts, 2);
        assert_eq!(host.statuses(3_000)[0]["status"]["state"], "stopped");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn first_spawn_rotates_a_bearer_left_by_a_previous_host_process() {
        let root = temp_plugin_root("stale-host-token");
        let tokens = token_store(&root);
        let stale_token = tokens
            .ensure_plugin_token("agent-vm", &[RiskClass::Read])
            .unwrap();
        let fake = FakeSpawner::new(4242);
        let host = PluginHost::with_spawner(vec![root.clone()], Arc::new(fake.clone()));
        host.discover();
        let socket = root.join("run.sock");

        host.tick_with(1_000, &|_| true, &tokens, &socket);

        let fresh_token = fake.specs.lock().unwrap()[0].token.clone();
        assert_ne!(
            fresh_token, stale_token,
            "a new Jarvis process must not reuse a persisted plugin bearer"
        );
        assert!(tokens.resolve(&stale_token).is_none());
        assert!(tokens.resolve(&fresh_token).is_some());
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
