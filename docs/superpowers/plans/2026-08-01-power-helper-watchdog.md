# Hardened Power Helper and Watchdog Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace Jarvis's app-owned persistent `pmset disablesleep` path with a signed, attested, renewable
root-helper lease that autonomously restores the original macOS power state after app exit, crash, `SIGKILL`, helper
restart, update, or uninstall.

**Architecture:** A pure Rust `jarvis-power-core` crate owns the closed protocol and crash-recoverable state machine.
A development-only Unix-domain-socket helper exercises the same core. Production uses a notarized `SMAppService`
LaunchDaemon and XPC transport that re-attests every request from its XPC message. The helper is the only v2 registry
writer and production process allowed to mutate persistent `disablesleep`; normal IOKit assertions remain app-local.

**Tech Stack:** Rust 2021, serde/JSON, Unix domain sockets, Darwin XPC, Security.framework, ServiceManagement.framework,
launchd, `pmset`, Tauri 2, GitHub Actions, `codesign`, `notarytool`, and Cargo tests.

---

## Locked decisions

This plan supersedes the coarse Task 7 in
`docs/superpowers/plans/2026-07-31-host-power-exit-safety.md`. Its approved Tasks 1-6 remain prerequisites.

### Platform and release

- Minimum supported OS becomes **macOS 13.0**.
- Production uses `SMAppService.daemon(plistName:)`; there is no `SMJobBless`, direct `launchctl`, sudoers, or
  pre-macOS-13 fallback.
- An ad-hoc/debug app can exercise the dev helper but cannot satisfy production helper health or arm persistent
  clamshell mode.
- The production app and nested helper must share the pinned Developer ID Team ID, use hardened runtime, be notarized
  and stapled, and pass `spctl`.
- Official platform references:
  - <https://developer.apple.com/documentation/servicemanagement/smappservice>
  - <https://developer.apple.com/documentation/servicemanagement/smappservice/daemon(plistname:)>
  - <https://developer.apple.com/documentation/servicemanagement/updating-helper-executables-from-earlier-versions-of-macos>

### Safety invariants

1. `jarvis-power-helper` is the sole v2 registry writer and sole production caller of
   `pmset -a disablesleep {0,1}`.
2. Requests contain no command, executable path, state path, uid, Team ID, baseline, or raw `pmset` arguments.
3. The helper derives uid, exact process incarnation, bundle id, Team ID, signed build, and designated requirement
   from each XPC message; client-supplied identity is never trusted.
4. Acquire succeeds only after write-ahead state and parent directory are fsynced, mutation read-back matches, and an
   unpredictable helper-generated lease id exists.
5. State is cleared only after baseline read-back. `baseline=true` always means `did_mutate=false` and never causes a
   write to `false`.
6. Renew extends only an unexpired lease with the same attested principal and owner generation; it never resurrects a
   released/expired lease.
7. Exact process death/mismatch or authoritative deadline expiry runs compare-and-restore without another Jarvis
   request.
8. Corrupt, cross-boot, partially persisted, permission-ambiguous, or unverifiable state blocks new mutation and keeps
   recovery evidence.
9. All Jarvis profiles share logical leases; last mutating lease restores the baseline.
10. Existing `ShutdownGate` remains one-way. Graceful cleanup stops renewal, requests exact helper release, then
    releases app-local IOKit assertions. Helper TTL is the crash safety net.
11. Same-value external `disablesleep=1` writers cannot be detected; UI/consent describes Jarvis as temporary exclusive
    manager while its mutating lease exists.

### Fixed constants

Define once in `crates/jarvis-power-core/src/protocol.rs`:

```rust
pub const PROTOCOL_VERSION: u32 = 2;
pub const MIN_TTL_MS: u64 = 5_000;
pub const DEFAULT_TTL_MS: u64 = 45_000;
pub const MAX_TTL_MS: u64 = 120_000;
pub const RENEW_EVERY_MS: u64 = 15_000;
pub const MAX_FRAME_BYTES: usize = 16 * 1024;
pub const SERVICE_LABEL: &str = "app.jarvis.monitor.power-helper";
```

The 45-second deadline bounds unobserved post-`SIGKILL` battery exposure. The helper checks exact process identity once
per second, so observed death restores earlier.

### Root state

```text
/Library/Application Support/Jarvis/Power/v2/
  state.json       root:wheel 0600
  state.lock       root:wheel 0600
```

The directory is root:wheel `0700`. Open components with `openat` plus `O_NOFOLLOW`; reject symlinks, non-root owners,
group/other write bits, hard-linked state, and unexpected file kinds. Replace through a same-directory `0600` temp,
`fsync(temp)`, `renameat`, then `fsync(directory)`. Hold `flock(LOCK_EX)` through read/decision/write/mutate/read-back.

### Update strategy

The first version uses restore-and-cutover, not live handoff:

1. close app admission and stop renewal;
2. release the exact lease;
3. require `baseline_verified && active_leases == 0`;
4. asynchronously unregister old `SMAppService` and await completion;
5. install the app update;
6. register/attest the new helper on next launch before reopening power admission.

