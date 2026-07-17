# Jarvis System Hardening and Onboarding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove summary mode from the normal chat, build a readiness-based onboarding, and harden the live Claude/Codex/tmux/runtime integration.

**Architecture:** Rust owns readiness and the single install-job snapshot; the onboarding webview is a pure finite-state renderer. Core integration and optional models are separate plans. Session chat remains one transcript surface.

**Tech Stack:** Rust 2021, Tauri 2, vanilla HTML/CSS/JS, Node `node:test`, tmux 3.x, macOS.

---

### Task 1: Reconcile branches and remove chat summary mode

**Files:** `ui/index.html`, `ui/renderer.js`, plus the files introduced by `1a0b2c7` through its revert.

- [ ] Merge local `master` into the current branch without touching the external dirty files.
- [ ] Revert `1a0b2c7` so turn-summary generation is not part of the ordinary chat.
- [ ] Add a UI contract test asserting that `chatModeSeg`, `chatSummary`, `sumToggle`, and the placeholder copy are absent.
- [ ] Remove legacy summary markup, CSS, state, and listeners.
- [ ] Run the contract test and `node --check ui/renderer.js`.

### Task 2: Split core integration from optional models

**Files:** `src-tauri/src/install/mod.rs`, `src-tauri/src/onboarding.rs`.

- [ ] Add failing tests proving core install plans contain hooks/transport only and no Silero/Qwen download.
- [ ] Extract `install_core(progress)` from `install()`; keep CLI compatibility explicitly.
- [ ] Make `onboarding_run` call core only and persist proxy through the canonical `service.proxy` setting.
- [ ] Return the post-install readiness snapshot and never infer success from thread completion.
- [ ] Run focused install/onboarding tests.

### Task 3: Add readiness and a singleton install job

**Files:** `src-tauri/src/onboarding.rs`, `src-tauri/src/main.rs`, `ui/bridge.js`.

- [ ] Write tests for `idle -> running -> done/failed`, duplicate start, step replacement, and reopen snapshot.
- [ ] Add serializable `ReadinessSnapshot`, `ReadinessItem`, and `InstallJobSnapshot` pure builders.
- [ ] Add commands `onboarding_get`, `onboarding_run`, and job-aware `models_install` behavior.
- [ ] Broadcast model progress to both `main` and `onboarding` while retaining the latest state in Rust.
- [ ] Wire commands and run focused tests.

### Task 4: Build the onboarding state module and visual shell

**Files:** create `ui/onboarding-state.js`, create `ui/onboarding-state.test.mjs`, modify `ui/onboarding.html`, `ui/onboarding.js`, `package.json`, `.github/workflows/ci.yml`.

- [ ] Write failing Node tests for state derivation: missing core, ready core, running job, partial optional install, error, and empty selection.
- [ ] Implement the pure state reducer/export in UMD form usable by both browser and Node.
- [ ] Replace the current one-page installer with welcome/agents/capabilities/verify/ready screens and a sticky footer.
- [ ] Add ARIA labels/live regions, keyboard activation, visible focus, responsive overflow, and reduced-motion CSS.
- [ ] Add `test:ui` and run it in CI before Rust tests.

### Task 5: Align Claude and Codex hook contracts

**Files:** `src-tauri/src/install/mod.rs`, `src-tauri/src/daemon.rs`, `src-tauri/src/model.rs`, preserve the external `bin/jarvis-hook` diff.

- [ ] Add tests that Claude event registration includes SubagentStart/SubagentStop and all configured events are supported by the current contract.
- [ ] Add tests that only actionable notification types produce Waiting.
- [ ] Add tests that Codex Stop prefers `last_assistant_message` over rollout fallback.
- [ ] Implement the event/filter/final-reply changes.
- [ ] Stop automatically baking `--dangerously-bypass-hook-trust`; expose trust as readiness instead.

### Task 6: Remove tmux races and secret-bearing argv

**Files:** `bin/agent-shim`, `bin/jarvis-tmux.conf`, `src-tauri/src/install/mod.rs`, `src-tauri/src/tmux.rs`.

- [ ] Add failing tests for unique buffer names, `paste-buffer -d`, `-L jarvis` focus, and absence of proxy interpolation in the tmux shell command.
- [ ] Use a process-wide atomic nonce for per-send buffer names.
- [ ] Configure `update-environment` for supported network variables and launch the real agent via direct tmux argv.
- [ ] Add `-L jarvis` to focus/select commands.
- [ ] Run fake-agent PTY smoke with and without proxy and confirm the proxy does not appear in `ps` argv.

### Task 7: Fix actionable quality findings

**Files:** targeted Rust files reported by Clippy; do not reformat unrelated files.

- [ ] Replace Rust APIs newer than declared MSRV 1.77.2 (`is_none_or`).
- [ ] Remove newly exposed unused imports/variables only where ownership is clear; do not alter external STT work.
- [ ] Run Clippy with release features and record remaining intentional warnings in the audit.
- [ ] Run all JavaScript syntax checks.

### Task 8: Preview and live verification

**Files:** `output/playwright/*` test artifacts only; no source changes unless a defect is reproduced.

- [ ] Run Playwright at 480×600 for all onboarding states; verify no clipping.
- [ ] Verify keyboard-only navigation and reduced-motion mode.
- [ ] Run full Rust tests with release features.
- [ ] Reinstall dev integration, rebuild/sign/start Jarvis, then verify process, socket, status, hooks, sidecars, and sanitized logs.
- [ ] Confirm the installed hook/shims match the audited source modulo baked profile data.

### Task 9: Publish the audit artifact

**Files:** create `docs/audits/2026-07-10-jarvis-system-audit.md`, update `README.md` and `README.ru.md` only for changed user-visible behavior.

- [ ] Document architecture, commands/evidence, fixed findings, remaining risks, business gaps, and ranked roadmap.
- [ ] Include the non-commercial default model, notarization, profile drift, resource usage, supportability, and hook-version risks.
- [ ] Re-read the design and verify every requirement against code/tests/live evidence.

