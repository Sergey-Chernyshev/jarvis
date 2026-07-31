# Provider-Neutral Project Runtime Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Use
> `superpowers:test-driven-development` for every behavior change and
> `superpowers:verification-before-completion` before claiming a task complete.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace cwd/FNV-derived Projects and the UI's three-way
history/EntityStore/settings merge with one provider-neutral
Project → Runtime → Session → Turn model, one stable Project Catalog, and one
Broker-revisioned Projects snapshot consumed identically by trusted UI and the
future CLI.

**Architecture:** Core assigns opaque Project IDs and persists roots,
preferences, aliases and migration receipts in a private SQLite/WAL Project
Catalog. Catalog mutations use a transactional outbox to project Core-owned
Project entities into Increment B's Broker. Provider Runtime/Session/Turn
observations enter through the same Broker outbox ingress and are mediated into
safe Core Views; raw process, resume, attach and file provenance remains
host/provider-private. Trusted UI and the headless/CLI port read one immutable
Broker snapshot under Increment B's coordinator guard, including the exact
Catalog source/acknowledgement checkpoint and preferences; neither surface
joins raw cwd, legacy EntityStore, provider-private state or settings.
Provider control first persists an Increment B runtime Operation and only then
dispatches an exact typed command. Route resolution and page opening are
read-only and cannot provision a runtime, start a session, mint a file/attach
handle, attach a terminal or create an Operation.

**Tech Stack:** Rust 2021; Rust 1.77.2 only for public/pure Core crates whose
locked graph is proven by the dedicated MSRV job; the Tauri host uses the
current stable toolchain from CI unless its complete dependency graph is
separately pinned and tested. Tauri 2, Increment B's SQLite/WAL Data Broker, a
separate private SQLite/WAL Project Catalog, `serde`/`schemars` generated
Rust/JSON Schema/TypeScript contracts, Node's test runner, existing host DOM
helpers/design tokens and the approved official Figma workflow.

**Approved design:** `docs/superpowers/specs/2026-07-31-plugin-platform-agent-vm-v2-design.md`

**Master roadmap:** `docs/superpowers/plans/2026-07-31-plugin-platform-agent-vm-v2.md`

**Planning base:** integration commit
`5b9d2463c1e7acd8deef4ef8cbe0622d215eb8a9`. Implementation must start from a
clean integration branch containing completed A and B increments, not from this
planning snapshot.

---

## Increment ownership, dependency gates and claims

Increment B owns all generic platform mechanics:

- contract registry and schema validation;
- durable Broker entities/events/cursors/outbox receipts;
- Broker transaction and `brokerRevision` allocation;
- typed command registry, durable runtime Operation service, `OperationRef`,
  Gate v2, grants and audit;
- adapter-private provenance bindings that never enter Broker
  query/snapshot entities;
- opaque resource handles and typed plugin settings;
- isolated plugin pages, Bridge v1 and declarative contribution outlets;
- package/activation reconciliation and the coordinator snapshot.

Increment C consumes those public contracts. C owns:

- Core Project/CatalogPreferences/CatalogProjectionReceipt/Runtime/Session/Turn/ChangeSet Views,
  allowlisted provider observations and state validation;
- the stable Project Catalog, roots, preferences, aliases and projection outbox;
- compilation of `contributes.projectRuntimes` into a provider-neutral catalog;
- Core projections and the immutable Project Runtime snapshot;
- migration aliases and the read-only compatibility projection;
- generic Projects routes, list/detail UI and `project.*` outlets.

C must not add:

- a second Broker, event log, cursor, Operation table, grant system or
  revision allocator;
- Agent VM lifecycle logic, raw `plugins_cmd`, `agent_vm_terminal_*`, VM
  discovery or provider-private RunStore reads in generic modules;
- the launchd controller, guest supervisor, standalone CLI or multi-session
  Agent VM implementation owned by D;
- final Agent VM package/pages/actions migration owned by E;
- memory snapshots, mounts, credential leases, resource budgets or durable
  notification delivery/dedupe owned by F;
- an embedded terminal as the primary generic Project or Session UI.
- raw paths in root-registration payloads, Session/ChangeSet snapshots or
  contribution context.

The old design delivery labels are reconciled in design section 26. Canonical
ownership is now B = Plugin UI + Broker, C = Project Runtime Core,
D = Agent VM controller/CLI, E = Agent VM plugin migration,
F = memory/mounts/notifications and G = release.

### Hard dependency gate

Before editing C production code, run from the repository root:

```bash
test -f crates/jarvis-plugin-protocol/src/manifest.rs
test -f crates/jarvis-plugin-protocol/src/broker.rs
test -f crates/jarvis-plugin-protocol/src/contribution.rs
test -f src-tauri/src/plugin_platform/broker/outbox_ingress.rs
test -f src-tauri/src/plugin_platform/broker/projection_adapter.rs
test -f src-tauri/src/plugin_platform/security/command_registry.rs
test -f src-tauri/src/plugin_platform/operations/store.rs
test -f src-tauri/src/plugin_platform/operations/dispatch.rs
test -f src-tauri/src/plugin_platform/operations/watch.rs
test -f src-tauri/src/plugin_platform/operations/recovery.rs
test -f src-tauri/src/plugin_platform/coordinator/snapshot.rs
test -f src-tauri/src/plugin_platform/core_projection.rs
test -s docs/design/plugin-platform-v2-figma.md
cargo test --manifest-path crates/jarvis-plugin-protocol/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test broker_projection_consistency \
  --test broker_projection_adapter \
  --test broker_projection_adapter_privacy \
  --test runtime_operations \
  --test runtime_operation_recovery \
  --test runtime_operation_watch \
  --test runtime_operation_cancel \
  --test plugin_runtime_command_resolution \
  --test plugin_platform_snapshot \
  --test plugin_page_no_side_effects
bash scripts/check-plugin-platform-boundaries.sh
```

Expected: every command exits `0`. The B tests must prove:

1. Core and plugin readers can observe the same immutable `brokerRevision`;
2. outbox replay is idempotent;
3. typed commands bind exact contract/digest/provider generations;
4. runtime Operations are persisted before dispatch, recover/query by subject
   after restart, have gap/resync watches, authorized cancellation and immutable
   terminal states;
5. the coordinator cannot publish mixed package/grant/contribution revisions;
6. page/route opening dispatches zero provider commands.

If any file or behavior is absent, stop. Complete or correct B in its own
increment first. Do not recreate a local Broker shortcut inside C.

The B handoff must also expose a trusted-Core contract/projection writer and a
transactional projection-adapter hook for authenticated provider outbox
batches. The hook must record the provider outbox receipt and apply
Core-owned mutations in one Broker transaction without pretending that the
provider owns a Core contract. If B lands without this generic hook, add and
review it in B before C starts; a fake Core plugin receipt or delegated
owner-only write is not an acceptable C workaround.

The B handoff must also expose `RuntimeOperationService`. An
`Accepted(OperationRef)` without a durable row committed before provider
dispatch does not pass the gate. C does not emulate pending work in UI memory
or reuse the package-manager-specific operation journal.

The Figma gate from B must contain real node IDs and dated approval for the
generic Projects and Agent VM references. C has an additional UI-detail
checkpoint before its first UI task.

---

## Audited baseline and concrete defects

This plan is based on the current implementation at the planning base, not on
an assumed future rewrite.

| Current source | Audited behavior | Required C disposition |
|---|---|---|
| `src-tauri/src/entities.rs` | Process-local `HashMap` EntityStore with owner strings, no durability, CAS, schema contract or shared snapshot revision | Treat as legacy import input only; all new Core reads use B Broker |
| `src-tauri/src/capability/native/entities_cap.rs` | Every legacy upsert directly calls `agent_vm::route_transition` and emits a full store snapshot | Add no such coupling to Broker/Core; fence the old notification bridge until E/F |
| `src-tauri/src/agent_vm.rs` and `plugins/agent-vm/src/project.rs` | Duplicate FNV-1a identity over canonical cwd; unavailable, moved or renamed roots cannot preserve identity | Freeze the algorithm only in migration fixtures; assign one opaque Catalog ID and store aliases |
| `src-tauri/src/agent_vm.rs` | `projectManager.folders`, favorites, view and `agentVm.projects` are owned by the Agent VM module | Import idempotently into Core Catalog/preferences; keep rollback receipts |
| `src-tauri/src/history.rs` | Chat history groups by cwd or basename and returns untyped JSON independent of VM state | Expose a typed read-only import source; never join it in UI |
| `plugins/agent-vm/src/service.rs` | Runtime API is keyed by raw cwd and returns provider-specific snapshots | E will publish canonical provider outbox rows; C accepts only Core envelopes |
| `plugins/agent-vm/src/run_supervisor.rs` | One active run per project, one replaceable queued Turn, JSONL journal and `runId` acting as Session | Map `runId` to a Session compatibility alias and shadow projection; do not claim managed adoption |
| `plugins/agent-vm/src/plugin.rs` | Free-string runtime commands and transient legacy `operation` entities | Generic UI invokes B exact typed command receipts and watches durable `OperationRef` |
| `src-tauri/src/agent_vm_terminal.rs` | Host tmux terminal is keyed by legacy projectId/backend, resolves a legacy `vm` entity and directly controls Lima/tmux | Generic Project UI never imports it; typed attach/multi-session ownership moves through D/E |
| `src-tauri/src/launch.rs` | `agent_command` interpolates unchecked agent/session strings into shell text; unknown agent silently becomes Claude; `session_launch` accepts caller cwd | Harden behind an exact legacy provider with typed argv/ID validation; generic Project Runtime must never call `crate::launch` |
| `src-tauri/src/project_folder_picker.rs` and `project_manager_folder_pick` | Picker returns a raw path which is later canonicalized/mutated without an fd-bound one-time selection capability | Core owns selection, binds an opened directory identity to a one-time handle and rejects raw path registration/symlink swaps |
| `ui/agent-vm.js` | Manually merges history, legacy entities and project settings by cwd/projectId; derives state in JavaScript | Replace generic Projects data path with one canonical snapshot/store |
| `ui/renderer.js` | Project cards always open Agent VM; opening a running VM calls terminal warm-up and `runtime.ensure`; a stopped VM calls provider `runtime.status` | Project route is read-only; runtime/session start requires an explicit confirmed action |
| `ui/renderer.js` | Embedded Agent VM terminal polls every 350 ms and is the primary project workspace | Generic Session detail is chat/results first; copyable attach/resume is secondary |
| `ui/bridge.js` and `src-tauri/src/ipc.rs` | Generic Projects path exposes `entities_get`, `history_get`, project-manager cwd calls, `session_launch`, raw Agent VM focus/terminal/files APIs | Add one typed Project Runtime query/action boundary; retain old calls only behind a measured rollback adapter |
| `src-tauri/src/ipc.rs::toast_click` | Agent VM targets contain raw cwd, FNV projectId and optional runId | Resolve trusted legacy targets through alias tables into canonical routes |
| `src-tauri/src/agent_vm.rs::Coordinator` and `notification_for` | Focus is process-local and completion/waiting notifications are derived directly from legacy entity transitions with no durable receipt | C publishes canonical state only; F owns durable notification dedupe/supersession and focus-safe delivery |
| all current UI sources | History/settings/entities/terminal polls have unrelated update times and no common revision | Apply one coordinator/Broker snapshot and monotonic watch stream; gap means full resync |

There is no durable alias table today. Existing fallback comparisons such as
`item.projectId === target.projectId || item.cwd === target.cwd` are not
migration. C must make aliases explicit, conflict-detecting, observable and
reversible.

---

## Canonical domain and state invariants

### Stable identity

Core IDs are opaque validated strings:

```text
prj_<random 128-bit base32>
rt_<random 128-bit base32>
ses_<random 128-bit base32>
turn_<random 128-bit base32>
```

Reuse B's locked, current-host-tested OS-CSPRNG dependency and proven entropy/error
handling. If B kept its volatile handle generator private, implement the small
persistent-ID encoder in C's `catalog/identity.rs`; do not expose or reuse
handle tokens. IDs are generated once with a bounded collision retry, are
never derived from display name/path/provider/backend and are never reused
after deletion. Public Rust newtypes validate prefix, encoded length and
alphabet. Payload callers cannot choose a Core ID except when replaying an
already committed idempotent receipt.

A Project owns one or more root records. A root records the lexical path,
currently canonical path when available, private path digest and best-effort
filesystem identity (`st_dev`/`st_ino` on macOS) as evidence. File identity
helps verify an explicit rebind after a rename; Jarvis does not scan the disk
for moved projects. A missing root changes health, not Project ID. Basename is
display metadata only.

Interactive registration never accepts that path in an IPC/Bridge payload.
The host picker opens the selected directory without following a final
symlink, binds its file descriptor and `fstat` identity to a short-lived,
single-use `DirectorySelectionHandle`, then consumes the handle in the Catalog
transaction after revalidation. Cancel, expiry, replay or identity/path swap
creates no Catalog revision or outbox row.

### Core contracts

Core registers these exact contracts through B's trusted-Core registration
path:

```text
dev.jarvis.core/project@1.0.0
dev.jarvis.core/catalog-preferences@1.0.0
dev.jarvis.core/catalog-projection-receipt@1.0.0
dev.jarvis.core/project-runtime-provider@1.0.0
dev.jarvis.core/runtime@1.0.0
dev.jarvis.core/session@1.0.0
dev.jarvis.core/turn@1.0.0
dev.jarvis.core/change-set@1.0.0
```

Project path fields are classified private. Trusted Core may read them;
plugins receive `projectId` and B opaque resource handles only when granted.
Runtime/Session/Turn provider fields are allowed only inside:

```json
{
  "extension": {
    "contract": {
      "id": "dev.example.provider/session",
      "version": "1.0.0",
      "schemaDigest": "sha256:..."
    },
    "data": {}
  }
}
```

The extension contract must be registered by the same exact active provider
receipt. Generic UI never branches on extension data.

### Project and provider

The Core Project envelope contains:

```text
projectId, displayName, projectKind, state, primaryRootId,
rootHealth, rootDisplayPath, revision, catalogSourceRevision,
acknowledgedBrokerRevision, createdAt, updatedAt, lastActivityAt
```

`rootDisplayPath` is a private field available to trusted Core readers only.
Plugins receive no raw root through Broker projection or contribution context.
`state = active | unavailable | archived | quarantined`.

Favorites and list/cards mode are not split back into local UI/settings state.
The Broker-projected Core view is:

```text
CatalogPreferencesView {
  preferencesId, viewMode, favoriteProjectIds, revision,
  catalogSourceRevision, acknowledgedBrokerRevision, updatedAt
}
```

`favoriteProjectIds` is a bounded ordered list of canonical IDs. Every Project
and preferences projection produced by one Catalog transaction carries the
same `catalogSourceRevision`; B applies that change set atomically and returns
its exact `acknowledgedBrokerRevision`. The Catalog persists that pair before
reporting the mutation complete.

The Catalog outbox cannot predict B's next revision. Its private projection
input contains `catalogSourceRevision` and Project/preferences source fields,
but no acknowledgement. Inside one trusted-Core Broker transaction B allocates
the revision and injects that exact value into
`ProjectView.acknowledgedBrokerRevision` and
`CatalogPreferencesView.acknowledgedBrokerRevision`. Provider/caller payloads
cannot set either field.

The same Broker transaction writes an immutable, host-only
`CatalogProjectionReceipt { catalogSourceRevision,
acknowledgedBrokerRevision, payloadDigest, appliedAt }`. Exact source revision
and Broker revision are each unique for a payload digest. It is readable only
by trusted Core, never projected to plugins. Snapshot validation reads these
receipts from the same Broker read transaction, so it never joins the private
Catalog database. The public `CatalogCheckpoint` is the latest applicable
receipt; older Project rows are validated against their own immutable Broker
receipt without serializing the whole ledger.

The Core provider envelope contains:

```text
providerId, displayName, supportedProjectKinds, supportedBackends,
requiredLifecycleRoles, optionalLifecycleRoles, pageRoute,
availability, packageDigest, activationGeneration, revision, reason
```

The receipt/digest/generation values are host-computed. A payload cannot claim
provider availability.

### Runtime

Required Core fields are:

```text
runtimeId, providerId, providerInstance, projectId,
desiredState, observedState, generation, revision, reason,
resourceSummary, lastActivityAt, extension
```

`desiredState = stopped | running | destroyed`.

`observedState = missing | provisioning | stopped | starting | running |
stopping | error | unmanaged | quarantined`.

`(projectId, providerId, providerInstance)` is unique. A lifecycle intent
increments `generation`. Provider observations with an older generation,
receipt, activation generation or incompatible host-boot evidence are rejected
or quarantined; they never regress the visible current runtime.

Within one generation the normal graph is:

```text
missing -> provisioning -> stopped -> starting -> running -> stopping -> stopped
```

Provision/start/stop edges may end in `error`. Discovery may produce
`unmanaged`. Contract/provenance/state conflicts produce `quarantined`.
Recovery from `quarantined`, adoption from `unmanaged`, restart after a terminal
failure and reprovision after `destroyed` require an explicit typed Operation
and a new generation/verified receipt. A destroyed runtime remains a tombstone
long enough to resolve old aliases and cannot silently return to running.

### Session

`SessionView` is the only Session shape allowed in Broker snapshots, trusted
UI, headless output or Bridge field projections:

```text
sessionId, runtimeId, backend, mode, state, desiredState, revision,
currentTurnId, resumability, displayTitle, changeSetId,
createdAt, lastActivityAt, extension
```

`state = creating | ready | working | waiting | draining | stopped | failed |
interrupted | quarantined`.

`desiredState = running | stopped`.
`mode = interactive | batch | legacy_shadow`.
`resumability = none | attach | resume | legacy_attach_only`.
`displayTitle` is a bounded provider-neutral label; it is not raw transcript
content and may be absent.

`SessionView` never contains `transportId`, `backendSessionId`, guest/host boot
IDs, tmux target, PID/process-start identity, provenance receipt, attach
command/environment or raw provider session key. The schema uses
`deny_unknown_fields`, and golden tests prove those names are rejected from
provider observations and absent from serialized snapshots/Broker rows.

One Session identifies one agent process ownership domain and may contain many
Turns, but Core exposes only the safe state/capability View.
`stopped | failed | interrupted -> creating` is allowed only after an explicit
resume Operation and a new private provenance receipt; it is not a poll-induced
transition. A private guest/process epoch change invalidates old evidence. An
already terminal legacy process can be represented as a read-only shadow
Session but is not adopted as supervisor-managed.

### Host-private provenance and provider observations

Full backend session ID, transport/tmux/PID identity, boot IDs, attach
environment and resume material remain in the controller/provider's private
domain store. C stores only a non-exportable adapter-private binding:

```text
SessionProvenanceBinding {
  providerReceiptId, sourceInstanceId, providerSessionKeyDigest,
  guestEpochDigest, processEpochDigest, provenanceReceiptDigest,
  observedProviderRevision
}
```

Runtime host-boot and lifecycle-lease evidence follows the same rule in a
private `RuntimeProvenanceBinding`; neither value occurs in `RuntimeView`.

B's projection-adapter private-state API persists that binding in the same
transaction as the outbox receipt/Core projection. It has no Broker query,
watch, Bridge, UI or CLI serialization path. Raw provenance is not copied into
that state.

The provider may submit only:

```text
ProviderSessionObservation {
  providerSessionKey, providerRevision, observedState,
  backend, mode, currentTurnProviderKey, resumabilityCapabilities,
  displayTitle, changeSetProviderKey, lastActivityAt,
  provenanceReceiptDigest, guestEpochDigest, processEpochDigest, extension
}
```

The authenticated outbox channel and existing operation/runtime binding supply
provider identity, package/activation generation, canonical Project/Runtime,
desired state, canonical Session/Turn IDs, host revision/timestamps and grant.
Those host-owned fields are absent from the observation DTO; attempts to send
`sessionId`, `runtimeId`, `providerId`, `desiredState`, `revision`,
`currentTurnId`, `createdAt` or any raw process/attach field fail schema
validation before projection.

### Turn

Required Core fields are:

```text
turnId, sessionId, operationId, state, seq, idempotencyKey,
inputRef, attachmentRefs, memorySnapshotId, errorCode,
startedAt, completedAt, resultSummary
```

`state = queued | admitted | starting | working | waiting | completed | failed |
cancelled | interrupted | timed_out`.

Turn `seq` is strictly increasing within a Session. The v1 queue policy is one
active Turn and at most eight queued Turns per Session. Queue admission is a
provider/controller responsibility in D, but C validates the published
projection. Terminal Turn states are immutable. Retry creates a new Turn with
a new `turnId` and links the prior `operationId`; it never rewrites a failed
Turn.

### Change sets and changed files

Session/Turn snapshots reference a provider-neutral Core `ChangeSetView`:

```text
ChangeSetView {
  changeSetId, projectId, sessionId, turnId, revision,
  changedFiles, additionsTotal, deletionsTotal, updatedAt
}

ChangedFileView {
  changedFileId, displayName, changeKind, binary,
  additions, deletions, availability
}
```

`changedFileId` is opaque and `displayName` is a bounded basename/display
label, not an absolute/relative path. Neither View contains a root, directory,
provider file key, raw path, bytes, diff, durable resource handle or
provider-private identity. Provider observations carry a bounded
provider-local file key plus safe display metadata; the trusted adapter maps
it to a canonical opaque ID and keeps the provider-key digest private.

The strict provider input is:

```text
ProviderChangeSetObservation {
  providerChangeSetKey, providerRevision,
  providerSessionKey, providerTurnKey,
  changedFiles: [{
    providerFileKey, displayName, changeKind, binary,
    additions, deletions, availability
  }],
  additionsTotal, deletionsTotal, updatedAt
}
```

It rejects canonical ChangeSet/Project/Session/Turn/file IDs, path/root,
content/diff/handle fields and host revisions. The authenticated receipt and
private subject mappings supply all canonical ownership.

File content/diff/open/reveal is click-only. At invocation time Core
reauthorizes the current principal, subject, provider generation, snapshot
revision and exact method, then asks B7 to mint a volatile, single-purpose
`ResourceHandle` bound to invocation, grant revision, byte/read quota and
expiry. Handles never enter ChangeSet/Session/Turn entities, events, cursor
rows, durable Operations, audit payloads or logs. A grant revoke, route/page
close, provider update, terminal Operation, expiry or first-use exhaustion
invalidates them.

### Provider-neutral command set

Every normal `contributes.projectRuntimes` registration resolves these exact
typed command roles:

```text
runtime.provision
runtime.start
runtime.stop
runtime.destroy
session.create
session.stop
```

Optional roles are:

```text
runtime.doctor
session.attach
session.cancel-turn
```

Local manifest IDs are compiled by B10 to exact contract, schema digest,
package digest and activation-generation receipts before C sees them. A
missing/ambiguous/stale role disables the provider/action and yields a
repairable reason; C never dispatches a textual fallback. `session.attach`
is click-only and returns a B7 volatile handle from which trusted Core can
resolve one bounded copyable command/result. No attach descriptor or evidence
is part of a Session snapshot, and generic UI never auto-executes it.

### One snapshot revision

Catalog tables remain the canonical source for Project identity, but no reader
surface joins Catalog rows with Broker rows. Catalog commits append an outbox
change set in the same transaction. That private change set includes affected
Project projection inputs and the complete preferences projection input, all
tagged with one `catalogSourceRevision` and no predicted acknowledgement. The
trusted projector idempotently materializes the final `ProjectView`s and
`CatalogPreferencesView` at one B revision, injects that revision as their
`acknowledgedBrokerRevision`, and persists the exact
`catalogSourceRevision -> acknowledgedBrokerRevision` pair. A Catalog mutation
is not reported as complete until that acknowledgement is durable or
represented by a durable pending Operation.

`ProjectRuntimeSnapshotService` then:

1. enters B12's coordinator read guard;
2. opens one Broker read transaction and reads its committed
   `brokerRevision`;
3. reads Project/CatalogPreferences/Provider/Runtime/Session/Turn/ChangeSet
   Core envelopes inside that transaction;
4. verifies that the coordinator still advertises that Broker revision and
   provider activation generation;
5. on mismatch releases both guards and retries a bounded number of times
   rather than requesting unsupported historical/time-travel rows;
6. reads host-only `CatalogProjectionReceipt`s in that Broker transaction and
   verifies every Catalog-derived row has one exact persisted
   `(catalogSourceRevision, acknowledgedBrokerRevision, payloadDigest)`
   mapping and that the acknowledgement is not newer than the snapshot;
7. returns `snapshotRevision == brokerRevision`, the platform revision and an
   exact `CatalogCheckpoint { catalogSourceRevision,
   acknowledgedBrokerRevision, payloadDigest }`;
8. reads B's durable nonterminal runtime Operations for the returned canonical
   subjects plus the operation-change high-water cursor in that same B read
   transaction.

Trusted UI and the future CLI call the same Rust service and the same canonical
serializer. Given one snapshot, their complete JSON bytes—including
`CatalogPreferencesView`, `CatalogCheckpoint`, `snapshotRevision`, pending
Operations and `operationCursor`—must be identical. They cannot request
"latest" from separate stores. Domain watch messages carry the next Broker
revision and Operation watch messages carry B's durable operation cursor. A
gap or retention gap in either stream forces the corresponding full
snapshot/query-by-subject resync.

---

## Migration, compatibility and rollback rules

Legacy import is one-way data normalization, not a new provider shortcut.
Provider-specific readers live only under
`src-tauri/src/project_runtime/migration/legacy/`, are read-only and cannot
invoke plugin, VM, terminal or session commands.

Import sources, in precedence order, are:

1. an existing successful C migration receipt;
2. explicit `projectManager.folders` order/favorites;
3. `agentVm.projects` profiles;
4. valid legacy `vm`/`agent_run` EntityStore projections;
5. typed Claude/Codex history records.

Import is a two-phase evidence barrier. `Collecting` obtains bounded snapshots,
input digests and source-generation/watermark receipts from every source,
including an explicit empty EntityStore proof. No Project ID or alias is
finalized while the EntityStore candidate reader is unavailable, behind its
advertised watermark or still delivering a pre-watermark row. `Planned` builds
one deterministic evidence graph and detects conflicts. Only `Finalized`
allocates IDs/aliases and appends Catalog outbox rows.

Candidates are grouped only by an available matching filesystem identity or an
exact normalized absolute path. They are never grouped by basename. Nested
projects remain separate. A settings/history/FNV candidate that disagrees with
an EntityStore VM/run path or identity is quarantined before ID allocation and
requires explicit resolution.

EntityStore evidence arriving after finalization is handled by source
generation and sequence. Matching evidence augments the existing receipt;
conflicting/stale/rewound evidence creates a migration conflict and cannot
remap an ID, steal an alias or allocate a second Project silently.

For each imported Project, store:

- the new opaque `projectId`;
- the original FNV-v1 `project-<hex>` value as `project-fnv-v1`;
- normalized/canonical path aliases as private `project-path-v1`;
- source fingerprint, precedence, input digest and receipt;
- complete source-watermark set, including EntityStore generation/sequence;
- favorites/order/view preference migration receipt;
- availability and filesystem evidence.

For each legacy Agent VM run visible through a published legacy entity, create
a read-only shadow Runtime/Session/latest-Turn projection. Store
`(legacy provider, runId) -> sessionId` and any legacy Turn ID aliases.
`runId` is never reused as the canonical Session ID. The shadow is explicitly
`unmanaged`/`legacy_attach_only`; C does not read private JSONL RunStore or
claim guest-supervisor adoption. D/E later performs the controlled active-run
handoff described by the approved design and reuses the alias.

Trusted legacy notification targets shaped as
`{kind:"agent-vm", projectId, cwd, runId}` resolve read-only through the alias
registry to:

```text
/projects/<projectId>
/projects/<projectId>/sessions/<sessionId>
```

Route resolution never creates an alias from an untrusted raw path. Unknown or
ambiguous targets show a recovery screen and preserve the original target
digest for diagnostics without exposing the path.

During the rollback window:

- old settings, EntityStore and plugin directories are not deleted;
- generic UI reads only the Core snapshot;
- a fenced compatibility projector may continue translating legacy entity
  changes to Core shadow envelopes;
- old IPC remains callable only by the legacy view, never by `ui/projects`;
- every legacy resolution increments privacy-safe compatibility telemetry
  outside the domain transaction;
- E switches the projection source with an explicit handoff receipt and
  watermark before the adapter is removed.

Rollout mode is explicit:

- `legacy`: old UI/readers remain authoritative while Catalog import may be
  prepared;
- `shadow`: both snapshots are compared and telemetry is collected, but only
  old UI accepts mutations;
- `canonical`: generic Projects UI/query/action service is authoritative and
  legacy calls are restricted to the fallback view.

Mode changes are revisioned Core operations with parity/precondition checks;
startup never promotes a mode automatically. Compatibility telemetry is
written asynchronously after a successful render/resolution and cannot alter
the route's domain snapshot or make an unknown alias valid.

Rollback disables the new UI/query route and returns to legacy readers without
deleting Catalog IDs/aliases. Re-enabling C resumes from receipts and must not
allocate new IDs for the same source.

---

### Task C1: Add Core Project Runtime wire contracts and transition conformance

**Files:**

- Create: `crates/jarvis-plugin-protocol/src/project_runtime.rs`
- Create: `crates/jarvis-plugin-protocol/tests/project_runtime_wire.rs`
- Create: `schemas/core-project-v1.schema.json`
- Create: `schemas/core-catalog-preferences-v1.schema.json`
- Create: `schemas/core-catalog-projection-receipt-v1.schema.json`
- Create: `schemas/core-project-runtime-provider-v1.schema.json`
- Create: `schemas/core-runtime-v1.schema.json`
- Create: `schemas/core-session-v1.schema.json`
- Create: `schemas/core-turn-v1.schema.json`
- Create: `schemas/core-change-set-v1.schema.json`
- Create: `schemas/core-project-runtime-api-v1.schema.json`
- Create: `packages/jarvis-plugin-ui/test/project-runtime-wire.test.mjs`
- Create: `packages/jarvis-plugin-ui/test/project-runtime-types.ts`
- Modify: `crates/jarvis-plugin-protocol/src/lib.rs`
- Modify: `crates/jarvis-plugin-protocol/src/bin/export_ui_contracts.rs`
- Modify: `packages/jarvis-plugin-ui/src/generated/contracts.ts`
- Modify: `scripts/generate-plugin-ui-contracts.mjs`
- Modify: `scripts/check-plugin-contract-generation.sh`
- Modify: `package.json`
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Add RED Rust wire tests**

Test strict round-trip fixtures for:

- opaque `ProjectId`, `RuntimeId`, `SessionId`, `TurnId`;
- all enum spellings and unknown-field rejection;
- exact Core contract refs and schema digests;
- `ProviderExtension` requiring a registered exact `ContractRef`;
- `ProjectRuntimeObservationBatch` carrying bounded provider observations but
  no caller/owner identity or raw Project path;
- `ProviderSessionObservation` rejecting every host-owned field and every raw
  process/attach field;
- `SessionView` serializing no transport/backend-session/boot/tmux/PID/resume
  evidence;
- `CatalogPreferencesView`, immutable host-only Catalog projection receipt and
  exact Catalog checkpoint fields;
- `ChangeSetView`/`ChangedFileView` with opaque IDs/display metadata and no
  path, bytes, diff or handle;
- `ProviderChangeSetObservation` rejecting canonical IDs, host revisions,
  path/content/diff/handle fields while allowing only provider-local keys and
  safe display/count metadata;
- full required Runtime lifecycle roles and optional attach/doctor roles;
- one Project with two Runtimes, three Sessions and multiple Turns;
- a snapshot whose `snapshotRevision` is the Broker revision;
- no `cwd`, VM name, Lima, `runId`, Claude/Codex control flag or provider
  process field in generic Runtime/Session/Turn DTOs.

Start with:

```rust
use jarvis_plugin_protocol::project_runtime::{
    ProjectId, ProviderSessionObservation,
};
use serde_json::json;

#[test]
fn canonical_ids_are_not_paths_or_legacy_hashes() {
    assert!(ProjectId::parse("prj_01jabcde23456789abcdefghij").is_ok());
    assert!(ProjectId::parse("project-deadbeef").is_err());
    assert!(ProjectId::parse("/Users/alice/repo").is_err());
}

#[test]
fn provider_observation_rejects_host_owned_and_process_fields() {
    let error = serde_json::from_value::<ProviderSessionObservation>(json!({
        "providerSessionKey": "provider-session-1",
        "providerRevision": 3,
        "observedState": "working",
        "backend": "claude",
        "mode": "interactive",
        "currentTurnProviderKey": null,
        "resumabilityCapabilities": ["attach"],
        "displayTitle": "Review",
        "changeSetProviderKey": null,
        "lastActivityAt": 1,
        "provenanceReceiptDigest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "guestEpochDigest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "processEpochDigest": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        "extension": null,
        "sessionId": "ses_01jabcde23456789abcdefghij",
        "tmuxTarget": "secret:0"
    }))
    .unwrap_err();
    assert!(error.to_string().contains("sessionId")
        || error.to_string().contains("tmuxTarget"));
}
```

Run:

```bash
cargo test --manifest-path crates/jarvis-plugin-protocol/Cargo.toml \
  --test project_runtime_wire
```

Expected RED: `project_runtime` does not exist. Fix fixture syntax before
continuing; missing types, not malformed JSON, must be the failure.

- [ ] **Step 2: Define strict Rust DTOs and state enums**

All public wire structs use camelCase and `deny_unknown_fields`. IDs are
newtypes with bounded parsing, not bare strings internally. Reuse B's
`ContractRef`, `OperationRef` and opaque resource-reference DTOs; do not define
duplicates.

Define:

```rust
pub struct ProjectRuntimeSnapshot {
    pub snapshot_revision: u64,
    pub platform_revision: u64,
    pub catalog_checkpoint: CatalogCheckpoint,
    pub preferences: CatalogPreferencesView,
    pub projects: Vec<ProjectView>,
    pub providers: Vec<ProjectRuntimeProviderView>,
    pub runtimes: Vec<RuntimeView>,
    pub sessions: Vec<SessionView>,
    pub turns: Vec<TurnView>,
    pub change_sets: Vec<ChangeSetView>,
    pub operation_cursor: u64,
    pub pending_operations: Vec<RuntimeOperationView>,
}
```

Also define `ProjectRuntimeObservationBatch` and per-entity observation DTOs
used by B's transactional projection-adapter hook. They carry the provider's
stable source instance/outbox identity, provider-local subject keys/revisions
and allowlisted observation data. Canonical IDs, desired state, Core revision,
provider ID, package digest, signer, grants and activation generation are
absent from the payload because B/C bind them from authenticated receipts and
durable Operations.

`RuntimeOperationView` and its subject/cursor types come from B's durable
runtime Operation contract. C only filters and deterministically sorts them by
canonical Project, Runtime, Session or Turn subject; it does not define or
persist a second Operation shape.

Collections are bounded and deterministically sorted by stable ID after their
user-visible ordering key. Timestamps are integer epoch milliseconds. Reasons
use stable code plus safe display message; raw provider errors stay in
redacted audit/private provider state.

- [ ] **Step 3: Generate schemas and TypeScript through B's one pipeline**

Extend B's existing exporter rather than adding a second generator. Committed
JSON Schemas are strict, local-reference-only and byte-for-byte reproducible.
The TypeScript output is generated from the same schemas.

Run:

```bash
npm run generate:plugin-contracts
npm run check:plugin-contracts
node --test packages/jarvis-plugin-ui/test/project-runtime-wire.test.mjs
npx tsc --noEmit -p packages/jarvis-plugin-ui/tsconfig.contracts.json
```

Expected after implementation: generation succeeds and a second check produces
no diff.

- [ ] **Step 4: Add cross-language golden fixtures**

Rust and Node consume the same snapshot fixtures. Assert exact enum tags,
optional field behavior, ID validation, extension shape, sorted output and
round-trip equality. Negative fixtures cover oversized arrays/strings,
top-level provider/host-owned Session fields, process/attach evidence, path-like
IDs, raw ChangedFile path/bytes/handles and an extension contract without an
exact digest. Scan canonical JSON bytes for provenance/path canaries.

- [ ] **Step 5: Run the C1 gate**

```bash
cargo test --manifest-path crates/jarvis-plugin-protocol/Cargo.toml
cargo +1.77.2 test --locked \
  --manifest-path crates/jarvis-plugin-protocol/Cargo.toml
npm run check:plugin-contracts
npm run test:plugin-ui-sdk
npm run check:plugin-boundaries
git diff --check
```

Expected: all commands exit `0`; public protocol crates still have no Tauri,
SQLite, Jarvis Core or Agent VM dependency.

- [ ] **Step 6: Commit C1**

```bash
git add crates/jarvis-plugin-protocol schemas packages/jarvis-plugin-ui \
  scripts/generate-plugin-ui-contracts.mjs \
  scripts/check-plugin-contract-generation.sh package.json .github/workflows/ci.yml
git commit -m "feat(projects): define provider-neutral runtime contracts"
```

---

### Task C2: Build the durable Project Catalog with opaque identity

**Files:**

- Create: `src-tauri/src/project_runtime/mod.rs`
- Create: `src-tauri/src/project_runtime/catalog/mod.rs`
- Create: `src-tauri/src/project_runtime/catalog/database.rs`
- Create: `src-tauri/src/project_runtime/catalog/migrations.rs`
- Create: `src-tauri/src/project_runtime/catalog/identity.rs`
- Create: `src-tauri/src/project_runtime/catalog/selection_handles.rs`
- Create: `src-tauri/src/project_runtime/catalog/store.rs`
- Create: `src-tauri/src/project_runtime/catalog/outbox.rs`
- Create: `src-tauri/src/project_root_picker.rs`
- Create: `src-tauri/migrations/project-runtime/0001_project_catalog.sql`
- Create: `src-tauri/tests/project_root_selection.rs`
- Create: `src-tauri/tests/project_catalog_identity.rs`
- Create: `src-tauri/tests/project_catalog_store.rs`
- Create: `src-tauri/tests/project_catalog_recovery.rs`
- Modify: `src-tauri/src/main.rs`
- Modify: `src-tauri/src/shutdown.rs`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/Cargo.lock`

- [ ] **Step 1: Add RED host picker/selection-handle tests**

`project_root_selection.rs` proves:

1. the Core registration DTO rejects `path`, `cwd`, bookmark bytes and caller
   identity as unknown fields;
2. host picker cancel returns `cancelled` and leaves Catalog revision/outbox
   count byte-for-byte unchanged;
3. a successful pick opens a real directory with
   `O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC` and returns only an opaque,
   single-use `DirectorySelectionHandle`;
4. replay, expiry and another main-window instance fail without mutation;
5. replacing the selected path with another inode, a symlink or a
   symlink-swap between pick and commit returns `project_root_changed`;
6. rename of the already-open directory either resolves the same fd identity
   to its current path or asks the user to pick again—never registers the old
   replacement;
7. cancellation/error closes the fd and mints no handle.

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_root_selection
```

Expected RED: the host picker still returns a raw `PathBuf` and there is no
selection-handle store.

- [ ] **Step 2: Implement the minimal host-owned selection boundary**

`project_root_picker.rs` owns `NSOpenPanel` and immediately opens the chosen
directory without following the final component. `selection_handles.rs` keeps
the `OwnedFd`, current fd-derived path, device/inode evidence, main-view
instance, random token digest, one-use counter and at most 60-second expiry in
bounded process memory. It serializes only the token.

Commit consumes the token atomically, re-`fstat`s the fd, resolves its current
path, performs a no-follow metadata check against the current path and compares
device/inode before entering the Catalog transaction. There is no public
`register(PathBuf)` method. The legacy importer gets a separate crate-private
`LegacyRootEvidence` constructor and cannot be reached from IPC/Bridge.

- [ ] **Step 3: Add RED stable-identity tests**

`project_catalog_identity.rs` must prove:

1. registering a consumed directory selection returns one opaque `prj_...` ID;
2. registering the same filesystem identity/path is idempotent;
3. basename collisions and nested directories remain distinct;
4. a symlink selection is either resolved by the host picker to the already
   opened target identity before handle creation or rejected; it never creates
   a second root keyed by the symlink string;
5. renaming a directory and explicitly rebinding it preserves `projectId`;
6. temporarily removing a directory preserves Project and aliases with
   `rootHealth=unavailable`;
7. recreating a different directory at the old path does not silently adopt
   the prior Project when file identity conflicts;