Any failure before replacement blocks update/uninstall with a repair action. There is no forced unregister.

## Release blockers

- **B1:** current release is ad-hoc. Protected secrets must provide `APPLE_CERTIFICATE`,
  `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, and a ten-character `APPLE_TEAM_ID`.
- **B2:** notarization needs `APPLE_ID`, `APPLE_PASSWORD`, and `APPLE_TEAM_ID`; a LaunchDaemon-containing build is
  never published without notarization/stapling.
- **B3:** helper trust pins Team ID plus `app.jarvis.monitor`; no test/ad-hoc requirement may enter a production binary.
- **B4:** automatic and manual updater paths must both use restore-and-cutover.
- **B5:** `jarvis-setup uninstall` must not report success before clean helper status and completed unregister.
- **B6:** signed live smoke must cover a clean macOS 13 machine and the current supported macOS release.
- **B7:** independent security, runtime, and release reviewers must approve the final range.

Until B1-B7 are green, release clamshell arm returns `helper_unavailable`/`helper_unapproved`; it never falls back.

## Target file map

```text
crates/jarvis-power-core/
  Cargo.toml
  src/{lib,protocol,state,engine}.rs
  tests/{protocol_vectors,crash_matrix}.rs
crates/jarvis-power-helper/
  Cargo.toml
  build.rs
  native/{xpc_server.h,xpc_server.m}
  src/{lib,coordinator,root_store,dev_store,pmset,watchdog,xpc_server,dev_uds}.rs
  src/bin/{jarvis-power-helper,jarvis-power-helper-dev}.rs
  tests/{root_store,watchdog,dev_uds}.rs
src-tauri/src/power/helper/
  mod.rs client.rs renewal.rs xpc.rs dev_uds.rs migration.rs lifecycle.rs
src-tauri/native/{power_helper_client.h,power_helper_client.m}
src-tauri/PowerHelper/
  app.jarvis.monitor.power-helper.plist
  helper.entitlements.plist
scripts/
  build-power-helper.sh
  check-power-helper-bundle.sh
  test-power-helper-xpc.sh
  live-power-helper-smoke.sh
docs/release/power-helper.md
```

## Task 1: Shared closed protocol

**Files:**

- Create: `crates/jarvis-power-core/Cargo.toml`
- Create: `crates/jarvis-power-core/src/lib.rs`
- Create: `crates/jarvis-power-core/src/protocol.rs`
- Create: `crates/jarvis-power-core/tests/protocol_vectors.rs`
- Modify: `src-tauri/Cargo.toml`
- Modify: `.github/workflows/ci.yml`

- [x] **Step 1: Write RED protocol tests**

```rust
#[test]
fn arbitrary_commands_and_unknown_fields_are_rejected() {
    let input = r#"{"protocolVersion":2,"requestId":"018f0000-0000-7000-8000-000000000001",
                    "method":"run","args":["/usr/bin/pmset","-a","disablesleep","1"]}"#;
    assert_eq!(
        decode_request(input.as_bytes()),
        Err(ProtocolError::MalformedFrame)
    );
}

#[test]
fn recovery_is_not_a_client_triggered_protocol_method() {
    for method in ["recoverExpired", "recover", "tick", "restoreBaseline"] {
        let input = format!(
            r#"{{"protocolVersion":2,"requestId":"018f0000-0000-7000-8000-000000000001",
                 "method":"{method}"}}"#
        );
        assert_eq!(
            decode_request(input.as_bytes()),
            Err(ProtocolError::MalformedFrame)
        );
    }
}

#[test]
fn wire_contract_has_no_caller_identity_or_mutation_fields() {
    let text = String::from_utf8(
        encode_request(&acquire_request("prod", "generation-a", 45_000)).unwrap()
    ).unwrap();
    for forbidden in ["command", "args", "path", "uid", "pid", "teamId", "baseline", "pmset"] {
        assert!(!text.contains(forbidden), "{forbidden} leaked into protocol");
    }
}

#[test]
fn wrong_version_ttl_and_oversized_identifiers_fail_closed() {
    assert_eq!(decode_request(request_with_version(1)), Err(ProtocolError::IncompatibleVersion));
    assert!(acquire_request("prod", "g", 120_001).validate().is_err());
    assert!(acquire_request(&"p".repeat(129), "g", 45_000).validate().is_err());
}
```

- [x] **Step 2: Prove RED**

```bash
cargo test --manifest-path crates/jarvis-power-core/Cargo.toml --test protocol_vectors
```

Expected: FAIL because the crate/types do not exist.

- [x] **Step 3: Implement only this method surface**

```rust
#[serde(tag = "method", rename_all = "camelCase", deny_unknown_fields)]
pub enum Request {
    AcquireLease { profile: String, owner_generation: String, ttl_ms: u64 },
    RenewLease { lease_id: String, owner_generation: String, ttl_ms: u64 },
    ReleaseLease { lease_id: String, owner_generation: String },
    Status,
}
```

`RequestEnvelope` has only `protocol_version`, UUIDv7-shaped `request_id`, and flattened `Request`.
Neither envelope implements public `Deserialize`: untrusted bytes must enter through the bounded `decode_request` or
`decode_response` API, which checks `MAX_FRAME_BYTES` before parsing. `Acquired` and `Renewed` return bounded
`grantedTtlMs`; the helper's authoritative monotonic deadline never crosses the wire.
`ResponseEnvelope` echoes request id and returns `Acquired`, `Renewed`, `Released`, `Status`, or a finite error code.
Use `#[serde(deny_unknown_fields)]` everywhere. Lease ids are helper-generated 128-bit CSPRNG values. Recovery has no
wire request/response variant.

