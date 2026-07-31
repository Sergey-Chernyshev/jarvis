# Host Power Exit Safety Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Guarantee that Jarvis closes admission and releases every Jarvis-owned macOS sleep blocker before slower shutdown work, including GUI, headless and SIGTERM paths, with durable recovery for clamshell state.

**Architecture:** A pure ownership state machine records baseline, mutation and per-profile leases in one user-global registry. `Power` adds a one-way shutdown gate, while `shutdown::cleanup` owns deterministic teardown ordering. The first shipment fixes all graceful paths and startup recovery; a signed renewable watchdog/helper then closes the SIGKILL gap without granting arbitrary root commands.

**Tech Stack:** Rust, Tokio, serde/serde_json, libc file locking, macOS `pmset`, IOKit, Tauri RunEvent, launchd/XPC helper packaging, Cargo unit/integration tests.

---

### Task 1: Pure ownership registry and restore decisions

**Files:**

- Create: `src-tauri/src/power/ownership.rs`
- Modify: `src-tauri/src/power/mod.rs`
- Test: `src-tauri/src/power/ownership.rs`

- [ ] **Step 1: Write failing state-machine tests**

Add tests for a first lease, a second profile lease, last-lease restore,
baseline-already-on and stale lease removal:

```rust
#[test]
fn last_mutating_lease_restores_original_baseline() {
    let mut state = OwnershipState::new(false, "boot-a", 7);
    state.acquire(Lease::test("prod", 101, "a"));
    state.acquire(Lease::test("dev", 202, "b"));
    assert_eq!(state.release("prod", "a"), ReleaseDecision::KeepApplied);
    assert_eq!(state.release("dev", "b"), ReleaseDecision::Restore(false));
}

#[test]
fn baseline_already_on_is_never_changed() {
    let mut state = OwnershipState::new(true, "boot-a", 7);
    state.acquire(Lease::test("prod", 101, "a"));
    assert_eq!(state.release("prod", "a"), ReleaseDecision::ClearWithoutMutation);
}

#[test]
fn dead_leases_are_removed_before_recovery() {
    let mut state = OwnershipState::new(false, "boot-a", 7);
    state.acquire(Lease::test("prod", 101, "dead"));
    assert_eq!(
        state.recover(|lease| lease.pid != 101),
        RecoveryDecision::Restore(false)
    );
}
```

- [ ] **Step 2: Run the focused test and verify failure**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml power::ownership --no-default-features
```

Expected: compilation fails because `power::ownership` and its types do not
exist.

- [ ] **Step 3: Implement the minimal serializable state machine**

Create these public types and methods:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Lease {
    pub profile: String,
    pub pid: u32,
    pub process_identity: String,
    pub owner_generation: String,
    pub acquired_at_ms: i64,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipState {
    pub schema_version: u32,
    pub boot_id: String,
    pub baseline: bool,
    pub applied: bool,
    pub did_mutate: bool,
    pub generation: u64,
    pub leases: Vec<Lease>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseDecision {
    KeepApplied,
    Restore(bool),
    ClearWithoutMutation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryDecision {
    KeepApplied,
    Restore(bool),
    ClearWithoutMutation,
}
```

`OwnershipState::new(baseline, boot_id, generation)` sets
`did_mutate = !baseline`, `applied = true` and an empty lease list. `acquire`
replaces only an identical
`(profile, owner_generation)` lease. `release` removes exactly that owner and
returns `KeepApplied` while another lease exists. `recover` retains leases for
which the supplied predicate returns true and applies the same last-lease
decision.

- [ ] **Step 4: Run focused tests**

Run the command from Step 2.

Expected: all `power::ownership` tests pass.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/power/ownership.rs src-tauri/src/power/mod.rs
git commit -m "feat(power): model shared sleep ownership"
```

### Task 2: Atomic global registry store

**Files:**

- Create: `src-tauri/src/power/ownership_store.rs`
- Modify: `src-tauri/src/power/mod.rs`
- Test: `src-tauri/src/power/ownership_store.rs`

- [ ] **Step 1: Write failing atomic-store tests**

Use a unique directory under `std::env::temp_dir()` and assert round-trip,
corrupt JSON quarantine and exclusive locking:

```rust
#[test]
fn atomic_round_trip_preserves_registry() {
    let dir = unique_test_dir("round-trip");
    let store = OwnershipStore::at(dir.join("ownership.json"));
    let expected = OwnershipState::new(false, "boot-a", 3);
    store.write(&expected).unwrap();
    assert_eq!(store.read().unwrap(), Some(expected));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn corrupt_registry_is_fail_closed() {
    let dir = unique_test_dir("corrupt");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("ownership.json"), b"{").unwrap();
    let store = OwnershipStore::at(dir.join("ownership.json"));
    assert!(matches!(store.read(), Err(StoreError::Corrupt(_))));
    std::fs::remove_dir_all(dir).unwrap();
}
```

- [ ] **Step 2: Run the focused test and verify failure**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml power::ownership_store --no-default-features
```

