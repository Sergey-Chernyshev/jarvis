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
legacy reader for `<jarvis_dir>/clamshell.json`, but treat that marker as
ambiguous: it has no recorded baseline and cannot prove a Jarvis-owned
`0 → 1` mutation. Legacy recovery must therefore never write `false`
automatically. It reports blocked/manual repair and keeps the marker until an
explicit repair can establish ownership or the user confirms the restore.

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

#[test]
fn acquire_that_finishes_after_close_is_rolled_back() {
    let harness = PausedAcquire::new();
    let epoch = harness.begin();
    harness.close_shutdown();
    harness.finish_backend_acquire(epoch);
    assert!(!harness.lease_present());
    assert_eq!(harness.current_sleep_disabled(), harness.baseline());
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

Store it on `Power` together with an operation epoch. `arm`, `peer_sync`,
keep-awake commands and async callbacks must reject/no-op after `close`.
Every async acquire captures the current epoch before `spawn_blocking` and
checks it again after completion. If shutdown closed or advanced the epoch
while the backend was running, the just-acquired exact lease is synchronously
released and read-back must confirm the baseline before the callback returns.
Serialize acquire/release with a bounded operation barrier so cleanup cannot
race a late mutation indefinitely.

`Power::dispose` closes the gate and advances the epoch first, disposes the
IOKit engine, waits only for the bounded power-operation barrier, then calls
registry-backed clamshell release even when in-memory `clam.active` or
`clam.armed` is false. The barrier test must deterministically pause an acquire
between admission and completion; it may not use sleeps.

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
- Modify: `src-tauri/src/tray.rs`
- Modify: `src-tauri/src/ipc.rs`
- Test: `src-tauri/src/shutdown.rs`

- [ ] **Step 1: Write a failing order/idempotency test**

```rust
#[test]
fn cleanup_runs_power_before_blocking_subsystems_and_retries_failed_phase() {
    let trace = RefCell::new(Vec::new());
    let state = CleanupState::default();
    run_ordered(
        &state,
        || {
            trace.borrow_mut().push("power-failed");
            Err("blocked")
        },
        || trace.borrow_mut().push("rest"),
    );
    run_ordered(
        &state,
        || {
            trace.borrow_mut().push("power-retry");
            Ok(())
        },
        || trace.borrow_mut().push("rest"),
    );
    assert_eq!(
        trace.into_inner(),
        ["power-failed", "rest", "power-retry"]
    );
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

Implement a structured, per-phase cleanup state rather than one whole-cleanup
boolean. Admission closure and IOKit disposal are once-only; an unsuccessful
clamshell restore remains retryable on a later fallback hook. Every phase has
its own bounded deadline and failure logging, and failure in one subsystem
does not skip the rest:

```rust
pub fn cleanup(d: &Arc<Daemon>) -> CleanupReport {
    let power = Power::dispose(d);
    d.write_state_now();
    d.plugins.dispose(d);
    d.voice.dispose();
    d.stt.dispose();
    d.wake.dispose();
    d.audio.dispose();
    let _ = std::fs::remove_file(crate::util::sock_path());
    CleanupReport { power, /* per-phase outcomes */ }
}
```

Add Jarvis-owned `shutdown::request_exit` and `shutdown::request_restart`
wrappers which call cleanup before asking Tauri to terminate/relaunch. Use
them from the tray, SIGTERM and updater IPC. Replace `app.restart()` with
event-preserving `request_restart()` only after cleanup. Keep
`RunEvent::Exit` as an idempotent fallback, not the sole owner of cleanup;
Tauri restart on the main thread may skip normal exit events. Remove the
`!is_headless()` power condition. Window close remains hide-only and is tested
separately from application quit.

- [ ] **Step 4: Run shutdown and power tests**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml shutdown:: --no-default-features
cargo test --manifest-path src-tauri/Cargo.toml power:: --no-default-features
```

Expected: tests pass and the order trace starts with `power`.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/shutdown.rs src-tauri/src/main.rs src-tauri/src/tray.rs src-tauri/src/ipc.rs
git commit -m "fix(shutdown): restore power before subsystem teardown"
```

### Task 6: Recover stale state before headless branching

**Files:**

- Modify: `src-tauri/src/power/clamshell.rs`
- Modify: `src-tauri/src/power/mod.rs`
- Modify: `src-tauri/src/main.rs`
- Test: `src-tauri/src/power/clamshell.rs`
- Test: `src-tauri/src/power/mod.rs`

- [x] **Step 1: Write failing recovery tests**

Cover a stale mutating lease, another live profile lease, PID reuse,
cross-boot state, expiry, a corrupt registry and the old profile-local marker:

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

#[test]
fn reused_pid_with_different_start_identity_is_not_live() {
    let lease = lease_for_process(123, "start-a");
    let inspector = FakeProcesses::alive(123, "start-b");
    assert!(!inspector.matches(&lease));
}

#[test]
fn expired_but_identity_matching_lease_blocks_until_helper_renewal_exists() {
    let lease = lease_for_process(123, "start-a").expired_at(100);
    let inspector = FakeProcesses::alive(123, "start-a");
    assert_eq!(
        classify_lease(&inspector, &lease, 101),
        LeaseLiveness::ExpiredButLive
    );
}

#[test]
fn ambiguous_legacy_marker_is_never_mutated_or_cleared_automatically() {
    let backend = FakePmset::new(true);
    recover_legacy_with(&backend, legacy_marker()).unwrap();
    assert!(backend.current());
    assert_eq!(backend.set_calls(), vec![]);
    assert!(legacy_marker_still_exists());
}
```

- [x] **Step 2: Run and verify failure**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml power::clamshell --no-default-features
```

Expected: recovery tests fail because `recover_with` is missing.

- [x] **Step 3: Run recovery at the start of Tauri setup**

Implement `recover_with` using `OwnershipState::recover` and an injected
`ProcessInspector`. On Darwin, process identity is derived from
`proc_pidinfo(PROC_PIDTBSDINFO)` start time plus UID in a versioned string; PID
existence, profile equality or the provisional `pid:acquiredAt` string are
never sufficient. Zombies are stale. Permission errors, partial reads and
unknown identity formats are ambiguous and fail closed. Cross-boot state cannot
retain leases.

The current five-minute TTL is not authoritative until Task 7 adds autonomous
helper renewal. Before that helper exists, `expired + exact live PID/start
identity` is a blocked health state, not permission to restore under a live
profile. `expired + proven dead/mismatched identity` is stale and recoverable.
Task 7 moves expiry authority to the renewable helper.

`power_lease()` must record the real process start identity. A blocking startup
recovery result is stored in process-global power health and makes `arm`
fail-closed until a later explicit repair. The old legacy-marker callback must
not clear a marker merely because `SleepDisabled=0`: a legacy marker has no
baseline proof, so present/corrupt markers are observation-only blocked repair
states and are never automatically mutated or deleted.

Add
`power::recover_on_startup()` immediately after
`install::prepare_clean_start()` and before settings, bundled-plugin install,
Daemon creation and the `is_headless()` early return. Log each explicit
outcome; leave corrupt/unrestorable state intact and expose a repairable health
error instead of arming again.

- [x] **Step 4: Run tests**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml power:: --no-default-features
cargo test --manifest-path src-tauri/Cargo.toml shutdown:: --no-default-features
```

Expected: all focused tests pass.

- [x] **Step 5: Commit**

```bash
git add src-tauri/src/power/clamshell.rs src-tauri/src/power/mod.rs src-tauri/src/main.rs
git commit -m "fix(power): recover before headless startup"
```

### Task 7: Renewable watchdog/helper lease and production release gate

> Superseded by the executable hardened plan in
> `docs/superpowers/plans/2026-08-01-power-helper-watchdog.md`. That plan locks
> the minimum OS to macOS 13, removes all production fallback paths, separates
> shared core/dev UDS/attested XPC verticals, and adds migration, updater,
> uninstall, signing, notarization, and live-smoke gates. Keep this section only
> as the original high-level context.

**Files:**

- Create: `src-tauri/power-core/Cargo.toml`
- Create: `src-tauri/power-core/src/lib.rs`
- Create: `src-tauri/src/power/helper_protocol.rs`
- Create: `src-tauri/src/bin/jarvis-power-helper.rs`
- Create: `src-tauri/PowerHelper/` signed XPC/SMAppService service sources
- Create: `src-tauri/Resources/com.sergey-chernyshev.jarvis.power-helper.plist`
- Modify: `src-tauri/src/power/mod.rs`
- Modify: `src-tauri/src/power/clamshell.rs`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/tauri.conf.json`
- Modify: release/signing workflow
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
unpredictable lease IDs, `1s..120s` TTL, boot identity and monotonic deadlines.
Renew may update only a still-live matching lease and can never recreate a
released lease. The helper runs an autonomous expiry loop, owns the only
post-migration global registry writer, and restores baseline without waiting
for another request. Add a 30-second app renewal task and make loss of renewal
visible in power status.

Put serialization/state-machine primitives in the minimal shared
`power-core` crate so the app and helper cannot drift. The development
`jarvis-power-helper` may use the existing exact-command sudoers rule and a
private `0600` UDS, but it is explicitly non-release and proves only UID/GID.

The production path is a signed XPC service installed and managed through
`SMAppService`, packaged under `Contents/Library/LaunchDaemons`, and validates
the caller's designated requirement/audit token on every connection. Add
install/status/update/uninstall and fenced handoff tests; update/uninstall
must restore and verify baseline or refuse to proceed. Record and enforce the
minimum-macOS decision (raise to macOS 13 for `SMAppService`, or implement and
test an older-system fallback). Persistent clamshell mutation remains disabled
in release builds until the signed service and its code-signing verification
are present; the development UDS helper never satisfies AC22.

- [ ] **Step 4: Run helper and power tests**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml power:: --no-default-features
cargo build --manifest-path src-tauri/Cargo.toml --no-default-features --bin jarvis-power-helper
```

Expected: tests pass, the development helper binary builds, helper package
layout/signing checks pass, and release configuration fails closed when the
signed service is absent.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/power-core src-tauri/src/power/helper_protocol.rs src-tauri/src/bin/jarvis-power-helper.rs src-tauri/PowerHelper src-tauri/Resources src-tauri/src/power/mod.rs src-tauri/src/power/clamshell.rs src-tauri/Cargo.toml src-tauri/tauri.conf.json .github/workflows
git commit -m "feat(power): recover expired clamshell leases"
```

### Task 8: UI repair actions and documentation

**Files:**

- Create: `ui/power-state.js`
- Create: `ui/power-state.test.mjs`
- Modify: `src-tauri/src/power/mod.rs`
- Modify: `ui/index.html`
- Modify: `ui/settings2.js`
- Modify: `ui/renderer.js`
- Modify: `README.md`

- [ ] **Step 1: Write a failing UI-state test**

```javascript
import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { repairView } = require("./power-state.js");

test("clamshell repair state explains blocked safe restore", () => {
  assert.deepEqual(
    repairView({
      health: "blocked_restore",
      repairAction: "power.repair",
    }),
    {
      visible: true,
      message: "Не удалось безопасно вернуть сон",
      actionLabel: "Починить",
      action: "power.repair",
    },
  );
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
Distinguish `jarvis_owned`, `held_by_other_profile`, `external_baseline_on`,
`blocked_restore`, `helper_unavailable` and `corrupt_ownership`. Render unsafe
health through the active `settings2.js` page as well as the compact renderer;
do not label the mode off while `SleepDisabled=1`.

Implement an allowlisted `clamshell/repair` backend command; the JS action
cannot dispatch an arbitrary command. Repair may mutate only when the durable
registry proves Jarvis ownership or after a separate explicit user-confirmed
legacy flow. `SleepDisabled=1` by itself never enables a destructive action.
Document that safe clamshell requires the signed helper (the exact-command
sudoers path is development-only) and that Jarvis never kills external
Amphetamine/caffeinate processes.

Implement the pure browser/Node helper:

```javascript
function repairView(status) {
  const messages = {
    blocked_restore: "Не удалось безопасно вернуть сон",
    helper_unavailable: "Безопасный помощник питания недоступен",
    corrupt_ownership: "Состояние режима сна повреждено",
  };
  const message = status && messages[status.health];
  return message
    ? {
        visible: true,
        message,
        actionLabel: "Починить",
        action: status.repairAction || "power.repair",
      }
    : { visible: false, message: "", actionLabel: "", action: "" };
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
git add src-tauri/src/power/mod.rs ui/index.html ui/settings2.js ui/renderer.js ui/power-state.js ui/power-state.test.mjs README.md
git commit -m "feat(power): expose safe restore health"
```

### Task 9: Full validation and live macOS smoke

**Files:**

- Create: `scripts/smoke-power-exit.sh`
- Modify: `.github/workflows/ci.yml`
- Test: `scripts/smoke-power-exit.sh`

- [ ] **Step 1: Add a non-destructive smoke harness**

The script records an exactly parsed initial `SleepDisabled` and Jarvis IOKit
assertion count, launches an isolated `JARVIS_DIR`, and exercises normal exit,
headless exit, SIGTERM and updater-restart cleanup. Unknown baseline aborts
before launch. The default smoke never mutates persistent power state.

Start with this fail-safe shell:

```bash
#!/usr/bin/env bash
set -euo pipefail

initial_sleep_disabled="$(pmset -g | awk '/SleepDisabled/{print $2; exit}')"
case "${initial_sleep_disabled}" in
  0|1) ;;
  *) echo "cannot parse SleepDisabled baseline; refusing smoke" >&2; exit 2 ;;
esac
smoke_dir="$(mktemp -d "${TMPDIR:-/tmp}/jarvis-power-smoke.XXXXXX")"
jarvis_pid=""

cleanup() {
  if [[ -n "${jarvis_pid}" ]] && kill -0 "${jarvis_pid}" 2>/dev/null; then
    kill -TERM "${jarvis_pid}" 2>/dev/null || true
    wait "${jarvis_pid}" 2>/dev/null || true
  fi
  current="$(pmset -g | awk '/SleepDisabled/{print $2; exit}')"
  if [[ "${current}" != "${initial_sleep_disabled}" ]]; then
    echo "power state drifted; recover through Jarvis helper lease, never raw pmset" >&2
  fi
  rm -rf -- "${smoke_dir}"
}
trap cleanup EXIT INT TERM
```

Use only the explicit `smoke_dir` for cleanup. Never signal a PID unless it is
the still-tracked direct shell job started by the script; do not trust a
detached/reused numeric PID. Compare the count of `pmset -g assertions` lines
containing `Jarvis: не спать` with the captured baseline rather than assuming
zero.

The live clamshell mutation portion is a separate explicit opt-in
(`JARVIS_POWER_LIVE_SMOKE=1`) and acquires/releases only through the installed
production helper protocol. It never invokes raw `sudo pmset`, including from
an EXIT trap, and is never enabled in CI. A controlled fixture must acquire a
real helper lease/assertion before testing headless exit; a plain headless
launch may otherwise hold nothing.

- [ ] **Step 2: Validate shell syntax before execution**

Run:

```bash
bash -n scripts/smoke-power-exit.sh
```

Expected: exit `0`.

- [ ] **Step 3: Run automated suites**

Run:

```bash
rustfmt --edition 2021 --check src-tauri/src/power/*.rs src-tauri/src/shutdown.rs
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --no-default-features -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features
node --test ui/*.test.mjs
git diff --check
```

Expected: focused formatting for changed Rust files, clippy/tests/UI tests and
diff checks pass. Repository-wide formatting remains informational if existing
untouched files are not rustfmt-clean; do not churn unrelated code.

- [ ] **Step 4: Run live smoke on this Mac**

Run:

```bash
JARVIS_POWER_LIVE_SMOKE=1 bash scripts/smoke-power-exit.sh
```

Expected: each tested exit reports no Jarvis IOKit assertion and
`SleepDisabled` equals the captured baseline. External processes are listed
but never signalled.

- [ ] **Step 5: Commit evidence and CI wiring**

```bash
git add scripts/smoke-power-exit.sh .github/workflows/ci.yml
git commit -m "test(power): verify exit restores macOS sleep"
```