- [x] **Step 4: Run and commit**

```bash
cargo test --manifest-path crates/jarvis-power-core/Cargo.toml
git add crates/jarvis-power-core src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat(power-core): define closed helper protocol"
```

## Task 2: Shared crash-recoverable lease engine

**Files:**

- Create: `crates/jarvis-power-core/src/state.rs`
- Create: `crates/jarvis-power-core/src/engine.rs`
- Create: `crates/jarvis-power-core/tests/crash_matrix.rs`
- Modify: `crates/jarvis-power-core/src/lib.rs`

- [x] **Step 1: Write RED transition tests**

```rust
#[test]
fn expired_last_mutating_lease_restores_before_clear() {
    let plan = Engine::new(applied_state(false, lease_deadline(100)))
        .tick(101, |_| ProcessState::Dead).unwrap();
    assert_eq!(plan.effects, [
        Effect::PersistPhase(MutationPhase::RestorePending),
        Effect::CompareAndSetDisabled(false),
        Effect::VerifyDisabled(false),
        Effect::ClearState,
    ]);
}

#[test]
fn renew_never_resurrects_and_baseline_on_is_never_disabled() {
    assert_eq!(expired_engine().renew(owner(), "lease-a", "g", 101, 45_000),
               Err(EngineError::Expired));
    let plan = Engine::empty().acquire(owner(), "prod", "g", 0, 45_000, true).unwrap();
    assert!(!plan.state.did_mutate);
    assert!(!plan.effects.contains(&Effect::CompareAndSetDisabled(false)));
}

#[test]
fn crash_after_each_effect_converges_safely() {
    for crash_after in 0..acquire_effect_count() {
        let recovered = reconcile(execute_until_crash(acquire_plan(), crash_after), system(), 200);
        assert!(recovered.unwrap().is_safe_and_explained());
    }
}
```

- [x] **Step 2: Prove RED**

```bash
cargo test --manifest-path crates/jarvis-power-core/Cargo.toml --test crash_matrix
```

- [x] **Step 3: Implement v2 state and ordered effects**

```rust
pub struct HelperState {
    pub schema_version: u32,
    pub service_version: u64,
    pub minimum_client_build: u64,
    pub boot_id: String,
    pub baseline: bool,
    pub applied: bool,
    pub did_mutate: bool,
    pub mutation_generation: u64,
    pub phase: MutationPhase,
    pub leases: Vec<Lease>,
}

pub enum MutationPhase { Prepared, Applied, RestorePending }
```

Persist `Prepared` before mutation and `RestorePending` before restore. `ClearState` is legal only after matching
`VerifyDisabled(baseline)`. `Principal` is helper-derived: uid, pid, versioned Darwin start identity, bundle id, Team
ID, requirement digest, and signed build. Protocol deserialization cannot create it.

- [x] **Step 4: Run and commit**

```bash
cargo test --manifest-path crates/jarvis-power-core/Cargo.toml
git add crates/jarvis-power-core
git commit -m "feat(power-core): model renewable watchdog leases"
```

**Completion evidence (2026-07-31):** integrated commits `516f360`,
`cb72933`, `a5be900`, `0dd6394`, and `d394ba4` close admission caps,
policy drift, idempotent acquire, no-shorten renewals, post-persist deadline
checks, runtime-derived granted TTL, and the finite reconciliation outcomes
`LeaseExpired`/`RecoveryRequired`. The independent review approved all 38
crash-matrix tests, 13 protocol tests, five compile-fail doctests, current
all-target clippy, Rust 1.77.2 tests and changed-scope clippy. The integrated
branch reran the same 56 tests on current Rust plus all-target clippy with
warnings denied.

## Task 3: Root store, fixed pmset coordinator, and watchdog

**Files:**

- Create: `crates/jarvis-power-helper/Cargo.toml`
- Create: `crates/jarvis-power-helper/src/lib.rs`
- Create: `crates/jarvis-power-helper/src/coordinator.rs`
- Create: `crates/jarvis-power-helper/src/root_store.rs`
- Create: `crates/jarvis-power-helper/src/pmset.rs`
- Create: `crates/jarvis-power-helper/src/watchdog.rs`
- Create: `crates/jarvis-power-helper/tests/root_store.rs`
- Create: `crates/jarvis-power-helper/tests/watchdog.rs`

