# PluginHost Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Добавить в Jarvis безопасный out-of-process `PluginHost`: обнаружение плагинов,
версионированный handshake по UDS, выдачу и отзыв plugin-токенов, bounded long-poll RPC,
supervision с backoff, объединение статусов со встроенными power-плагинами и квоты
`EntityStore`.

**Architecture:** Новый модуль `plugins/` владеет внешними процессами и их протоколом, но
использует существующие `TokenStore`, capability gate и `EntityStore` как границы безопасности.
`Daemon` хранит один `PluginHost`; UDS server аутентифицирует `/plugin/*` по уже существующему
`x-jarvis-token`; UI продолжает использовать текущие `plugins_status`/`plugins_cmd`, получая
объединённый список встроенных и внешних плагинов.

**Tech Stack:** Rust 2021, Tauri 2, Tokio, Axum 0.8, `serde`/`serde_json`, Unix domain socket,
`std::process::Command`, существующие inline `#[cfg(test)]`-тесты и shell fake-plugin fixture.

**Spec:** `docs/superpowers/specs/2026-07-28-agent-vm-project-runtime-design.md` §§4, 16, 18.1;
архитектурная база — `docs/superpowers/specs/2026-07-03-plugin-system-agent-vm-design.md`
§§6.2–6.4 и заметки к инкременту 2.

**Working directory:** `/Users/se.chernyshev/jarvis`. Команды Rust запускать с аргументом
`--manifest-path src-tauri/Cargo.toml`, чтобы не менять cwd в инструкциях.

---

## Public contract fixed by this increment

Manifest v1:

```json
{
  "id": "agent-vm",
  "name": "Agent VM",
  "version": "0.1.0",
  "protocolVersion": 1,
  "entry": {
    "type": "binary",
    "path": "bin/agent-vm-plugin",
    "args": []
  },
  "capabilities": ["read", "control"],
  "projectRuntimes": []
}
```

Spawn environment:

```text
JARVIS_SOCKET=/absolute/path/to/run.sock
JARVIS_PLUGIN_ID=agent-vm
JARVIS_PLUGIN_TOKEN=<64-char hex token>
JARVIS_PLUGIN_PROTOCOL=1
```

Registration:

```http
POST /plugin/register
x-jarvis-token: <token>
content-type: application/json

{"protocolVersion":1,"pid":12345}
```

Successful response:

```json
{ "ok": true, "protocolVersion": 1, "pluginId": "agent-vm" }
```

Core-to-plugin event poll:

```http
GET /plugin/events?after=41&limit=64&waitMs=25000
x-jarvis-token: <token>
```

```json
{
  "ok": true,
  "protocolVersion": 1,
  "events": [
    {
      "seq": 42,
      "kind": "command",
      "payload": {
        "requestId": "agent-vm-42",
        "name": "runtime.ensure",
        "args": {}
      }
    }
  ],
  "nextSeq": 42
}
```

Hard limits:

- manifest file: 256 KiB;
- event payload: 256 KiB;
- queued events per plugin: 256;
- events returned per poll: 64;
- `waitMs`: at most 25 seconds;
- `EntityStore`: 1000 entities per owner and 64 KiB serialized `attrs` per entity;
- existing Axum body limit remains 4 MiB.

---

### Task 1: Add `EntityStore` quotas before any live plugin can publish

**Files:**

- Modify: `src-tauri/src/entities.rs`

- [ ] **Step 1: Write failing quota tests**

Add constants and tests to `src-tauri/src/entities.rs`:

```rust
pub const MAX_ENTITIES_PER_OWNER: usize = 1_000;
pub const MAX_ATTRS_BYTES: usize = 64 * 1024;
```

```rust
#[test]
fn rejects_attrs_larger_than_quota() {
    let s = EntityStore::new();
    let attrs = json!({ "blob": "x".repeat(MAX_ATTRS_BYTES) });
    let err = s
        .upsert("plugin:avm", "vm", "too-big", "running", attrs)
        .unwrap_err();
    assert!(err.contains("attrs"), "понятная ошибка квоты: {err}");
}

#[test]
fn rejects_new_entity_after_owner_quota_but_allows_update() {
    let s = EntityStore::new();
    for n in 0..MAX_ENTITIES_PER_OWNER {
        s.upsert(
            "plugin:avm",
            "vm",
            &format!("vm-{n}"),
            "running",
            json!({}),
        )
        .unwrap();
    }
    let err = s
        .upsert("plugin:avm", "vm", "overflow", "running", json!({}))
        .unwrap_err();
    assert!(err.contains("1000"), "ошибка называет лимит: {err}");
    s.upsert("plugin:avm", "vm", "vm-0", "stopped", json!({}))
        .expect("обновление существующей сущности не расходует новую квоту");
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml entities::tests::rejects_ -- --nocapture
```