Expected: compilation fails because `OwnershipStore` does not exist.

- [ ] **Step 3: Implement locking and fsynced atomic replace**

The store path is:

```rust
pub fn global_registry_path() -> PathBuf {
    crate::util::home_dir()
        .join("Library/Application Support/Jarvis/power/ownership.json")
}
```

Implement `OwnershipStore::lock()` with an adjacent `ownership.lock`,
`OpenOptionsExt::mode(0o600)` and `libc::flock(LOCK_EX)`. Implement `write` as
`create_new` temporary file → `write_all` → `sync_all` → `rename` → parent
directory `sync_all`. `read` returns `StoreError::Corrupt` for invalid JSON and
never silently replaces it. `clear` removes the registry only after confirmed
restore and fsyncs the parent.

- [ ] **Step 4: Run focused tests**

Run the command from Step 2.

Expected: round-trip, corrupt-state and lock tests pass.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/power/ownership_store.rs src-tauri/src/power/mod.rs
git commit -m "feat(power): persist global ownership atomically"
```

### Task 3: Safe clamshell acquire/release transaction

**Files:**

- Modify: `src-tauri/src/power/clamshell.rs`
- Modify: `src-tauri/src/power/mod.rs`
- Test: `src-tauri/src/power/clamshell.rs`

- [ ] **Step 1: Write failing backend transaction tests**

Introduce a fake `PmsetBackend` and test write-ahead ordering, read-back,
baseline-on and rollback failure:

```rust
#[test]
fn acquire_writes_registry_before_mutation() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let backend = FakePmset::with_trace(false, trace.clone());
    let store = TracingStore::new(test_store(), trace.clone());
    let outcome = acquire_with(&backend, &store, test_lease()).unwrap();
    assert_eq!(outcome, AcquireOutcome::Mutated);
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        ["read:0", "preflight:0", "store:write", "set:1", "read:1"]
    );
}

#[test]
fn acquire_refuses_when_noninteractive_restore_is_unavailable() {
    let backend = FakePmset::without_rollback(false);
    assert!(matches!(
        acquire_with(&backend, &test_store(), test_lease()),
        Err(PowerError::RollbackUnavailable)
    ));
    assert_eq!(backend.current(), false);
}

#[test]
fn baseline_on_does_not_write_zero_on_release() {
    let backend = FakePmset::new(true);
    acquire_with(&backend, &test_store(), test_lease()).unwrap();
    release_with(&backend, &test_store(), "prod", "generation").unwrap();
    assert_eq!(backend.current(), true);
}
```

- [ ] **Step 2: Run tests and verify failure**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml power::clamshell --no-default-features
```

Expected: compilation fails for `PmsetBackend`, `acquire_with` and
`release_with`.

- [ ] **Step 3: Implement the backend and transaction**

Add:

```rust
pub trait PmsetBackend {
    fn read_disabled(&self) -> Result<bool, PowerError>;
    fn can_restore_noninteractive(&self) -> bool;
    fn set_disabled(&self, value: bool) -> Result<(), PowerError>;
}

pub enum AcquireOutcome {
    Mutated,
    Joined,
    BaselineAlreadyOn,
}
```

Production `SystemPmset` uses bounded synchronous `pmset -g` and
`sudo -n /usr/bin/pmset -a disablesleep <0|1>`. `acquire_with` holds the global
store lock across baseline read, fsynced write-ahead registry, mutation and
read-back. If mutation/read-back fails, it restores baseline; the registry is
cleared only after confirmed restore. `release_with` removes only the calling
lease and restores baseline only for the last mutating lease.

Delete the old profile-local write-after-mutation `write_marker` path. Keep a
legacy reader for `<jarvis_dir>/clamshell.json`; legacy recovery may restore
`false` once, then removes that old marker after read-back.

- [ ] **Step 4: Run clamshell and ownership tests**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml power:: --no-default-features
```

Expected: all power tests pass; no test invokes real `sudo` or `pmset`.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/power/clamshell.rs src-tauri/src/power/mod.rs
git commit -m "fix(power): make clamshell ownership transactional"
```

### Task 4: One-way shutdown admission gate

