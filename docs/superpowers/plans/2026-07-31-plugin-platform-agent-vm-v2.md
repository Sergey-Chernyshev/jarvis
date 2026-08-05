# Plugin Platform v2 and Agent VM v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the approved installable plugin platform, migrate Agent VM onto it without losing data or isolation, and make every Jarvis-owned macOS sleep override fail-safe.

**Architecture:** Delivery is split into independently shippable vertical increments. Core owns package trust, plugin UI isolation, Broker contracts and provider-neutral Project state; the Agent VM plugin owns its launchd controller, guest supervisor and provider adapter behind those public APIs. Power cleanup is deliberately independent and ships first.

**Tech Stack:** Rust 2021, Tauri 2/WebKit child WebViews, Tokio, SQLite/WAL, JSON Schema, Node test runner, macOS IOKit/launchd/privileged helper, Lima and pinned `MikD1/agent-vm`.

---

## Delivery map

| Order | Detailed plan                                                               | Independently testable result                                                                                                                              |
| ----- | --------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0     | `docs/superpowers/plans/2026-07-31-host-power-exit-safety.md`               | GUI, headless and SIGTERM shutdown release keep-awake/clamshell; crash recovery has a renewable watchdog lease                                             |
| A     | `docs/superpowers/plans/2026-08-01-plugin-package-contracts.md`             | Manifest v2, deterministic package, signed catalog, receipts, Developer Mode and install/update/rollback CLI                                               |
| B     | `docs/superpowers/plans/2026-08-01-plugin-ui-and-broker.md`                 | Isolated multi-page child WebViews, Bridge v1, extension points, typed settings, durable Data Broker and trusted-Core per-row projection receipts          |
| C     | `docs/superpowers/plans/2026-08-01-project-runtime-core.md`                 | Provider-neutral Project/CatalogPreferences/Runtime/Session/Turn/ChangeSet schemas, mixed-source-safe snapshots, generic Projects UI and legacy ID aliases |
| D     | `docs/superpowers/plans/2026-08-01-agent-vm-controller-cli.md`              | launchd controller, private protocol, guest supervisor/journal, multi-session CLI and safe lifecycle                                                       |
| E     | `docs/superpowers/plans/2026-08-01-agent-vm-plugin-migration.md`            | Independently installed Agent VM pages/actions, pinned provider, current-data importer and bundled-install removal                                         |
| F     | `docs/superpowers/plans/2026-08-01-agent-vm-memory-mounts-notifications.md` | Per-Turn memory snapshots, initial multi-mount grants, credential leases, resource budgets and durable notification dedupe                                 |
| G     | `docs/superpowers/plans/2026-08-01-plugin-platform-release.md`              | Migration rehearsal, negative security suites, live macOS smoke, docs and release/rollback evidence                                                        |

Each detailed plan is written and self-reviewed immediately before its code
increment starts. A later increment may depend only on a committed public
contract from an earlier row, never on an internal module.

## Cross-increment invariants

- [ ] `plugins/agent-vm` stays in this monorepo, but its package/version/install receipt are independent from the Jarvis app.
- [ ] Clean Jarvis neither bundles nor launches Agent VM after migration.
- [ ] Sandboxed plugin UI has no main DOM or Tauri global and reaches Core only through Bridge v1.
- [ ] Verified native code is approved per exact digest and is described as trusted user-level code, not an OS sandbox.
- [ ] Shared entities/events/commands flow through Core Broker with subject- and field-scoped grants.
- [ ] Increment B is committed and green before C code: B alone allocates
      `brokerRevision` and atomically stores query-invisible host receipts for every
      trusted-Core row it projects.
- [ ] `CatalogProjectionReceipt` is host-only Broker evidence, never a public
      plugin contract/schema/TypeScript export.
- [ ] Every Catalog-derived Project row validates against its own immutable
      receipt; the snapshot checkpoint identifies the latest applicable
      CatalogPreferences receipt. A preferences-only or single-Project mutation
      never requires untouched Projects to claim the latest source revision.
- [ ] Core Project UI and the headless/CLI port serialize one byte-identical
      Broker snapshot and read only core-owned Project/CatalogPreferences/Runtime/
      Session/Turn/ChangeSet envelopes.
- [ ] Agent VM has exactly one fenced controller writer per profile and one supervisor-owned PTY per Session.
- [ ] Normal Jarvis close does not stop Agent VM sessions; plugin disable/uninstall does stop live mounts and scrubs provisioned credentials before succeeding.
- [ ] Upstream artifacts are pinned by tag, commit and SHA-256; `latest` and `avm --version=dev` are never trusted as provenance.
- [ ] Existing `runId`, path-hash project IDs, records, settings, credentials precedence and VM disks have explicit migration receipts and rollback windows.
- [ ] A successful Jarvis shutdown leaves no Jarvis-owned IOKit assertion, caffeinate process or `disablesleep` mutation.

## Merge and review gates

- [ ] Every increment lands as one or more focused commits on `agent/agent-vm-integration`.
- [ ] `origin/master` is merged before an increment starts; user work is never reset or overwritten.
- [ ] Each increment has a failing-test-first commit or clearly records why a live-only macOS test cannot fail in CI.
- [ ] Security, UI/API and runtime reviewers examine their own boundaries after implementation, not only the final aggregate diff.
- [ ] PR #70 body is updated after each shippable increment with commands and exact pass counts.
- [ ] No merge occurs until required CI is green, the branch is up to date, local release build succeeds and live macOS smoke evidence is attached.

## Final verification commands

Run from the repository root:

```bash
git diff --check origin/master...HEAD
npm test
npm run test:ui
npm run check:public
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --features wakeword-ort,whisper-native,stt-vad -- -D warnings
cargo build --release --manifest-path src-tauri/Cargo.toml --features wakeword-ort,whisper-native,stt-vad --bin jarvis
```

Expected: every command exits `0`; ignored live Agent VM tests are listed and
then executed separately against a disposable managed VM during Increment G.