8. callers cannot supply or overwrite a Core ID;
9. concurrent identical registration creates one Project;
10. deleting/archiving never permits ID reuse.

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_catalog_identity
```

Expected RED: `project_runtime::catalog` is absent.

- [ ] **Step 4: Create the private SQLite/WAL schema**

Store the database under the Jarvis profile directory at
`project-runtime/catalog-v1.sqlite3`; directory mode is `0700`, file mode
`0600`. Reuse B's tested SQLite worker/migration conventions but not B's Broker
tables or revision allocator. Apply:

```sql
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;
PRAGMA busy_timeout = 5000;
```

`0001_project_catalog.sql` creates:

- `catalog_meta(singleton, schema_version, catalog_revision, clean_shutdown,
  opened_at_ms)`;
- `catalog_migrations(version, name, sha256, applied_at_ms)`;
- `projects(project_id, display_name, project_kind, state, revision,
  created_at_ms, updated_at_ms, archived_at_ms)`;
- `project_roots(root_id, project_id, lexical_path, canonical_path,
  path_digest, device_id, file_id, availability, is_primary, revision,
  last_seen_at_ms)`;
- `project_preferences(project_id, favorite_rank, revision, updated_at_ms)`;
- `catalog_preferences(singleton, view_mode, revision, updated_at_ms)`;
- `catalog_outbox(outbox_id, source_instance_id, catalog_source_revision,
  payload_digest, payload_json, created_at_ms, acknowledged_broker_revision,
  acknowledged_at_ms)`;
- `catalog_projection_acks(catalog_source_revision PRIMARY KEY, outbox_id,
  payload_digest, acknowledged_broker_revision UNIQUE, acknowledged_at_ms)`.

Paths are private Catalog values. Logs, errors and public audit use path
digests/display basename only.

- [ ] **Step 5: Implement transactional registration and rebind**

Interactive registration consumes only a revalidated
`DirectorySelectionHandle`; it cannot canonicalize a caller string. Missing
paths may be retained for an already known Project but cannot create a new
Project without explicit validated legacy import evidence. In one `IMMEDIATE`
transaction:

1. consume the already revalidated fd-bound selection evidence;
2. find an exact existing root or reject an identity conflict;
3. allocate an opaque ID only if no Project matches;
4. mutate Project/root/preferences using expected revisions;
5. increment `catalogRevision` once;
6. append one private Core projection input containing affected Project fields
   and the full preferences fields, all with the same
   `catalogSourceRevision` and no caller-chosen acknowledgement, to
   `catalog_outbox`;
7. commit before publishing work.

Rebind requires expected Project revision plus a new one-time directory
selection. Matching file identity is strong evidence; a mismatch requires a
separate host confirmation before a fresh pick/commit and records a reason. No
background volume scan or raw path fallback is introduced.

- [ ] **Step 6: Add durability/recovery and exact acknowledgement tests**

`project_catalog_store.rs` covers CAS conflicts, deterministic list/favorite
order, cards/list preference, path privacy in public errors, outbox in the same
transaction, exact
`catalogSourceRevision -> acknowledgedBrokerRevision` persistence and no
partial state at each injected crash point. A changed acknowledgement for the
same source revision or reused Broker revision for changed bytes is a hard
conflict.

`project_catalog_recovery.rs` covers immutable migration checksums, WAL reopen,
unclean `quick_check`, corrupt database quarantine, an unacknowledged outbox
surviving restart and clean shutdown only after projector drain/checkpoint.

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_root_selection \
  --test project_catalog_identity \
  --test project_catalog_store \
  --test project_catalog_recovery
```

Expected: all commands exit `0`; no test database/WAL appears in the worktree.

- [ ] **Step 7: Wire startup/shutdown without changing provider lifecycle**

Open/recover Catalog after mandatory power recovery and before Project Runtime
routes are admitted. Shutdown stops Catalog mutation admission, drains bounded
projector work, checkpoints WAL and marks clean. It does not stop an Agent VM,
session or provider; those lifecycles remain outside C.

- [ ] **Step 8: Run current-host-toolchain checks and commit C2**

```bash
cargo test --locked --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_catalog_identity --test project_catalog_recovery
cargo clippy --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_root_selection --test project_catalog_identity \
  --test project_catalog_store -- -D warnings
git diff --check
git add src-tauri/src/project_runtime src-tauri/migrations/project-runtime \
  src-tauri/tests/project_catalog_identity.rs \
  src-tauri/tests/project_root_selection.rs \
  src-tauri/tests/project_catalog_store.rs \
  src-tauri/tests/project_catalog_recovery.rs \
  src-tauri/src/project_root_picker.rs src-tauri/src/main.rs \
  src-tauri/src/shutdown.rs \
  src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat(projects): persist stable project catalog"
```

---

### Task C3: Import folders, favorites, profiles and history with explicit aliases

**Files:**

- Create: `src-tauri/src/project_runtime/migration/mod.rs`
- Create: `src-tauri/src/project_runtime/migration/aliases.rs`
- Create: `src-tauri/src/project_runtime/migration/importer.rs`
- Create: `src-tauri/src/project_runtime/migration/receipts.rs`
- Create: `src-tauri/src/project_runtime/migration/legacy/mod.rs`
- Create: `src-tauri/src/project_runtime/migration/legacy/fnv_v1.rs`
- Create: `src-tauri/src/project_runtime/migration/legacy/settings.rs`
- Create: `src-tauri/src/project_runtime/migration/legacy/history.rs`
- Create: `src-tauri/src/project_runtime/migration/legacy/entities.rs`
- Create: `src-tauri/migrations/project-runtime/0002_aliases_imports.sql`
- Create: `src-tauri/tests/project_catalog_import.rs`
- Create: `src-tauri/tests/project_aliases.rs`
- Create: `src-tauri/tests/project_import_recovery.rs`
- Create: `src-tauri/tests/fixtures/project-runtime/legacy-settings.json`
- Create: `src-tauri/tests/fixtures/project-runtime/legacy-history.json`
- Create: `src-tauri/tests/fixtures/project-runtime/legacy-entities.json`
- Create: `src-tauri/tests/fixtures/project-runtime/legacy-import-conflicts.json`
- Create: `src-tauri/tests/fixtures/project-runtime/fnv-v1.json`
- Modify: `src-tauri/src/entities.rs`
- Modify: `src-tauri/src/history.rs`
- Modify: `src-tauri/src/project_runtime/catalog/store.rs`
- Modify: `src-tauri/src/project_runtime/mod.rs`

- [ ] **Step 1: Freeze the old identity algorithm in golden tests**

Copy the exact existing FNV-1a/canonical-path behavior only into
`migration/legacy/fnv_v1.rs`. Golden fixtures cover ASCII, Unicode, spaces,
symlink canonicalization and historical `project-<16 hex>` formatting. No
generic source may import this module after migration.

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_aliases fnv
```

Expected RED: migration/alias code does not exist.

- [ ] **Step 2: Add alias, import and receipt tables**

`0002_aliases_imports.sql` creates:

- `entity_aliases(subject_kind, provider_id, alias_kind, alias_value,
  alias_digest, canonical_id, valid_from_ms, valid_until_ms, reason,
  source_generation, state)`;
- `migration_sources(source_kind, source_key_digest, source_generation,
  snapshot_watermark, input_digest, precedence, collection_state,
  observed_at_ms)`;
- `migration_collection_runs(collection_id, migration_version, phase,
  required_sources_json, source_watermarks_digest, evidence_graph_digest,
  started_at_ms, finalized_at_ms)`, where phase is
  `collecting | planned | finalized | conflicted`;
- `migration_receipts(receipt_id, migration_version, source_kind,
  source_key_digest, input_digest, canonical_subject_kind, canonical_id,
  collection_id, source_generation, snapshot_watermark, phase, result_digest,
  rollback_until_ms, created_at_ms, updated_at_ms)`;
- `migration_conflicts(conflict_id, alias_kind, alias_digest,
  candidate_ids_json, reason_code, created_at_ms, resolved_at_ms,
  resolution_receipt_id)`;
- `project_runtime_rollout(singleton, mode, revision, enabled_at_ms,
  rollback_until_ms, updated_at_ms)`, where mode is
  `legacy | shadow | canonical`;
- `compatibility_usage(alias_kind, alias_digest, resolution_count,
  last_resolved_at_ms)`.

`alias_value` is private and bounded; telemetry/audit use `alias_digest`.
Uniqueness prevents one live alias from resolving to two canonical IDs.
Changing an input under the same source key produces a new deterministic
receipt phase, not silent overwrite.

- [ ] **Step 3: Expose typed history and EntityStore candidate readers**

Add a crate-private `HistoryProjectRecord`/`HistorySessionRecord` API to
`history.rs`. Keep the current JSON IPC for rollback but build it from the
typed records. Scanning stays read-only with respect to providers and is
scheduled independently; route opening cannot trigger a scan or cache rebuild.

Typed history records include source transcript identity, backend, session ID,
root candidate, title, model and timestamps. The importer never treats a
session ID or basename as a Project ID.

Add a crate-private bounded EntityStore candidate reader in
`migration/legacy/entities.rs`. It requests one atomic legacy snapshot plus
`sourceGeneration` and `snapshotWatermark`, accepts only the documented
`plugin:agent-vm` `vm`/`agent_run` candidate fields, and returns an explicit
signed/digested empty-snapshot receipt when no rows exist. It cannot subscribe
for control, mutate EntityStore, invoke Agent VM, or finalize IDs. The reader
must report unavailable, generation rewind and a row arriving at or below the
claimed watermark as distinct errors.

- [ ] **Step 4: Add RED evidence-barrier and deterministic import tests**

Fixtures combine:

- repeated Project Manager folders;
- favorites with stale/duplicate FNV IDs;
- `agentVm.projects` with enabled/disabled autostart;
- an EntityStore-only VM/run root that must become a candidate before any
  canonical ID is allocated;
- Claude and Codex history at the same root;
- same basename at two roots;
- one missing root;
- nested projects;
- an alias that conflicts with an existing receipt;
- settings/FNV/history evidence for one root while EntityStore reports a
  different path or filesystem identity;
- late, duplicate, stale and generation-rewound EntityStore evidence.

Assert precedence, stable IDs across repeated import, original favorite order,
global view preference, no basename merge, missing-root preservation, conflict
quarantine, legal `legacy -> shadow -> canonical -> legacy` rollout with
revision/precondition checks and no provider command. Autostart is imported as
migration metadata for D/E; C does not start anything.

Additionally assert:

1. `Collecting` cannot advance while the EntityStore reader is unavailable,
   behind its advertised watermark or lacks an explicit empty proof;
2. an EntityStore-only candidate and evidence arriving before the collection
   watermark participate in the same deterministic graph;
3. settings/FNV/path evidence conflicting with EntityStore VM/run evidence
   produces `migration_conflict` before a Project ID or alias is allocated;
4. an explicit empty EntityStore snapshot permits `Planned`;
5. `Planned` freezes the complete source-watermark digest, and only
   `Finalized` allocates IDs and emits Catalog rows;
6. conflicting evidence observed after finalization never remaps the existing
   Project, steals an alias or allocates a second Project; it creates a
   quarantined conflict linked to the immutable receipt;
7. a stale/rewound generation is quarantined and cannot be interpreted as an
   empty source.

- [ ] **Step 5: Implement the idempotent two-phase importer**

`Collecting` reads one bounded snapshot of every required legacy source and
persists its generation, watermark and input digest. It does not allocate
Project IDs, aliases or outbox rows. The source set always includes
EntityStore; zero candidates is valid only with its explicit empty proof.

After every reader proves completeness through its frozen watermark,
`Planned` normalizes candidates, sorts by documented precedence/source key,
builds one deterministic evidence graph and records its digest. Resolve
filesystem/path contradictions and alias conflicts here. Any conflict marks
only the candidate as conflicted before allocation.

`Finalized` rechecks the frozen source watermark/generation set and, in one
transaction per conflict-independent group, allocates each logical Project,
aliases, preferences and immutable receipt and appends its Catalog outbox row.
There is no provisional canonical ID visible during `Collecting` or `Planned`.
Source deletion does not delete a Project; it updates evidence/health only
after the rollback window and explicit policy.

On crash, receipts resume from the last committed phase. Replay with an equal
input digest is a no-op. An alias collision quarantines only that candidate and
does not block unrelated Projects. Late matching evidence may augment a
receipt without changing identity. Late conflicting or stale-generation
evidence records a conflict against the finalized receipt and cannot reopen
allocation.

- [ ] **Step 6: Verify and commit C3**

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_catalog_import \
  --test project_aliases \
  --test project_import_recovery
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  history::tests
git diff --check
git add src-tauri/src/project_runtime/migration \
  src-tauri/migrations/project-runtime/0002_aliases_imports.sql \
  src-tauri/tests/project_catalog_import.rs \
  src-tauri/tests/project_aliases.rs \
  src-tauri/tests/project_import_recovery.rs \
  src-tauri/tests/fixtures/project-runtime \
  src-tauri/src/history.rs src-tauri/src/project_runtime
git commit -m "feat(projects): migrate legacy catalog aliases"
```

---

### Task C4: Compile providers and validate canonical projections

**Files:**

- Create: `src-tauri/src/project_runtime/provider_registry.rs`
- Create: `src-tauri/src/project_runtime/state_machine.rs`
- Create: `src-tauri/src/project_runtime/provenance.rs`
- Create: `src-tauri/src/project_runtime/change_sets.rs`
- Create: `src-tauri/src/project_runtime/projection.rs`
- Create: `src-tauri/src/project_runtime/projector.rs`
- Create: `src-tauri/tests/project_runtime_provider_registry.rs`
- Create: `src-tauri/tests/project_runtime_state_machine.rs`
- Create: `src-tauri/tests/project_runtime_projection.rs`
- Create: `src-tauri/tests/project_runtime_session_privacy.rs`
- Create: `src-tauri/tests/project_runtime_change_sets.rs`
- Create: `src-tauri/tests/project_catalog_projector.rs`
- Create: `src-tauri/tests/fixtures/project-runtime/provider-resolved.json`
- Create: `src-tauri/tests/fixtures/project-runtime/provider-missing-command.json`
- Modify: `src-tauri/src/project_runtime/mod.rs`
- Modify: `src-tauri/src/plugin_platform/core_projection.rs`
- Modify: `src-tauri/src/plugin_platform/contributions/runtime_commands.rs`
- Modify: `src-tauri/src/plugin_platform/coordinator/snapshot.rs`

- [ ] **Step 1: Add RED exact-provider registration tests**

Build verified manifests with:

- all six required lifecycle roles and exact Core projection contracts;
- a missing stop/destroy command;
- an ambiguous local handler;
- a mismatched args/result digest;
- an extension contract owned by another signer;
- stale package/activation generations;
- two provider instances for one Project.

