# Plugin Package Contracts and Manager Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship Plugin Platform v2's public Rust contracts, strict manifest and deterministic signed package format,
catalog trust chain, durable install receipts, transactional package manager, Developer Mode and management CLI while
keeping the current Agent VM usable through an explicit legacy bridge.

**Architecture:** Three host-independent crates own the public wire/manifest DTOs, plugin author SDK and executable test
host. Jarvis Core owns verification, immutable package storage and a receipt-backed resolver; all CLI and future UI
operations call the same `PluginManager` service and return durable `Operation` records. Existing Manifest v1 Agent VM
remains a narrowly scoped compatibility source until Increment E imports its data and writes a v2 receipt; a present but
invalid v2 receipt never silently falls back to legacy code.

**Tech Stack:** Rust 2021/MSRV 1.77.2, `serde`/JSON Schema, SemVer, JCS canonical JSON, SHA-256, Ed25519, deterministic
uncompressed tar archives, SQLite/WAL for operation journaling, Axum/Tauri IPC, Node boundary tests, existing
PluginHost supervision and Darwin `posix_spawn` with an inherited verified file descriptor.

**Approved design:** `docs/superpowers/specs/2026-07-31-plugin-platform-agent-vm-v2-design.md` §§3, 5–7, 12, 23, 25.1,
26 Increment A.

**Roadmap:** `docs/superpowers/plans/2026-07-31-plugin-platform-agent-vm-v2.md`.

**Implementation base:** `c32cbb9`. Run every command from the repository root. Do not remove the bundled Agent VM
sidecar, its v1 manifest, `externalBin`, current VM disks, settings or data in this increment.

---

## Increment boundary and non-negotiable gates

Increment A creates a production-grade package/trust layer, but it does not claim that Agent VM is already migrated.
These gates prevent the package manager from breaking the working reference plugin:

| Profile state at Jarvis startup | Increment A behavior | Required future transition |
|---|---|---|
| Fresh profile, no receipt and bundled sidecar available | Existing bundled Agent VM is staged as the explicit `legacy-bundled-v1` source and remains usable | Increment E changes clean-install behavior after the importer exists |
| Existing legacy `~/.jarvis/plugins/agent-vm` and no receipt | Load through the v1 bridge, preserve settings/data, expose `migrationAvailable` | Increment E imports and writes a v2 receipt |
| Valid current v2 receipt for `agent-vm` plus legacy files | Receipt-backed immutable package wins; legacy is ignored, not deleted | Increment E verifies imported state and may retire the bridge |
| Invalid/revoked/incompatible current v2 receipt plus legacy files | Block activation with a repair/rollback status; never downgrade to legacy automatically | User performs rollback/repair or Increment E importer repairs the receipt |
| Developer link | Run only an immutable digest snapshot while Developer Mode is on | Increment B adds the full Developer section UI |

Additional invariants:

- `plugins/<id>` is source code, never an implicit install or activation source.
- Trust follows publisher signature and exact package digest, never `plugin_id == "agent-vm"`.
- New installed plugins are disabled until a receipt records explicit enablement and grants.
- Package verification, schema validation, quotas and audit remain active in Developer Mode.
- Native code cannot execute before exact-digest consent; health checks obey the same rule.
- Update/uninstall preserve `~/.jarvis/plugin-data/<id>` unless a separate exact-ID purge is approved.
- Normal Jarvis exit still uses existing PluginHost shutdown semantics; Agent VM controller survival changes only in
  Increment D/E.
- Full Plugin Manager pages, custom plugin pages and extension-point rendering are Increment B scope. Increment A
  provides stable DTOs and IPC/CLI methods that those pages consume.
- Broker, Project Runtime and Agent VM controller state do not move into package-manager storage.

### Agent VM upstream hand-off gate

Increment A records, but does not implement, the provider migration input verified on 2026-07-31:

```text
repository: MikD1/agent-vm
tag: v0.2
commit: e11870c3881716ecfdae3dd32efe1f534cc2d7aa
darwin-arm64 sha256: b601b0b5fc4dd3fca5c1661d0f10041f5d17bacdb23d27c9d863b1b9021d82ef
darwin-amd64 sha256: cd4dbf0248bd94d32ab6447ec51526dfc71a68b67a5001d4d8f0b3e6e99bfa73
```

`v0.2` accepts additional mounts through repeated `--mount`/spec entries, but upstream mounts are always read-write and
appear in the guest under `~/basename/name`. Increment E must write these exact values to
`plugins/agent-vm/plugin.lock.json`, test the Record schema and add the host-side read-only enforcement strategy.
Increment A must not create or mutate that lock file.

## Target file map

### Public, host-independent surface

- `crates/jarvis-plugin-protocol/` — Manifest v2, package/catalog/receipt/operation and process-wire DTOs only.
- `crates/jarvis-plugin-sdk/` — plugin-side environment parsing and typed lifecycle client; no Jarvis Core imports.
- `crates/jarvis-plugin-test-host/` — fake host, contract assertions and fixtures for plugin authors.
- `schemas/plugin-manifest-v2.schema.json` — strict bundled JSON Schema with no remote references.
- `schemas/plugin-package-v1.schema.json` — strict `package.json` schema.
- `schemas/plugin-catalog-v1.schema.json` — strict catalog envelope/payload schema.

### Jarvis-owned implementation

- `src-tauri/src/plugins/manifest_v2.rs` — bounded schema validation and source-template target resolution.
- `src-tauri/src/plugins/package/` — deterministic pack, safe archive inspection/extraction and file hashing.
- `src-tauri/src/plugins/trust/` — catalog freshness, root/publisher signatures, rotations and revocations.
- `src-tauri/src/plugins/package_manager/` — paths, receipts, operation journal, transactions and recovery.
- `src-tauri/src/plugins/developer.rs` — immutable local snapshots and Developer Mode invalidation.
- `src-tauri/src/plugins/resolver.rs` — receipt/legacy source precedence and activation decisions.
- `src-tauri/src/plugins/verified_spawn.rs` — exact open-file execution for receipt-backed native runtimes.
- `src-tauri/src/plugin_cli.rs` — `jarvis plugin ...` parser and presentation over the shared manager service.

### Compatibility surface retained

- `src-tauri/src/plugins/manifest.rs` remains the Manifest v1 parser.
- `src-tauri/src/plugins/install.rs` remains the bundled Agent VM compatibility installer, renamed internally to make
  its legacy status explicit.
- `plugins/agent-vm/manifest.json`, `scripts/prepare-agent-vm-sidecar.sh` and
  `src-tauri/tauri.conf.json#bundle.externalBin` stay active until Increment E.

---

### Task A1: Establish the public protocol, SDK and test-host boundary

**Files:**

- Create: `crates/jarvis-plugin-protocol/Cargo.toml`
- Create: `crates/jarvis-plugin-protocol/src/lib.rs`
- Create: `crates/jarvis-plugin-protocol/src/process.rs`
- Create: `crates/jarvis-plugin-protocol/src/operation.rs`
- Create: `crates/jarvis-plugin-protocol/tests/wire_compat.rs`
- Create: `crates/jarvis-plugin-sdk/Cargo.toml`
- Create: `crates/jarvis-plugin-sdk/src/lib.rs`
- Create: `crates/jarvis-plugin-sdk/src/client.rs`
- Create: `crates/jarvis-plugin-sdk/tests/environment.rs`
- Create: `crates/jarvis-plugin-test-host/Cargo.toml`
- Create: `crates/jarvis-plugin-test-host/src/lib.rs`
- Create: `crates/jarvis-plugin-test-host/tests/contract.rs`
- Create: `scripts/check-plugin-boundaries.sh`
- Modify: `src-tauri/Cargo.toml`
- Modify: `package.json`
- Modify: `.github/workflows/ci.yml`

- [x] **Step 1: Add RED wire fixtures for protocol v2**

Create `crates/jarvis-plugin-protocol/tests/wire_compat.rs` with exact stable JSON names:

```rust
use jarvis_plugin_protocol::{
    operation::{Operation, OperationState},
    process::{HostHello, PluginHello, PLUGIN_PROCESS_PROTOCOL},
};
use serde_json::json;

#[test]
fn process_hello_is_versioned_and_camel_case() {
    let hello: PluginHello = serde_json::from_value(json!({
        "protocolVersion": 2,
        "pluginId": "dev.example.echo",
        "pid": 42,
        "packageDigest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "activationGeneration": 7
    })).unwrap();
    assert_eq!(hello.protocol_version, PLUGIN_PROCESS_PROTOCOL);
    assert_eq!(hello.activation_generation, 7);

    let host = HostHello::accepted("dev.example.echo", 7);
    assert_eq!(serde_json::to_value(host).unwrap()["protocolVersion"], 2);
}

#[test]
fn operation_state_names_are_stable() {
    let op = Operation::new_fixture("op-1", "install", "dev.example.echo");
    assert_eq!(op.state, OperationState::Queued);
    assert_eq!(serde_json::to_value(op).unwrap()["state"], "queued");
}
```

- [x] **Step 2: Run the protocol test and verify RED**

Run:

```bash
cargo test --manifest-path crates/jarvis-plugin-protocol/Cargo.toml --test wire_compat
```

Expected: FAIL because the crate and DTOs do not exist.

- [x] **Step 3: Create the protocol crate and stable DTOs**

`crates/jarvis-plugin-protocol/src/lib.rs` must forbid host coupling:

```rust
#![forbid(unsafe_code)]

pub mod operation;
pub mod process;
```

`process.rs` defines `PLUGIN_PROCESS_PROTOCOL: u32 = 2`, `PluginHello`, `HostHello`,
`ActivationRequest`, `ShutdownRequest`, `Heartbeat`, `CommandRequest`, `CommandResponse` and a tagged
`PluginFrame` enum. Every public JSON type uses `#[serde(rename_all = "camelCase", deny_unknown_fields)]`; every
request carries `plugin_id`, `package_digest`, `activation_generation` and a bounded `request_id`.