- [ ] **Step 1: Write RED store/watchdog tests**

```rust
#[test]
fn unsafe_metadata_is_rejected_without_following_or_overwriting() {
    for case in [UnsafeCase::Symlink, UnsafeCase::Owner(501), UnsafeCase::Mode(0o620),
                 UnsafeCase::HardLink] {
        let fixture = StoreFixture::with(case);
        assert_eq!(fixture.store().load(), Err(StoreError::UnsafeMetadata));
        assert_eq!(fixture.outside_bytes(), b"sentinel");
    }
}

#[test]
fn acquire_orders_write_ahead_before_mutation_and_readback() {
    let h = Harness::baseline(false);
    h.acquire(owner(), "prod", "g").unwrap();
    assert_eq!(h.events(), [
        "lock", "read-0", "persist-prepared", "fsync-parent",
        "pmset-1", "readback-1", "persist-applied", "reply", "unlock"
    ]);
}

#[test]
fn dead_process_restores_autonomously_and_failed_restore_keeps_tombstone() {
    let h = Harness::applied(dead_owner(), false);
    h.watchdog_tick().unwrap();
    assert!(!h.disabled());
    assert!(!h.state_exists());

    let blocked = Harness::applied(expired_owner(), false).fail_restore();
    assert!(blocked.watchdog_tick().is_err());
    assert_eq!(blocked.state().phase, MutationPhase::RestorePending);
}
```

- [ ] **Step 2: Prove RED**

```bash
cargo test --manifest-path crates/jarvis-power-helper/Cargo.toml --test root_store
cargo test --manifest-path crates/jarvis-power-helper/Cargo.toml --test watchdog
```

- [ ] **Step 3: Implement the fixed backend and coordinator**

`PmsetBackend` exposes only `read_disabled()`, `set_disabled(bool)`, and `boot_id()`. `SystemPmset` uses absolute
`/usr/bin/pmset`, constructs `0/1` from `bool`, has null stdin and eight-second kill/reap timeout, and accepts no
argument vector.

The coordinator holds the root lock across engine decision, durable transition, mutation, and read-back. The watchdog
ticks each second, checks monotonic deadline and exact `proc_pidinfo` start identity, and executes the same release
transaction. Before publishing or accepting the XPC/UDS listener, helper startup synchronously runs this same
serialized internal watchdog tick; the timer is its only later trigger. No client request can invoke recovery. Logs
contain request id, finite error, generation, and lease-id prefix only.

- [ ] **Step 4: Run and commit**

```bash
cargo test --manifest-path crates/jarvis-power-helper/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml power:: --no-default-features
git add crates/jarvis-power-helper crates/jarvis-power-core src-tauri/Cargo.lock
git commit -m "feat(power-helper): persist and recover root power state"
```

## Task 4: Development-only UDS vertical

**Files:**

- Create: `crates/jarvis-power-helper/src/dev_store.rs`
- Create: `crates/jarvis-power-helper/src/dev_uds.rs`
- Create: `crates/jarvis-power-helper/src/bin/jarvis-power-helper-dev.rs`
- Create: `crates/jarvis-power-helper/tests/dev_uds.rs`
- Create: `src-tauri/src/power/helper/mod.rs`
- Create: `src-tauri/src/power/helper/client.rs`
- Create: `src-tauri/src/power/helper/dev_uds.rs`
- Modify: `src-tauri/src/power/mod.rs`
- Modify: `src-tauri/Cargo.toml`
- Modify: `package.json`

- [ ] **Step 1: Write RED peer/framing/trust tests**

```rust
#[test]
fn socket_is_private_and_wrong_peer_is_rejected_before_decode() {
    let server = DevFixture::start();
    assert_eq!(server.parent_mode(), 0o700);
    assert_eq!(server.socket_mode(), 0o600);
    assert_eq!(server.send_as_uid(502, valid_frame()), Err(TransportError::PeerRejected));
    assert_eq!(server.dispatch_count(), 0);
}

#[test]
fn malformed_frames_and_release_trust_are_rejected() {
    for frame in [oversized_frame(), truncated_frame(), concatenated_frames()] {
        assert!(DevFixture::start().send_raw(frame).is_err());
    }
    assert_eq!(DevFixture::start().status().trust, HelperTrust::DevelopmentOnly);
}
```

- [ ] **Step 2: Prove RED**

```bash
cargo test --manifest-path crates/jarvis-power-helper/Cargo.toml --features dev-uds --test dev_uds
cargo test --manifest-path src-tauri/Cargo.toml power::helper::dev_uds --no-default-features \
  --features power-helper-dev
```

- [ ] **Step 3: Implement explicit dev selection**