Assert that only the exact complete registration yields a
`ProjectRuntimeProviderReceipt`. Missing or stale roles are visible as
repairable disabled reasons; no call falls back to a string command.

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_runtime_provider_registry
```

Expected RED: C provider registry does not exist.

- [ ] **Step 2: Compile `projectRuntimes` from B receipts**

`ProjectRuntimeProviderRegistry` consumes only B's already validated manifest,
exact `ResolvedRuntimeCommand`s and active package receipt. Store:

- provider ID/display metadata and supported Project kinds/backends;
- exact Runtime/Session/Turn/extension contracts;
- required and optional command registration receipts;
- provider page route and contribution IDs;
- package digest, signer lineage and activation generation;
- availability/repair reason.

Registration and unregistration happen during B12 lifecycle reconciliation,
never during page open. A partial provider is unavailable, not half-active.

- [ ] **Step 3: Add RED state-machine/property tests**

Generate every pair of Runtime, Session and Turn states and assert only the
documented edges pass. Add focused tests for:

- stale runtime generation/receipt and conflicting private host-boot/lifecycle
  provenance binding;
- duplicate `(project, provider, instance)`;
- guest boot or process identity drift;
- stopped/interrupted Session resume without an Operation;
- Turn sequence gap/duplicate, two active Turns and ninth queued Turn;
- terminal Turn rewrite;
- provider top-level extension leakage;
- valid multi-Runtime/multi-Session/multi-Turn projection.

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_runtime_state_machine
```

Expected RED: validator functions are absent.

- [ ] **Step 4: Implement projection admission using B Broker**

Providers do not write Core-owned contracts directly. Their SDK outbox stores
a C1 `ProjectRuntimeObservationBatch` beside their domain mutation and submits
it to B's authenticated transactional projection-adapter hook. C's
`ProjectRuntimeProjectionAdapter` then:

1. resolves exact active provider receipt/generation;
2. schema-validates Core envelope and exact extension;
3. rejects any observation containing canonical IDs, desired state, Core
   revision/timestamps, provider identity, process/transport/attach evidence or
   another host-owned field;
4. proves the receipt's subject grant/lifecycle binding covers the canonical
   Project/provider instance and the durable Operation that admitted this
   generation;
5. resolves canonical Project/Runtime/Session/Turn/ChangeSet IDs from
   host-owned mappings without accepting raw path or provider-chosen Core IDs;
6. loads prior Core envelope through B projection API;
7. validates private provenance digests, generation/revision and state edge;
8. maps allowlisted observations to public Views and stores only the
   `SessionProvenanceBinding`/`RuntimeProvenanceBinding` digests through B's
   adapter-private state API;
9. asks B's trusted-Core writer to apply all related entity/event changes in
   the same Broker transaction that records the provider outbox receipt and
   private binding;
10. returns B's idempotent outbox acknowledgement only after commit.

Do not allocate `brokerRevision`, persist a C event/cursor or open Broker SQL
directly. Use B's ingress and trusted Core projection APIs. Raw backend session
IDs, tmux/PID/boot/attach material remain in the provider/controller store;
neither B private adapter state nor C stores their raw values.

`change_sets.rs` maps bounded provider-local changed-file keys and allowlisted
display metadata to opaque `changeSetId`/`changedFileId` values. The provider
key digest is private. Public rows contain no path, file bytes, diff or
resource handle. Content/diff/open/reveal is a separate click-time B7 handle
request and cannot be reconstructed from snapshot data.

`project_runtime_session_privacy.rs` and
`project_runtime_change_sets.rs` scan public DTO bytes, Broker query/watch
results, logs and TypeScript snapshots for raw provider keys plus
`backendSessionId`, `transportId`, `tmux`, `pid`, boot/attach fields, path
canaries, diff bytes and resource handles. Provider attempts to set
`sessionId`, `runtimeId`, `desiredState`, `revision` or those private fields
must fail before a Broker write.

- [ ] **Step 5: Project Catalog outbox through the same ingress**

`ProjectCatalogProjector` replays each committed Catalog outbox change set
through B's trusted-Core writer into its affected Core Project envelopes and
the complete `CatalogPreferencesView` at one Broker revision. B injects that
allocated revision as the acknowledgement on every final View; the outbox
cannot supply it, and writes the immutable host-only
`CatalogProjectionReceipt` in that transaction. A duplicate
outbox ID/digest and `catalogSourceRevision` returns the original
`acknowledgedBrokerRevision`; changed bytes, a second Broker revision for the
same source revision, or one Broker revision reused for different Catalog
bytes is a hard conflict. Persist that exact source/ack pair only after B
commit. It does not manufacture a plugin principal or pass through the
provider-owned ingress path.

Catalog preference/path updates visible to trusted UI are projected in the
same Core transaction. Plugin field projection removes private root path.

- [ ] **Step 6: Verify and commit C4**

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_runtime_provider_registry \
  --test project_runtime_state_machine \
  --test project_runtime_projection \
  --test project_runtime_session_privacy \
  --test project_runtime_change_sets \
  --test project_catalog_projector \
  --test broker_outbox \
  --test broker_projection_consistency
bash scripts/check-plugin-platform-boundaries.sh
git diff --check
git add src-tauri/src/project_runtime \
  src-tauri/src/plugin_platform/core_projection.rs \
  src-tauri/src/plugin_platform/contributions/runtime_commands.rs \
  src-tauri/src/plugin_platform/coordinator/snapshot.rs \
  src-tauri/tests/project_runtime_provider_registry.rs \
  src-tauri/tests/project_runtime_state_machine.rs \
  src-tauri/tests/project_runtime_projection.rs \
  src-tauri/tests/project_runtime_session_privacy.rs \
  src-tauri/tests/project_runtime_change_sets.rs \
  src-tauri/tests/project_catalog_projector.rs \
  src-tauri/tests/fixtures/project-runtime
git commit -m "feat(projects): validate runtime provider projections"
```

---

### Task C5: Expose one immutable snapshot to UI and the future CLI

**Files:**

- Create: `src-tauri/src/project_runtime/query/mod.rs`
- Create: `src-tauri/src/project_runtime/query/service.rs`
- Create: `src-tauri/src/project_runtime/query/ui_adapter.rs`
- Create: `src-tauri/src/project_runtime/query/headless_adapter.rs`
- Create: `src-tauri/src/project_runtime/query/watch.rs`
- Create: `src-tauri/src/project_runtime/actions.rs`
- Create: `src-tauri/tests/project_runtime_snapshot.rs`
- Create: `src-tauri/tests/project_runtime_surface_consistency.rs`
- Create: `src-tauri/tests/project_runtime_watch.rs`
- Create: `src-tauri/tests/project_runtime_action_dispatch.rs`
- Create: `src-tauri/tests/project_runtime_operation_recovery.rs`
- Create: `src-tauri/tests/project_runtime_operation_watch.rs`
- Create: `src-tauri/tests/project_runtime_operation_cancel.rs`
- Modify: `src-tauri/src/project_runtime/mod.rs`
- Modify: `src-tauri/src/plugin_platform/core_projection.rs`
- Modify: `src-tauri/src/plugin_platform/coordinator/snapshot.rs`

- [ ] **Step 1: Add RED immutable-snapshot tests**

In one Broker transaction publish:

- two Projects;
- one complete `CatalogPreferencesView`;
- two providers at one active platform generation;
- multiple Runtimes/Sessions/Turns/ChangeSets;
- one B durable nonterminal runtime Operation for a returned subject.

Pause a concurrent writer between logical records and assert a reader sees
either the full old transaction or full new transaction, never a mixed graph.
Every referenced parent must exist at the same revision or the row is
quarantined/omitted with a recovery reason.

Assert:

```text
snapshot.snapshotRevision == broker snapshot revision
snapshot.catalogCheckpoint.catalogSourceRevision
  maps exactly to catalogCheckpoint.acknowledgedBrokerRevision
catalogCheckpoint.payloadDigest == immutable Broker receipt payloadDigest
Project/Preferences catalogSourceRevision == checkpoint source revision
UI complete JSON bytes == headless complete JSON bytes
UI operationCursor == headless operationCursor
provider receipt generation == coordinator generation
catalogCheckpoint.acknowledgedBrokerRevision <= snapshotRevision
```

The byte comparison covers the complete canonical payload, including ordered
preferences, Projects, ChangeSets, pending Operations, both cursors and both
revisions. A matching Project list with missing/different preferences or
checkpoint is a failure.

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_runtime_snapshot \
  --test project_runtime_surface_consistency
```

Expected RED: no Project Runtime query service exists.

- [ ] **Step 2: Implement one domain service**

Define a Tauri-independent `ProjectRuntimeQuery` trait and concrete
`ProjectRuntimeSnapshotService`. The service reads through B's
`CoreProjectionReader` under the coordinator guard. It never receives a
Catalog/EntityStore/settings/history handle.

Both `UiProjectRuntimeAdapter` and `HeadlessProjectRuntimeAdapter` call that
service and serialize the same public DTO. The headless adapter is the exact
port D's CLI will use; C does not create a CLI parser/binary or Agent VM
commands.

- [ ] **Step 3: Add watch/gap/resync tests**

Subscribe at known Broker and runtime-Operation cursors, apply consecutive
changes and verify ordered patch application. Force:

- duplicate event;
- out-of-order event;
- retention gap;
- provider activation-generation switch;
- Project deletion/tombstone;
- UI disconnect/reconnect.

Duplicates are idempotent. Any gap/generation mismatch invalidates the local
projection and returns a full immutable snapshot. An Operation cursor gap
queries B's durable nonterminal/terminal changes by exact canonical subject and
returns a new high-water cursor. Restarting the service with no UI memory must
recover the same pending Operation. Neither stream is patched with a second
source.

- [ ] **Step 4: Add RED persist-before-dispatch/recovery tests**

`project_runtime_action_dispatch.rs`,
`project_runtime_operation_recovery.rs`,
`project_runtime_operation_watch.rs` and
`project_runtime_operation_cancel.rs` prove:

1. authorization, exact command resolution and confirmation complete before
   admission, then B commits `RuntimeOperationView(state=queued)`, its subject,
   args digest, idempotency key and first change-cursor row before any provider
   dispatch counter increments;
2. an `Accepted(OperationRef)` is returned only after that commit;
3. a crash before commit dispatches nothing, while a crash after commit/before
   dispatch is recovered from the durable queue exactly once by idempotency;
4. crashes after dispatch/before provider acknowledgement and after
   acknowledgement/before Core projection reconcile through the provider
   operation receipt/status contract, never by blindly reporting success or
   issuing an unbounded duplicate;
5. after host/UI restart, query-by-exact Project/Runtime/Session/Turn subject
   returns the pending Operation and its latest durable state;
6. ordered cursor watch survives reconnect; duplicate rows are idempotent and
   retention/cursor gaps force query-by-subject resync;
7. cancellation reauthenticates the current principal, command cancellation
   permission, current grant revision and exact subject. Revoked/cross-subject
   cancellation fails; queued cancellation is terminal without dispatch and
   running cancellation follows the provider's typed cancellation receipt;
8. `succeeded | failed | cancelled | interrupted | timed_out` are immutable
   under late dispatch, cancellation, provider or replay updates.

Expected RED: B's package install/update journal cannot satisfy runtime
Operation admission, subject query or watch semantics.

- [ ] **Step 5: Implement provider-neutral actions through B operations**

`ProjectRuntimeActionService` accepts canonical IDs, action role,
expected snapshot/entity revision and validated args. It resolves the
generation-bound provider command receipt and asks B Gate v2 to authorize and
confirm. For any asynchronous provider command, it then calls B's
`RuntimeOperationService::admit` with:

```text
exact command contract/version/schema digest
canonical subject contract + subject ID
provider receipt/package/activation generation
principal and grant revision digests
bounded validated canonical args/reference payload + digest + idempotency key
deadline/cancellation policy
```

Admission transactionally persists `queued` plus the first durable operation
change before the dispatcher can claim it. Only the post-commit B worker
dispatches the exact provider command. Results are `Completed` only for a
documented synchronous no-provider mutation, or `Accepted(OperationRef)` after
durable admission. Accepted remains pending until B reaches an immutable
terminal state and the resulting Core projection is visible.

The service rejects raw plugin IDs, handler strings, cwd, runId, VM names,
terminal IDs and caller-supplied risk/grants. Stale revisions fail before
Operation admission/provider dispatch. C adds no Operation table, local retry
journal or UI-only pending map.

- [ ] **Step 6: Verify surface parity and commit C5**

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_runtime_snapshot \
  --test project_runtime_surface_consistency \
  --test project_runtime_watch \
  --test project_runtime_action_dispatch \
  --test project_runtime_operation_recovery \
  --test project_runtime_operation_watch \
  --test project_runtime_operation_cancel \
  --test broker_projection_consistency \
  --test plugin_platform_snapshot
cargo test --locked --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_runtime_surface_consistency
git diff --check
git add src-tauri/src/project_runtime/query \
  src-tauri/src/project_runtime/actions.rs src-tauri/src/project_runtime/mod.rs \
  src-tauri/src/plugin_platform/core_projection.rs \
  src-tauri/src/plugin_platform/coordinator/snapshot.rs \
  src-tauri/tests/project_runtime_snapshot.rs \
  src-tauri/tests/project_runtime_surface_consistency.rs \
  src-tauri/tests/project_runtime_watch.rs \
  src-tauri/tests/project_runtime_action_dispatch.rs \
  src-tauri/tests/project_runtime_operation_recovery.rs \
  src-tauri/tests/project_runtime_operation_watch.rs \
  src-tauri/tests/project_runtime_operation_cancel.rs
