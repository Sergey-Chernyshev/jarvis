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
- Create: `crates/jarvis-plugin-protocol/src/package.rs`
- Create: `src-tauri/src/plugins/package/mod.rs`
- Create: `src-tauri/src/plugins/package/pack.rs`
- Create: `src-tauri/src/plugins/package/archive.rs`
- Create: `src-tauri/src/plugins/package/hash.rs`
- Create: `src-tauri/tests/fixtures/plugin-packages/pack-source/plugin.json`
- Create: `src-tauri/tests/fixtures/plugin-packages/pack-source/ui/index.html`
- Create: `src-tauri/tests/fixtures/plugin-packages/pack-source/schemas/message.schema.json`
- Modify: `crates/jarvis-plugin-protocol/src/lib.rs`
- Modify: `src-tauri/src/plugins/mod.rs`
- Modify: `src-tauri/Cargo.toml`

- [ ] **Step 1: Add RED deterministic pack tests**

In `src-tauri/src/plugins/package/pack.rs`, add tests against a `FixtureSigner` whose signature is
`sha256(domain || canonical_package_json)`:

```rust
#[test]
fn identical_input_produces_identical_archive_and_digest() {
    let source = fixture("pack-source");
    let first = pack_to_vec(&source, Target::darwin_arm64(), &FixtureSigner).unwrap();
    let second = pack_to_vec(&source, Target::darwin_arm64(), &FixtureSigner).unwrap();
    assert_eq!(first.bytes, second.bytes);
    assert_eq!(first.archive_digest, second.archive_digest);
}

#[test]
fn package_manifest_covers_every_payload_entry() {
    let packed = pack_to_vec(&fixture("pack-source"), Target::darwin_arm64(), &FixtureSigner).unwrap();
    let inspected = inspect_bytes(&packed.bytes, ArchiveLimits::test()).unwrap();
    assert_eq!(
        inspected.metadata.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        ["plugin.json", "schemas/message.schema.json", "ui/index.html"]
    );
}
```

- [ ] **Step 2: Run deterministic tests and verify RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::package::pack::tests::identical_input_produces_identical_archive_and_digest
```

Expected: FAIL because the package module does not exist.

- [ ] **Step 3: Define canonical package metadata**

`package.rs` defines `PackageMetadataV1`, `PackageFile`, `PackageTarget`, state schema and migration graph. File records
contain normalized UTF-8 path, regular-file kind, mode (`0444` or `0555`), size and `sha256:<hex>`. The file list excludes
only `package.json` and detached `SIGNATURE`.

Canonical bytes are JCS with domain:

```text
jarvis-plugin-package-v1\0<canonical package.json bytes>
```

The final archive SHA-256 is separately bound by the catalog release record.

- [ ] **Step 4: Implement deterministic uncompressed tar packing**

`pack.rs`:

1. validates the source Manifest v2 and resolves the concrete target;
2. walks only regular files below the canonical source root without following links;
3. rejects reserved archive names supplied by source;
4. sorts normalized NFC UTF-8 paths bytewise;
5. assigns mode `0555` only to declared native entries and `0444` to all other files;
6. sets uid/gid/mtime to zero and owner/group names to empty;
7. computes `package.json`, obtains detached signature from `PackageSigner`, then writes entries in the fixed order
   `plugin.json`, payload paths, `package.json`, `SIGNATURE`;
8. emits an uncompressed tar stream, so the final bytes have no compressor-version or dictionary variability.

The source `plugin.json` becomes a concrete packaged `plugin.json`; source files are never modified.

- [ ] **Step 5: Add RED malicious archive tests**

In `archive.rs`, construct raw fixture archives and assert these stable errors:

```rust
for (fixture, code) in [
    ("absolute-path", "archive_path"),
    ("dot-dot", "archive_path"),
    ("symlink", "archive_entry_type"),
    ("hardlink", "archive_entry_type"),
    ("fifo", "archive_entry_type"),
    ("duplicate-normalized-name", "archive_duplicate"),
    ("unicode-case-collision", "archive_case_collision"),
    ("oversized-entry", "archive_quota"),
    ("oversized-total", "archive_quota"),
] {
    assert_eq!(inspect_fixture(fixture).unwrap_err().code(), code);
}
```

- [ ] **Step 6: Implement bounded inspection and extraction**

`ArchiveLimits::production()` fixes:

```text
archive bytes: 2 GiB
unpacked total: 2 GiB
single file: 512 MiB
entry count: 20,000
path bytes: 1,024
```

Inspection streams tar without materializing unbounded input. It rejects absolute, `..`, `.`, empty, NUL,
backslash and non-NFC paths; links, devices, sockets and sparse entries; duplicate normalized paths and
case-insensitive collisions. Extraction uses an already-created owner-only quarantine directory, `openat` with
`O_NOFOLLOW|O_CREAT|O_EXCL`, verifies every written size/digest, fsyncs files and directories, and never delegates path
joining to the archive library.

- [ ] **Step 7: Run the package security gate**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features plugins::package
```