Bind `$JARVIS_DIR/run/power-helper-dev.sock` under `0700`, socket `0600`, validate `getpeereid`, and accept one
length-prefixed JSON frame up to `MAX_FRAME_BYTES`. Dev state is `$JARVIS_DIR/power/dev-helper-v2.json` mode `0600`.
Selection requires compile-time `power-helper-dev` plus runtime `JARVIS_DEV=1`; environment alone is insufficient.
The dev helper may use the historical exact-command sudoers rule but always reports `development_only`.

- [ ] **Step 4: Run and commit**

```bash
cargo test --manifest-path crates/jarvis-power-helper/Cargo.toml --features dev-uds
cargo test --manifest-path src-tauri/Cargo.toml power:: --no-default-features \
  --features power-helper-dev
git add crates/jarvis-power-helper src-tauri/src/power src-tauri/Cargo.toml \
  src-tauri/Cargo.lock package.json
git commit -m "feat(power): add development helper transport"
```

## Task 5: Production SMAppService/XPC attestation vertical

**Files:**

- Create: `crates/jarvis-power-helper/build.rs`
- Create: `crates/jarvis-power-helper/src/xpc_server.rs`
- Create: `crates/jarvis-power-helper/src/bin/jarvis-power-helper.rs`
- Create: `crates/jarvis-power-helper/native/xpc_server.h`
- Create: `crates/jarvis-power-helper/native/xpc_server.m`
- Create: `src-tauri/src/power/helper/xpc.rs`
- Create: `src-tauri/src/power/helper/lifecycle.rs`
- Create: `src-tauri/native/power_helper_client.h`
- Create: `src-tauri/native/power_helper_client.m`
- Create: `src-tauri/PowerHelper/app.jarvis.monitor.power-helper.plist`
- Create: `src-tauri/PowerHelper/helper.entitlements.plist`
- Create: `scripts/build-power-helper.sh`
- Create: `scripts/test-power-helper-xpc.sh`
- Modify: `src-tauri/{build.rs,Cargo.toml}`

- [ ] **Step 1: Write RED policy and per-message tests**

```rust
#[test]
fn policy_pins_team_bundle_requirement_and_version_floor() {
    let policy = production_policy("ABCDEFGHIJ", 340);
    assert!(policy.authorize(claims("ABCDEFGHIJ", "app.jarvis.monitor", 340)).is_ok());
    assert_eq!(policy.authorize(claims("ZZZZZZZZZZ", "app.jarvis.monitor", 340)),
               Err(AuthError::WrongTeam));
    assert_eq!(policy.authorize(claims("ABCDEFGHIJ", "app.jarvis.fake", 340)),
               Err(AuthError::WrongIdentifier));
    assert_eq!(policy.authorize(claims("ABCDEFGHIJ", "app.jarvis.monitor", 339)),
               Err(AuthError::Downgrade));
}

#[test]
fn each_message_is_attested_not_only_connection_setup() {
    let server = TestXpcServer::connected(valid_claims());
    assert!(server.send(valid_claims(), status()).is_ok());
    assert_eq!(server.send(wrong_team_claims(), status()), Err(AuthError::WrongTeam));
    assert_eq!(server.attestation_count(), 2);
}
```

- [ ] **Step 2: Prove RED**

```bash
cargo test --manifest-path crates/jarvis-power-helper/Cargo.toml xpc
bash scripts/test-power-helper-xpc.sh
```

Expected: FAIL because production bridge/harness do not exist.

- [ ] **Step 3: Implement message-bound attestation**

For each XPC dictionary message, native code calls:

```objc
SecCodeCreateWithXPCMessage(message, kSecCSDefaultFlags, &guest);
SecCodeCheckValidity(
    guest,
    kSecCSStrictValidate | kSecCSCheckAllArchitectures,
    productionRequirement
);
```

The requirement pins `anchor apple generic`, identifier `app.jarvis.monitor`, Team ID, Developer ID intermediate, and
Developer ID Application leaf OIDs. Extract signed build/identifier/Team ID from signing info; derive euid/pid from
the peer and exact process start via `proc_pidinfo`. Reject missing/partial/changing data. Persist highest accepted app
build as the downgrade floor. The XPC dictionary has one bounded data field, `payload`; JSON remains the shared schema.

- [ ] **Step 4: Implement SMAppService lifecycle**

The app bridge exposes fixed `status`, `register`, async `unregister`, and bounded `request` C functions.
`requiresApproval` is not success. Await unregister completion before re-register/replacement.

The plist under `Contents/Library/LaunchDaemons` contains:

```xml
<key>Label</key><string>app.jarvis.monitor.power-helper</string>
<key>BundleProgram</key>
<string>Contents/Library/LaunchDaemons/app.jarvis.monitor.power-helper</string>
<key>MachServices</key>
<dict><key>app.jarvis.monitor.power-helper</key><true/></dict>
<key>ThrottleInterval</key><integer>1</integer>
```

Do not add `Program`, user-derived arguments, shell execution, network entitlement, or unconditional `KeepAlive`.

- [ ] **Step 5: Test and commit**