git commit -m "feat(projects): expose revisioned runtime snapshot"
```

---

### Task C6: Migrate legacy runs and deep links through a fenced read-only adapter

**Files:**

- Modify: `src-tauri/src/project_runtime/migration/legacy/entities.rs`
- Create: `src-tauri/src/project_runtime/migration/legacy/run_projection.rs`
- Create: `src-tauri/src/project_runtime/migration/legacy/deep_links.rs`
- Create: `src-tauri/src/project_runtime/migration/legacy/handoff.rs`
- Create: `src-tauri/migrations/project-runtime/0003_legacy_runtime_handoff.sql`
- Create: `src-tauri/tests/project_runtime_legacy_runs.rs`
- Create: `src-tauri/tests/project_runtime_legacy_deep_links.rs`
- Create: `src-tauri/tests/project_runtime_legacy_handoff.rs`
- Modify: `src-tauri/tests/fixtures/project-runtime/legacy-entities.json`
- Create: `src-tauri/tests/fixtures/project-runtime/legacy-deep-links.json`
- Modify: `src-tauri/src/entities.rs`
- Modify: `src-tauri/src/ipc.rs`
- Modify: `src-tauri/src/main.rs`
- Modify: `src-tauri/src/project_runtime/migration/mod.rs`

- [ ] **Step 1: Add RED run/Turn alias tests**

Translate fixture legacy `vm` and `agent_run` entities into:

- one canonical Project alias resolution;
- one read-only shadow Runtime;
- one canonical public `SessionView` with a private `runId` alias/digest;
- the latest canonical Turn, preserving terminal/waiting/interrupted meaning;
- provider-safe reason/resumability fields.

Assert that repeated snapshots update revisions without allocating another
Session, a different `runId` allocates a different Session, raw cwd is not
copied into Runtime/Session, raw run/backend/tmux/process/attach evidence is
not copied into `SessionView` or Broker query/watch rows, and no private
RunStore file is opened. Legacy evidence maps first to the allowlisted provider
observation/private provenance path from C4; it cannot construct a privileged
Core Session envelope directly.

- [ ] **Step 2: Persist handoff fencing**

`0003_legacy_runtime_handoff.sql` creates:

- `legacy_projection_sources(source_id, owner, source_generation,
  last_input_digest, last_applied_broker_revision, state, updated_at_ms)`;
- `legacy_runtime_handoffs(provider_id, project_id, legacy_source_id,
  canonical_runtime_id, legacy_run_id_digest, canonical_session_id, phase,
  legacy_watermark, provider_watermark, receipt_digest, updated_at_ms)`.

Only one source generation may publish a canonical Runtime/Session at a time.
E must commit `provider-caught-up` and then `legacy-fenced` before provider
outbox becomes authoritative. C never chooses "latest timestamp wins".

- [ ] **Step 3: Implement the bounded compatibility projector**

Subscribe read-only to the old EntityStore snapshot/update stream. Accept only
owner `plugin:agent-vm`, kinds `vm`/`agent_run`, bounded validated attrs and a
Catalog-resolved Project alias. Convert state with an explicit table and write
Core shadow envelopes through B ingress. Reuse C3's frozen
generation/watermark reader and immutable source receipt; do not create a
second EntityStore reader. Raw run/session/process identity is hashed/bound in
adapter-private provenance and never appears in the public shadow envelope.

The adapter cannot import:

```text
plugins_cmd
runtime.ensure/status/start/stop/restart
agent_vm_terminal_*
session_launch
Agent VM private runs directory
```

Unknown provider/provenance is `legacy/unknown`, unmanaged and blocked for
mutation. Existing legacy notification delivery remains outside this generic
projection until F; C does not claim durable notification receipts.

- [ ] **Step 4: Add RED deep-link tests**

Cover valid/invalid forms of the current trusted target:

```json
{
  "kind": "agent-vm",
  "projectId": "project-...",
  "project": "jarvis",
  "cwd": "/absolute/path",
  "runId": "run-a"
}
```

Valid known aliases resolve to canonical Project/Session routes. Unknown,
relative, oversized, path-conflicting, ambiguous and untrusted targets fail
closed without creating Projects/aliases. Resolution executes zero provider,
terminal, Operation, Catalog mutation and Broker mutation calls.

- [ ] **Step 5: Route toast targets through the canonical resolver**

`toast_click` resolves trusted legacy targets with
`LegacyProjectRouteResolver` and emits a canonical `open-project-runtime`
event. Keep the old `open-agent-vm` fallback behind the rollback feature flag
only when the new route is disabled; do not silently use it after an alias
conflict.

- [ ] **Step 6: Verify and commit C6**

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_runtime_legacy_runs \
  --test project_runtime_legacy_deep_links \
  --test project_runtime_legacy_handoff
git diff --check
git add src-tauri/src/project_runtime/migration \
  src-tauri/migrations/project-runtime/0003_legacy_runtime_handoff.sql \
  src-tauri/tests/project_runtime_legacy_runs.rs \
  src-tauri/tests/project_runtime_legacy_deep_links.rs \
  src-tauri/tests/project_runtime_legacy_handoff.rs \
  src-tauri/tests/fixtures/project-runtime \
  src-tauri/src/entities.rs src-tauri/src/ipc.rs src-tauri/src/main.rs
git commit -m "feat(projects): bridge legacy runtime aliases"
```

---

### Task C7: Make Project routes read-only and replace direct launch IPC

**Files:**

- Create: `src-tauri/src/project_runtime/routes.rs`
- Create: `src-tauri/src/project_runtime/ipc.rs`
- Create: `src-tauri/src/legacy_project_runtime/mod.rs`
- Create: `src-tauri/src/legacy_project_runtime/local_cli_provider.rs`
- Create: `src-tauri/src/legacy_project_runtime/launch_spec.rs`
- Create: `src-tauri/tests/project_route_no_side_effects.rs`
- Create: `src-tauri/tests/project_runtime_ipc.rs`
- Create: `src-tauri/tests/project_root_ipc.rs`
- Create: `src-tauri/tests/project_runtime_local_history.rs`
- Create: `src-tauri/tests/legacy_local_cli_provider.rs`
- Create: `src-tauri/tests/launch_injection.rs`
- Modify: `src-tauri/src/history.rs`
- Modify: `src-tauri/src/launch.rs`
- Modify: `src-tauri/src/ipc.rs`
- Modify: `src-tauri/src/main.rs`
- Modify: `ui/bridge.js`

- [ ] **Step 1: Define route-open as a RED invariant**

Instrument counters for:

```text
Catalog writes/outbox append
Broker writes/events
Operation creation
resource-handle mint
typed provider command dispatch
legacy plugins_cmd
session_launch
Agent VM runtime calls
agent_vm_terminal_ensure/snapshot/input
terminal/tmux process spawn
history scan trigger
```

Open:

- Project list;
- canonical Project detail;
- runtime/session/Turn subroutes;
- known legacy FNV/path/run link;
- missing provider and unavailable-root recovery routes.

Every counter remains zero. Alias lookup and immutable snapshot reads are
allowed. Privacy-safe navigation telemetry is outside domain state and is
emitted only after render; it cannot affect the returned revision.

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_route_no_side_effects
```

Expected RED: current Project card path warms/ensures Agent VM terminal/runtime.

- [ ] **Step 2: Implement canonical route parsing**

Support:

```text
/projects
/projects/<projectId>
/projects/<projectId>/runtimes/<runtimeId>
/projects/<projectId>/sessions/<sessionId>
/projects/<projectId>/sessions/<sessionId>/turns/<turnId>
```

Parser validates ID newtypes and parent membership against one snapshot.
Opening a route calls only `ProjectRuntimeQuery`. It returns typed selected IDs,
snapshot and recovery reason. It cannot invoke an action method by
construction.

- [ ] **Step 3: Add one trusted UI IPC boundary**

Expose main-window-only commands:

```text
project_runtime_snapshot
project_runtime_watch_open
project_runtime_watch_poll
project_runtime_watch_close
project_route_resolve
project_catalog_pick_root
project_catalog_register_selected_root
project_catalog_pick_rebind_root
project_catalog_rebind_selected_root
project_catalog_favorite_set
project_catalog_favorite_move
project_catalog_view_set
project_runtime_action_invoke
project_runtime_operation_watch
project_runtime_operation_cancel
```

The mutation payload has canonical IDs, expected revisions and at most the
opaque one-time `DirectorySelectionHandle` from C2. It rejects `path`, `cwd`,
bookmark bytes and caller-supplied filesystem identity. Picker cancellation
returns a typed `cancelled` outcome and leaves Catalog/outbox revision
unchanged. The picker is host-owned, binds the open directory fd before
returning, and consumption performs the C2 inode/no-follow revalidation; the UI
never receives a raw path. Authorization and provider identity are
host-derived. Update the B2 command inventory. Plugin pages continue to use
Bridge v1, not these Tauri commands.

- [ ] **Step 4: Add RED legacy-provider and launch-injection tests**

`legacy_local_cli_provider.rs` is intentionally outside
`src-tauri/src/project_runtime`. Tests prove it registers
`dev.jarvis.legacy.local-cli` through B's exact provider registry with explicit
`legacy-local.session.create`/`legacy-local.session.resume` roles, exact
args/result schema digests, package/activation generation and projection
receipt. It is not a normal `contributes.projectRuntimes` managed provider and
cannot claim provision/start/stop/destroy support or receive C4's complete
managed-provider receipt. Missing/ambiguous/stale declared roles disable its
corresponding action like any other B command. Generic Core sees only exact B
receipts plus safe unmanaged/shadow observations; there is no Core-owned
dispatch special case.

`launch_injection.rs` exercises the current real
`src-tauri/src/launch.rs` boundary. It must reject before process/AppleScript
spawn:

```text
unknown agent/backend
session IDs containing LF, CR, NUL or any whitespace
$(), backticks, quotes, semicolon, pipe, ampersand or shell redirection
leading dash, slash/path syntax and IDs longer than 128 bytes
caller-supplied cwd/path and malformed custom-terminal templates
```

Valid resume IDs match exactly
`[A-Za-z0-9][A-Za-z0-9_-]{0,127}`. Golden cases cover a new session plus valid
Claude/Codex resume IDs. Real temporary Catalog roots containing spaces,
quotes, a newline and literal `$()` must remain one cwd argument and execute no
expansion; compare the adapter's exact argv/working-directory result at the
spawn seam. Separate tests prove raw root IPC rejection,
picker cancel with no revision, selection replay/expiry, symlink/inode swap and
cross-window handle rejection.

Expected RED: `agent_command` currently interpolates unchecked session IDs,
unknown agents fall back to Claude and caller cwd reaches shell text.

- [ ] **Step 5: Normalize local history/launch as an exact compatibility provider**

Represent current Claude/Codex transcript sessions as read-only Core shadow
Sessions under explicit provider `dev.jarvis.legacy.local-cli`.
They are `unmanaged` and keep backend resume identity; they do not pretend to
have a managed Runtime lifecycle. Raw resume/session identity stays in the
provider's private store; public `SessionView` contains only safe resumability
and display metadata.

Move terminal spawning behind the exact typed compatibility provider using the
same `ProjectRuntimeActionService` and durable B Operation admission. Generic
UI never imports or calls `crate::launch`/`session_launch`; it resolves the
provider receipt like any other provider.

Replace string concatenation in `launch.rs` with:

```text
ExactLocalBackend enum
ValidatedResumeId
TrustedProjectRoot (host-private Catalog resolution)
LaunchSpec { program, argv, trusted_root }
```

Build program/argv first. Never place provider/caller text into a shell
template. Terminal/iTerm adapters shell-quote each validated argv/root
component only at their final OS boundary; custom terminal configuration must
contain exactly one `{cmd}` placeholder and receives one host-rendered,
fully-quoted command. New/resume admission persists canonical Project/Session
IDs and expected revisions, never the path. Immediately before spawn the
post-commit compatibility-provider worker resolves a host-private
`TrustedProjectRoot` from Catalog and revalidates root/project revision.
Unknown backend, raw caller cwd/path or caller resume ID fails before Operation
admission. A corrupt provider-private stored resume ID fails typed launch
validation before spawn and makes the admitted Operation fail safely.

The old rollback `session_launch` IPC must delegate to this same exact provider
and typed validation or be disabled; it cannot retain the vulnerable
`agent_command` path during the rollback window.

- [ ] **Step 6: Make current Agent VM workspace explicit-action only**

During the rollback window the old Agent VM view may remain reachable through
the provider page route. Remove route-open calls to
`warmAgentVmTerminal`, `ensureAgentVmEnvironment`, `runtime.status` and terminal
poll startup. The legacy page renders the current Core snapshot first and
starts/attaches only after an explicit user action. This is a compatibility
fix, not a new generic Agent VM API.

Keep Agent VM- and legacy-local-CLI-specific code outside
`src-tauri/src/project_runtime` generic modules and `ui/projects`. E owns its
final plugin-page replacement.

- [ ] **Step 7: Verify and commit C7**

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_route_no_side_effects \
  --test project_runtime_ipc \
  --test project_root_ipc \
  --test project_runtime_local_history \
  --test legacy_local_cli_provider \
  --test launch_injection \
  --test plugin_page_no_side_effects
node --test ui/agent-vm.test.mjs
git diff --check
git add src-tauri/src/project_runtime/routes.rs \
  src-tauri/src/project_runtime/ipc.rs \
  src-tauri/src/legacy_project_runtime \
  src-tauri/tests/project_route_no_side_effects.rs \
  src-tauri/tests/project_runtime_ipc.rs \
  src-tauri/tests/project_root_ipc.rs \
  src-tauri/tests/project_runtime_local_history.rs \
  src-tauri/tests/legacy_local_cli_provider.rs \
  src-tauri/tests/launch_injection.rs \
  src-tauri/src/history.rs src-tauri/src/launch.rs \
  src-tauri/src/ipc.rs src-tauri/src/main.rs \
  ui/bridge.js ui/renderer.js ui/agent-vm.test.mjs
git commit -m "refactor(projects): make routes provider neutral"
```

---

## Mandatory C Figma checkpoint before Task C8

Task B's approved Figma file is the starting point. Before implementing C UI,
the worker must load the official Figma generation workflow, call
`search_design_system` first and reuse Jarvis components/tokens. Use the
screenshot capture plus editable-component workflow required by the Figma
skill. Record real node IDs, comparison screenshots and dated approval in the
Project Runtime section of `docs/design/plugin-platform-v2-figma.md`.