**Files:**

- Modify: `src-tauri/src/power/mod.rs`
- Test: `src-tauri/src/power/mod.rs`

- [ ] **Step 1: Write failing gate tests**

```rust
#[test]
fn shutdown_gate_is_one_way_and_idempotent() {
    let gate = ShutdownGate::default();
    assert!(gate.accepting());
    assert!(gate.close());
    assert!(!gate.accepting());
    assert!(!gate.close());
}
```

- [ ] **Step 2: Run and verify failure**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml power::tests::shutdown_gate --no-default-features
```

Expected: compilation fails because `ShutdownGate` is undefined.

- [ ] **Step 3: Implement and apply the gate**

Add:

```rust
#[derive(Default)]
struct ShutdownGate(AtomicBool);

impl ShutdownGate {
    fn accepting(&self) -> bool {
        !self.0.load(Ordering::Acquire)
    }

    fn close(&self) -> bool {
        !self.0.swap(true, Ordering::AcqRel)
    }
}
```

Store it on `Power`. `arm`, `peer_sync`, keep-awake commands and async callbacks
must reject/no-op after `close`. `Power::dispose` closes the gate first,
disposes the IOKit engine, then calls registry-backed clamshell release even
when in-memory `clam.active` or `clam.armed` is false.

- [ ] **Step 4: Run all power tests**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml power:: --no-default-features
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/power/mod.rs
git commit -m "fix(power): block rearm during shutdown"
```

### Task 5: Deterministic cleanup order for every exit path

**Files:**

- Modify: `src-tauri/src/shutdown.rs`
- Modify: `src-tauri/src/main.rs`
- Test: `src-tauri/src/shutdown.rs`

- [ ] **Step 1: Write a failing order/idempotency test**

```rust
#[test]
fn cleanup_runs_power_before_blocking_subsystems_once() {
    let trace = RefCell::new(Vec::new());
    let gate = CleanupGate::default();
    run_ordered_once(
        &gate,
        || trace.borrow_mut().push("power"),
        || trace.borrow_mut().push("rest"),
    );
    run_ordered_once(
        &gate,
        || trace.borrow_mut().push("power"),
        || trace.borrow_mut().push("rest"),
    );
    assert_eq!(trace.into_inner(), ["power", "rest"]);
}
```

- [ ] **Step 2: Run and verify failure**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml shutdown:: --no-default-features
```

Expected: compilation fails because `CleanupGate` and `run_ordered_once` do not
exist.

- [ ] **Step 3: Centralize production cleanup**

Implement:

```rust
pub fn cleanup(d: &Arc<Daemon>) {
    if !CLEANUP_GATE.close() {
        return;
    }
    Power::dispose(d);
    d.write_state_now();
    d.plugins.dispose(d);
    d.voice.dispose();
    d.stt.dispose();
    d.wake.dispose();
    d.audio.dispose();
    let _ = std::fs::remove_file(crate::util::sock_path());
}
```

Call only `shutdown::cleanup(&Daemon::get(app))` from `RunEvent::Exit`. Remove
the `!is_headless()` power condition. Keep the merged SIGTERM handler calling
`app.exit(0)`, so service-manager/logout termination reaches the same cleanup.

- [ ] **Step 4: Run shutdown and power tests**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml shutdown:: --no-default-features
cargo test --manifest-path src-tauri/Cargo.toml power:: --no-default-features
```

Expected: tests pass and the order trace starts with `power`.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/shutdown.rs src-tauri/src/main.rs
git commit -m "fix(shutdown): restore power before subsystem teardown"
```

### Task 6: Recover stale state before headless branching

**Files:**

- Modify: `src-tauri/src/power/clamshell.rs`
- Modify: `src-tauri/src/main.rs`
- Test: `src-tauri/src/power/clamshell.rs`

- [ ] **Step 1: Write failing recovery tests**

Cover a stale mutating lease, another live profile lease, a corrupt registry
and the old profile-local marker:

```rust
#[test]
fn startup_keeps_state_while_another_profile_lease_is_live() {
    let backend = FakePmset::new(true);
    let store = store_with_two_leases();
    let result = recover_with(&backend, &store, |lease| lease.profile == "dev").unwrap();
    assert_eq!(result, RecoveryOutcome::KeptForLiveLease);
    assert!(backend.current());
}

#[test]
fn corrupt_registry_refuses_new_arm() {
    let backend = FakePmset::new(false);
    let store = corrupt_store();
    assert!(matches!(
        recover_with(&backend, &store, |_| false),
        Err(PowerError::CorruptOwnership(_))
    ));
}
```

- [ ] **Step 2: Run and verify failure**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml power::clamshell --no-default-features
```