```bash
cargo test --manifest-path crates/jarvis-power-helper/Cargo.toml xpc
cargo test --manifest-path src-tauri/Cargo.toml power::helper::xpc --no-default-features
bash scripts/test-power-helper-xpc.sh
MACOSX_DEPLOYMENT_TARGET=13.0 bash scripts/build-power-helper.sh --unsigned-test
git add crates/jarvis-power-helper src-tauri/src/power/helper src-tauri/native \
  src-tauri/PowerHelper src-tauri/build.rs src-tauri/Cargo.toml src-tauri/Cargo.lock scripts
git commit -m "feat(power-helper): add attested SMAppService transport"
```

## Task 6: App renewable lease lifecycle

**Files:**

- Create: `src-tauri/src/power/helper/renewal.rs`
- Modify: `src-tauri/src/power/helper/mod.rs`
- Modify: `src-tauri/src/power/helper/client.rs`
- Modify: `src-tauri/src/power/clamshell.rs`
- Modify: `src-tauri/src/power/mod.rs`
- Modify: `src-tauri/src/shutdown.rs`
- Modify: `src-tauri/src/main.rs`

- [ ] **Step 1: Write RED lifecycle/race tests**

```rust
#[test]
fn shutdown_stops_renewal_then_releases_exact_lease() {
    let h = PowerHarness::armed();
    assert!(h.dispose().clamshell_restored);
    assert_eq!(h.events(), [
        "close-admission", "stop-renewal", "release:lease-a:g", "dispose-iokit"
    ]);
    h.advance_ms(30_000);
    assert_eq!(h.helper().renew_count(), 0);
}

#[test]
fn helper_loss_never_falls_back_and_late_renew_cannot_rearm() {
    let unavailable = PowerHarness::helper_unavailable();
    assert_eq!(unavailable.arm(), Err(PowerError::HelperUnavailable));
    assert_eq!(unavailable.app_pmset_write_count(), 0);

    let late = PowerHarness::blocked_renew();
    late.begin_renew();
    late.dispose();
    late.finish_renew();
    assert!(!late.is_armed());
}
```

- [ ] **Step 2: Prove RED**

```bash
cargo test --manifest-path src-tauri/Cargo.toml power::helper --no-default-features
```

- [ ] **Step 3: Replace production app mutation**

Start one cancellable renewal after acquire. Renewal obtains the existing `PowerOperation`/epoch guard, renews the
exact receipt every 15 seconds, and never auto-acquires replacement. Shutdown cancellation wins over timer/XPC
completion. `arm`, `disarm`, battery guard, and `dispose` use helper acquire/release. App-side
`SystemPmset::set_disabled` remains only for v1 migration/tests. Low-battery helper loss keeps the existing
non-privileged `sleepnow` safety action; it never writes `disablesleep`.

- [ ] **Step 4: Run and commit**

```bash
cargo test --manifest-path src-tauri/Cargo.toml power:: --no-default-features
cargo test --manifest-path src-tauri/Cargo.toml shutdown:: --no-default-features
git add src-tauri/src/power src-tauri/src/shutdown.rs src-tauri/src/main.rs
git commit -m "feat(power): use renewable helper leases"
```

## Task 7: One-way v1 migration and no dual writer

**Files:**

- Create: `src-tauri/src/power/helper/migration.rs`
- Modify: `src-tauri/src/power/clamshell.rs`
- Modify: `src-tauri/src/power/ownership.rs`
- Modify: `src-tauri/src/power/ownership_store.rs`
- Modify: `src-tauri/src/power/helper/mod.rs`
- Modify: `src-tauri/src/main.rs`
- Modify: `src-tauri/src/install/mod.rs`

- [ ] **Step 1: Write RED migration tests**

```rust
#[test]
fn v1_restores_before_v2_registration() {
    let h = MigrationHarness::v1_applied(false);
    h.start().unwrap();
    assert_eq!(h.events(), [
        "close-admission", "v1-restore-0", "v1-readback-0", "v1-clear",
        "register-v2", "attest-status", "write-cutover-receipt"
    ]);
}

#[test]
fn ambiguous_v1_blocks_registration_and_is_never_imported_as_root_authority() {
    for fixture in [V1Fixture::Corrupt, V1Fixture::LegacyMarker, V1Fixture::RestorePending] {
        let h = MigrationHarness::new(fixture);
        assert_eq!(h.start(), Err(MigrationError::V1RepairRequired));
        assert_eq!(h.v2_register_count(), 0);
        assert_eq!(h.helper_import_count(), 0);
    }
}
```

- [ ] **Step 2: Prove RED**

```bash
cargo test --manifest-path src-tauri/Cargo.toml power::helper::migration --no-default-features
```

- [ ] **Step 3: Implement startup cutover**

Immediately after `install::prepare_clean_start()` and before settings/plugins/Daemon/headless branching:

1. run approved v1 recovery;
2. block on corrupt/legacy/expired-live/tombstone/read-back/privilege ambiguity;
3. require v1 registry absent after verified recovery;
4. register and attest compatible v2 helper;
5. atomically write mode-`0600` `~/.jarvis/power/helper-cutover-v2.json`;
6. select v2 permanently.