`operation.rs` defines:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Operation {
    pub id: String,
    pub kind: String,
    pub plugin_id: String,
    pub state: OperationState,
    pub phase: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperationState {
    Queued,
    Running,
    WaitingForConsent,
    Succeeded,
    Failed,
    Cancelled,
}
```

Keep path handling, HTTP, Tauri, tokens, settings and secret-store types out of this crate.

- [x] **Step 4: Add RED SDK environment and fake-host tests**

`crates/jarvis-plugin-sdk/tests/environment.rs`:

```rust
use jarvis_plugin_sdk::PluginEnvironment;

#[test]
fn environment_rejects_cross_plugin_identity() {
    let vars = [
        ("JARVIS_PLUGIN_ID", "dev.example.echo"),
        ("JARVIS_PLUGIN_TOKEN", "token"),
        ("JARVIS_PLUGIN_PROTOCOL", "2"),
        ("JARVIS_PLUGIN_PACKAGE_DIGEST",
         "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        ("JARVIS_PLUGIN_ACTIVATION_GENERATION", "9"),
        ("JARVIS_SOCKET", "/tmp/jarvis.sock"),
    ];
    let env = PluginEnvironment::from_pairs(vars).unwrap();
    assert_eq!(env.plugin_id, "dev.example.echo");
    assert_eq!(env.activation_generation, 9);
    assert!(env.assert_hello_identity("other", 9).is_err());
}
```

`crates/jarvis-plugin-test-host/tests/contract.rs`:

```rust
use jarvis_plugin_test_host::TestHost;

#[test]
fn test_host_rejects_stale_generation_and_replays_nothing() {
    let mut host = TestHost::new("dev.example.echo", 4);
    assert_eq!(
        host.register_fixture(3).unwrap_err().code(),
        "stale_activation_generation"
    );
    assert!(host.commands_after(0).is_empty());
}
```

- [x] **Step 5: Run SDK/test-host tests and verify RED**

Run:

```bash
cargo test --manifest-path crates/jarvis-plugin-sdk/Cargo.toml
cargo test --manifest-path crates/jarvis-plugin-test-host/Cargo.toml
```

Expected: both FAIL because their APIs do not exist.

- [x] **Step 6: Implement the minimal SDK and in-memory test host**

`jarvis-plugin-sdk` depends only on `jarvis-plugin-protocol`, `serde`, `serde_json` and a transport trait. Define
`PluginEnvironment::from_pairs`, `PluginEnvironment::from_process`, `PluginClient<T: Transport>`, and redacted
`SdkError`. `from_process` requires all six environment variables and never logs token values.

`jarvis-plugin-test-host` exposes a deterministic `TestHost` that validates identity/generation, queues bounded command
frames, records lifecycle frames and returns stable `ContractError::code()` values. It must not depend on
`src-tauri`.

- [x] **Step 7: Enforce the source boundary in CI**

Create `scripts/check-plugin-boundaries.sh` that exits non-zero when:

1. any `crates/jarvis-plugin-*` manifest contains a path into `src-tauri`;
2. any `plugins/*/Cargo.toml` contains a path into `src-tauri`;
3. a plugin adds a new direct `jarvis-secret-store` dependency beyond the single existing
   `plugins/agent-vm/Cargo.toml` legacy exception;
4. Rust files under `plugins/` contain `src_tauri`, `jarvis::daemon`, or `jarvis::plugins`.

The script prints the matching file and line. Add:

```json
"check:plugin-boundaries": "bash scripts/check-plugin-boundaries.sh"
```

to `package.json`, run it after `check:public` in `.github/workflows/ci.yml`, and add Cargo test steps for all three
public crates.

- [x] **Step 8: Run the complete A1 gate**

Run:

```bash
cargo test --manifest-path crates/jarvis-plugin-protocol/Cargo.toml
cargo test --manifest-path crates/jarvis-plugin-sdk/Cargo.toml
cargo test --manifest-path crates/jarvis-plugin-test-host/Cargo.toml
npm run check:plugin-boundaries
```

Expected: all commands exit `0`; no secret/token appears in test output.

- [x] **Step 9: Commit**

```bash
git add crates/jarvis-plugin-protocol crates/jarvis-plugin-sdk crates/jarvis-plugin-test-host \
  scripts/check-plugin-boundaries.sh src-tauri/Cargo.toml package.json .github/workflows/ci.yml
git commit -m "feat(plugins): add public v2 contract crates"
```

---

### Task A2: Validate Manifest v2 strictly without breaking Manifest v1

**Files:**

- Create: `schemas/plugin-manifest-v2.schema.json`
- Create: `crates/jarvis-plugin-protocol/src/manifest.rs`
- Create: `crates/jarvis-plugin-protocol/tests/manifest_contract.rs`
- Create: `src-tauri/src/plugins/manifest_v2.rs`
- Create: `src-tauri/tests/fixtures/plugin-packages/valid-ui/plugin.json`
- Create: `src-tauri/tests/fixtures/plugin-packages/valid-native/plugin.json`
- Create: `docs/plugins/manifest.md`
- Modify: `crates/jarvis-plugin-protocol/src/lib.rs`
- Modify: `crates/jarvis-plugin-protocol/Cargo.toml`
- Modify: `src-tauri/src/plugins/mod.rs`
- Modify: `src-tauri/Cargo.toml`

- [x] **Step 1: Add RED contract tests for strict manifest parsing**

Create `crates/jarvis-plugin-protocol/tests/manifest_contract.rs`:

```rust
use jarvis_plugin_protocol::manifest::{ManifestV2, RuntimeKind};

#[test]
fn parses_namespaced_ui_manifest() {
    let manifest = ManifestV2::parse(include_bytes!(
        "../../../src-tauri/tests/fixtures/plugin-packages/valid-ui/plugin.json"
    )).unwrap();
    assert_eq!(manifest.id.as_str(), "dev.example.hello-page");
    assert_eq!(manifest.compatibility.plugin_api, 2);
    assert_eq!(manifest.runtime.kind, RuntimeKind::UiOnly);
}

#[test]
fn unknown_security_field_is_rejected() {
    let raw = br#"{
      "schemaVersion":2,"id":"dev.example.echo","name":"Echo","version":"1.0.0",
      "publisher":"example","compatibility":{"jarvis":">=0.4.0, <0.5.0","pluginApi":2},
      "runtime":{"kind":"ui-only","protocol":2,"activationEvents":[],"escapeSandbox":true},
      "permissions":[],"state":{"schemaVersion":1,"migrations":[],"rollbackCompatibleThrough":1},
      "contributes":{"pages":[],"commands":[],"actions":[],"hotkeys":[],"settings":[],
                     "projectRuntimes":[],"dataContracts":[]}
    }"#;
    assert_eq!(ManifestV2::parse(raw).unwrap_err().code(), "manifest_schema");
}
```

Also cover invalid SemVer/range, non-namespaced community ID, reserved short ID with non-owner publisher, duplicate
contribution IDs, remote `$ref`, manifest over 256 KiB, more than 64 nested levels and unresolved `${target}` in a
packaged manifest.

- [x] **Step 2: Run the contract test and verify RED**

Run:

```bash
cargo test --manifest-path crates/jarvis-plugin-protocol/Cargo.toml --test manifest_contract
```

Expected: FAIL because `manifest` and its fixtures do not exist.

- [x] **Step 3: Add the complete strict schema and typed DTOs**

`schemas/plugin-manifest-v2.schema.json` is the bundled source of truth. It has:

- root and every object definition set to `"additionalProperties": false`;
- required `schemaVersion`, `id`, `name`, `version`, `publisher`, `compatibility`, `runtime`, `permissions`, `state`
  and `contributes`;
- typed definitions for runtime/service, permission, page, command/handler, action, hotkey, setting, project runtime
  and data contract exactly matching the approved design;
- local `#/$defs/...` references only;
- bounded strings/arrays, unique IDs and enums for risk, placement, runtime kind/lifecycle and sensitivity.

`manifest.rs` exposes newtypes `PluginId`, `PublisherId`, `Digest`, `ContractId`, `RelativePackagePath` and
`ManifestV2`. Parse SemVer/ranges with `semver`, reject unresolved template markers and perform cross-field validation
that JSON Schema cannot express:

```rust
pub struct ManifestV2 {
    pub schema_version: u32,
    pub id: PluginId,
    pub name: String,
    pub version: semver::Version,
    pub publisher: PublisherId,
    pub compatibility: Compatibility,
    pub runtime: RuntimeDeclaration,
    pub permissions: Vec<PermissionDeclaration>,
    pub state: StateDeclaration,
    pub contributes: Contributions,
}
```

All contribution IDs are unique within a plugin; handlers may reference only declared pages/commands; manifest paths
must be normalized relative paths; `admin`/arbitrary shell permissions do not exist.

- [x] **Step 4: Add host-side bounded validation**

`src-tauri/src/plugins/manifest_v2.rs` embeds the schema bytes and provides:

```rust
pub fn validate_source_manifest(bytes: &[u8], target: &Target) -> Result<ManifestV2, ManifestError>;
pub fn validate_packaged_manifest(bytes: &[u8], target: &Target) -> Result<ManifestV2, ManifestError>;
```

Before schema compilation, walk JSON iteratively and reject more than 20,000 nodes, depth over 64, strings over 64 KiB
and input over 256 KiB. Reject every `$ref` not beginning with `#/$defs/`. Source validation substitutes the finite
`${target}` token only in declared runtime entry fields; packaged validation rejects every `${...}` sequence. Return
stable codes `manifest_too_large`, `manifest_too_deep`, `manifest_schema`, `manifest_semver`,
`manifest_incompatible`, and `manifest_unresolved_target`.

Do not modify or delete `src-tauri/src/plugins/manifest.rs`; that file remains the v1 compatibility parser.

- [x] **Step 5: Run focused host and protocol tests**

Run:

```bash
cargo test --manifest-path crates/jarvis-plugin-protocol/Cargo.toml manifest
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features plugins::manifest_v2::tests
```

Expected: all manifest tests pass, including bounded malicious inputs in under one second each.

- [x] **Step 6: Document the author contract**

`docs/plugins/manifest.md` includes the complete minimal UI-only and verified-native examples, ID/publisher rules,
compatibility rules, path restrictions, permission diff behavior, schema command and the explicit statement that
Manifest v1 is accepted only for the built-in `agent-vm` transition.

- [x] **Step 7: Commit**

```bash
git add schemas/plugin-manifest-v2.schema.json crates/jarvis-plugin-protocol \
  src-tauri/src/plugins/manifest_v2.rs src-tauri/src/plugins/mod.rs src-tauri/Cargo.toml \
  src-tauri/tests/fixtures/plugin-packages docs/plugins/manifest.md
git commit -m "feat(plugins): validate strict manifest v2"
```

Completion evidence (2026-07-31): A1 landed as `61c6fbb`; A2 landed as
`be060f1` plus review fixes in `b8a89c7`. Independent re-review approved the
closed schema, path and namespace rules. Rust 1.77.2 passed all three public
crate test and clippy gates; the integrated branch additionally passed 18
Manifest v2 host tests, seven unchanged Manifest v1 tests, plugin-boundary and
public-secret guards.

---

### Task A3: Build and inspect deterministic `.jarvis-plugin` tar archives

**Files:**

- Create: `schemas/plugin-package-v1.schema.json`
- Create: `schemas/plugin-package-signature-v1.schema.json`
- Create: `crates/jarvis-plugin-protocol/schema/plugin-package-v1.schema.json`
- Create: `crates/jarvis-plugin-protocol/schema/plugin-package-signature-v1.schema.json`
- Create: `crates/jarvis-plugin-protocol/src/package.rs`
- Create: `src-tauri/src/plugins/package/mod.rs`
- Create: `src-tauri/src/plugins/package/pack.rs`
- Create: `src-tauri/src/plugins/package/source.rs`
- Create: `src-tauri/src/plugins/package/spool.rs`
- Create: `src-tauri/src/plugins/package/archive.rs`
- Create: `src-tauri/src/plugins/package/extract.rs`
- Create: `src-tauri/src/plugins/package/hash.rs`
- Create: `src-tauri/src/plugins/package/jcs.rs`
- Create: `src-tauri/tests/fixtures/plugin-packages/pack-source/plugin.json`
- Create: `src-tauri/tests/fixtures/plugin-packages/pack-source/ui/index.html`
- Create: `src-tauri/tests/fixtures/plugin-packages/pack-source/schemas/message.schema.json`
- Create: `src-tauri/tests/fixtures/plugin-packages/golden/darwin-arm64.jarvis-plugin`
- Create: `src-tauri/tests/fixtures/plugin-packages/golden/darwin-arm64.sha256`
- Create: `src-tauri/tests/fixtures/plugin-trust/README.md`
- Create: `src-tauri/tests/fixtures/plugin-trust/package-test-signing-seed.hex`
- Create: `src-tauri/tests/fixtures/plugin-trust/package-test-public-key.hex`
- Create: `src-tauri/tests/plugin_package_dependency_msrv.rs`
- Modify: `crates/jarvis-plugin-protocol/src/lib.rs`
- Modify: `crates/jarvis-plugin-protocol/src/manifest.rs`
- Modify: `crates/jarvis-plugin-protocol/Cargo.toml`
- Modify: `crates/jarvis-plugin-sdk/Cargo.lock`
- Modify: `crates/jarvis-plugin-test-host/Cargo.lock`
- Modify: `src-tauri/src/plugins/mod.rs`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/Cargo.lock`
- Modify: `scripts/check-plugin-boundaries.sh`
- Modify: `scripts/check-plugin-boundaries.test.sh`

This task owns the package wire format, byte-for-byte archive profile, bounded parser and quarantine extraction. It
does **not** decide whether a signer is trusted. A4 owns trust, catalog and revocation checks and is the only production
implementation of the verification callback defined below. A5 owns atomic activation of an already extracted,
verified quarantine directory.

- [ ] **Step 1: Pin the dependency and MSRV surface before writing format code**

Use these exact direct dependencies and features:

```toml
# crates/jarvis-plugin-protocol/Cargo.toml
unicode-normalization = { version = "=0.1.24", default-features = false, features = ["std"] }

# src-tauri/Cargo.toml [dependencies]
base64 = { version = "=0.22.1", default-features = false, features = ["std"] }
caseless = "=0.2.2"
libc = { version = "=0.2.186", default-features = false, features = ["std"] }
rustix = { version = "=1.1.4", default-features = false, features = ["fs", "std"] }
serde_json_canonicalizer = "=0.3.2"
sha2 = { version = "=0.10.9", default-features = false, features = ["std"] }
tar = { version = "=0.4.46", default-features = false }
tempfile = { version = "=3.27.0", default-features = false, features = ["getrandom"] }
unicode-normalization = { version = "=0.1.24", default-features = false, features = ["std"] }

# src-tauri/Cargo.toml [dev-dependencies]
ed25519-dalek = { version = "=2.1.1", default-features = false, features = ["fast", "std", "zeroize"] }
```

`tar` is permitted only for `tar::Header::new_gnu()` and low-level `Builder::append()` when producing a stream.
Production code must not call `append_path`, `append_file`, `append_dir_all`, `Archive::entries`, `Entry::unpack` or
`Archive::unpack`; those APIs infer metadata or interpret extensions outside this profile. `tempfile` owns the
unlinked spool. `rustix` owns fd-relative filesystem calls. The only A3 `libc` use is a small macOS-only RAII wrapper
around `fdopendir`/`readdir`, because `rustix` does not expose macOS directory iteration.

Keep the protocol, SDK, test-host and host at their declared `rust-version = "1.77.2"`. `ed25519-dalek` 2.1.1 has
MSRV 1.60 and is test-only in A3; A4 promotes the same exact version and feature set to a host production dependency.
JCS, tar, filesystem and signature libraries must not enter the protocol, SDK or test-host normal dependency graphs.

Because not every selected crate declares reliable MSRV metadata, add
`src-tauri/tests/plugin_package_dependency_msrv.rs`. It must compile and call the exact A3 APIs used from base64,
caseless, rustix/fs, serde_json_canonicalizer, sha2, tar, tempfile, unicode-normalization and the test-only
ed25519-dalek signer; a type-only unused import is insufficient. Do not claim the host MSRV is preserved until this
target and the integrated host have actually compiled with Rust 1.77.2. If either fails, choose an older compatible
exact version and repeat the gate; do not silently raise the declared MSRV in A3.

After adding the probe but before adding the Cargo dependencies, run:

```bash
cargo +1.77.2 test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test plugin_package_dependency_msrv
```

Expected: FAIL with unresolved A3 dependency/API imports. Then add the exact Cargo entries above, update each committed
lockfile without changing unrelated versions, and run:

```bash
cargo +1.77.2 update --manifest-path crates/jarvis-plugin-protocol/Cargo.toml \
  -p unicode-normalization --precise 0.1.24
cargo +1.77.2 update --manifest-path crates/jarvis-plugin-sdk/Cargo.toml \
  -p unicode-normalization --precise 0.1.24
cargo +1.77.2 update --manifest-path crates/jarvis-plugin-test-host/Cargo.toml \
  -p unicode-normalization --precise 0.1.24
cargo +1.77.2 update --manifest-path src-tauri/Cargo.toml \
  -p unicode-normalization --precise 0.1.24
cargo +1.77.2 check --locked --manifest-path crates/jarvis-plugin-protocol/Cargo.toml
cargo +1.77.2 check --locked --manifest-path crates/jarvis-plugin-sdk/Cargo.toml
cargo +1.77.2 check --locked --manifest-path crates/jarvis-plugin-test-host/Cargo.toml
cargo +1.77.2 test --locked --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test plugin_package_dependency_msrv
cargo +1.77.2 check --locked --manifest-path src-tauri/Cargo.toml --no-default-features
cargo tree --manifest-path crates/jarvis-plugin-protocol/Cargo.toml -e normal
cargo tree --locked --manifest-path src-tauri/Cargo.toml \
  -i unicode-normalization@0.1.24
! cargo tree --locked --manifest-path src-tauri/Cargo.toml \
  | rg 'unicode-normalization v0\\.1\\.25'
```

Expected after the edit: every command exits `0`; the public protocol tree contains `unicode-normalization 0.1.24` but none of
`tar`, `rustix`, `libc`, `tempfile`, `sha2`, `base64`, `caseless`, `serde_json_canonicalizer` or `ed25519-dalek`.

- [ ] **Step 2: Add RED wire-schema, JCS and signature tests**

Define the following strict public DTOs in `crates/jarvis-plugin-protocol/src/package.rs`. All structs use
`#[serde(rename_all = "camelCase", deny_unknown_fields)]`; all enum values below are the exact JSON strings:

```rust
pub const PACKAGE_SCHEMA_VERSION: u32 = 1;

pub struct PackageMetadataV1 {
    pub schema_version: u32,
    pub plugin_id: PluginId,
    pub publisher: PublisherId,
    pub version: Version,
    pub manifest_digest: Digest,
    pub target: PackageTarget,
    pub minimum_macos: MacOsVersion,
    pub jarvis_range: VersionRange,
    pub plugin_api: u32,
    pub state: StateDeclaration,
    pub files: Vec<PackageFile>,
    pub payload_root: Digest,
}

pub struct PackageFile {
    pub path: PackagePath,
    pub kind: PackageFileKind, // only "regular"
    pub mode: PackageFileMode, // only "0444" or "0555"
    pub size: u64,
    pub digest: Digest,
}

pub enum PackageTarget { DarwinArm64, DarwinAmd64 } // "darwin-arm64", "darwin-amd64"
pub enum PackageFileKind { Regular }                // "regular"
pub enum PackageFileMode { ReadOnly, Executable }   // "0444", "0555"

pub struct PackageSignatureV1 {
    pub algorithm: SignatureAlgorithm, // only "ed25519"
    pub key_id: String,
    pub value: String,
}
```

`MacOsVersion` accepts and emits exactly three decimal numeric components (`major.minor.patch`), with no leading zero
except the component `0`, no sign, whitespace, prerelease or build suffix. `PackagePath` is relative NFC UTF-8, at most
1,024 UTF-8 bytes total and 255 bytes per non-empty `/`-separated component. It rejects `.`, `..`, a leading or trailing
slash, repeated slash, backslash, NUL, C0/C1 controls and `%`, `?`, `#`, `:`. `PackageSignatureV1.keyId` is 1–128 ASCII
bytes matching `[A-Za-z0-9._:-]+`; `value` is canonical RFC 4648 padded standard base64 and decodes to exactly 64 bytes.

Create closed Draft 2020-12 metadata and detached-signature schemas in both schema locations. Every object has
`additionalProperties: false`, every string has explicit length/pattern constraints, every integer has a minimum and
maximum, and local `$ref` values stay inside its file. Each root schema is byte-identical to its protocol-crate copy.
The host validates `package.json` against the metadata schema and `SIGNATURE` against the detached-signature schema
before typed deserialization. Add protocol tests named:

```text
package_schema_copies_are_byte_identical
package_signature_schema_copies_are_byte_identical
package_schema_rejects_unknown_fields_and_wrong_enum_spellings
package_path_accepts_exact_1024_bytes_and_rejects_1025
package_path_accepts_exact_255_byte_component_and_rejects_256
macos_version_requires_canonical_three_component_form
signature_requires_canonical_padded_base64_of_64_bytes
package_metadata_round_trips_without_wire_field_drift
```

In `src-tauri/src/plugins/package/jcs.rs`, add RED golden tests for RFC 8785 number formatting, JSON string escaping and
UTF-16 property-name ordering, plus rejection tests for duplicate object keys, a BOM, trailing newline/whitespace and
non-canonical property/number encodings. A valid raw `package.json` or `SIGNATURE` must equal the exact bytes produced
by `serde_json_canonicalizer 0.3.2` after bounded parsing; syntactically equivalent non-JCS bytes are invalid.

Refactor the existing bounded duplicate-key-aware manifest JSON reader into
`parse_bounded_json_with_limits(reader, JsonLimits)` in the protocol crate and preserve the old manifest wrapper and
its tests. Package limits are specified in Step 7. The canonicalizer is invoked only after this parser succeeds.

Run:

```bash
cargo test --manifest-path crates/jarvis-plugin-protocol/Cargo.toml package
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package::jcs::tests
```

Expected: FAIL first because the package module, schemas and JCS adapter do not exist.

- [ ] **Step 3: Implement the exact metadata, equality and hash contract**

The source `plugin.json` is parsed with Manifest v2 limits, `${target}` is resolved to the selected
`darwin-arm64`/`darwin-amd64` token, the resolved manifest is validated, then the concrete packaged `plugin.json` is
written as exact JCS bytes. Packing options must explicitly provide `minimum_macos`; it is not inferred from the host.

`PackageMetadataV1` must equal its packaged content:

- `pluginId`, `publisher`, `version`, `jarvisRange`, `pluginApi` and the entire `state` object equal the concrete
  packaged Manifest v2 fields;
- `target` is the target used for `${target}` substitution;
- `manifestDigest` is SHA-256 of the exact packaged `plugin.json` bytes;
- `files[0]` is `plugin.json`; the remaining records are every other source payload file in NFC UTF-8 byte order;
- only package-generated `package.json` and `SIGNATURE` are excluded from `files`;
- `0555` appears exactly on verified-native `runtime.bridgeEntry` and, when present, `runtime.service.entry`; every
  other payload file is `0444`.

Reject a missing declared native entry, an executable file not declared by the manifest, any source entry named
`package.json` or any case/canonical-equivalent of `SIGNATURE`, and any metadata/manifest mismatch. Do not inherit
source mode, owner, timestamp or extended attributes.

Compute `payloadRoot` over the already sorted `files` array:

```text
leaf   = SHA256("jarvis-plugin-file-v1\0" || JCS(PackageFile))
parent = SHA256("jarvis-plugin-merkle-v1\0" || left_32_bytes || right_32_bytes)
```

Duplicate the final node at each odd-width level. There is no empty-tree value because `plugin.json` is mandatory.
`Digest` always serializes as lowercase `sha256:` followed by exactly 64 lowercase hex digits.

The exact detached-signature message is:

```text
ASCII bytes "jarvis-plugin-package-v1" || one NUL byte || exact canonical package.json bytes
```

`SIGNATURE` is the exact JCS serialization of `PackageSignatureV1`, with no BOM, whitespace or newline. Replace the
old hash-shaped fake signer with an Ed25519 fixture signer built from
`package-test-signing-seed.hex`; commit only this explicitly test-only seed and its matching public key under
`src-tauri/tests/fixtures/plugin-trust`. Its README must say the seed is public deterministic test material, is not a
credential and is forbidden for release catalogs. Add tests for a fixed expected signature value, one-bit
message/signature/key changes, field equality, file ordering, executable-mode selection, Merkle roots for
one/two/three/five leaves and schema/runtime DTO equality.

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package::hash::tests
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package::pack::tests::metadata_equals_concrete_manifest
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package::pack::tests::fixed_ed25519_signature_matches_golden
```

Expected after implementation: all commands exit `0`; no production private key or fixture signer is compiled outside
`#[cfg(test)]`.

- [ ] **Step 4: Add RED source-race tests, then build an fd-only immutable spool**

`source.rs` and `spool.rs` must never use a pre-validated pathname later. Open the source root once as an `OwnedFd`
with `RDONLY|DIRECTORY|NOFOLLOW|CLOEXEC` and retain its `fstat` identity. Enumerate through an isolated safe RAII
wrapper around `fdopendir`/`readdir` operating on a duplicated directory fd. Only directories and regular files are
allowed. Retain at most one fd per current recursion-depth component; the maximum depth is 64.

For each candidate, record normalized path plus device, inode, type, size, mtime and ctime from `fstatat` without
following links; regular files must also have link count one. Compare full second and nanosecond timestamp fields, not
rounded display values. Sort paths, then reopen every component from the held root fd with `openat` and
`NOFOLLOW|CLOEXEC` (`DIRECTORY` on parent components), and require the reopened `fstat` identity/type to match.
Copy each regular file exactly once into one `0600`, link-count-zero aggregate `tempfile::tempfile()` spool, use checked `u64`
offsets, hash while copying, and compare file metadata before and after the copy. Any replacement or mutation returns
stable code `source_raced`. Tar construction reads only `(offset, length)` spans from this spool and never reopens the
source. Manifest parsing and `${target}` resolution consume the spooled `plugin.json` span, never the source path;
the bounded concrete JCS manifest may then be appended to the spool as generated content. Before returning archive
content, write the archive into a second owner-only unlinked tempfile, inspect that completed file, and compare its
metadata and payload digests to the packer's expected records. Only then seek that same archive fd to zero and stream
it to the caller's `Write`; a short/erroring destination returns failure. The public API never exposes the unchecked
temporary file.

Add deterministic barrier-driven tests:

```text
source_file_replaced_after_enumeration_never_packages_outside_bytes
source_file_changed_to_symlink_before_open_is_source_raced
source_parent_directory_swap_is_source_raced
source_file_mutated_during_copy_is_source_raced
source_inode_reused_with_different_metadata_is_source_raced
tar_writer_reads_only_spool_after_source_snapshot
```

For every race, the only allowed outcomes are `source_raced` or a valid self-consistent package containing bytes read
from the original in-root fd. It must never contain attacker-controlled out-of-root bytes. Do not use sleeps; expose
test-only barriers at the enumerate/open/copy boundaries.

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package::source::tests -- --test-threads=1
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package::spool::tests -- --test-threads=1
```

Expected: RED until fd-relative snapshotting exists, then every race test is deterministic and passes 100 consecutive
iterations:

```bash
for i in $(seq 1 100); do
  cargo test --quiet --manifest-path src-tauri/Cargo.toml --no-default-features \
    plugins::package::source::tests::source_parent_directory_swap_is_source_raced -- --exact
done
```

- [ ] **Step 5: Specify and golden-test the only accepted GNU tar byte profile**

Packing is uncompressed. Use GNU headers only: bytes 257–262 are exactly `b"ustar "` and bytes 263–264 exactly
`b" \0"`. A test first compares those bytes with `tar 0.4.46`'s `Header::new_gnu()` and then asserts the literal bytes,
so a future tar upgrade cannot silently redefine the profile.

Numeric fields never use base-256 and have these exact encodings:

```text
mode, uid, gid, devmajor, devminor: 7 leading-zero octal digits + NUL (8 bytes)
size and mtime:                       11 leading-zero octal digits + NUL (12 bytes)
checksum:                            6 leading-zero octal digits + NUL + ASCII space (8 bytes)
```

Thus zero is exactly `b"0000000\0"` in an 8-byte numeric field and `b"00000000000\0"` in a 12-byte field; modes are
exactly `b"0000444\0"`, `b"0000555\0"` or, for the long-name extension, `b"0000644\0"`. Reject values above
`0o7777777` for an 8-byte field, above `0o77777777777` for a 12-byte field and above `0o777777` for checksum before
encoding. The checksum sum is calculated with eight ASCII spaces in its field. Do not use `Header::set_cksum`, whose
0.4.46 spelling is not this six-digit-plus-NUL-plus-space profile; write the checked profile checksum into
`Header::as_mut_bytes()` after every other byte is final.

Every logical regular file has typeflag ASCII `b'0'`, never NUL, uid/gid/mtime/devmajor/devminor `0`, all-NUL
owner/group/link fields, and mode exactly its metadata mode. All GNU atime, ctime, offset, longnames, sparse,
isextended, realsize, unused and padding fields are all-NUL bytes. File bodies are followed by zero padding to the next
512-byte boundary.

For a logical path of at most 100 bytes, put it directly in the regular header name. For a path of 101 through 1,024
bytes, manually emit exactly one GNU long-name extension immediately before its regular header:

```text
extension header name: "././@LongLink"
extension typeflag:    "L"
extension mode:        0644
extension uid/gid/mtime/devmajor/devminor: 0
extension size:        logical path byte length + 1
extension body:        exact UTF-8 path bytes + exactly one NUL + zero block padding
following regular header name: "././@LongFile"
```

The following regular header contains the real payload metadata and size. Never set a prefix. Never emit PAX,
USTAR, GNU sparse, global extensions, link entries or directory entries. Logical order is exact:

```text
plugin.json
remaining payload paths in NFC UTF-8 byte order
package.json
SIGNATURE
```

Finish with exactly two 512-byte zero blocks and immediate EOF. No additional zero record or concatenated archive is
allowed.

Add raw 512-byte header assertions for the literal magic/version, all three exact zero encodings, modes `0444`,
`0555`, `0644`, checksum spelling and ASCII `0`/`L` typeflags, plus paths of length 100, 101 and 1,024. The 1,024 case
must be a valid multi-component NFC Unicode path whose components remain at most 255 bytes. Add negative 1,025-byte path and
256-byte-component cases and direct encoder overflow tests for every numeric field width. Commit
`golden/darwin-arm64.jarvis-plugin` and its lowercase archive SHA-256, generated only
by an ignored `regenerate_package_golden` test. The normal test compares every byte and digest to the committed files.

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package::pack::tests::gnu_header_profiles_are_byte_exact
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package::pack::tests::identical_input_matches_committed_archive_golden
git diff --exit-code -- src-tauri/tests/fixtures/plugin-packages/golden
```

Expected: the tests exit `0` and a normal test run never rewrites the goldens.

- [ ] **Step 6: Add RED raw-parser failures, then implement a bounded 512-byte state machine**

`archive.rs` parses raw `Read` input itself in fixed buffers; it must not use tar's logical archive iterator. The state
machine has only `ExpectHeader`, `ExpectLongNameBody`, `ExpectLongNameTarget`, `ReadRegularBody`,
`ExpectSecondZeroBlock` and `ExpectEof`. A long-name record is allowed at most once, only in the canonical form from
Step 5, and must be immediately followed by the canonical placeholder regular header for a logical name longer than
100 bytes. It also enforces the Step 5 logical order: exactly one `plugin.json` first, strictly increasing remaining
payload names, exact canonical `package.json` penultimate and exact canonical `SIGNATURE` final. Generated metadata
names are short and therefore may not use a long-name record.

Before decoding, reject a numeric field whose high bit is set, non-octal bytes, non-canonical octal padding, a bad
checksum, non-zero body padding and any header byte that differs from this profile. Explicitly reject typeflags
`x`, `g`, `K`, `S`, links, directories and all special types; that includes PAX `size`, `path` and `GNU.sparse.*`
overrides, old GNU sparse continuation chains, repeated/orphan long-name records and use of a long-name record for a
path of at most 100 bytes. Reject truncation at every header/body/padding/terminator boundary, one zero block, non-zero
bytes or another archive after the two terminator blocks, and read errors after the apparent terminator.

Assert stable public codes rather than matching prose: malformed/non-canonical header, number or checksum is
`archive_header`; forbidden type/extension is `archive_entry_type`; invalid path is `archive_path`; duplicate exact
name is `archive_duplicate`; caseless collision is `archive_case_collision`; wrong mandatory order is
`archive_order`; a short stream is `archive_truncated`; post-terminator data is `archive_trailing`; any table limit or
checked-arithmetic overflow is `archive_quota`; package/signature schema, JCS or cross-field failure is
`package_metadata`.

Count raw records separately from logical entries:

```text
maximum payload files:          20,000
maximum logical entries:        20,002 (payload + package.json + SIGNATURE)
maximum raw records:            40,002 (one optional long-name record per logical entry)
maximum GNU long-name body:      1,025 bytes (1,024-byte path + NUL)
```

All counters, sizes, offsets, block rounding and totals use checked `u64` arithmetic. Add raw byte fixtures generated
inside tests, not through `tar`, for:

```text
archive_rejects_base256_size_before_decode
archive_rejects_noncanonical_octal_and_checksum
archive_rejects_pax_global_local_and_sparse_extensions
archive_rejects_repeated_orphan_and_short_gnu_longname
archive_rejects_truncated_header_body_padding_and_terminator
archive_rejects_nonzero_padding_and_trailing_concatenated_archive
archive_rejects_links_directories_devices_fifo_socket_and_sparse
archive_rejects_absolute_dot_empty_backslash_nul_and_non_nfc_paths
archive_rejects_duplicate_normalized_names
archive_accepts_exact_raw_and_logical_record_limits
archive_rejects_raw_and_logical_record_limits_plus_one
```

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package::archive::tests
```

Expected: RED before the manual state machine, then every malformed stream returns a stable `archive_*` code without
panic, allocation proportional to declared entry size, or bytes written to disk.

- [ ] **Step 7: Pin Unicode collision semantics and enforce every streaming quota**

The collision key for a complete logical path is exactly:

```text
Unicode 16.0 NFD -> Unicode 16.0 full default non-Turkic case fold -> Unicode 16.0 NFD
```

Use `unicode-normalization 0.1.24` and `caseless 0.2.2`; add source-code comments and tests asserting that both pinned
tables are Unicode 16.0 with
`assert_eq!(unicode_normalization::UNICODE_VERSION, (16, 0, 0))` and
`assert_eq!(caseless::UNICODE_VERSION, (16, 0, 0))`. Do not use locale-sensitive casing or macOS filesystem
comparison. Apply the key to all
payload names and the generated/reserved names `package.json` and `SIGNATURE`. Test `Straße`/`STRASSE`, all Greek
sigma forms, `I`/`i` collision, `İ`/`i` non-collision, Kelvin-sign input rejection because its NFC form is `K`,
`K`/`k` collision, decomposed non-NFC input rejection and reserved-name case/canonical collisions.

Collision checking covers a namespace, not only final file strings: insert every logical file and every proper implicit
directory prefix with its file/directory kind, plus the generated metadata files. Reject two spellings with one key
and reject any file/directory conflict, including `a` versus `A/b` and `signature/payload` versus generated
`SIGNATURE`. Extraction must consume this already validated namespace plan rather than recomputing looser rules.

`ArchiveLimits::production()` is the following immutable table:

```text
physical archive bytes:         2 GiB
unpacked payload bytes:         2 GiB
single payload file:            512 MiB
payload files:                  20,000
logical entries:                20,002
raw records:                    40,002
path bytes:                     1,024
component bytes:                255
path depth:                     64
namespace trie nodes:           100,000 (files plus unique implicit directories)
caseless collision-key bytes:   4,096 per namespace path/prefix
GNU long-name body:             1,025 bytes
package.json bytes:             16 MiB
SIGNATURE bytes:                4 KiB
plugin.json bytes:              256 KiB
package JSON nesting:           64
package JSON nodes:             250,000
package JSON string bytes:      64 KiB
```

The physical count includes headers, extension bodies, padding, both zero blocks and bytes observed while proving
EOF. Unpacked total counts only logical payload file bodies, including `plugin.json`, and excludes generated metadata
entries. Packing stages through the Step 4 archive tempfile and then copies to a caller-supplied `Write`; only
`#[cfg(test)] pack_to_vec` may materialize a complete archive.

Inspection pass 1 streams and hashes every physical and payload byte in fixed buffers. It may retain only bounded
`package.json`, bounded `SIGNATURE`, the bounded packaged `plugin.json`, and at most 20,000 compact observation
records containing path, mode, size, digest and raw offsets plus the bounded 100,000-node collision trie; it may not
retain payload bodies. Add exact-limit and
limit-plus-one tests for every table row, short/chunked/error-injecting readers, checked-overflow synthetic headers and
an ignored RSS probe which inspects a sparse synthetic near-2-GiB stream while staying below 128 MiB RSS. The normal
suite must prove the same property through an allocation-counting reader without allocating a 2-GiB `Vec`.

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package::archive::tests::unicode_collision_vectors
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package::archive::tests::all_limits_accept_exact_and_reject_plus_one
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package::archive::tests::inspection_memory_is_bounded
```

Expected: all commands exit `0`; limit failures are reported before any output file is created.

- [ ] **Step 8: Add the opaque A4 verification handoff and fd-only two-pass extraction**

Pass 1 takes an already opened archive `File`, runs the strict parser, computes the physical archive digest and all
payload digests, parses and cross-checks package metadata, and records the archive fd identity. Cross-checking requires
schema version 1; exact observed file order/path/mode/size/digest equality with `files`; recomputed manifest digest and
payload Merkle root; and exact Manifest v2 identity, compatibility, state and native-entry mode equality from Step 3.
It then invokes:

```rust
pub(crate) trait PackageTrustVerifier {
    fn verify(
        &self,
        observation: &UntrustedPackageObservation<'_>,
    ) -> Result<(), PackageTrustError>;
}
```

`UntrustedPackageObservation` contains exact canonical `package.json` and `SIGNATURE` bytes, physical archive digest
and parsed metadata but exposes no method that can create trusted state. Only the package module can construct
`VerifiedPackageEvidence`; its fields are private and it owns the same open `File`, the pass-1 observation and fd
identity. Production has no permissive verifier: A4 supplies `CatalogPackageVerifier`; only `#[cfg(test)]` may supply a
fixture verifier. Extend `check:plugin-boundaries` with a source guard that permits production
`impl PackageTrustVerifier` only in `src-tauri/src/plugins/trust/package.rs` and permits fixture implementations only
inside `#[cfg(test)]` modules.

A4 must verify the catalog/root/publisher lineage, rotation, freshness and revocation first, then require exact
equality between the selected `CatalogRelease` and this observation for plugin ID, publisher, version, target,
`minimumMacos`, Jarvis range, plugin API, archive digest, package-signature algorithm/key ID/value and publisher-key
lineage. It verifies Ed25519 over the exact Step 3 message before returning success. An A3 caller cannot extract from
an observation, boolean, path or parsed DTO; extraction consumes the opaque evidence.

Pass 2 seeks the same held fd to byte zero and reruns the strict raw parser. It recomputes the physical digest and
requires the exact same canonical `package.json`, `SIGNATURE`, entry plan and fd identity before exposing output.
A pathname replacement is irrelevant because it is never reopened; mutation of the held inode causes
`archive_changed_after_verification`. No destination entry is created until all A4 checks have passed.

Create an owner-only new quarantine root below a caller-supplied held parent `OwnedFd`, using `mkdirat` then
`openat(DIRECTORY|NOFOLLOW|CLOEXEC)` and validating owner, type and identity. Archive directory entries do not exist.
Create implicit directories in sorted order through held fd stacks with mode `0700`; `EEXIST` is an error. Create each
file with `openat(WRONLY|CREATE|EXCL|NOFOLLOW|CLOEXEC, 0600)`, stream exact bytes while hashing, require regular type
and link count one, then `fchmod` to `0444` or `0555`. `fsync` and macOS `fcntl(F_FULLFSYNC)` every file, then fsync
directories bottom-up. A5 owns final rename/activation and its power-loss semantics.

On any failure, close files and remove exactly the recorded created files/directories in reverse order using
`unlinkat` under retained parent fds; remove the quarantine root by its retained parent fd/name only after matching its
recorded identity. A cleanup failure returns a quarantined/manual-cleanup result and never returns activation
evidence. Never recurse by pathname during cleanup.

Add RED tests:

```text
bad_signature_never_creates_quarantine_output
catalog_digest_mismatch_never_creates_quarantine_output
extract_requires_opaque_verified_package_evidence
archive_path_swap_after_verification_reads_same_fd
same_inode_mutation_after_verification_is_rejected
second_pass_requires_identical_package_signature_and_entry_plan
symlink_parent_and_final_component_are_rejected
preexisting_file_hardlink_and_special_file_are_rejected
short_write_digest_mismatch_and_fsync_failure_cleanup_exactly
cleanup_race_cannot_unlink_outside_quarantine
successful_extract_uses_declared_modes_and_link_count_one
```

Use compile-fail doctests or `trybuild` fixtures for the opaque-evidence construction test. Use injected fd/fsync
adapters and barriers, never sleeps. Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package::extract::tests
cargo test --doc --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package
```

Expected: RED until the verifier/evidence split and same-fd two-pass path exist, then all commands exit `0`;
verification failure leaves no quarantine directory and no production caller can forge evidence.

- [ ] **Step 9: Run the complete package security and reproducibility gate**

Run:

```bash
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo +1.77.2 test --locked --manifest-path crates/jarvis-plugin-protocol/Cargo.toml
cargo +1.77.2 check --locked --manifest-path crates/jarvis-plugin-sdk/Cargo.toml
cargo +1.77.2 check --locked --manifest-path crates/jarvis-plugin-test-host/Cargo.toml
cargo +1.77.2 clippy --locked --manifest-path crates/jarvis-plugin-protocol/Cargo.toml \
  --all-targets -- -D warnings
cargo +1.77.2 test --locked --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test plugin_package_dependency_msrv
cargo +1.77.2 test --locked --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package
cargo +1.77.2 clippy --locked --manifest-path src-tauri/Cargo.toml \
  --no-default-features --all-targets -- -D warnings
npm run check:plugin-boundaries
npm run check:public
```

Expected: all commands exit `0`. The package test log includes the committed archive digest, exact/plus-one quota
vectors, raw-parser failures, 100-iteration source races and same-fd mutation tests. `check:public` scans the documented
test seed as public fixture material and finds no credential or production private key.

- [ ] **Step 10: Commit**

```bash
git add schemas/plugin-package-v1.schema.json schemas/plugin-package-signature-v1.schema.json \
  crates/jarvis-plugin-protocol \
  crates/jarvis-plugin-sdk/Cargo.lock crates/jarvis-plugin-test-host/Cargo.lock \
  src-tauri/src/plugins/package src-tauri/src/plugins/mod.rs src-tauri/Cargo.toml \
  src-tauri/Cargo.lock src-tauri/tests/fixtures/plugin-packages \
  src-tauri/tests/fixtures/plugin-trust/README.md \
  src-tauri/tests/fixtures/plugin-trust/package-test-signing-seed.hex \
  src-tauri/tests/fixtures/plugin-trust/package-test-public-key.hex \
  src-tauri/tests/plugin_package_dependency_msrv.rs scripts/check-plugin-boundaries.sh \
  scripts/check-plugin-boundaries.test.sh
git diff --cached --check
git commit -m "feat(plugins): add deterministic package format"
```

---

### Task A4: Verify signed catalogs, publishers, rotations and revocations

**Files:**

- Create: `schemas/plugin-catalog-v1.schema.json`
- Create: `crates/jarvis-plugin-protocol/schema/plugin-catalog-v1.schema.json`
- Create: `crates/jarvis-plugin-protocol/src/catalog.rs`
- Create: `src-tauri/src/plugins/trust/mod.rs`
- Create: `src-tauri/src/plugins/trust/signature.rs`
- Create: `src-tauri/src/plugins/trust/catalog.rs`
- Create: `src-tauri/src/plugins/trust/package.rs`
- Create: `src-tauri/resources/plugin-trust-roots.json`
- Modify: `src-tauri/tests/fixtures/plugin-trust/README.md`
- Create: `src-tauri/tests/fixtures/plugin-trust/root-public.json`
- Create: `src-tauri/tests/fixtures/plugin-trust/catalog-seq-1.json`
- Create: `src-tauri/tests/fixtures/plugin-trust/catalog-seq-2-rotated.json`
- Create: `docs/plugins/security.md`
- Modify: `crates/jarvis-plugin-protocol/src/lib.rs`
- Modify: `src-tauri/src/plugins/mod.rs`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/Cargo.lock`
- Modify: `src-tauri/tauri.conf.json`

- [ ] **Step 1: Add RED catalog trust tests**

In `src-tauri/src/plugins/trust/catalog.rs`, use deterministic fixture public/private keys that are documented as
test-only:

```rust
#[test]
fn accepts_fresh_monotonic_catalog_and_binds_release_digest() {
    let mut state = CatalogState::empty();
    let verified = verify_fixture("catalog-seq-1.json", at("2026-08-01T00:00:00Z"), &mut state).unwrap();
    let release = verified.release("dev.example.echo", "1.0.0", Target::darwin_arm64()).unwrap();
    assert_eq!(
        release.archive_digest.as_str(),
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    assert_eq!(state.sequence, 1);
}

#[test]
fn rejects_expired_replayed_conflicting_and_revoked_catalogs() {
    assert_catalog_error("expired.json", "catalog_expired");
    assert_catalog_error("replayed-sequence.json", "catalog_replayed");
    assert_catalog_error("same-sequence-other-digest.json", "catalog_conflict");
    assert_catalog_error("revoked-release.json", "package_revoked");
}
```

Add tests for unknown root, insufficient threshold, old-only key rotation, new-only key rotation, valid old+new threshold
rotation, publisher key not bound to plugin ID, and every release/package equality field. The latter is a table test
which changes exactly one of plugin ID, publisher, version, target, minimum macOS, Jarvis range, plugin API, archive
digest, signature algorithm, signature key ID, signature value or publisher lineage and expects
`package_catalog_mismatch`. Add separate RED tests proving that a correct catalog with a bad Ed25519 package signature
returns `package_signature_invalid`, a revoked package returns `package_revoked`, and neither case can produce
`VerifiedPackageEvidence` or create a quarantine directory.

- [ ] **Step 2: Run trust tests and verify RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features plugins::trust
```

Expected: FAIL because trust modules and fixtures do not exist.

- [ ] **Step 3: Define the catalog envelope and trust state**

`catalog.rs` in the protocol crate defines:

```rust
pub struct SignedCatalog {
    pub schema_version: u32,
    pub sequence: u64,
    pub issued_at: String,
    pub expires_at: String,
    pub previous_digest: Option<Digest>,
    pub payload: CatalogPayload,
    pub signatures: Vec<CatalogSignatureV1>,
}

pub struct CatalogSignatureV1 {
    pub algorithm: SignatureAlgorithm, // only "ed25519"
    pub key_id: String,
    pub value: String,
}

pub struct CatalogRelease {
    pub plugin_id: PluginId,
    pub publisher: PublisherId,
    pub version: Version,
    pub publisher_key_id: String,
    pub publisher_lineage: String,
    pub jarvis_range: VersionRange,
    pub plugin_api: u32,
    pub target: PackageTarget,
    pub minimum_macos: MacOsVersion,
    pub url: String,
    pub archive_digest: Digest,
    pub package_signature: PackageSignatureV1,
    pub revoked: bool,
}
```

The payload also carries publisher key lineages, root rotation proposal, revoked package digests and revoked publisher
keys. `PackageSignatureV1` is the exact A3 DTO; the catalog embeds the same algorithm, key ID and canonical base64
value, not a second lossy representation. All catalog structs are camelCase and deny unknown fields.
`CatalogSignatureV1` uses the same algorithm, key-ID and canonical 64-byte Ed25519 base64 constraints as
`PackageSignatureV1`, but signs the catalog domain. Signatures cover:

```text
ASCII "jarvis-plugin-catalog-v1" || NUL || JCS of the catalog object with the signatures field omitted
jarvis-plugin-package-v1\0<JCS package.json bytes>
```

The root catalog schema and protocol-crate copy are byte-identical, closed Draft 2020-12 schemas. Add
`catalog_schema_copies_are_byte_identical` and a schema/DTO round-trip fixture containing every release equality field.

- [ ] **Step 4: Implement fail-closed verification**

`trust/catalog.rs` verifies schema, RFC3339 dates, issued/expires interval, current time, sequence, previous digest,
threshold root signatures, rotation overlap, publisher lineage, release compatibility and revocation before exposing a
release. Persist `sequence + digest + accepted root set` only after all checks pass. Same sequence/same digest is
idempotent; lower sequence or same sequence/different digest is rejected.

`trust/signature.rs` accepts only Ed25519, validates canonical base64 and key length, uses constant-time library
verification and returns stable public errors without key material.

Move A3's exact
`ed25519-dalek = { version = "=2.1.1", default-features = false, features = ["fast", "std", "zeroize"] }`
from host dev-dependencies to host dependencies; keep the protocol, SDK and test-host free of crypto and keep the
Rust 1.77.2 gates.

`trust/package.rs` defines `CatalogPackageVerifier`, the only production implementation of A3's
`PackageTrustVerifier`. It receives a previously selected verified catalog release and an A3
`UntrustedPackageObservation`. It first proves catalog/root/publisher freshness, lineage and revocation, then requires
exact equality for plugin ID, publisher, version, target, minimum macOS, Jarvis range, plugin API, physical archive
digest, the complete package-signature object and its publisher-key lineage. Finally it verifies Ed25519 over the exact
A3 package domain plus canonical `package.json`. Only after all checks return `Ok(())` may the A3 package module mint
its opaque same-fd `VerifiedPackageEvidence`; A4 never constructs that type directly and no API accepts a boolean
`verified` flag.

Add the integration test file `src-tauri/src/plugins/trust/package.rs` tests:

```text
catalog_package_verifier_accepts_exact_observation_and_signature
catalog_package_verifier_rejects_each_release_field_mismatch
catalog_package_verifier_rejects_bad_signature_before_extraction
catalog_package_verifier_rejects_revocation_before_extraction
verified_evidence_keeps_the_pass_one_archive_fd
```

Run:

```bash
cargo +1.77.2 test --locked --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::trust::package::tests
```

Expected: all five tests exit `0`; mismatch/revocation/signature failures leave no filesystem output.

- [ ] **Step 5: Add production root resource safely**

`src-tauri/resources/plugin-trust-roots.json` contains the public owner root IDs, public Ed25519 keys, threshold and
validity metadata only. No private/test signing key may appear outside `src-tauri/tests/fixtures/plugin-trust`.
`src-tauri/tests/fixtures/plugin-trust/README.md` states that fixture private keys are public test material and forbidden
for release catalogs. Bundle the public root resource through `tauri.conf.json`.

If the release owner key is not provisioned during this task, commit an empty production root set with threshold `1`.
That state deliberately makes catalog install/update return `catalog_trust_not_provisioned`; local signed fixtures and
Developer Mode remain testable. Never promote the deterministic fixture key to production trust.

- [ ] **Step 6: Document the native trust boundary**

`docs/plugins/security.md` explains:

- sandboxed UI versus trusted native code;
- exact-digest consent and why grants are not an OS sandbox for native code;
- catalog/root/publisher rotation and revocation;
- immutable package directories;
- Developer Mode warning and repeated native consent;
- no execution before verification/consent.

- [ ] **Step 7: Run trust, public-boundary and secret scans**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features plugins::trust
npm run check:plugin-boundaries
npm run check:public
```

Expected: all commands exit `0`; expired/replayed/conflicting fixtures are rejected.

- [ ] **Step 8: Commit**

```bash
git add schemas/plugin-catalog-v1.schema.json crates/jarvis-plugin-protocol \
  src-tauri/src/plugins/trust src-tauri/src/plugins/mod.rs src-tauri/Cargo.toml \
  src-tauri/Cargo.lock \
  src-tauri/resources/plugin-trust-roots.json src-tauri/tests/fixtures/plugin-trust \
  src-tauri/tauri.conf.json docs/plugins/security.md
git commit -m "feat(plugins): verify signed plugin catalogs"
```

---

### Task A5: Persist immutable versions, exact receipts and durable operations

**Files:**

- Create: `crates/jarvis-plugin-protocol/src/receipt.rs`
- Create: `src-tauri/src/plugins/package_manager/mod.rs`
- Create: `src-tauri/src/plugins/package_manager/paths.rs`
- Create: `src-tauri/src/plugins/package_manager/receipt.rs`
- Create: `src-tauri/src/plugins/package_manager/operation.rs`
- Create: `src-tauri/src/plugins/package_manager/lock.rs`
- Create: `src-tauri/src/plugins/package_manager/schema.sql`
- Modify: `crates/jarvis-plugin-protocol/src/lib.rs`
- Modify: `src-tauri/src/plugins/mod.rs`
- Modify: `src-tauri/Cargo.toml`

- [ ] **Step 1: Add RED private-path and receipt tests**

In `paths.rs`:

```rust
#[test]
fn profile_layout_matches_the_v2_contract() {
    let paths = PluginPaths::new(PathBuf::from("/profile"));
    assert_eq!(paths.versions("dev.example.echo"), Path::new("/profile/plugins/dev.example.echo/versions"));
    assert_eq!(paths.current("dev.example.echo"), Path::new("/profile/plugins/dev.example.echo/current"));
    assert_eq!(paths.data("dev.example.echo"), Path::new("/profile/plugin-data/dev.example.echo"));
    assert_eq!(paths.cache("dev.example.echo"), Path::new("/profile/plugin-cache/dev.example.echo"));
    assert_eq!(paths.runtime("dev.example.echo"), Path::new("/profile/plugin-runtime/dev.example.echo"));
}

#[test]
fn refuses_symlinked_profile_components() {
    let root = temp_root("symlink");
    symlink(root.join("outside"), root.join("profile/plugins")).unwrap();
    assert_eq!(
        PluginPaths::new(root.join("profile")).prepare().unwrap_err().code(),
        "plugin_path_symlink"
    );
}
```

In `receipt.rs`:

```rust
#[test]
fn current_receipt_round_trips_with_previous_generation() {
    let store = fixture_store();
    let first = receipt("dev.example.echo", "1.0.0", 1, None);
    store.commit(&first).unwrap();
    let second = receipt("dev.example.echo", "1.1.0", 2, Some(first.summary()));
    store.commit(&second).unwrap();
    assert_eq!(store.current("dev.example.echo").unwrap().unwrap(), second);
}
```

- [ ] **Step 2: Run path/receipt tests and verify RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package_manager::paths::tests
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package_manager::receipt::tests
```

Expected: FAIL because `package_manager` does not exist.

- [ ] **Step 3: Define the receipt contract**

`crates/jarvis-plugin-protocol/src/receipt.rs` defines:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallReceipt {
    pub schema_version: u32,
    pub plugin_id: PluginId,
    pub version: Version,
    pub package_digest: Digest,
    pub publisher_key_id: String,
    pub publisher_lineage: String,
    pub target: PackageTarget,
    pub source: InstallSource,
    pub enabled: bool,
    pub granted_permissions: Vec<GrantedPermission>,
    pub native_trust_digest: Option<Digest>,
    pub installed_at_ms: i64,
    pub generation: u64,
    pub state_schema_version: u32,
    pub rollback_compatible_through: u32,
    pub previous: Option<ReceiptSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallSource {
    Catalog,
    LocalPackage,
    DeveloperSnapshot,
    LegacyBundledV1,
}
```

`LegacyBundledV1` may only be constructed by the bridge for canonical ID `agent-vm`; it never implies owner signature
or native v2 trust.

- [ ] **Step 4: Implement owner-only paths and atomic receipt files**

`PluginPaths::prepare` creates each root as a real directory with mode `0700` and rejects symlinks/non-directories at
every existing component. Installed package directories are `versions/<version>/<archive-digest>/` with directory mode
`0555`, executable mode `0555` and all other files `0444`. Runtime/cache/data remain `0700`.

`ReceiptStore` writes canonical JSON to a same-directory `current.next-<uuid>` file with `0600`, fsyncs, renames it to
the regular file named `current`, then fsyncs the plugin directory. It reopens and verifies the receipt after rename.
Directory-sync failure is an uncertain commit: recovery re-reads `current` and reports whether the exact generation is
visible instead of blindly retrying a previous generation.

- [ ] **Step 5: Add RED operation-recovery tests**

In `operation.rs`:

```rust
#[test]
fn operation_transitions_are_durable_and_terminal_is_final() {
    let journal = fixture_journal();
    let id = journal.begin("install", "dev.example.echo").unwrap();
    journal.transition(&id, OperationState::Running, "verify", None).unwrap();
    journal.transition(&id, OperationState::Succeeded, "complete", None).unwrap();
    assert_eq!(
        journal.transition(&id, OperationState::Running, "retry", None)
            .unwrap_err().code(),
        "operation_terminal"
    );
}

#[test]
fn restart_lists_only_recoverable_non_terminal_operations() {
    let journal = fixture_journal();
    seed_operation(&journal, "op-running", OperationState::Running, "extract");
    seed_operation(&journal, "op-consent", OperationState::WaitingForConsent, "consent");
    seed_operation(&journal, "op-done", OperationState::Succeeded, "complete");
    assert_eq!(
        journal.recoverable().unwrap().iter().map(|op| op.id.as_str()).collect::<Vec<_>>(),
        ["op-consent", "op-running"]
    );
}
```

- [ ] **Step 6: Implement the SQLite/WAL operation journal and profile lock**

`schema.sql` creates:

```sql
CREATE TABLE IF NOT EXISTS operations (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  plugin_id TEXT NOT NULL,
  state TEXT NOT NULL,
  phase TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  error_code TEXT,
  error_message TEXT,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS operations_plugin_updated
  ON operations(plugin_id, updated_at_ms DESC);
CREATE TABLE IF NOT EXISTS catalog_state (
  singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
  sequence INTEGER NOT NULL,
  digest TEXT NOT NULL,
  roots_json TEXT NOT NULL,
  accepted_at_ms INTEGER NOT NULL
);
```

Open with WAL, `foreign_keys=ON`, `busy_timeout=5000`, owner-only DB files and explicit transactions. State transitions
use a legal transition table and reject terminal rewrites. `ManagerLock` uses `flock(LOCK_EX|LOCK_NB)` on
`~/.jarvis/plugins/.manager.lock`, records PID/process-start identity for diagnostics, waits at most five seconds and
never deletes another process's lock file.

- [ ] **Step 7: Add crash-point tests**

Inject failpoints after version-directory rename, after current-receipt rename and before terminal operation update.
On reopen:

- a version without `current` is unreferenced cache and safe to retain;
- a visible exact `current` generation is treated as committed even if the prior caller saw fsync failure;
- a running operation is reconciled to `succeeded` when its target receipt is exact, otherwise to `failed` with
  `install_interrupted`;
- no recovery path deletes plugin data.

- [ ] **Step 8: Run the durable-store gate**

Run:

```bash
cargo test --manifest-path crates/jarvis-plugin-protocol/Cargo.toml receipt
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features plugins::package_manager
```

Expected: all path, receipt, operation, lock and crash-recovery tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/jarvis-plugin-protocol src-tauri/src/plugins/package_manager \
  src-tauri/src/plugins/mod.rs src-tauri/Cargo.toml
git commit -m "feat(plugins): persist install receipts and operations"
```

---

### Task A6: Execute install, update, rollback, disable and uninstall transactions

**Files:**

- Create: `src-tauri/src/plugins/package_manager/manager.rs`
- Create: `src-tauri/src/plugins/package_manager/downloader.rs`
- Create: `src-tauri/src/plugins/package_manager/consent.rs`
- Create: `src-tauri/src/plugins/package_manager/migration.rs`
- Create: `src-tauri/src/plugins/package_manager/health.rs`
- Create: `src-tauri/src/plugins/package_manager/recovery.rs`
- Create: `src-tauri/src/plugins/package_manager/tests.rs`
- Modify: `src-tauri/src/plugins/package_manager/mod.rs`
- Modify: `src-tauri/Cargo.toml`

- [ ] **Step 1: Add RED two-phase install tests**

Create `src-tauri/src/plugins/package_manager/tests.rs` using a local signed catalog/package fixture and injected
`Downloader`, `Clock`, `HealthRunner` and failpoint filesystem:

```rust
#[test]
fn native_install_cannot_extract_or_execute_before_digest_consent() {
    let fixture = ManagerFixture::native_plugin();
    let prepared = fixture.manager.prepare_install(InstallSourceRef::Catalog {
        id: "dev.example.native".into(),
        version: Some("1.0.0".into()),
    }).unwrap();

    assert!(prepared.permission_diff.added.contains(&"process.native".into()));
    assert_eq!(prepared.state, OperationState::WaitingForConsent);
    assert!(!fixture.health.was_called());
    assert!(!fixture.paths.versions("dev.example.native").exists());

    let wrong = Approval::native(prepared.operation_id.clone(), Digest::fixture('b'));
    assert_eq!(
        fixture.manager.commit_install(wrong).unwrap_err().code(),
        "native_digest_consent_mismatch"
    );
    assert!(!fixture.health.was_called());
}

#[test]
fn approved_install_commits_exact_verified_receipt() {
    let fixture = ManagerFixture::native_plugin();
    let prepared = fixture.prepare();
    let receipt = fixture.manager.commit_install(
        Approval::all(prepared.operation_id, prepared.package_digest.clone())
    ).unwrap();
    assert_eq!(receipt.package_digest, prepared.package_digest);
    assert_eq!(receipt.enabled, false);
    assert!(fixture.health.was_called_with(&receipt.package_digest));
}
```

- [ ] **Step 2: Run integration tests and verify RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package_manager::tests -- --nocapture
```

Expected: FAIL because `PluginManager` is not implemented.

- [ ] **Step 3: Implement download and prepare phases**

`PluginManager` is generic over injected ports in tests and exposes one shared API:

```rust
pub trait PackageManagerApi {
    fn catalog(&self, query: CatalogQuery) -> Result<Vec<CatalogItem>, ManagerError>;
    fn info(&self, id: &PluginId) -> Result<PluginDetails, ManagerError>;
    fn prepare_install(&self, source: InstallSourceRef) -> Result<InstallPlan, ManagerError>;
    fn commit_install(&self, approval: Approval) -> Result<InstallReceipt, ManagerError>;
    fn update(&self, id: Option<&PluginId>) -> Result<Vec<Operation>, ManagerError>;
    fn rollback(&self, id: &PluginId, version: Option<&Version>) -> Result<InstallReceipt, ManagerError>;
    fn set_enabled(&self, id: &PluginId, enabled: bool) -> Result<Operation, ManagerError>;
    fn uninstall(&self, id: &PluginId) -> Result<Operation, ManagerError>;
    fn purge(&self, id: &PluginId, confirmation: &str) -> Result<Operation, ManagerError>;
    fn doctor(&self, id: Option<&PluginId>) -> Result<DoctorReport, ManagerError>;
}
```

`prepare_install` acquires the manager lock and records these phases: `catalog`, `download`, `archive-digest`,
`package-signature`, `inspect`, `manifest`, `compatibility`, `permission-diff`, `consent`. Download streams to a unique
quarantine file with archive-size and deadline limits. It never extracts or executes native code. The returned
`InstallPlan` persists expected catalog sequence, archive/package digests, target, permission diff and exact native
trust challenge.

- [ ] **Step 4: Implement consent-bound commit and safe extraction**

`commit_install` reloads the operation, revalidates current catalog freshness/revocation and matches the exact
operation/digest/grant approval. It then:

1. extracts to a fresh owner-only quarantine directory;
2. rehashes every file and validates concrete Manifest v2;
3. performs declarative migrations on a copied state directory;
4. executes a native health check only after exact-digest consent, with cleared environment, bounded args, timeout and
   no network token;
5. changes package files/directories to immutable modes;
6. atomically renames into `versions/<version>/<digest>/`;
7. writes the exact `current` receipt disabled by default;
8. marks the durable operation succeeded.

An existing same version with a different digest is `version_digest_conflict`, not an overwrite.

- [ ] **Step 5: Add migration refusal tests and host-interpreted subset**

`migration.rs` accepts versioned declarative files that contain only:

- JSON `set`, `rename`, `delete` over plugin-owned settings/state paths;
- SQLite `CREATE TABLE`, `CREATE INDEX`, `ALTER TABLE ... ADD COLUMN`, and parameterized `UPDATE`;
- no `ATTACH`, `DETACH`, `PRAGMA load_extension`, triggers, virtual tables, arbitrary functions, filesystem or network.

Tests assert a migration containing `ATTACH DATABASE`, extension loading, an absolute path or a version-graph gap fails
before current-receipt switch. Irreversible migration sets `rollback_available=false` in `InstallPlan` and requires a
separate approval bit.

- [ ] **Step 6: Add RED update/rollback/uninstall tests**

```rust
#[test]
fn update_failure_restores_compatible_previous_receipt() {
    let fixture = ManagerFixture::installed("1.0.0");
    fixture.health.fail_for_version("1.1.0");
    let err = fixture.manager.update(Some(&id("dev.example.echo"))).unwrap_err();
    assert_eq!(err.code(), "health_check_failed");
    assert_eq!(fixture.receipts.current_version("dev.example.echo"), "1.0.0");
}

#[test]
fn revoked_digest_is_never_a_rollback_target() {
    let fixture = ManagerFixture::with_history(["1.0.0", "1.1.0"]);
    fixture.catalog.revoke_version("1.0.0");
    assert_eq!(
        fixture.manager.rollback(&id("dev.example.echo"), Some(&version("1.0.0")))
            .unwrap_err().code(),
        "package_revoked"
    );
}

#[test]
fn uninstall_keeps_data_and_purge_requires_exact_id() {
    let fixture = ManagerFixture::installed("1.0.0");
    fixture.write_plugin_data("keep-me");
    fixture.manager.uninstall(&id("dev.example.echo")).unwrap();
    assert_eq!(fixture.read_plugin_data(), "keep-me");
    assert_eq!(
        fixture.manager.purge(&id("dev.example.echo"), "echo").unwrap_err().code(),
        "purge_confirmation"
    );
}
```

- [ ] **Step 7: Implement lifecycle transactions and recovery**

Update closes activation admission for the old generation, waits for the host's bounded drain callback, snapshots
rollback-compatible state, commits the new receipt, then asks the resolver to activate the new generation. Failure
restores the exact previous non-revoked receipt and compatible snapshot.

Disable/uninstall use a `RuntimeTeardown` port. A busy plugin returns durable `pending-disable`; current receipt remains
enabled until teardown confirms tokens, handles, sockets and processes are gone. Uninstall removes `current`, runtime
and cache only after teardown, retains version history for the rollback window and retains data. Purge refuses while
any current receipt, runtime process, socket or mount lease exists.

`recovery.rs` reconciles every non-terminal operation on manager open using receipt generation, immutable version
presence and saved transaction phase. It never guesses that a native health check succeeded.

- [ ] **Step 8: Run package-manager lifecycle tests**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package_manager::tests -- --nocapture
```

Expected: install/update/rollback/uninstall, revocation, migration and every injected crash point pass.

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/plugins/package_manager src-tauri/Cargo.toml
git commit -m "feat(plugins): transact plugin package lifecycle"
```

---

### Task A7: Add immutable Developer Mode plus shared manager IPC and CLI

**Files:**

- Create: `src-tauri/src/plugins/developer.rs`
- Create: `src-tauri/src/plugin_cli.rs`
- Create: `src-tauri/src/plugin_cli_tests.rs`
- Create: `docs/plugins/getting-started.md`
- Modify: `src-tauri/src/plugins/mod.rs`
- Modify: `src-tauri/src/plugins/package_manager/manager.rs`
- Modify: `src-tauri/src/settings.rs`
- Modify: `src-tauri/src/daemon.rs`
- Modify: `src-tauri/src/ipc.rs`
- Modify: `src-tauri/src/main.rs`
- Modify: `src-tauri/Cargo.toml`
- Modify: `ui/bridge.js`

- [ ] **Step 1: Add RED immutable-link tests**

In `developer.rs`:

```rust
#[test]
fn link_runs_from_digest_snapshot_not_mutable_source() {
    let fixture = DevFixture::enabled();
    fixture.write_source("ui/index.html", "version-one");
    let linked = fixture.link().unwrap();
    fixture.write_source("ui/index.html", "version-two");

    assert_eq!(fs::read_to_string(linked.snapshot.join("ui/index.html")).unwrap(), "version-one");
    assert_eq!(
        fixture.reload_without_approval().unwrap_err().code(),
        "developer_source_changed"
    );
}

#[test]
fn unverified_native_link_requires_new_consent_after_restart() {
    let fixture = DevFixture::enabled_native();
    let linked = fixture.link_with_consent().unwrap();
    assert!(linked.receipt.native_trust_digest.is_some());
    fixture.simulate_jarvis_restart();
    assert_eq!(fixture.resolve().unwrap_err().code(), "developer_native_reconsent");
}
```

Add tests that Developer Mode off rejects link, schema/quotas still apply, source symlinks are rejected, disabling the
mode revokes linked activation generations and linked persistent services are forbidden.

- [ ] **Step 2: Run Developer Mode tests and verify RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features plugins::developer
```

Expected: FAIL because the developer module does not exist.

- [ ] **Step 3: Implement digest-addressed snapshots**

Add `pluginDeveloperMode: false` to settings defaults. `DeveloperLinker::link` canonicalizes source path and records
device/inode, validates and packs it through the same A2/A3 pipeline, then extracts into
`~/.jarvis/plugin-cache/<id>/developer/<digest>/`. The receipt uses `DeveloperSnapshot` and contains the disclosed
source path only in a private diagnostic field, never as activation root.

`reload` re-reads source, creates a new digest snapshot and permission diff, requires a fresh native consent when
digest changes, then atomically switches receipt generation. Disabling Developer Mode closes admission, calls the
same teardown port used by uninstall, revokes grants/tokens and marks all developer receipts inactive while retaining
their disclosed data.

- [ ] **Step 4: Add RED CLI parser/dispatch tests**

Create `src-tauri/src/plugin_cli_tests.rs` and register it from `main.rs` as
`#[cfg(test)] mod plugin_cli_tests;`:

```rust
#[test]
fn parses_all_public_plugin_commands_without_starting_tauri() {
    for args in [
        vec!["jarvis", "plugin", "catalog", "agent"],
        vec!["jarvis", "plugin", "info", "dev.example.echo"],
        vec!["jarvis", "plugin", "install", "dev.example.echo@1.0.0"],
        vec!["jarvis", "plugin", "update", "dev.example.echo"],
        vec!["jarvis", "plugin", "rollback", "dev.example.echo", "--to", "1.0.0"],
        vec!["jarvis", "plugin", "enable", "dev.example.echo"],
        vec!["jarvis", "plugin", "disable", "dev.example.echo"],
        vec!["jarvis", "plugin", "uninstall", "dev.example.echo"],
        vec!["jarvis", "plugin", "purge", "dev.example.echo", "--confirm", "dev.example.echo"],
        vec!["jarvis", "plugin", "doctor", "dev.example.echo"],
        vec!["jarvis", "plugin", "validate", "./plugin"],
        vec!["jarvis", "plugin", "pack", "./plugin"],
        vec!["jarvis", "plugin", "link", "./plugin"],
        vec!["jarvis", "plugin", "unlink", "dev.example.echo"],
        vec!["jarvis", "plugin", "reload", "dev.example.echo"],
        vec!["jarvis", "plugin", "logs", "dev.example.echo"],
        vec!["jarvis", "plugin", "list", "--dev"],
        vec!["jarvis", "plugin", "developer-mode", "enable"],
    ] {
        assert!(PluginCli::try_parse_from(args).is_ok());
    }
}

#[test]
fn cli_and_ipc_dispatch_the_same_manager_request() {
    let api = RecordingManager::default();
    dispatch_cli(parse(["jarvis", "plugin", "disable", "dev.example.echo"]), &api).unwrap();
    dispatch_ipc(ManagerRequest::Disable { id: id("dev.example.echo") }, &api).unwrap();
    assert_eq!(api.requests(), [
        ManagerRequest::Disable { id: id("dev.example.echo") },
        ManagerRequest::Disable { id: id("dev.example.echo") },
    ]);
}
```

- [ ] **Step 5: Implement one typed management API**

Add `PluginManager` to `Daemon`. Define serializable `ManagerRequest`/`ManagerResponse` in the protocol crate and one
pure `dispatch_manager_request(&dyn PackageManagerApi, ManagerRequest)` function. Tauri IPC
`plugin_manager_request(request)` and CLI dispatch both call it; neither duplicates lifecycle logic.

The CLI is detected at the first line of `main()` before Tauri builder/profile GUI initialization:

```rust
fn main() {
    if let Some(exit_code) = plugin_cli::run_if_requested(std::env::args_os()) {
        std::process::exit(exit_code);
    }
    run_tauri();
}
```

Human output is concise; `--json` prints exactly `ManagerResponse`. Consent is explicit: prepare prints the exact
digest/permission diff, and commit requires the operation ID plus `--accept-permissions` and, for native code,
`--trust-native-digest <sha256:...>`. Non-interactive stdin without those flags returns exit code `2`.

`ui/bridge.js` exposes only `pluginManagerRequest(request)` for Increment B; it does not render a manager page here.

- [ ] **Step 6: Implement every requested command over the shared API**

Command behavior:

- `catalog/info/list/doctor/logs` are read-only;
- `validate` performs A2/A3 checks without install;
- `pack` writes `<id>_<version>_<target>.jarvis-plugin` and prints digest;
- `install ID[@VERSION]|FILE`, `update`, `rollback`, `enable`, `disable`, `uninstall` return durable operation IDs;
- `purge` requires byte-for-byte exact plugin ID after `--confirm`;
- `link/unlink/reload/list --dev` use `DeveloperLinker`;
- `developer-mode disable` completes teardown before flipping the setting;
- `logs` prints the manager/runtime log path and redacted tail, never token/env values.

- [ ] **Step 7: Add getting-started documentation**

`docs/plugins/getting-started.md` walks through a UI-only example under `plugins/dev.example.hello/`, validation, pack,
Developer Mode enable/link/reload/unlink, signed install preparation/consent and doctor output. It explicitly says that
placing a folder under `plugins/` does not install it.

- [ ] **Step 8: Run CLI/Developer Mode/API gates**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features plugins::developer
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features plugin_cli_tests
node --test ui/*.test.mjs
npm run check:plugin-boundaries
```

Expected: all commands exit `0`; CLI tests never create a Tauri window.

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/plugins/developer.rs src-tauri/src/plugin_cli.rs \
  src-tauri/src/plugin_cli_tests.rs src-tauri/src/plugins/mod.rs \
  src-tauri/src/plugins/package_manager/manager.rs src-tauri/src/settings.rs \
  src-tauri/src/daemon.rs src-tauri/src/ipc.rs src-tauri/src/main.rs \
  src-tauri/Cargo.toml ui/bridge.js docs/plugins/getting-started.md
git commit -m "feat(plugins): add manager CLI and developer snapshots"
```

---

### Task A8: Resolve only verified receipts while preserving the Agent VM legacy bridge

**Files:**

- Create: `src-tauri/src/plugins/resolver.rs`
- Create: `src-tauri/src/plugins/activation.rs`
- Create: `src-tauri/src/plugins/verified_spawn.rs`
- Create: `src-tauri/src/plugins/resolver_tests.rs`
- Modify: `src-tauri/src/plugins/mod.rs`
- Modify: `src-tauri/src/plugins/supervisor.rs`
- Modify: `src-tauri/src/plugins/install.rs`
- Modify: `src-tauri/src/plugins/manifest.rs`
- Modify: `src-tauri/src/daemon.rs`
- Modify: `src-tauri/src/main.rs`
- Modify: `package.json`

- [ ] **Step 1: Add the RED receipt/legacy compatibility matrix**

Create `src-tauri/src/plugins/resolver_tests.rs` and register it from `plugins/mod.rs` as
`#[cfg(test)] mod resolver_tests;`:

```rust
#[test]
fn fresh_profile_keeps_legacy_agent_vm_during_increment_a() {
    let fixture = ResolutionFixture::fresh_with_bundled_sidecar();
    fixture.reconcile_legacy().unwrap();
    let resolved = fixture.resolve("agent-vm").unwrap();
    assert_eq!(resolved.source, ActivationSource::LegacyBundledV1);
    assert!(resolved.status.migration_available);
}

#[test]
fn valid_receipt_wins_without_deleting_legacy_files() {
    let fixture = ResolutionFixture::legacy_and_valid_receipt();
    let resolved = fixture.resolve("agent-vm").unwrap();
    assert_eq!(resolved.source, ActivationSource::ReceiptV2);
    assert!(fixture.legacy_package_exists());
}

#[test]
fn invalid_receipt_never_silently_downgrades_to_legacy() {
    for state in [
        ReceiptFault::DigestMismatch,
        ReceiptFault::Revoked,
        ReceiptFault::Incompatible,
        ReceiptFault::MissingVersionDirectory,
    ] {
        let fixture = ResolutionFixture::legacy_and_faulty_receipt(state);
        let blocked = fixture.resolve("agent-vm").unwrap_err();
        assert_eq!(blocked.code(), "receipt_activation_blocked");
        assert_eq!(fixture.spawn_count(), 0);
    }
}

#[test]
fn arbitrary_v1_manifest_is_not_a_legacy_trust_escape() {
    let fixture = ResolutionFixture::v1_plugin("dev.example.evil");
    assert_eq!(fixture.resolve("dev.example.evil").unwrap_err().code(), "legacy_manifest_forbidden");
}
```

Also test valid receipt plus raw Developer source, Developer Mode off, receipt generation replay, catalog revocation
after install, activation rehash mismatch, explicit false legacy setting and two Jarvis profiles with independent
receipts.

- [ ] **Step 2: Run compatibility tests and verify RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::resolver_tests -- --nocapture
```

Expected: FAIL because receipt-backed resolution does not exist.

- [ ] **Step 3: Implement deterministic source precedence**

`resolver.rs` returns:

```rust
pub enum ActivationSource {
    ReceiptV2,
    DeveloperSnapshot,
    LegacyBundledV1,
}

pub struct ResolvedPlugin {
    pub manifest: ResolvedManifest,
    pub root: PathBuf,
    pub executable: Option<VerifiedExecutable>,
    pub source: ActivationSource,
    pub package_digest: Digest,
    pub generation: u64,
    pub grants: Vec<GrantedPermission>,
    pub status: CompatibilityStatus,
}
```

Resolution order is:

1. read `current` receipt if it exists;
2. verify receipt schema, target, generation, immutable root, every package file digest, catalog state/revocation and
   native exact-digest consent;
3. if any receipt check fails, return blocked and stop;
4. otherwise consider a Developer receipt only while Developer Mode is on;
5. only with no current receipt, canonical ID `agent-vm` and exact bundled-v1 layout may use the legacy bridge.

There is no directory scan whose first root wins for v2. `plugins/<id>` and `JARVIS_PLUGIN_DEV_DIR` are never passed
directly to the runtime. During the transition, `JARVIS_PLUGIN_DEV_DIR` may request an explicit developer snapshot
refresh only when `JARVIS_DEV=1` and Developer Mode is already enabled.

- [ ] **Step 4: Make activation revalidate trust and grants**

`activation.rs` performs resolver checks before every activation event and returns an `ActivationLease` containing the
receipt generation/digest. Token capabilities come only from receipt grants. A changed receipt, disable, Developer Mode
off or catalog revocation invalidates the generation, cancels in-flight admission and revokes tokens before another
spawn.

Keep existing heartbeat, redacted status, bounded event queue, handshake deadline and crash backoff. Extend the v2
handshake to require exact plugin ID, PID, process protocol, package digest and generation. The v1 handshake remains
available only inside `LegacyBundledV1`.

- [ ] **Step 5: Add RED native path-swap execution test**

In `verified_spawn.rs`:

```rust
#[cfg(target_os = "macos")]
#[test]
fn pathname_swap_after_verification_cannot_change_executed_bytes() {
    let fixture = ExecutableFixture::new("#!/bin/sh\necho verified\n");
    let executable = VerifiedExecutable::open_and_hash(
        fixture.path(),
        &Digest::sha256(fixture.original_bytes())
    ).unwrap();
    fixture.replace_path("#!/bin/sh\necho swapped\n");
    let output = executable.spawn_capture(&[], &BTreeMap::new()).unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "verified");
}
```

- [ ] **Step 6: Execute the verified open file, not a re-resolved pathname**

`VerifiedExecutable::open_and_hash` uses `open(O_RDONLY|O_CLOEXEC|O_NOFOLLOW)`, verifies regular file, owner, mode,
device/inode/size and SHA-256 from that descriptor, and retains the descriptor through spawn. On Darwin,
`posix_spawn_file_actions_adddup2` maps it to fixed child descriptor `63`, clears close-on-exec for that child mapping,
and `posix_spawn` executes `/dev/fd/63`; argv contains the logical plugin entry as `argv[0]`, while the mutable package
pathname is never resolved again. The child environment starts empty and receives only the existing allowlisted host
variables plus v2 identity/digest/generation. Args come from the validated manifest as separate argv values; no shell
is involved.

Add a macOS-only feasibility test that executes both a tiny Mach-O fixture and a shebang fixture through inherited
`/dev/fd/63`, then swaps their original path before spawn and proves the verified bytes still run. If the target macOS
cannot execute the inherited descriptor, activation fails closed with `verified_fd_exec_unsupported`; it must not
fall back to `Command::new(original_path)`.

Keep `SystemSpawner` for `LegacyBundledV1` until Increment E, but label it legacy in status/logs. Never pass a v2
receipt executable through the pathname-based spawner.

- [ ] **Step 7: Make the bundled installer an explicit bridge**

Rename internal `install_bundled` behavior to `reconcile_legacy_agent_vm` and return a typed outcome:

```rust
pub enum LegacyAgentVmOutcome {
    NotAvailable,
    PreservedExisting,
    StagedFresh,
    SkippedBecauseReceiptExists,
}
```

It checks for `current` first and never overwrites/deletes a receipt or immutable version. It stages only the exact
bundled `jarvis-agent-vm-plugin` plus existing `plugins/agent-vm/manifest.json`, using current private/atomic writes.
It does not create a fake owner-signed v2 receipt. Existing settings
`plugins.agent-vm.enabled=false` continue to disable legacy activation; v2 enablement comes from receipt.

Leave these in place:

- `plugins/agent-vm/manifest.json`;
- `src-tauri/tauri.conf.json` `externalBin`;
- `scripts/prepare-agent-vm-sidecar.sh`;
- `package.json` Agent VM build/prepare steps.

Add comments pointing to the Increment E importer gate rather than deleting or bypassing them.

- [ ] **Step 8: Prove receipt-backed host integration**

Adapt `PluginHost::discover` into resolver reconciliation without discarding its running-child safety. A receipt
generation change closes admission and drains the old process before replacing its slot. First-party ID no longer
implies default enablement for receipt-backed plugins; legacy Agent VM alone preserves the old setting default.

Tests must prove:

- v2 package is spawned from immutable version directory with exact generation;
- native token reflects receipt grants, not manifest-requested permissions;
- a post-install revocation prevents next activation;
- bad receipt does not expose legacy process/status as running;
- legacy Agent VM's existing UDS v1 fake-plugin smoke still passes unchanged;
- normal Jarvis close still performs existing PluginHost disposal and does not delete package/data.

- [ ] **Step 9: Run all Increment A compatibility gates**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::resolver_tests -- --nocapture
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features plugins::tests
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features plugins::supervisor::tests
cargo test --manifest-path plugins/agent-vm/Cargo.toml
npm run check:plugin-boundaries
```

Expected: all commands exit `0`; legacy Agent VM remains usable, and every invalid v2 receipt is visibly blocked.

- [ ] **Step 10: Commit**

```bash
git add src-tauri/src/plugins/resolver.rs src-tauri/src/plugins/activation.rs \
  src-tauri/src/plugins/verified_spawn.rs src-tauri/src/plugins/resolver_tests.rs \
  src-tauri/src/plugins/mod.rs src-tauri/src/plugins/supervisor.rs \
  src-tauri/src/plugins/install.rs src-tauri/src/plugins/manifest.rs \
  src-tauri/src/daemon.rs src-tauri/src/main.rs package.json
git commit -m "feat(plugins): activate verified receipt packages"
```

---

## Increment A acceptance and compatibility hand-off

- [ ] Public protocol/SDK/test-host crates compile without Jarvis Core dependencies.
- [ ] Manifest v2 rejects unknown security fields, invalid IDs/ranges, remote references and bounded-input violations.
- [ ] Repacking identical input produces byte-identical archives and the same SHA-256.
- [ ] Archive inspection rejects traversal, links, special files, normalized duplicates, case collisions and bombs.
- [ ] Catalog verification rejects expiry, replay, freeze/conflict, invalid rotation and revoked publisher/package.
- [ ] Native code and native health checks cannot execute before exact-digest consent.
- [ ] Receipts and operation transitions survive every injected crash point without deleting plugin data.
- [ ] Install/update/rollback never overwrite a same-version different digest or resurrect a revoked digest.
- [ ] Developer links execute immutable snapshots and are fully invalidated when Developer Mode turns off.
- [ ] CLI and Tauri IPC dispatch identical typed manager requests and return durable operations.
- [ ] Receipt-backed activation rehashes package files, binds grants/generation and executes the verified descriptor.
- [ ] A bad v2 receipt blocks; it never silently falls back to a working legacy Agent VM.
- [ ] Fresh and existing profiles keep current Agent VM behavior through `LegacyBundledV1`.
- [ ] No bundled Agent VM binary, v1 manifest, VM disk, config, setting or data directory is removed in Increment A.
- [ ] Increment E hand-off records the exact upstream `v0.2` tag/commit/two platform digests and multi-mount limitation.

## Final verification

Run in this order:

```bash
git diff --check origin/master...HEAD
cargo test --manifest-path crates/jarvis-plugin-protocol/Cargo.toml
cargo test --manifest-path crates/jarvis-plugin-sdk/Cargo.toml
cargo test --manifest-path crates/jarvis-plugin-test-host/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features plugins::
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features plugins::package_manager::tests
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features plugin_cli_tests
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features plugins::resolver_tests
cargo test --manifest-path plugins/agent-vm/Cargo.toml
node --test ui/*.test.mjs
npm run check:plugin-boundaries
npm run check:public
```

Expected: every command exits `0`. Then run the normal full project gate:

```bash
npm test
npm run test:ui
cargo clippy --manifest-path src-tauri/Cargo.toml \
  --all-targets --features wakeword-ort,whisper-native,stt-vad
```

The Increment A review package must include:

1. manifest/package/trust security review;
2. package-manager crash/recovery review;
3. SDK/API compatibility review;
4. a migration reviewer confirming that the Agent VM bridge is retained and no silent downgrade exists.

Do not declare the clean-Jarvis-without-Agent-VM invariant complete here. Its removal gate belongs to Increment E and
requires successful legacy data/provider import, a durable v2 receipt and rollback evidence.