Expected: recovery tests fail because `recover_with` is missing.

- [ ] **Step 3: Run recovery at the start of Tauri setup**

Implement `recover_with` using `OwnershipState::recover`. Add
`clamshell::recover_on_startup()` immediately after
`install::prepare_clean_start()` and before settings, bundled-plugin install,
Daemon creation and the `is_headless()` early return. Log each explicit
outcome; leave corrupt/unrestorable state intact and expose a repairable health
error instead of arming again.

- [ ] **Step 4: Run tests**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml power:: --no-default-features
cargo test --manifest-path src-tauri/Cargo.toml shutdown:: --no-default-features
```

Expected: all focused tests pass.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/power/clamshell.rs src-tauri/src/main.rs
git commit -m "fix(power): recover before headless startup"
```

### Task 7: Renewable watchdog/helper lease

**Files:**

- Create: `src-tauri/src/power/helper_protocol.rs`
- Create: `src-tauri/src/bin/jarvis-power-helper.rs`
- Modify: `src-tauri/src/power/mod.rs`
- Modify: `src-tauri/src/power/clamshell.rs`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/tauri.conf.json`
- Test: `src-tauri/src/power/helper_protocol.rs`

- [ ] **Step 1: Write failing protocol and expiry tests**

```rust
#[test]
fn expired_last_lease_requires_restore() {
    let state = HelperState::with_lease(false, lease_expiring_at(100));
    assert_eq!(state.tick(101), HelperAction::Restore(false));
}

#[test]
fn caller_cannot_supply_arbitrary_pmset_arguments() {
    let json = r#"{"v":1,"method":"run","args":["rm","-rf"]}"#;
    assert!(serde_json::from_str::<Request>(json).is_err());
}

#[test]
fn renew_requires_matching_generation() {
    let mut state = HelperState::with_lease(false, lease_for("prod", "a"));
    assert_eq!(
        state.renew("prod", "b", 500),
        Err(ProtocolError::GenerationMismatch)
    );
}
```

- [ ] **Step 2: Run and verify failure**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml power::helper_protocol --no-default-features
```

Expected: compilation fails because the protocol module does not exist.

- [ ] **Step 3: Implement the closed protocol**

Define only:

```rust
#[serde(tag = "method", rename_all = "camelCase")]
pub enum Request {
    Acquire { v: u32, profile: String, generation: String, ttl_ms: u64 },
    Renew { v: u32, lease_id: String, generation: String, ttl_ms: u64 },
    Release { v: u32, lease_id: String, generation: String },
    Status { v: u32 },
    RecoverExpired { v: u32 },
}
```

Responses contain no command/path fields. Enforce protocol version, bounded
IDs, `1s..120s` TTL and monotonic deadlines. The helper binary owns the global
registry lock, validates peer UID and the packaged caller identity, executes
only fixed `pmset disablesleep 0|1`, and restores baseline on last release or
TTL expiry. Add a 30-second app renewal task and make loss of renewal visible
in power status.

For the initial development build, the helper uses the existing exact-command
sudoers rule and private `0600` UDS; release packaging must use the signed
privileged-service path from the design spec before AC22 is marked complete.

- [ ] **Step 4: Run helper and power tests**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml power:: --no-default-features
cargo build --manifest-path src-tauri/Cargo.toml --no-default-features --bin jarvis-power-helper
```

Expected: tests pass and helper binary builds.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/power/helper_protocol.rs src-tauri/src/bin/jarvis-power-helper.rs src-tauri/src/power/mod.rs src-tauri/src/power/clamshell.rs src-tauri/Cargo.toml src-tauri/tauri.conf.json
git commit -m "feat(power): recover expired clamshell leases"
```

### Task 8: UI repair actions and documentation

**Files:**

- Create: `ui/power-state.js`
- Create: `ui/power-state.test.mjs`
- Modify: `src-tauri/src/power/mod.rs`
- Modify: `ui/index.html`
- Modify: `ui/renderer.js`
- Modify: `README.md`

- [ ] **Step 1: Write a failing UI-state test**

```javascript
import test from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { repairView } = require('./power-state.js');

test('clamshell repair state explains blocked safe restore', () => {
  assert.deepEqual(repairView({
    health: 'blocked_restore',
    repairAction: 'power.repair',
  }), {
    visible: true,
    message: 'Не удалось безопасно вернуть сон',
    actionLabel: 'Починить',
    action: 'power.repair',
  });
});
```

- [ ] **Step 2: Run and verify failure**