Required detailed frames/states:

1. Projects list in cards/list modes with favorites, filtering and unavailable
   roots.
2. Project Detail with multiple runtime cards and provider selector.
3. Unified Session list with provider/backend/state badges and multiple active
   sessions.
4. Session Detail with chat/results first, changed files and copyable
   attach/resume command as a secondary action.
5. New Session flow with runtime/backend selection, confirmation, accepted
   Operation progress, terminal success and failure.
6. Missing/disabled provider install/enable CTA.
7. Drift/unmanaged/quarantined Doctor/Repair states.
8. Loading, empty, watch gap/resync, offline/recovery and stale-revision states.
9. `project.header`, `project.actions`, `project.session.context` and
   `project.file.context` contribution outlets including overflow.
10. Keyboard-only, visible focus, VoiceOver names, reduced motion, light/dark
    contrast and 200% zoom.

The visual approval does not permit provider-specific fields in generic UI and
does not move final Agent VM custom pages from E into C. If B's existing nodes
already cover a state, record/reuse their exact IDs; do not duplicate them.

Gate:

```bash
test -s docs/design/plugin-platform-v2-figma.md
rg -n 'Project Runtime|Projects list|Project Detail|Session Detail|New Session|node ID|200%' \
  docs/design/plugin-platform-v2-figma.md
```

Expected: every required state maps to a real editable node and approved
comparison. A prose-only description or generated screenshot alone does not
pass.

---

### Task C8: Replace the three-source Projects list with one snapshot store

**Files:**

- Create: `ui/projects/index.js`
- Create: `ui/projects/store.js`
- Create: `ui/projects/router.js`
- Create: `ui/projects/project-list.js`
- Create: `ui/projects/project-card.js`
- Create: `ui/projects/project-root-picker.js`
- Create: `ui/projects/recovery.js`
- Create: `ui/projects/dom.js`
- Create: `ui/projects/projects.css`
- Create: `ui/projects/store.test.mjs`
- Create: `ui/projects/router.test.mjs`
- Create: `ui/projects/project-list.test.mjs`
- Create: `ui/projects/accessibility.test.mjs`
- Modify: `ui/index.html`
- Modify: `ui/renderer.js`
- Modify: `ui/bridge.js`
- Modify: `ui/agent-vm.js`
- Modify: `package.json`

- [ ] **Step 1: Add RED monotonic-store tests**

`store.test.mjs` starts from one snapshot and proves:

- only a strictly newer `snapshotRevision` applies;
- equal duplicate watches are idempotent;
- a revision gap invalidates and resyncs rather than merging;
- selected Project/Runtime/Session survives only if present in the new graph;
- provider generation drift enters recovery;
- `CatalogPreferencesView` and Catalog checkpoint apply atomically with their
  Projects or trigger resync;
- pending Operations are restored from the snapshot/subject resync after a
  fresh store with no prior browser memory;
- no call to history/entities/project-manager/terminal readers occurs;
- one immutable render selector returns Projects, Providers, Runtimes, Sessions
  Turns, ChangeSets, Catalog preferences/checkpoint and pending Operations from
  the same canonical payload.

Run:

```bash
node --test ui/projects/store.test.mjs
```

Expected RED: `ui/projects/store.js` is absent.

- [ ] **Step 2: Implement the snapshot store/router**

`ProjectRuntimeStore` owns:

```text
snapshot
snapshotRevision
watch cursor
route selection
loading/recovery/error
pending Operation refs
```

It never imports `ui/agent-vm.js` or reads global `historyData`,
`agentVmEntities`, `projectManagerState` or terminal maps. Route selection is a
pure projection over the current snapshot. Pending Operation state is derived
only from B's durable rows/cursor; no local-only pending map may survive a
render or be treated as authoritative.

- [ ] **Step 3: Add RED list and route tests**

Render hostile names/reasons as `textContent`, stable sorted favorites,
cards/list preference, filter, unavailable roots, empty/loading/offline states
and missing-provider badges. Clicking a Project changes only route/selection
and invokes no action method. Keyboard arrows/Home/End/Enter and focus
restoration follow the approved interaction.

- [ ] **Step 4: Implement the Figma-approved list**

Mount `ui/projects/index.js` from the existing renderer and delete the generic
Project list's manual state merge/render path. Use existing host tokens and
small safe DOM helpers. Private path is displayed only in trusted Core UI and
is not placed in dataset attributes, contribution context or logs.

Favorites/view mutations use Catalog expected revisions. Optimistic paint may
show pending state, but authoritative order changes only when the newer Broker
snapshot arrives.

Add/rebind Project uses the host picker flow only: request a
`DirectorySelectionHandle`, render cancel without mutation, then submit that
opaque handle plus expected revision. The module cannot read or send a path,
bookmark or cwd. Expired/consumed/changed-root responses return to the picker
without optimistic Catalog mutation.

- [ ] **Step 5: Accessibility/visual verification**

Automated tests assert landmarks, list semantics, button labels, selected
state, focus order and reduced-motion classes. Compare the live app to approved
Figma at 100% and 200% zoom in light/dark modes; record screenshots and any
resolved mismatch in the design evidence file.

- [ ] **Step 6: Verify and commit C8**

```bash
node --test ui/projects/store.test.mjs \
  ui/projects/router.test.mjs \
  ui/projects/project-list.test.mjs \
  ui/projects/accessibility.test.mjs
npm run test:ui
npm run check:plugin-contracts
git diff --check
git add ui/projects ui/index.html ui/renderer.js ui/bridge.js \
  ui/agent-vm.js package.json docs/design/plugin-platform-v2-figma.md
git commit -m "feat(projects): render canonical project catalog"
```

---

### Task C9: Build provider-neutral Project and Session detail

**Files:**

- Create: `ui/projects/project-detail.js`
- Create: `ui/projects/runtime-card.js`
- Create: `ui/projects/session-list.js`
- Create: `ui/projects/session-detail.js`
- Create: `ui/projects/turn-list.js`
- Create: `ui/projects/changed-files.js`
- Create: `ui/projects/new-session.js`
- Create: `ui/projects/operation-status.js`
- Create: `ui/projects/attach-command.js`
- Create: `ui/projects/contributions.js`
- Create: `ui/projects/project-detail.test.mjs`
- Create: `ui/projects/changed-files.test.mjs`
- Create: `ui/projects/new-session.test.mjs`
- Create: `ui/projects/operation-status.test.mjs`
- Create: `ui/projects/contributions.test.mjs`
- Create: `src-tauri/tests/project_runtime_project_outlets.rs`
- Create: `src-tauri/tests/project_runtime_ui_contract.rs`
- Modify: `ui/projects/index.js`
- Modify: `ui/projects/router.js`
- Modify: `ui/projects/projects.css`
- Modify: `ui/index.html`
- Modify: `ui/renderer.js`

- [ ] **Step 1: Add RED Project Detail tests**

Render a Project with:

- two providers and Runtimes;
- multiple Sessions/backends/states;
- many Turns including waiting/completed/failed;
- one missing provider;
- one quarantined runtime;
- a ChangeSet with opaque changed-file rows, favorites and contributions.

Assert generic code reads only Core fields, uses provider badges/links, shows
chat/results before attach, and never renders an embedded terminal. Provider
extension bytes are not inspected or interpolated. Changed-file rows expose
only safe display metadata; snapshot render never receives a path, bytes,
diff, provider key or resource handle.

- [ ] **Step 2: Attach B's exact `project.*` outlets**

Wire the already defined B10 outlets:

```text
project.header
project.actions
project.session.context
project.file.context
```

Context creation happens at click time through B and receives canonical IDs
plus newly granted opaque handles, not path/chat/file content. Handles are
single-purpose, invocation-bound and never cached in store/DOM datasets.
Disabled/missing
contributions show host-safe reasons. Opening a provider page is read-only and
does not invoke its lifecycle command.

- [ ] **Step 3: Add RED New Session/Operation tests**

Cover:

- runtime/backend selection;
- unavailable provider and root;
- stale snapshot confirmation failure;
- command risk confirmation;
- `Completed` and `Accepted(OperationRef)`;
- progress, cancellation, failure and retry;
- duplicate submit idempotency;
- watch disconnect and resync;
- page/app restart recovering the same pending Operation from B by subject;
- unauthorized/cross-subject cancellation and immutable terminal state;
- no success state before terminal Operation and resulting Session projection;
- explicit action only—route open never invokes create/start.

- [ ] **Step 4: Implement provider-neutral actions**

New Session resolves `session.create` from the selected provider receipt and
submits canonical Project/Runtime IDs, backend and opaque input/attachment
references. If the Runtime must be provisioned/started, show separate explicit
steps/confirmation; never make `session.create` silently call
`runtime.provision`.

Runtime start/stop/destroy/doctor and Session stop/cancel are host actions with
expected revisions and B risk floors. Accepted Operations remain visible
across route changes/restart.

- [ ] **Step 5: Implement Session Detail**

Show:

- current Session state/resumability/provider/backend;
- Turn timeline with user input/result summaries;
- waiting/failure recovery;
- changed-file display metadata from `ChangeSetView`;
- click-only content/diff/open/reveal actions that reauthorize current
  subject/grant/revision, mint one volatile B7 handle and consume it once;
- a secondary attach/resume button that invokes the exact typed action, then
  receives/consumes one volatile B7 handle to copy the bounded descriptor;
- provider page link for advanced UI.

Do not add xterm, terminal screen polling, input/key forwarding or direct
process spawning. Do not serialize, log, cache or persist file/attach handles
or descriptors. Route open and snapshot render mint zero handles. D/E provide
multi-session attach behavior through the typed contract and CLI.

- [ ] **Step 6: Verify UI/backend contract and commit C9**

```bash
node --test ui/projects/project-detail.test.mjs \
  ui/projects/changed-files.test.mjs \
  ui/projects/new-session.test.mjs \
  ui/projects/operation-status.test.mjs \
  ui/projects/contributions.test.mjs
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_runtime_project_outlets \
  --test project_runtime_ui_contract \
  --test project_route_no_side_effects \
  --test project_runtime_action_dispatch
npm run test:ui
git diff --check
git add ui/projects ui/index.html ui/renderer.js \
  src-tauri/tests/project_runtime_project_outlets.rs \
  src-tauri/tests/project_runtime_ui_contract.rs
git commit -m "feat(projects): add runtime and session detail"
```

---

### Task C10: Prove recovery, rollback and provider-neutral boundaries

**Files:**

- Create: `src-tauri/tests/project_runtime_crash_matrix.rs`
- Create: `src-tauri/tests/project_runtime_end_to_end.rs`
- Create: `src-tauri/tests/project_runtime_no_provider_shortcuts.rs`
- Create: `scripts/check-project-runtime-boundaries.sh`
- Create: `scripts/check-project-runtime-boundaries.test.sh`
- Modify: `src-tauri/src/project_runtime/mod.rs`
- Modify: `src-tauri/src/shutdown.rs`
- Modify: `package.json`
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Add the RED crash/recovery matrix**

Inject a process failure before/after:

1. Project/root/catalog commit;
2. Catalog outbox append;
3. Broker Core Project apply;
4. Broker acknowledgement persistence;
5. legacy import receipt phases;
6. legacy shadow projection apply;
7. provider generation handoff;
8. UI watch cursor persistence/resync;
9. clean-shutdown marker/checkpoint;
10. every EntityStore evidence-barrier transition, including snapshot
    watermark freeze and pre-allocation conflict;
11. directory selection creation/consume/revalidation immediately before
    Catalog commit;
12. runtime Operation admission commit, dispatch claim, provider
    acknowledgement, Core projection and terminal update.

After restart assert:

- Project/alias IDs are unchanged;
- no partial Project/root/preference relation exists;
- outbox replay applies once;
- snapshot graph has one revision and valid parents;
- only one legacy/provider source generation is authoritative;
- pending Operations remain pending/terminal according to B state;
- no provider dispatch exists without a prior durable Operation row and a
  restart recovers pending work by exact subject;
- terminal Operation state cannot be rewritten by late replay/cancel;
- an incomplete EntityStore source set allocates no ID and late conflicting
  evidence cannot create/remap one;
- consumed/expired/replaced directory selections produce no Catalog mutation;
- corrupt input is quarantined without deleting unrelated Projects;
- no provider lifecycle is auto-started during recovery.

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_runtime_crash_matrix
```

Expected RED: at least one injected phase leaves a duplicate/missing
projection, an unfinished handoff or an incorrect clean-shutdown marker. A
fixture/compile typo is not the intended failure.

- [ ] **Step 2: Close every crash point**

Add only the fault-injection seams and recovery orchestration needed by the
matrix. Reuse Catalog receipts/outbox, B idempotent ingress and handoff fences;
do not add a second journal. Recovery finishes before canonical route/action
admission and remains bounded/retryable when Broker/provider is unavailable.

Run the matrix again. Expected: all injected phases converge and the test exits
`0`.

- [ ] **Step 3: Add a RED enforceable boundary lint**

`check-project-runtime-boundaries.sh` scans generic surfaces:

```text
src-tauri/src/project_runtime
ui/projects
```

Exclude only the exact
`src-tauri/src/project_runtime/migration/legacy` compatibility directory from
legacy-name checks; scan it separately for forbidden control calls.

Generic paths reject:

```text
agent_vm / agent-vm / agentVm
runId or cwd-based joins
plugins_cmd / entities_get / history_get
session_launch
crate::launch / legacy_project_runtime
agent_vm_terminal_*
direct legacy EntityStore
Lima/avm/tmux/Claude/Codex process control
direct Broker SQLite handles
raw path/cwd registration or picker results
backendSessionId/transportId/PID/boot/attach provenance
durable file/attach ResourceHandle fields
```

The legacy migration directory may parse old owner/kind/field names but rejects
all control/terminal/process symbols and private RunStore paths. Shell tests
contain a safe fixture and one failing fixture for every rule with exact
diagnostics. `src-tauri/src/legacy_project_runtime` is scanned separately: it
may import hardened launch adapters and exact provider types, but may not be
imported by generic Core, accept raw cwd/path IPC, bypass B provider receipts
or dispatch without durable Operation admission. Add
`check:project-runtime-boundaries` to package scripts and CI.

Run the shell test before the checker exists:

```bash
bash scripts/check-project-runtime-boundaries.test.sh
```

Expected RED: the test reports the missing checker. After the checker exists,
each unsafe fixture must fail for its intended exact diagnostic and the safe
fixture must pass.

- [ ] **Step 4: Add RED end-to-end consistency tests and implement the lint**

Exercise:

1. legacy folders/history import;
2. Catalog Project projection;
3. provider registration;
4. multiple Runtimes/Sessions/Turns;
5. immutable complete UI/headless bytes with Catalog preferences, exact
   source/ack checkpoint, ChangeSets and pending Operations;
6. old FNV/run deep-link resolution;
7. route open with zero dispatch;
8. explicit session action whose durable Operation exists before dispatch,
   survives restart and reaches immutable terminal state;
9. provider disable/update generation and watch resync;
10. unavailable-root rebind preserving Project ID;
11. rollback UI flag then re-enable without duplicate IDs;
12. click-only changed-file and attach handles with expiry/revoke/one-use
    exhaustion;
13. host picker cancellation/symlink swap and launch-injection rejection.

The test compares canonical serialized UI/headless payload bytes and exact
`snapshotRevision`.

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_runtime_end_to_end \
  --test project_runtime_no_provider_shortcuts
```