Expected: compilation fails because quota constants/logic are not implemented, or the new
tests fail by accepting oversized input.

- [ ] **Step 3: Enforce both quotas in `EntityStore::upsert`**

Normalize `Value::Null` to `{}` first, serialize `attrs` with `serde_json::to_vec`, reject a
payload larger than `MAX_ATTRS_BYTES`, then take the items lock. Before inserting a previously
unknown full entity id, count entries with the same owner and reject at
`MAX_ENTITIES_PER_OWNER`. Updating an existing entity remains allowed.

Use stable error messages:

```rust
return Err(format!(
    "attrs сущности превышает лимит {} байт",
    MAX_ATTRS_BYTES
));
```

```rust
return Err(format!(
    "владелец {owner} превысил лимит {MAX_ENTITIES_PER_OWNER} сущностей"
));
```

- [ ] **Step 4: Run focused and module tests and verify GREEN**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml entities::tests -- --nocapture
```

Expected: all `entities::tests` pass.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/entities.rs
git commit -m "feat(plugins): bound entity store payloads"
```

---

### Task 2: Parse and discover versioned plugin manifests safely

**Files:**

- Create: `src-tauri/src/plugins/manifest.rs`
- Create: `src-tauri/src/plugins/mod.rs`
- Create: `src-tauri/tests/fixtures/plugin-host/fake-plugin/manifest.json`
- Modify: `src-tauri/src/main.rs`

- [ ] **Step 1: Write failing manifest tests**

In `manifest.rs`, define these tests with concrete assertions:

- `loads_valid_v1_manifest_and_canonical_entry`: write an executable file and valid manifest
  under one temp plugin root; assert id, protocol, capabilities and canonical executable path.
- `rejects_admin_capability`: write the same manifest with `["read","admin"]`; assert the load
  error contains `admin`.
- `rejects_protocol_mismatch_as_incompatible`: set `protocolVersion` to `2`; assert
  `LoadError.incompatible` is true and no package is returned.
- `rejects_entry_outside_plugin_root`: point entry at `../escape`; assert the load error contains
  `вне каталога плагина`.
- `rejects_invalid_plugin_id`: test `AgentVm`, `agent/vm` and `agent vm`; assert each is rejected.
- `discovery_is_sorted_and_first_root_wins_duplicate_id`: create two roots with the same id and
  distinct versions; assert the package version comes from the first root and one duplicate error
  is returned.