Never copy user v1 JSON into root state. The root helper removes historical `/etc/sudoers.d/jarvis-pmset` only when
bytes exactly match a committed Jarvis template; mismatch remains untouched and surfaces manual repair.

- [ ] **Step 4: Run and commit**

```bash
cargo test --manifest-path src-tauri/Cargo.toml power::helper::migration --no-default-features
cargo test --manifest-path src-tauri/Cargo.toml power:: --no-default-features
git add src-tauri/src/power src-tauri/src/main.rs src-tauri/src/install/mod.rs
git commit -m "feat(power): cut over legacy ownership to helper"
```

## Task 8: Updater and uninstall cutover

**Files:**

- Modify: `src-tauri/src/power/helper/lifecycle.rs`
- Modify: `src-tauri/src/ipc.rs`
- Modify: `src-tauri/src/main.rs`
- Modify: `src-tauri/src/install/mod.rs`
- Modify: `src-tauri/src/bin/setup.rs`
- Modify: `src-tauri/src/shutdown.rs`
- Modify: `docs/release/versioning-and-migration.md`

- [ ] **Step 1: Write RED ordering/failure tests**

```rust
#[test]
fn update_waits_for_verified_restore_and_unregister_completion() {
    let h = UpdateHarness::armed();
    h.install().unwrap();
    assert_eq!(h.events(), [
        "close-admission", "stop-renewal", "release", "status-clean",
        "unregister-begin", "unregister-complete", "replace-app"
    ]);
}

#[test]
fn unregister_failure_preserves_bundle_and_other_profile_blocks_uninstall() {
    let update = UpdateHarness::unregister_failure();
    assert!(update.install().is_err());
    assert_eq!(update.replace_count(), 0);

    let uninstall = UninstallHarness::other_live_profile();
    assert_eq!(uninstall.run(), Err(UninstallError::PowerLeasesActive(1)));
    assert!(uninstall.integration_files_exist());
}
```

- [ ] **Step 2: Prove RED**

```bash
cargo test --manifest-path src-tauri/Cargo.toml power::helper::lifecycle --no-default-features
```

- [ ] **Step 3: Fence both updater paths and uninstall**

Both `ipc::update_check_install` and automatic update call one
`PowerHelperLifecycle::prepare_app_replacement()` before `download_and_install`. Persist
`replacement-journal.json` phases `power_released`, `helper_unregistered`, `app_replaced`. If install fails after
unregister, keep journal and require restart; never reopen the one-way gate in-process.

Make `install::uninstall` fallible. Before deleting hooks/files, close admission, release exact lease, require zero
leases and verified baseline, await unregister completion, then confirm `not_registered`. No force option exists.

- [ ] **Step 4: Run and commit**

```bash
cargo test --manifest-path src-tauri/Cargo.toml power::helper::lifecycle --no-default-features
cargo test --manifest-path src-tauri/Cargo.toml install:: --no-default-features
cargo test --manifest-path src-tauri/Cargo.toml shutdown:: --no-default-features
git add src-tauri/src docs/release/versioning-and-migration.md
git commit -m "fix(updater): fence power helper replacement"
```

## Task 9: macOS 13 packaging, signing, notarization, and evidence gate

**Files:**

- Create: `scripts/check-power-helper-bundle.sh`
- Create: `scripts/live-power-helper-smoke.sh`
- Create: `src-tauri/tauri.dev.conf.json`
- Create: `docs/release/power-helper.md`
- Modify: `scripts/build-power-helper.sh`
- Modify: `src-tauri/tauri.conf.json`
- Modify: `src-tauri/Info.plist`
- Modify: `src-tauri/entitlements.plist`
- Modify: `package.json`
- Modify: `.github/workflows/ci.yml`
- Modify: `.github/workflows/release.yml`
- Modify: `README.md`
- Modify: `README.ru.md`
- Modify: `docs/release/versioning-and-migration.md`

- [ ] **Step 1: Write a RED bundle gate**

`check-power-helper-bundle.sh APP` requires:

```bash
test -x "$app/Contents/Library/LaunchDaemons/app.jarvis.monitor.power-helper"
test -f "$app/Contents/Library/LaunchDaemons/app.jarvis.monitor.power-helper.plist"
codesign --verify --strict --verbose=4 "$helper"
codesign --verify --deep --strict --verbose=4 "$app"
test "$(team_id "$app")" = "$APPLE_TEAM_ID"
test "$(team_id "$helper")" = "$APPLE_TEAM_ID"
! codesign -dv --verbose=4 "$app" 2>&1 | grep -F 'Signature=adhoc'
! strings "$app/Contents/MacOS/jarvis" | grep -E 'jarvis-pmset|power-helper-dev\\.sock'
! find "$app" -name jarvis-power-helper-dev -print -quit | grep -q .
```