Expected RED: current generic paths still expose at least one legacy merge or
route shortcut. Implement the boundary checker, wire it to CI, remove/fence the
reported shortcut and rerun until both Rust tests and shell fixtures pass.

- [ ] **Step 5: Run full automated verification**

```bash
bash scripts/check-project-runtime-boundaries.test.sh
bash scripts/check-project-runtime-boundaries.sh
bash scripts/check-plugin-platform-boundaries.sh
cargo test --manifest-path crates/jarvis-plugin-protocol/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test project_runtime_crash_matrix \
  --test project_runtime_end_to_end \
  --test project_runtime_no_provider_shortcuts \
  --test project_route_no_side_effects \
  --test project_runtime_surface_consistency \
  --test project_runtime_operation_recovery \
  --test project_runtime_operation_watch \
  --test project_runtime_operation_cancel \
  --test launch_injection \
  --test broker_projection_consistency \
  --test plugin_platform_snapshot
npm run test:ui
npm run check:plugin-contracts
npm run check:public
git diff --check
```

Expected: all commands exit `0`; no route provider/Operation/resource-handle/
terminal counter increments; generated contracts are clean; no
legacy/provider symbol appears in generic paths.

- [ ] **Step 6: Commit C10**

```bash
git add src-tauri/tests/project_runtime_crash_matrix.rs \
  src-tauri/tests/project_runtime_end_to_end.rs \
  src-tauri/tests/project_runtime_no_provider_shortcuts.rs \
  scripts/check-project-runtime-boundaries.sh \
  scripts/check-project-runtime-boundaries.test.sh \
  src-tauri/src/project_runtime/mod.rs src-tauri/src/shutdown.rs \
  package.json .github/workflows/ci.yml
git commit -m "test(projects): prove recovery and neutral boundaries"
```

---

### Task C11: Independent review, live validation and handoff to D/E

**Files:**

- Create: `docs/audits/project-runtime-core-architecture-review.md`
- Create: `docs/audits/project-runtime-core-migration-review.md`
- Create: `docs/audits/project-runtime-core-ui-review.md`
- Create: `docs/audits/project-runtime-core-security-review.md`
- Create: `docs/audits/project-runtime-core-live-smoke.md`
- Create: `docs/project-runtime/core-contract.md`
- Create: `docs/project-runtime/migration-and-rollback.md`
- Create: `scripts/capture-agent-vm-inventory.sh`
- Create: `scripts/check-agent-vm-inventory-leaks.sh`
- Create: `scripts/check-agent-vm-inventory-leaks.test.sh`
- Modify: `docs/plugins/manifest.md`
- Modify: `docs/design/plugin-platform-v2-figma.md`
- Modify: `docs/superpowers/plans/2026-07-31-plugin-platform-agent-vm-v2.md`

- [ ] **Step 1: Run four independent reviews**

Use separate reviewers with no shared conclusion:

1. **Architecture/state reviewer:** ownership, state graphs, one revision,
   provider generation, public/private provenance split, B durable Operation
   dependency and no duplicate B mechanics.
2. **Migration/data reviewer:** stable identity, unavailable/moved roots,
   host picker/TOCTOU, complete EntityStore evidence barrier, FNV/path/run
   aliases, idempotency, late-conflict quarantine, rollback and E handoff.
3. **Security reviewer:** path privacy, extension validation, typed command/Gate
   use, launch injection cases, click-only file/attach handles, untrusted deep
   links, inventory ownership-safe cleanup, boundary lint and zero route side
   effects.
4. **UI/accessibility reviewer:** Figma parity, Projects/Agent VM visual
   consistency, multiple Runtime/Session states, keyboard/VoiceOver/zoom,
   preferences/checkpoint parity, restart-recovered Operation progress,
   chat/results-first and no embedded-terminal primary path.

Each report records commit reviewed, commands/evidence, severity, exact file
locations and verdict. Resolve all critical/high findings and rerun the
relevant reviewer. Medium findings must be fixed or accepted with explicit
owner/reason before merge.

- [ ] **Step 2: Perform live trusted-Core smoke**

Use a separate Jarvis dev profile and non-sensitive fixture Projects:

1. before starting, capture normalized `avm list`, `limactl list --json` and
   relevant Agent VM/Lima process inventory plus VM states into the audit
   artifact. Record an ownership ledger; only a uniquely named VM created by
   this exact smoke receipt/profile may be marked `test-owned`;
2. import existing folder/history/favorite data and record IDs;
3. open every Project route and prove provider/Operation/resource-handle/
   terminal counters stay zero;
4. rename one fixture root, mark it unavailable, then rebind and verify the
   same Project ID;
5. show multiple runtime/session/Turn fixture projections;
6. open old FNV and run notification targets and verify canonical routes;
7. disable a fixture provider and verify install/enable CTA;
8. force watch disconnect/gap and verify one full resync;
9. restart Jarvis uncleanly and verify Catalog/outbox recovery;
10. compare complete UI snapshot JSON bytes with headless adapter JSON,
   Catalog preferences/checkpoint, pending Operations and both cursors;
11. cancel a root picker and attempt a replaced/symlink-swapped fixture with no
    Catalog revision; exercise all invalid launch resume-ID classes with no
    process spawn;
12. restart with one admitted pending fixture Operation, query it by subject,
    force a cursor gap/resync, cancel under current authorization and prove its
    terminal state is immutable;
13. click changed-file/attach actions and prove handles are absent before click,
    one-use, expiring and revoked with their grant;
14. compare light/dark 100%/200% screens to approved Figma nodes;
15. on every exit path, clean up only inventory entries present in the
    test-owned ledger after revalidating their exact name/identity/receipt.
    Capture the same three inventories again and require exact pre/post VM set
    and state equality, no test-owned VM still running, no orphaned test-owned
    process and no extra VM.

Do not create/start/stop a real Agent VM as part of C route validation.
Disposable live Agent VM controller/provider smoke belongs to D/E/G. If a
running legacy VM is observed, the C smoke is read-only. The planning-time live
audit observed managed `sup`/`t-bank` entries stopped and an unrelated Colima
VM; recapture instead of assuming that state, and never stop/delete/reconfigure
Colima or any unledgered user VM/process. Inventory drift fails the smoke for
manual investigation; the cleanup trap does not “repair” unrelated inventory.

`check-agent-vm-inventory-leaks.test.sh` uses fixtures to prove equal inventory
passes, added/missing/state-changed VM fails, an orphan test-owned process
fails, an unledgered VM is never selected for cleanup, and unchanged unrelated
Colima is preserved.

- [ ] **Step 3: Document the public handoff**

`core-contract.md` documents schemas, state graphs, provider roles, snapshot
revision and explicit action semantics. `migration-and-rollback.md` documents
source precedence, aliases, shadow states, receipts, feature flag, rollback and
E handoff watermark.

For D:

- implement controller/CLI against `ProjectRuntimeQuery` and exact action
  contracts;
- prove CLI output revision equals the Core snapshot;
- do not add a controller-only state vocabulary.

For E:

- publish Agent VM Core envelopes through B outbox;
- register exact provider commands/pages/actions;
- acquire handoff receipt before fencing the legacy projector;
- remove old Agent VM Core rendering/IPC only after telemetry and rollback
  gates.

For F:

- consume canonical transitions for durable notification receipts;
- add memory/mount/credential/resource contracts without extending Core
  provider fields.

- [ ] **Step 4: Run the final Increment C gate**

```bash
git diff --check origin/master...HEAD
npm run check:project-runtime-boundaries
npm run check:plugin-platform-boundaries
npm run check:plugin-contracts
npm run test:ui
npm run check:public
bash scripts/check-agent-vm-inventory-leaks.test.sh
jarvis_vm_audit_dir="$(mktemp -d)"
bash scripts/capture-agent-vm-inventory.sh \
  --out "$jarvis_vm_audit_dir/before.json" \
  --init-ownership-ledger "$jarvis_vm_audit_dir/test-owned.json"
cargo test --manifest-path crates/jarvis-plugin-protocol/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features
cargo +1.77.2 test --locked \
  --manifest-path crates/jarvis-plugin-protocol/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --no-default-features \
  --all-targets -- -D warnings
cargo build --release --manifest-path src-tauri/Cargo.toml \
  --features wakeword-ort,whisper-native,stt-vad --bin jarvis
bash scripts/capture-agent-vm-inventory.sh \
  --out "$jarvis_vm_audit_dir/after.json"
bash scripts/check-agent-vm-inventory-leaks.sh \
  --before "$jarvis_vm_audit_dir/before.json" \
  --after "$jarvis_vm_audit_dir/after.json" \
  --ownership-ledger "$jarvis_vm_audit_dir/test-owned.json"
```

Expected: every command exits `0`; generated files produce no diff; all four
review verdicts are approve; live smoke has exact evidence; no Agent VM
provider mutation was needed to prove C.

- [ ] **Step 5: Commit docs/evidence and update the master checklist**

```bash
git add docs/audits docs/project-runtime docs/plugins/manifest.md \
  docs/design/plugin-platform-v2-figma.md \
  scripts/capture-agent-vm-inventory.sh \
  scripts/check-agent-vm-inventory-leaks.sh \
  scripts/check-agent-vm-inventory-leaks.test.sh \
  docs/superpowers/plans/2026-07-31-plugin-platform-agent-vm-v2.md
git commit -m "docs(projects): certify runtime core increment"
```

---

## Increment C merge checklist

- [ ] A2 and all B dependency gates are committed and green before C code.
- [ ] Core IDs are opaque, stable across rename/unavailability and never
  derived from cwd/basename/provider.
- [ ] FNV/path/run IDs exist only as private compatibility aliases.
- [ ] Project Catalog mutations and outbox rows are one transaction.
- [ ] Host picker returns only an fd-bound, one-time directory handle; raw
  path/cwd registration, cancellation mutation and symlink/inode TOCTOU are
  rejected.
- [ ] Every Catalog change set includes complete preferences and persists one
  exact `catalogSourceRevision -> acknowledgedBrokerRevision` mapping.
- [ ] Project, Runtime, Session and Turn readers use one Broker snapshot
  revision.
- [ ] UI and headless/CLI port serialize byte-identical complete snapshots,
  preferences/checkpoint, pending Operations and cursors.
- [ ] Provider projections are exact-schema, exact-receipt and
  generation/state validated.
- [ ] Session public Views contain no process/attach/resume provenance;
  host-owned fields are rejected from observations and only digests exist in
  adapter-private state.
- [ ] ChangeSet/ChangedFile Views contain only opaque IDs and display metadata;
  file/attach handles are click-only, volatile and never durable.
- [ ] A Project can have many Runtimes; a Runtime many Sessions; a Session many
  Turns.
- [ ] Generic routes and page opening create zero
  provider/Operation/resource-handle/terminal side effects.
- [ ] Every lifecycle/session mutation is an explicit exact typed command and
  durable B Operation committed before dispatch, recoverable/queryable by
  subject with cursor gap/resync, authorized cancel and immutable terminal.
- [ ] Generic Projects UI imports no Agent VM/provider-private module and
  inspects no extension data.
- [ ] The old three-source history/entities/settings merge is absent from the
  generic Projects path.
- [ ] Session UI is chat/results first; attach/resume is secondary and
  copyable; no embedded terminal is primary.
- [ ] `project.*` contribution outlets use minimized canonical context and
  opaque handles.
- [ ] Legacy projector is read-only, fenced, receipt-backed and removable by E.
- [ ] EntityStore participates in the frozen pre-allocation evidence barrier;
  unavailable/late/conflicting generations cannot finalize, remap or duplicate
  a Project.
- [ ] Legacy local CLI is an exact provider outside generic Core; hardened
  launch rejects unknown agents and newline/`$()`/quotes/path/invalid IDs.
- [ ] Active legacy runs are shadowed as unmanaged, not falsely adopted.
- [ ] C does not claim controller/CLI, Agent VM package migration,
  memory/mounts or durable notifications.
- [ ] Crash, rollback, alias conflict, watch gap and provider generation
  recovery are proven.
- [ ] Live before/after `avm`/Lima/process inventory is exact-set/state equal;
  no test-owned VM/process leaks and no unrelated Colima/user VM was touched.
- [ ] Rust 1.77.2 is claimed/tested only for public/pure Core crates; Tauri host
  gates use current stable unless the complete dependency graph is pinned.
- [ ] Approved Figma nodes and keyboard/VoiceOver/200% evidence are recorded.
- [ ] Architecture, migration, security and UI reviews have no unresolved
  high/critical findings.