Run:

```bash
node --test ui/power-state.test.mjs
```

Expected: the new test fails because the repair state is not rendered.

- [ ] **Step 3: Expose and render actionable health**

Add `health`, `healthMessage` and `repairAction` to the clamshell status DTO.
Render `blocked_restore`, `helper_unavailable` and `corrupt_ownership` with a
host action; do not label the mode off while `SleepDisabled=1`. Document that
safe clamshell requires the exact-command helper/sudoers rule and that Jarvis
never kills external Amphetamine/caffeinate processes.

Implement the pure browser/Node helper:

```javascript
function repairView(status) {
  const messages = {
    blocked_restore: 'Не удалось безопасно вернуть сон',
    helper_unavailable: 'Безопасный помощник питания недоступен',
    corrupt_ownership: 'Состояние режима сна повреждено',
  };
  const message = status && messages[status.health];
  return message
    ? { visible: true, message, actionLabel: 'Починить', action: status.repairAction || 'power.repair' }
    : { visible: false, message: '', actionLabel: '', action: '' };
}
```

Expose it as `window.JarvisPowerState.repairView` and `module.exports` for the
Node test, load `power-state.js` before `renderer.js`, and render its result
with DOM `textContent`.

- [ ] **Step 4: Run UI and Rust focused tests**

Run:

```bash
node --test ui/power-state.test.mjs
cargo test --manifest-path src-tauri/Cargo.toml power:: --no-default-features
cargo test --manifest-path src-tauri/Cargo.toml shutdown:: --no-default-features
```

Expected: both commands pass.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/power/mod.rs ui/index.html ui/renderer.js ui/power-state.js ui/power-state.test.mjs README.md
git commit -m "feat(power): expose safe restore health"
```

### Task 9: Full validation and live macOS smoke

**Files:**

- Create: `scripts/smoke-power-exit.sh`
- Modify: `.github/workflows/ci.yml`
- Test: `scripts/smoke-power-exit.sh`

- [ ] **Step 1: Add a non-destructive smoke harness**

The script must record the initial `SleepDisabled` and Jarvis IOKit assertion
count, launch an isolated `JARVIS_DIR`, exercise normal exit, headless exit and
SIGTERM, and restore the recorded baseline in an EXIT trap. It refuses to run
the clamshell mutation portion unless noninteractive restore preflight passes.

Start with this fail-safe shell:

```bash
#!/usr/bin/env bash
set -euo pipefail

initial_sleep_disabled="$(pmset -g | awk '/SleepDisabled/{print $2; exit}')"
initial_sleep_disabled="${initial_sleep_disabled:-0}"
smoke_dir="$(mktemp -d "${TMPDIR:-/tmp}/jarvis-power-smoke.XXXXXX")"
jarvis_pid=""

cleanup() {
  if [[ -n "${jarvis_pid}" ]] && kill -0 "${jarvis_pid}" 2>/dev/null; then
    kill -TERM "${jarvis_pid}" 2>/dev/null || true
    wait "${jarvis_pid}" 2>/dev/null || true
  fi
  if sudo -n /usr/bin/pmset -a disablesleep "${initial_sleep_disabled}" 2>/dev/null; then
    :
  elif [[ "$(pmset -g | awk '/SleepDisabled/{print $2; exit}')" != "${initial_sleep_disabled}" ]]; then
    echo "manual recovery required: SleepDisabled baseline was ${initial_sleep_disabled}" >&2
  fi
  rm -rf "${smoke_dir}"
}
trap cleanup EXIT INT TERM
```

Use only the explicit `smoke_dir` for cleanup. Never signal a PID unless it is
the direct process started by the script. Compare `pmset -g assertions` lines
containing `Jarvis: не спать` before/after each exit.

- [ ] **Step 2: Validate shell syntax before execution**

Run:

```bash
bash -n scripts/smoke-power-exit.sh
```

Expected: exit `0`.

- [ ] **Step 3: Run automated suites**

Run:

```bash
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --no-default-features -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features
node --test ui/*.test.mjs
git diff --check
```

Expected: every command exits `0`.

- [ ] **Step 4: Run live smoke on this Mac**

Run:

```bash
bash scripts/smoke-power-exit.sh
```

Expected: each tested exit reports no Jarvis IOKit assertion and
`SleepDisabled` equals the captured baseline. External processes are listed
but never signalled.

- [ ] **Step 5: Commit evidence and CI wiring**

```bash
git add scripts/smoke-power-exit.sh .github/workflows/ci.yml
git commit -m "test(power): verify exit restores macOS sleep"
```