It also verifies `Label`, `BundleProgram`, absence of `Program`, and `minos >= 13.0` for both Mach-O files.

```bash
bash scripts/check-power-helper-bundle.sh src-tauri/target/release/bundle/macos/Jarvis.app
```

Expected now: FAIL because helper is absent and production config is ad-hoc/minimum 11.0.

- [ ] **Step 2: Package with no release fallback**

Set `minimumSystemVersion` to `13.0` and Tauri `bundle.macOS.files`:

```json
{
  "PowerHelper/app.jarvis.monitor.power-helper":
    "Library/LaunchDaemons/app.jarvis.monitor.power-helper",
  "PowerHelper/app.jarvis.monitor.power-helper.plist":
    "Library/LaunchDaemons/app.jarvis.monitor.power-helper.plist"
}
```

Remove production `signingIdentity: "-"`; keep local ad-hoc behavior only in `tauri.dev.conf.json`. Export
`MACOSX_DEPLOYMENT_TARGET=13.0`. Sign helper leaf first, then outer app; never create signatures with `--deep`.

- [ ] **Step 3: Harden CI/release**

CI runs core, helper fake-backend/dev UDS, app power tests, unsigned layout checks, and a static no-fallback check.
Protected release imports Developer ID, signs leaf/app, validates Team IDs, notarizes with `notarytool --wait`, staples,
runs `spctl`, then creates a **draft** release with bundle report/checksums/notarization id. Missing Apple secret,
identity `-`, Team mismatch, or failed gate aborts before upload.

- [ ] **Step 4: Add live smoke and runbook**

Without `JARVIS_POWER_LIVE_SMOKE=1`, smoke is read-only. With opt-in it drives only the signed helper protocol, installs
an EXIT/INT/TERM restore trap, and never runs raw `sudo pmset`. Cover:

- baseline 0 and 1;
- two profiles/last lease;
- Jarvis `SIGKILL` restore within 45 seconds;
- helper restart and lease expiry;
- wrong identifier/Team/ad-hoc/downgraded clients;
- malformed/oversized requests;
- System Settings disable;
- update/uninstall cutover;
- corrupt/symlink/permission/fsync failure;
- GUI/headless/SIGTERM/updater/plugin-hang cleanup.

Record redacted before/after power state, service/helper status, elapsed restore, signatures, and cleanup result.

- [ ] **Step 5: Run automated gate**

```bash
cargo test --manifest-path crates/jarvis-power-core/Cargo.toml
cargo test --manifest-path crates/jarvis-power-helper/Cargo.toml --features dev-uds
cargo test --manifest-path src-tauri/Cargo.toml power:: --no-default-features
cargo test --manifest-path src-tauri/Cargo.toml shutdown:: --no-default-features
bash scripts/test-power-helper-xpc.sh
npm run test:ui
git diff --check
```

- [ ] **Step 6: Run signed smoke and independent reviews**

On clean macOS 13 and current supported macOS:

```bash
JARVIS_POWER_LIVE_SMOKE=1 JARVIS_APP=/Applications/Jarvis.app \
  bash scripts/live-power-helper-smoke.sh
```

Attach both evidence bundles to the draft. Require independent approvals for:

- security: XPC attestation, root paths, downgrade, allowlist, logs;
- runtime: crash matrix, renewal/shutdown races, multi-profile, update/uninstall;
- release: nested signing, Team/min OS, notarization, no dev/sudoers fallback.

- [ ] **Step 7: Commit**

```bash
git add scripts src-tauri/PowerHelper src-tauri/tauri.conf.json src-tauri/tauri.dev.conf.json \
  src-tauri/Info.plist src-tauri/entitlements.plist package.json .github/workflows \
  README.md README.ru.md docs/release docs/superpowers/plans/2026-07-31-host-power-exit-safety.md
git commit -m "build(macos): gate signed power helper releases"
```

## Final acceptance gate

- [ ] macOS minimum is 13.0 everywhere and no fallback exists.
- [ ] App production code has no persistent `pmset` write.
- [ ] Dev UDS cannot be selected/bundled by production.
- [ ] Protocol has no arbitrary command/path/caller-identity field.
- [ ] Every XPC message is re-attested against Team, identifier, requirement, and signed-build floor.
- [ ] Root store rejects symlink/owner/mode/link ambiguity and handles rename/fsync uncertainty.
- [ ] Parent death/expiry restores without app relaunch.
- [ ] Baseline 1 stays 1; baseline 0 returns to 0; multi-profile last lease is proven.
- [ ] v1 drains before v2 and is never imported as root authority.
- [ ] Update/uninstall restore and await unregister before replacement/removal.
- [ ] App/helper share Team ID and pass hardened signing, notarization, stapling, and `spctl`.
- [ ] Wrong signer, downgrade, malformed input, app/helper crash, and corrupt state have negative tests.
- [ ] Signed evidence exists for macOS 13 and current supported macOS.
- [ ] Security, runtime, and release reviews approve.
- [ ] Cargo/UI/bundle/live checks and PR CI are green.