Use a unique temp directory helper based on process id plus an atomic counter. Tests must remove
their own temp trees at the end; no third-party temp-dir crate is added.

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml plugins::manifest::tests -- --nocapture
```

Expected: compilation fails because `plugins`/`manifest` do not exist.

- [ ] **Step 3: Implement the manifest contract**

`src-tauri/src/plugins/manifest.rs` owns:

```rust
pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub protocol_version: u32,
    pub entry: Entry,
    #[serde(default)]
    pub capabilities: Vec<crate::capability::contract::RiskClass>,
    #[serde(default)]
    pub project_runtimes: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Entry {
    #[serde(rename = "type")]
    pub kind: String,
    pub path: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PluginPackage {
    pub manifest: Manifest,
    pub root: std::path::PathBuf,
    pub executable: std::path::PathBuf,
}

#[derive(Clone, Debug)]
pub struct LoadError {
    pub key: String,
    pub path: std::path::PathBuf,
    pub message: String,
    pub incompatible: bool,
}
```

Validation rules:

- id matches `[a-z0-9][a-z0-9-]{0,63}`;
- `name` and `version` are non-empty;
- `protocolVersion` equals `PROTOCOL_VERSION`;
- entry type is exactly `binary`;
- manifest metadata size is at most `MAX_MANIFEST_BYTES`;
- canonical entry exists, is a regular file and stays under canonical plugin root;
- executable mode contains at least one execute bit on Unix;
- `RiskClass::Admin` is rejected, not silently removed.

Discovery scans each explicit root only one directory deep for `*/manifest.json`, sorts paths,
and returns both valid packages and invalid records. The first root wins duplicate plugin ids;
the later duplicate becomes a `LoadError` so the conflict is visible in status/logs.

- [ ] **Step 4: Add production roots without embedding a developer machine path**

`src-tauri/src/plugins/mod.rs` exposes:

```rust
pub fn roots_from_settings(settings: &serde_json::Value) -> Vec<std::path::PathBuf>
```

Order:

1. `JARVIS_PLUGIN_DEV_DIR`, when non-empty;
2. string setting `pluginsDevDir`, when non-empty and not already present;
3. `jarvis_dir().join("plugins")`.

Never use compile-time `CARGO_MANIFEST_DIR` as a production discovery root.

Add to `src-tauri/src/main.rs`:

```rust
mod plugins;
```

- [ ] **Step 5: Add the fake manifest fixture**

`src-tauri/tests/fixtures/plugin-host/fake-plugin/manifest.json`:

```json
{
  "id": "fake-plugin",
  "name": "Fake Plugin",
  "version": "0.1.0",
  "protocolVersion": 1,
  "entry": {
    "type": "binary",
    "path": "fake-plugin.sh",
    "args": []
  },
  "capabilities": ["read"],
  "projectRuntimes": []
}
```

The executable itself is added in Task 7, before the fixture is used by an end-to-end test.

- [ ] **Step 6: Run tests and verify GREEN**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml plugins::manifest::tests -- --nocapture
```

Expected: all manifest/discovery tests pass.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/plugins src-tauri/src/main.rs src-tauri/tests/fixtures/plugin-host/fake-plugin/manifest.json
git commit -m "feat(plugins): discover validated plugin manifests"
```

---

### Task 3: Make plugin token issue/revoke atomic and least-privilege

**Files:**

- Modify: `src-tauri/src/capability/tokens.rs`

- [ ] **Step 1: Write failing token lifecycle tests**

Add:

```rust
#[test]
fn plugin_token_is_stable_updates_classes_and_uses_private_file() {
    use std::os::unix::fs::PermissionsExt;
    let p = tmp();
    let s = TokenStore::at(p.clone());
    let t1 = s
        .ensure_plugin_token("agent-vm", &[RiskClass::Read])
        .unwrap();
    let t2 = s
        .ensure_plugin_token("agent-vm", &[RiskClass::Read, RiskClass::Control])
        .unwrap();
    assert_eq!(t1, t2, "повторный выпуск сохраняет identity");
    let c = s.resolve(&t2).unwrap();
    assert!(c.grant.allows(RiskClass::Control));
    assert_eq!(std::fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn revoke_plugin_invalidates_token_without_touching_agent() {
    let s = TokenStore::at(tmp());
    let agent = s.ensure_agent_token();
    let plugin = s
        .ensure_plugin_token("agent-vm", &[RiskClass::Read])
        .unwrap();
    assert!(s.revoke_plugin("agent-vm").unwrap());
    assert!(s.resolve(&plugin).is_none());
    assert_eq!(s.resolve(&agent).unwrap().id, "agent");
    assert!(!s.revoke_plugin("agent-vm").unwrap());
}

#[test]
fn plugin_token_never_persists_admin_class() {
    let s = TokenStore::at(tmp());
    let token = s
        .ensure_plugin_token("agent-vm", &[RiskClass::Read, RiskClass::Admin])
        .unwrap();
    let c = s.resolve(&token).unwrap();
    assert!(!c.grant.allows(RiskClass::Admin));
}
```

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml capability::tokens::tests::plugin_token_ -- --nocapture
```

Expected: compilation fails because `ensure_plugin_token` and `revoke_plugin` are absent.

- [ ] **Step 3: Implement atomic token persistence**

Change the private writer to return `Result<(), String>` and use an owner-only temporary file
next to `tokens.json`, followed by `rename`. The temporary file must be created with mode `0600`
before bytes are written. Keep `ensure_agent_token() -> String` source-compatible; if persistence
fails it logs a diagnostic and returns the generated token as today.

Add:

```rust
pub fn ensure_plugin_token(
    &self,
    id: &str,
    classes: &[RiskClass],
) -> Result<String, String>
```

and:

```rust
pub fn revoke_plugin(&self, id: &str) -> Result<bool, String>
```

Persist only `read`, `control`, and `settings`; never persist `admin`. Reuse an existing non-empty
token for the same id while replacing its effective classes with the current manifest classes.

- [ ] **Step 4: Run token and server identity tests and verify GREEN**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml capability::tokens -- --nocapture
cargo test --manifest-path src-tauri/Cargo.toml server::tests -- --nocapture
```

Expected: all token tests and the existing INV-PANEL server test pass.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/capability/tokens.rs
git commit -m "feat(plugins): issue and revoke plugin tokens"
```

---

### Task 4: Implement handshake state machine and bounded event queue

**Files:**

- Create: `src-tauri/src/plugins/protocol.rs`
- Create: `src-tauri/src/plugins/supervisor.rs`
- Modify: `src-tauri/src/plugins/mod.rs`

- [ ] **Step 1: Write failing protocol and state-machine tests**

Required tests with concrete assertions:

- `register_accepts_matching_plugin_pid_and_protocol`: start runtime with PID 41, register PID 41
  with protocol 1, assert `Running`, `registered_at_ms`, and cleared error.
- `register_is_idempotent_for_same_running_pid`: repeat the same registration and assert success
  without changing PID or incrementing restart attempt.
- `register_rejects_wrong_plugin_pid_or_protocol`: assert wrong PID produces conflict and protocol
  2 produces incompatible while neither request can become `Running`.
- `handshake_timeout_schedules_exponential_restart`: feed deterministic timestamps through seven
  failures and assert delays `1, 2, 4, 8, 16, 30, 30` seconds.
- `clean_disable_stops_without_restart`: disable a starting and a backoff runtime; assert
  `Stopped`, no PID and no retry deadline.
- `event_queue_is_monotonic_bounded_and_replayable_after_seq`: enqueue 300 events, assert only
  the latest 256 remain, sequences strictly increase, and `after` excludes older events.
- `event_payload_over_256_kib_is_rejected`: enqueue a JSON string above the limit; assert an
  explicit size error and unchanged queue length.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml plugins:: -- --nocapture
```

Expected: new protocol/supervisor tests fail to compile.

- [ ] **Step 3: Implement versioned wire types**

`plugins/protocol.rs`:

```rust
pub const MAX_EVENT_BYTES: usize = 256 * 1024;
pub const MAX_QUEUED_EVENTS: usize = 256;
pub const MAX_POLL_EVENTS: usize = 64;
pub const MAX_WAIT_MS: u64 = 25_000;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRequest {
    pub protocol_version: u32,
    pub pid: u32,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginEvent {
    pub seq: u64,
    pub kind: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventsQuery {
    #[serde(default)]
    pub after: u64,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default = "default_wait_ms")]
    pub wait_ms: u64,
}
```

Clamp query values at `MAX_POLL_EVENTS` and `MAX_WAIT_MS`; do not trust caller-supplied limits.

- [ ] **Step 4: Implement pure lifecycle transitions**

`plugins/supervisor.rs` owns serializable state:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    Stopped,
    Starting,
    Running,
    Backoff,
    Error,
    Incompatible,
}

pub struct Runtime {
    pub lifecycle: Lifecycle,
    pub pid: Option<u32>,
    pub started_at_ms: Option<i64>,
    pub registered_at_ms: Option<i64>,
    pub retry_at_ms: Option<i64>,
    pub restart_attempt: u32,
    pub last_error: Option<String>,
}
```

Rules:

- spawn success: `Starting`, expected PID recorded, 10-second handshake deadline;
- register: token-derived plugin id is resolved outside this reducer; reducer verifies expected
  PID and protocol, then transitions to `Running`;
- repeated register with the same running PID succeeds;
- mismatched PID returns `409 conflict`; mismatched protocol returns
  `426 incompatible_protocol`;
- exit/handshake timeout: `Backoff`, stale owner requested, delay
  `min(2^attempt, 30)` seconds;
- disable: `Stopped`, PID/retry cleared, no automatic restart;
- successful registration resets restart attempt to zero.

- [ ] **Step 5: Implement bounded per-plugin events**

Each slot owns `VecDeque<PluginEvent>` plus `Arc<tokio::sync::Notify>`. `enqueue`:

1. serializes only the payload to enforce `MAX_EVENT_BYTES`;
2. increments a host-wide `AtomicU64` sequence;
3. pushes the event;
4. drops oldest entries while length exceeds `MAX_QUEUED_EVENTS`;
5. calls `notify_waiters`.

`poll_events(id, after, limit, wait_ms)` first reads immediately. If empty and `wait_ms > 0`,
waits once on the slot notify with `tokio::time::timeout`, then reads again. A reconnecting plugin
can replay retained events using `after`.

- [ ] **Step 6: Run focused tests and verify GREEN**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml plugins:: -- --nocapture
```

Expected: manifest, protocol, queue and transition tests all pass.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/plugins
git commit -m "feat(plugins): add versioned handshake and event queue"
```

---

### Task 5: Supervise real plugin processes with safe spawn and backoff

**Files:**

- Modify: `src-tauri/src/plugins/supervisor.rs`
- Modify: `src-tauri/src/plugins/mod.rs`

- [ ] **Step 1: Write failing process supervision tests**

Use a test-only `ManagedChild` fake, not timing sleeps. Required cases:

- `enabled_discovered_plugin_spawns_with_expected_identity_env`: assert the fake spawner receives
  canonical executable/cwd plus socket, id, token and protocol fields, and runtime records its PID.
- `child_exit_marks_owner_stale_and_enters_backoff`: fake `try_wait` returns exit code 1; assert
  `Backoff`, retry deadline and a `MarkOwnerStale("plugin:<id>")` action.
- `disabled_plugin_is_killed_and_token_revoked`: disable a running fake child; assert one kill,
  `Stopped`, and `TokenStore::resolve(old_token) == None`.
- `tick_does_not_spawn_before_retry_deadline`: run ticks one millisecond before and exactly at the
  deadline; assert zero then one spawn.

The production spawner and fake spawner share a small synchronous trait:

```rust
trait ProcessSpawner: Send + Sync {
    fn spawn(&self, spec: &SpawnSpec) -> Result<Box<dyn ManagedChild>, String>;
}

trait ManagedChild: Send {
    fn id(&self) -> u32;
    fn try_wait(&mut self) -> Result<Option<i32>, String>;
    fn kill(&mut self) -> Result<(), String>;
}
```

- [ ] **Step 2: Run supervision tests and verify RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml plugins::supervisor::tests -- --nocapture
```

Expected: process-spawner/supervision tests fail to compile.

- [ ] **Step 3: Add production process adapter**

Build `SpawnSpec` only from validated `PluginPackage`:

```rust
pub struct SpawnSpec {
    pub plugin_id: String,
    pub executable: std::path::PathBuf,
    pub args: Vec<String>,
    pub cwd: std::path::PathBuf,
    pub socket: std::path::PathBuf,
    pub token: String,
    pub protocol_version: u32,
}
```

Production spawn rules:

- call the executable directly, never through `sh -c`;
- current directory is plugin root;
- stdin is null;
- stdout and stderr are piped to background line readers and prefixed
  `[plugin:<id>]` in `jarvis.log`;
- only the four documented `JARVIS_*` identity variables are added;
- token is never written to logs, status JSON, argv or error messages.

- [ ] **Step 4: Implement `PluginHost::discover/init/tick/dispose`**

Public facade:

```rust
pub struct PluginHost {
    roots: Vec<std::path::PathBuf>,
    slots: std::sync::Mutex<std::collections::BTreeMap<String, PluginSlot>>,
    spawner: std::sync::Arc<dyn ProcessSpawner>,
    next_event_seq: std::sync::atomic::AtomicU64,
}

impl PluginHost {
    pub fn new(roots: Vec<std::path::PathBuf>) -> Self;
    pub fn init(&self, d: &std::sync::Arc<crate::daemon::Daemon>);
    pub fn tick(&self, d: &std::sync::Arc<crate::daemon::Daemon>);
    pub fn dispose(&self, d: &std::sync::Arc<crate::daemon::Daemon>);
    pub fn contains(&self, id: &str) -> bool;
    pub fn statuses(&self) -> serde_json::Value;
}
```

`init` discovers once. `tick`:

1. reads `settings.plugins.<id>.enabled` with manifest default `false`;
2. kills/revokes disabled plugins;
3. calls `try_wait` for active children;
4. kills a child that misses the 10-second handshake;
5. respawns enabled plugins when backoff expires;
6. issues token immediately before spawn;
7. marks `entities` owned by `plugin:<id>` stale on stop/crash/timeout.

`dispose` kills all children and marks entities stale, but does not revoke tokens merely because
Jarvis exits; explicit disable still revokes.

- [ ] **Step 5: Add host commands and status JSON**

`PluginHost::command(d, id, name, args)` supports:

- `_enable {on}`: persist `settings.plugins.<id>.enabled`; false kills and revokes immediately;
- `_restart`: kill current child and make enabled plugin eligible for immediate spawn;
- every other command: enqueue a `command` event with a generated request id and return
  `{ok:true, accepted:true, requestId}`.

External status shape:

```json
{
  "id": "agent-vm",
  "name": "Agent VM",
  "version": "0.1.0",
  "external": true,
  "enabled": true,
  "status": {
    "state": "running",
    "pid": 12345,
    "protocolVersion": 1,
    "retryInMs": null,
    "error": null
  }
}
```

Invalid manifests are present with `external:true`, `enabled:false`, state `error` or
`incompatible`, and a sanitized error; they can never be spawned or enabled.

- [ ] **Step 6: Run focused tests and verify GREEN**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml plugins:: -- --nocapture
```

Expected: all plugin host tests pass without real sleeps.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/plugins
git commit -m "feat(plugins): supervise external plugin processes"
```

---

### Task 6: Add authenticated `/plugin/*` routes and wire the host into Jarvis

**Files:**

- Modify: `src-tauri/src/server.rs`
- Modify: `src-tauri/src/daemon.rs`
- Modify: `src-tauri/src/ipc.rs`
- Modify: `src-tauri/src/power/mod.rs`
- Modify: `src-tauri/src/main.rs`

- [ ] **Step 1: Write failing route-auth and combined-status tests**

Extract pure helpers where a full `tauri::AppHandle` would otherwise make a unit test brittle.
Required tests:

- `plugin_route_rejects_agent_token`: resolve a valid agent token and assert the plugin identity
  helper returns `None`.
- `plugin_route_uses_token_identity_not_body_identity`: resolve an `agent-vm` token while the JSON
  contains `"pluginId":"other"`; assert the routed id is still `agent-vm`.
- `register_error_maps_to_stable_http_status_and_code`: assert unauthorized, conflict and
  incompatible errors map to HTTP 401/409/426 and the documented stable code.
- `combined_statuses_keep_builtins_and_append_external_plugins`: feed two builtin JSON entries and
  one external entry into the pure combiner; assert order and all three ids.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml server::tests plugins::tests -- --nocapture
```

If Cargo accepts only one filter on the local toolchain, run the two filters as separate commands.
Expected: tests fail because routes/wiring are absent.

- [ ] **Step 3: Add UDS routes**

Extend the Axum router:

```rust
.route("/plugin/register", post(plugin_register))
.route("/plugin/events", get(plugin_events))
```

Both handlers:

- read only `x-jarvis-token`;
- resolve via `d.tokens.resolve`;
- require `consumer.id.strip_prefix("plugin:")`;
- ignore any plugin id supplied in body/query;
- return JSON with `content-type: application/json`;
- never expose token values.

`plugin_register` parses `RegisterRequest`, calls
`d.plugins.register(token_plugin_id, request, now_ms())`, emits combined plugin status on success,
and maps:

- unknown/non-plugin token → `401 unauthorized`;
- malformed JSON → `400 bad_json`;
- PID/state conflict → `409 registration_conflict`;
- protocol mismatch → `426 incompatible_protocol`.

`plugin_events` clamps the query limits and awaits
`d.plugins.poll_events(plugin_id, query.after, query.limit, query.wait_ms)`.

- [ ] **Step 4: Attach `PluginHost` to `Daemon`**

Add:

```rust
pub plugins: crate::plugins::PluginHost,
```

In `Daemon::new`, reuse the already-loaded settings root:

```rust
let plugin_roots = crate::plugins::roots_from_settings(&root);
```

and initialize:

```rust
plugins: crate::plugins::PluginHost::new(plugin_roots),
```

- [ ] **Step 5: Preserve the existing UI contract by combining statuses**

In `plugins/mod.rs`, add:

```rust
pub fn combined_statuses(d: &std::sync::Arc<crate::daemon::Daemon>) -> serde_json::Value
pub fn emit_statuses(d: &std::sync::Arc<crate::daemon::Daemon>)
```

`combined_statuses` copies the array from `d.power.statuses(d)` and appends
`d.plugins.statuses()`. Update:

- `Daemon::do_push`;
- `power::changed`;
- `ipc::plugins_status`;

to use `plugins::combined_statuses`/`emit_statuses`.

In `ipc::plugins_cmd`, route ids known to `d.plugins` to `PluginHost::command`; all other ids
continue through `Power::cmd`. Existing `keep-awake` and `clamshell` behavior must remain
byte-for-byte compatible at the IPC boundary.

- [ ] **Step 6: Add lifecycle calls**

In `main.rs`:

- after spawning `server::serve`, call `d.plugins.init(&d)`;
- in `spawn_timers`, add a one-second plugin supervisor tick; first tick occurs after one second,
  allowing the UDS listener to bind before any enabled plugin spawns;
- on `RunEvent::Exit`, call `d.plugins.dispose(&d)` before removing `run.sock`.

- [ ] **Step 7: Run Rust and UI regression tests**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml
npm run test:ui
```

Expected: full Rust and Node UI test suites pass. Existing power cards, footer suffix and settings
tests remain green with a combined plugin array.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/server.rs src-tauri/src/daemon.rs src-tauri/src/ipc.rs src-tauri/src/power/mod.rs src-tauri/src/main.rs src-tauri/src/plugins
git commit -m "feat(plugins): wire plugin host into daemon and ipc"
```

---

### Task 7: Prove the wire contract with an executable fake plugin

**Files:**

- Create: `src-tauri/tests/fixtures/plugin-host/fake-plugin/fake-plugin.sh`
- Modify: `src-tauri/src/plugins/mod.rs`
- Modify: `package.json`

- [ ] **Step 1: Add the fake plugin executable**

```sh
#!/bin/sh
set -eu

register_body=$(printf '{"protocolVersion":%s,"pid":%s}' "$JARVIS_PLUGIN_PROTOCOL" "$$")

curl --silent --show-error --fail \
  --unix-socket "$JARVIS_SOCKET" \
  --header "x-jarvis-token: $JARVIS_PLUGIN_TOKEN" \
  --header "content-type: application/json" \
  --data "$register_body" \
  http://localhost/plugin/register

if [ "${JARVIS_FAKE_ONESHOT:-0}" = "1" ]; then
  exit 0
fi

after=0
while :; do
  response=$(curl --silent --show-error --fail \
    --unix-socket "$JARVIS_SOCKET" \
    --header "x-jarvis-token: $JARVIS_PLUGIN_TOKEN" \
    "http://localhost/plugin/events?after=$after&limit=64&waitMs=25000")
  next=$(printf '%s' "$response" | sed -n 's/.*"nextSeq":\([0-9][0-9]*\).*/\1/p')
  if [ -n "$next" ]; then
    after=$next
  fi
done
```

Set executable mode:

```bash
chmod 755 src-tauri/tests/fixtures/plugin-host/fake-plugin/fake-plugin.sh
```

- [ ] **Step 2: Write a real Unix-socket fixture test**

Add a `#[cfg(test)]` module test that:

1. binds a unique `tokio::net::UnixListener`;
2. launches the fixture directly with the four production identity env vars plus
   `JARVIS_FAKE_ONESHOT=1`;
3. accepts one HTTP request;
4. asserts the token header, `protocolVersion=1`, and body PID equals child PID;
5. sends a minimal HTTP 200 JSON response;
6. asserts the fake process exits successfully;
7. removes the socket.

This test validates the real shell/curl wire without constructing a Tauri `AppHandle`.

- [ ] **Step 3: Make repo plugins automatic in `npm start` only**

Extend the existing dev start environment in `package.json` with:

```text
JARVIS_PLUGIN_DEV_DIR="$PWD/plugins"
```

Do not change `start:prod`: installed production plugins are discovered from
`$JARVIS_DIR/plugins`.

- [ ] **Step 4: Run fake-plugin and complete regression tests**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml fake_plugin -- --nocapture
cargo test --manifest-path src-tauri/Cargo.toml
npm run test:ui
```

Expected: fake plugin request assertions pass; all Rust and UI tests pass.

- [ ] **Step 5: Format and lint touched Rust**

Run:

```bash
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
```

Expected: both commands exit 0. Fix warnings only in files touched by this increment; if an
unrelated pre-existing warning appears, capture the exact output in the handoff rather than
editing unrelated code.

- [ ] **Step 6: Commit**

```bash
git add package.json src-tauri/tests/fixtures/plugin-host/fake-plugin/fake-plugin.sh src-tauri/src/plugins
git commit -m "test(plugins): verify fake plugin handshake"
```

---

### Task 8: Live dev smoke and completion evidence

**Files:**

- Modify only if a defect is found in files already owned by Tasks 1–7.

- [ ] **Step 1: Prepare a temporary dev plugin root**

Copy the fake fixture to the dev profile plugin directory only for the smoke, keep it disabled by
default, then enable it through `plugins_cmd`. Do not commit profile data.

Expected observable sequence:

```text
discovered -> starting -> running
```

The status must include a PID but never a token.

- [ ] **Step 2: Exercise command delivery**

Send an arbitrary command through `plugins_cmd("fake-plugin", "ping", {})`.

Expected: IPC returns `{ok:true, accepted:true, requestId:"fake-plugin-<seq>"}` and the next
`GET /plugin/events` contains one `command` envelope with a monotonic sequence.

- [ ] **Step 3: Exercise crash recovery and disable**

Kill only the fake plugin child, not Jarvis.

Expected:

- status changes to `backoff`;
- owned entities are marked stale;
- process restarts after the scheduled delay and handshakes again;
- `_enable {on:false}` stops it and prevents another restart;
- its previous token no longer resolves.

- [ ] **Step 4: Verify no secret leakage**

Search the dev log and status response for the exact test token obtained from the temporary
profile’s `tokens.json`.

Expected: token exists only in `tokens.json` and the fake child environment; it is absent from
`jarvis.log`, UI status payloads, command argv and error messages.

- [ ] **Step 5: Final test matrix**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml
npm run test:ui
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
git status --short
```

Expected: tests and format check pass. `git status --short` shows only deliberate implementation
changes, or is clean after the final fix commit.

- [ ] **Step 6: Final commit if smoke required fixes**

```bash
git add src-tauri/src/entities.rs src-tauri/src/plugins src-tauri/src/server.rs src-tauri/src/daemon.rs src-tauri/src/ipc.rs src-tauri/src/power/mod.rs src-tauri/src/main.rs package.json
git commit -m "fix(plugins): close plugin host smoke gaps"
```

Skip this commit when the smoke needed no fixes.

---

## Increment acceptance checklist

- [ ] Invalid/path-traversing/admin/incompatible manifests never spawn.
- [ ] External processes receive identity only via environment and run without a shell wrapper.
- [ ] A plugin is `running` only after token + id + PID + protocol handshake.
- [ ] Plugin tokens are owner-only, idempotent, least-privilege and revoked on disable.
- [ ] Crash/timeout marks owned entities stale and restarts with bounded backoff.
- [ ] Event queues and EntityStore have explicit refusal quotas.
- [ ] `/plugin/*` never grants panel identity and never trusts a body-provided plugin id.
- [ ] Existing power plugin UI and IPC remain compatible.
- [ ] Fake plugin proves the actual UDS wire.
- [ ] Full Rust/UI tests pass and no token appears in logs/status/argv.

## Deferred deliberately to subsequent v2 increments

- Agent VM inventory, `avm`/`limactl` lifecycle and `.agent-vm.yaml`;
- Keychain-backed `SecretStore`, Claude/Codex config mirroring and guest bootstrap;
- Claude/Codex headless stream adapters, run journal/replay and normalized chat events;
- Project Manager active-VM rail, workspace/chat/result/files/diff UI;
- lifecycle notifications, pinned-project autostart and recovery UX.

Those are not optional scope reductions: they are the next ordered increments in the approved
v2 spec, built on the protocol established here.