Expected: deterministic and malicious archive tests pass; extraction writes nothing outside the test quarantine.

- [ ] **Step 8: Commit**

```bash
git add schemas/plugin-package-v1.schema.json crates/jarvis-plugin-protocol \
  src-tauri/src/plugins/package src-tauri/src/plugins/mod.rs src-tauri/Cargo.toml \
  src-tauri/tests/fixtures/plugin-packages
git commit -m "feat(plugins): add deterministic package format"
```

---

### Task A4: Verify signed catalogs, publishers, rotations and revocations

**Files:**

- Create: `schemas/plugin-catalog-v1.schema.json`
- Create: `crates/jarvis-plugin-protocol/src/catalog.rs`
- Create: `src-tauri/src/plugins/trust/mod.rs`
- Create: `src-tauri/src/plugins/trust/signature.rs`
- Create: `src-tauri/src/plugins/trust/catalog.rs`
- Create: `src-tauri/resources/plugin-trust-roots.json`
- Create: `src-tauri/tests/fixtures/plugin-trust/README.md`
- Create: `src-tauri/tests/fixtures/plugin-trust/root-public.json`
- Create: `src-tauri/tests/fixtures/plugin-trust/catalog-seq-1.json`
- Create: `src-tauri/tests/fixtures/plugin-trust/catalog-seq-2-rotated.json`
- Create: `docs/plugins/security.md`
- Modify: `crates/jarvis-plugin-protocol/src/lib.rs`
- Modify: `src-tauri/src/plugins/mod.rs`
- Modify: `src-tauri/Cargo.toml`
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
rotation, publisher key not bound to plugin ID, release target mismatch and package signature mismatch.

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
    pub signatures: Vec<Signature>,
}

pub struct CatalogRelease {
    pub plugin_id: PluginId,
    pub version: Version,
    pub publisher_key_id: String,
    pub jarvis_range: VersionReq,
    pub plugin_api: u32,
    pub target: PackageTarget,
    pub url: String,
    pub archive_digest: Digest,
    pub package_signature: Signature,
    pub revoked: bool,
}
```

The payload also carries publisher key lineages, root rotation proposal, revoked package digests and revoked publisher
keys. Signatures cover:

```text
jarvis-plugin-catalog-v1\0<JCS bytes without signatures>
jarvis-plugin-package-v1\0<JCS package.json bytes>
```

- [ ] **Step 4: Implement fail-closed verification**

`trust/catalog.rs` verifies schema, RFC3339 dates, issued/expires interval, current time, sequence, previous digest,
threshold root signatures, rotation overlap, publisher lineage, release compatibility and revocation before exposing a
release. Persist `sequence + digest + accepted root set` only after all checks pass. Same sequence/same digest is
idempotent; lower sequence or same sequence/different digest is rejected.

`trust/signature.rs` accepts only Ed25519, validates canonical base64 and key length, uses constant-time library
verification and returns stable public errors without key material.

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
