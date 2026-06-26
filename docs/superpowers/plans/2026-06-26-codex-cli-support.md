# Codex CLI Support — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Monitor & control interactive Codex CLI (TUI) sessions in Jarvis exactly like Claude Code sessions, via one `Agent` enum + `Backend` seam, with no feature duplication.

**Architecture:** A new `backend/` module holds an `enum Agent { Claude, Codex }`, a sync dyn-safe `trait Backend` (pure data/format) dispatched by `fn backend(Agent) -> &'static dyn Backend`, plus free functions `match agent` for async/stateful work (control, usage, service-LLM, agent-host). Everything already agent-neutral (registry keyed by bare `session_id`, `ChatItem`, `AgentEvent`, tmux transport, voice) stays shared. Claude behavior must stay byte-for-byte identical at every increment.

**Tech Stack:** Rust (tokio, serde_json, axum, tauri 2), POSIX sh (shim/hook), vanilla JS panel UI. Tests via `cargo test --manifest-path src-tauri/Cargo.toml`.

**Spec:** `docs/superpowers/specs/2026-06-26-codex-cli-support-design.md` (read it for rationale & the empirical Codex facts).

**Conventions:** comments/strings in Russian to match the repo. Build check: `cargo build --manifest-path src-tauri/Cargo.toml --bin jarvis`. Commit after each task. Do NOT stage pre-existing WIP (`bin/claude-shim`, `src-tauri/src/stt/hub.rs`, `src-tauri/src/wakeword/*`, `README.md`, `bin/stt-server.py`) — stage only files this plan touches.

---

## File Structure

**New:**
- `src-tauri/src/backend/mod.rs` — `Agent` enum, `Backend` trait, `backend()` dispatcher, `ClaudeBackend`.
- `src-tauri/src/backend/codex.rs` — `CodexBackend` (data/format methods).
- `src-tauri/src/backend/codex_transcript.rs` — Codex rollout JSONL → `Vec<Value>`/`ChatItem`.
- `src-tauri/src/backend/codex_agent.rs` — `CodexCliHost` + `parse_codex_line` + per-item kill.
- `bin/agent-shim` — generalized shim (basename dispatch), supersedes `bin/claude-shim`.

**Modified (by increment):** `model.rs`, `util.rs`, `daemon.rs`, `transcript.rs`, `tail.rs`, `ipc.rs`, `usage.rs`, `limits.rs`, `history.rs`, `commands_catalog.rs`, `claude_bin.rs`, `agent/mod.rs`, `wakeword/action.rs`, `install/mod.rs`, `main.rs`, `ui/renderer.js`, `ui/index.html`, `ui/onboarding.js`, `ui/onboarding.html`.

---

## Increment 0 — Foundation: `Agent` enum + `Backend` trait + `ClaudeBackend`

Pure scaffolding; zero behavior change. Claude routed through `backend(Agent::Claude)` which delegates to the existing functions.

### Task 0.1: `Agent` enum + module skeleton

**Files:** Create `src-tauri/src/backend/mod.rs`; Modify `src-tauri/src/main.rs` (add `mod backend;`).

- [ ] **Step 1 — test** (in `backend/mod.rs` `#[cfg(test)]`):
```rust
#[test]
fn agent_from_label_defaults_to_claude() {
    assert_eq!(Agent::from_label("codex"), Agent::Codex);
    assert_eq!(Agent::from_label("claude"), Agent::Claude);
    assert_eq!(Agent::from_label("anything-else"), Agent::Claude);
    assert_eq!(Agent::Codex.label(), "codex");
}
```
- [ ] **Step 2 — impl:**
```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Agent { #[default] Claude, Codex }

impl Agent {
    pub fn from_label(s: &str) -> Agent {
        if s.eq_ignore_ascii_case("codex") { Agent::Codex } else { Agent::Claude }
    }
    pub fn from_opt(s: Option<&str>) -> Agent { s.map(Agent::from_label).unwrap_or(Agent::Claude) }
    pub fn label(self) -> &'static str { match self { Agent::Claude => "claude", Agent::Codex => "codex" } }
    pub fn all() -> [Agent; 2] { [Agent::Claude, Agent::Codex] }
}
```
- [ ] **Step 3 — wire:** add `mod backend;` to `main.rs` near the other `mod` decls; add `pub use backend::Agent;` if convenient.
- [ ] **Step 4 — verify:** `cargo test --manifest-path src-tauri/Cargo.toml backend::`  → PASS; `cargo build` → OK.
- [ ] **Step 5 — commit:** `git add src-tauri/src/backend/mod.rs src-tauri/src/main.rs && git commit -m "feat(codex): Agent enum + backend module skeleton"`

### Task 0.2: `Backend` trait + dispatcher + `ClaudeBackend`

**Files:** Modify `backend/mod.rs`.

- [ ] **Step 1 — define the sync trait** (signatures from spec §3.2; import `crate::transcript::ChatItem`, `serde_json::Value`, `std::path::{Path,PathBuf}`):
```rust
pub trait Backend: Send + Sync {
    fn agent(&self) -> Agent;
    fn hook_events(&self) -> &'static [(&'static str, &'static str)];
    fn hooks_path(&self) -> PathBuf;
    fn cli_found(&self) -> bool;
    fn shim_passthrough(&self, argv1: Option<&str>) -> bool;
    fn read_entries(&self, file: &Path, max_bytes: u64) -> Vec<Value>;
    fn to_chat_items(&self, entry: &Value) -> Vec<ChatItem>;
    fn extract_title(&self, entries: &[Value]) -> Option<String>;
    fn extract_branch(&self, entries: &[Value]) -> Option<String>;
    fn extract_model(&self, entries: &[Value]) -> Option<String>;
    fn transcript_dir_for(&self, cwd: &str) -> Option<PathBuf>;
    fn resume_cmd(&self, sid: &str) -> String;
    fn friendly_model(&self, id: &str) -> String;
    fn models(&self) -> &'static [(&'static str, &'static str)];
    fn effort_levels(&self) -> &'static [&'static str];
    fn has_separate_effort(&self) -> bool;
    fn price(&self, model: &str) -> (f64, f64);
}

pub struct ClaudeBackend;
pub struct CodexBackend; // impl lands in codex.rs (Increment 1+)

pub fn backend(a: Agent) -> &'static dyn Backend {
    match a { Agent::Claude => &ClaudeBackend, Agent::Codex => &codex::CODEX }
}
```
- [ ] **Step 2 — implement `ClaudeBackend` by delegating to existing code** (NO logic moves yet — call the current functions so behavior is identical):
  - `hook_events` → the existing `install::EVENTS` (re-export or duplicate the 8-row table; keep `install::EVENTS` as source of truth and reference it).
  - `hooks_path` → `crate::util::home_dir().join(".claude/settings.json")`.
  - `cli_found` → `crate::claude_bin::resolve_claude_bin().is_some()`.
  - `read_entries` → `crate::transcript::chain_from_entries(crate::transcript::read_recent_entries(file, max_bytes))`.
  - `to_chat_items` → `crate::transcript::to_chat_items(entry)`.
  - `extract_*`/`transcript_dir_for` → the existing `daemon`/`transcript` helpers (e.g. `transcript::project_dir_for`, `transcript::read_model_from_project`); where a helper is currently private in `daemon.rs` (title/branch mining at `daemon.rs:963-1008`), leave `ClaudeBackend` methods as thin wrappers that the daemon will call into in Increment 3 — for now return `None`/delegate as available without changing daemon behavior.
  - `resume_cmd` → `format!("claude --resume {sid}")`.
  - `friendly_model` → `crate::util::friendly_model(id)`.
  - `models` → `&[("fable","Fable"),("opus","Opus"),("sonnet","Sonnet"),("haiku","Haiku")]`.
  - `effort_levels` → `&["low","medium","high","xhigh","max"]`.
  - `has_separate_effort` → `true`.
  - `price` → call the existing private `usage::price`; expose it as `pub(crate)` (mechanical visibility change, behavior identical).
- [ ] **Step 3 — codex stub:** in `codex.rs` add `pub static CODEX: CodexBackend = CodexBackend;` and a `todo!()`-free minimal impl returning Claude-safe defaults for now (so it compiles); real Codex impls land in later increments. Mark with `// инкремент N` comments.
- [ ] **Step 4 — verify:** `cargo build` OK; existing tests green (`cargo test`); NO call sites changed yet, so Claude is untouched.
- [ ] **Step 5 — commit:** `feat(codex): Backend trait + ClaudeBackend delegating to existing code`

---

## Increment 1 — Provisioning: installer + `agent-shim` (the core "wrap codex" step)

Delivers: interactive `codex` sessions appear in the panel labelled `codex`. Fixes the hand-written hooks.json label bug by having Jarvis own that file.

### Task 1.1: per-agent EVENTS + Codex event table

**Files:** Modify `src-tauri/src/install/mod.rs` (EVENTS at ~`:38-47`).

- [ ] Read `install/mod.rs:1-60` and `:980-1010` first (exact current code).
- [ ] Keep Claude `EVENTS` as-is. Add `pub const CODEX_EVENTS: &[(&str,&str)]` mapping Codex hook names → internal args:
```rust
pub const CODEX_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt"),
    ("PreToolUse", "pre-tool"),
    ("PostToolUse", "post-tool"),
    ("Stop", "stop"),
    ("PermissionRequest", "permission"),
    ("SubagentStart", "subagent-start"),
    ("SubagentStop", "subagent-stop"),
];
```
- [ ] Wire `ClaudeBackend::hook_events()`→`EVENTS`, `CodexBackend::hook_events()`→`CODEX_EVENTS`.
- [ ] **Test:** `backend(Agent::Codex).hook_events().len() == 8` and contains `("Stop","stop")`. Commit.

### Task 1.2: Codex `~/.codex/hooks.json` writer + uninstall + status

**Files:** Modify `install/mod.rs`; add `util::codex_dir()` in `util.rs`.

- [ ] Add `pub fn codex_dir() -> PathBuf { home_dir().join(".codex") }` to `util.rs` (mirror `claude_dir`).
- [ ] Generalize the hook-registration writer. Current Claude path merges into `~/.claude/settings.json` under `hooks[Event]` (literal at `install/mod.rs:~1003`: `format!("{} claude {arg}", hook_dst()...)`). Refactor to `install_hooks_for(agent, hook_bin)`:
  - target file = `backend(agent).hooks_path()`,
  - per `backend(agent).hook_events()` push group `{"hooks":[{"type":"command","command": format!("{} {} {}", hook_bin, agent.label(), arg), "timeout":5}]}` if `!event_installed(...)`,
  - reuse `read_settings`/`backup`/`atomic_write`/`is_ours` unchanged (MARKER `bin/jarvis-hook` is label-agnostic).
  - Codex file shape is identical: top-level `{"hooks":{Event:[...]}}`.
- [ ] In `install()` call `install_hooks_for(Agent::Claude, …)` always, and `install_hooks_for(Agent::Codex, …)` **iff** `backend(Agent::Codex).cli_found()`.
- [ ] Mirror in `uninstall()` and `status()`.
- [ ] **Test:** a unit test that, given a temp HOME, `install_hooks_for(Codex,…)` writes `~/.codex/hooks.json` with `command` containing `"jarvis-hook codex stop"` and is idempotent. Build, commit.

### Task 1.3: `bin/agent-shim` (generalized, basename dispatch)

**Files:** Create `bin/agent-shim` (copy current `bin/claude-shim` content, then generalize); Modify `install/mod.rs` shim install (`:22` `include_str!`, `:183` `shim_dst`, `:883-912`).

- [ ] Create `bin/agent-shim` from the CURRENT `bin/claude-shim` (preserves the NET_VARS-sync WIP), then change:
  - `BIN_NAME=$(basename -- "$0")` ; resolve real via `command -v "$BIN_NAME"` (replace literal `claude` at lines ~29/31).
  - passthrough by `$1`: claude → `-p|--print|auth|setup-token`; codex → `exec|e|login|logout|mcp|mcp-server|app-server|completion|doctor|resume|fork|apply|review|cloud|features|update|sandbox`.
  - if `BIN_NAME=codex` and wrapping: append `--dangerously-bypass-hook-trust` **only if** (a) `codex --help` contains the flag AND (b) our hook isn't already trusted (grep `~/.codex/config.toml` `[hooks.state]` for our hooks.json path). Insert before the user prompt args.
  - per-agent NET_VARS: codex omits `ANTHROPIC_BASE_URL`.
- [ ] Installer: `SHIM_SRC = include_str!("../../../bin/agent-shim")`; install to `shims/claude` always and `shims/codex` iff `codex_found()`. Update the test at `install/mod.rs:~1391`.
- [ ] **Test:** shell-level test (or Rust test invoking sh) that `argv0=codex` resolves codex passthrough for `exec` and wraps for a bare prompt. Build, commit.

### Task 1.4: onboarding phase + integration card (UI)

**Files:** Modify `ui/onboarding.js` (`PHASES` ~`:11`), `install/mod.rs` (emit a "Codex" progress phase iff codex_found), `ui/renderer.js` (`renderIntegrationCard` ~`:2639` parameterized per backend).
- [ ] Add `{ key:"Codex", name:"Codex CLI", desc:"Хуки/шим codex" }` to PHASES (only meaningful if codex installed; guard display).
- [ ] Generalize `renderIntegrationCard(title, statusKeys)` and render one card per detected backend.
- [ ] Manual smoke later. Build, commit.

---

## Increment 2 — Ingest: typed agent, model-from-payload (Codex), permission/subagent events

**Files:** Modify `daemon.rs` (common extraction ~`:601-641`, event match ~`:644-780`).

### Task 2.1: type the agent + model-from-payload guarded to Codex
- [ ] Read `daemon.rs:546-660` and `:960-1010` first.
- [ ] After `s.agent` is set (`:609-611`), compute `let agent = Agent::from_opt(s.agent.as_deref());` for use in this reduce call.
- [ ] **Test (daemon unit):** feed a Codex `session-start` envelope `{agent:"codex", payload:{session_id, model:"gpt-5.5", cwd}}` → `s.model == Some("GPT-5")` (via `backend.friendly_model`), and a Claude envelope with a stray `payload.model` → `s.model` unchanged (still mined later). 
- [ ] **Impl:** in the common block, `if agent==Agent::Codex { if let Some(m)=p.get("model").and_then(Value::as_str) { if s.model_at.map_or(true, |t| now - t > 30_000) { s.model = Some(backend(agent).friendly_model(m)); } } }`.
- [ ] Build, test, commit.

### Task 2.2: `permission` → Waiting; `subagent-start/stop`
- [ ] Add match arms mirroring `notification` (→`Status::Waiting`, default text "Codex ждёт подтверждения") for `"permission"`, and mirroring the `Task` pre/post-tool subagent logic (read `payload.agent_type`/`agent_id`) for `"subagent-start"`/`"subagent-stop"`.
- [ ] **Test:** `permission` event → `s.status == Waiting`; `subagent-start` then `subagent-stop` updates `s.subagents`.
- [ ] Build, test, commit.

---

## Increment 3 — Codex transcript parser + dispatch all consumers

**Files:** Create `backend/codex_transcript.rs`; Modify `backend/codex.rs`, `transcript.rs` call-sites, `tail.rs`, `daemon.rs`, `ipc.rs`, `capability/native/chats.rs`.

### Task 3.1: Codex rollout → ChatItem (TDD with real fixtures)
- [ ] Capture 1 real rollout: copy ~30 lines from a file under `~/.codex/sessions/2026/06/**/rollout-*.jsonl` into a test fixture string.
- [ ] **Test:** `codex_transcript::to_chat_items(&line)` for a `response_item` message(role=assistant, content[output_text]) → `ChatItem{role:"assistant",...}`; for `function_call` → a tool-label item; `session_meta`/`reasoning`/`token_count` → `[]` or hidden.
- [ ] **Impl** per spec §4.3 (parse `{timestamp,type,payload}`; map `response_item.payload.type` message/function_call/custom_tool_call; reuse `transcript::short_tool_label`, `parse_ts`). Add `extract_model` (last `turn_context.model`), `extract_branch` (`session_meta.git.branch`), `extract_title` (lookup `~/.codex/session_index.jsonl`), `transcript_dir_for`, and `full_final_reply` preferring a passed-in `last_assistant_message`.
- [ ] Wire `CodexBackend` methods to these. Build, test, commit.

### Task 3.2: dispatch every transcript call-site by `s.agent`
- [ ] Thread `Agent` to: `ipc.rs:305` (chat_open), `tail.rs` (add `agent: Agent` to `TailHandle::start` + `tail_loop`; `ipc.rs:313` passes `s.agent`), `daemon.rs:910` (`ai_toast_summary` → `backend(agent).full_final_reply` / pass Stop `last_assistant_message`), `daemon.rs:963-1008` (`refresh_meta` title/branch/model via `backend(agent).extract_*`), `daemon.rs:1152-1156`, `capability/native/chats.rs:43-48`.
- [ ] Replace direct `transcript::to_chat_items`/`chain_from_entries` calls at those sites with `backend(agent).to_chat_items` / `read_entries`.
- [ ] **Test:** existing Claude transcript tests still green (regression); a Codex `chat_open` smoke unit returns non-empty items from the fixture.
- [ ] Build, test, commit. → Codex chat/voice/toasts/summaries work.

---

## Increment 4 — Control + UI (resume, model/effort, badge, vocab, strings)

**Files:** `ipc.rs`, `tmux.rs`, `backend/codex.rs`, `ui/renderer.js`, `ui/index.html`.

- [ ] **4.1 resume_cmd everywhere:** route `ipc.rs:29` (tmux_needed), `renderer.js:696/699/2238` through `backend(agent).resume_cmd(sid)`. Test + commit.
- [ ] **4.2 model/effort per-agent:** `set_model`/`set_effort` dispatch (Claude slash unchanged; Codex `/model` picker hooks via tmux choreography — calibrate against live Codex TUI in smoke). UI: `MODELS_BY_AGENT`, `effortsFor(model, agent)`; hide effort picker when `!backend.has_separate_effort()`. Test + commit.
- [ ] **4.3 agent badge + string templatization:** add `.badge.agent` pill (render only when `s.agent!=='claude'`, before model badge so "codex" isn't shown as a model); replace `'claude'` literal at `renderer.js:239`; templatize the strings listed in spec §4.7. Test + commit.

---

## Increment 5 — Usage / limits (Codex), per-provider official

**Files:** `usage.rs`, `limits.rs`, `backend/codex.rs`.

- [ ] **5.1 Codex usage scan:** `scan_usage(Codex,…)` tails `~/.codex/sessions/**/*.jsonl`, extracts `event_msg.token_count.info.total_token_usage` → `Tok`; aggregate with `backend(Codex).price()`. Test with a `token_count` fixture. Commit.
- [ ] **5.2 per-provider official + Codex rate_limits:** split `Usage.official` into `official_claude`/`official_codex`; `official_info(agent)`; Codex synthesizes `PctReset` from latest `rate_limits.primary`. **Do NOT modify `limits.rs:78`.** Add a regression test asserting Claude limit banner still fires with `official=None`. Commit.

---

## Increment 6 — Secondary surfaces (history, commands, effort discovery)

**Files:** `history.rs`, `commands_catalog.rs`, `daemon.rs` (`detect_effort_levels`), `ipc.rs`.

- [ ] **6.1 History per-agent:** Codex scan of `~/.codex/sessions/**` reusing the rollout parser; `history_get` merges. Test + commit.
- [ ] **6.2 Commands palette per-agent:** `commands_get` branches on `s.agent`; Codex builtins minimal + `~/.codex/prompts`; hide claude-only `/usage`,`/compact`,`/effort` for codex. Test + commit.
- [ ] **6.3 effort discovery per-agent:** `detect_effort_levels` per-agent (Codex static list); retire the single global. Test + commit.

---

## Increment 7 — Internal Codex: service-LLM + gated agent-host

**Files:** `claude_bin.rs` (+ new `service.rs` or extend), `backend/codex_agent.rs`, `agent/mod.rs`, `wakeword/action.rs`, `ipc.rs`, `settings.rs`.

- [ ] **7.1 resolve_codex_bin + service-LLM dispatch:** add `resolve_codex_bin()`; `run_service_llm(agent, prompt, timeout)`; setting `internalBackend: auto|claude|codex` (default auto=Claude-if-present-else-Codex). Replace `resolve_claude_bin().is_none()` gates at `daemon.rs:905/1148` with "any service backend available". Codex path: `codex exec --json -m <model> -c model_reasoning_effort=low -C <tmp> "<HAIKU_SYSTEM+prompt>"` (env proxy inherited; no `minimal`). Test parse of final `agent_message.text`. Commit.
- [ ] **7.2 CodexCliHost + mandatory per-item kill:** `~/.jarvis/codex-agent-home/` (auth.json symlink + minimal config.toml jarvis-MCP, no skills dir); run `CODEX_HOME=… codex exec --json -s read-only -c mcp_servers.jarvis.* …`; `parse_codex_line` maps thread.started/item.*/turn.completed → `AgentEvent`; **kill on any `command_execution`/`local_shell`/non-jarvis `mcp_tool_call` item** (Codex INV-TOOLS analogue). 
  - **Test (security):** synthetic `--json` stream with a `command_execution` item → host kills (mirror the existing INV-TOOLS test in `agent/mod.rs`). 
  - Commit.
- [ ] **7.3 host selection:** route `ipc.rs:781` (`agent_send`) and `wakeword/action.rs:99/111` through `internalBackend` host choice. Test + commit.

---

## Increment 8 — Docs + final tests + smoke

- [ ] **8.1 README:** RU canon (`README.md`) Codex section (setup, "Hey Jarvis" unaffected, hook-trust-bypass risk note, что `codex exec` не мониторится), then mirror to `README.en.md` in the SAME commit (per project rule). Do not translate commands/paths/hooks.
- [ ] **8.2 full test pass:** `cargo test --manifest-path src-tauri/Cargo.toml` all green; `cargo build` clean.
- [ ] **8.3 manual smoke:** `JARVIS_DIR=~/.jarvis-dev JARVIS_DEV=1` build + run; launch interactive `codex` in a project; verify: panel row labelled `codex`, Stop toast + voice, chat renders, reply inserts, model picker, usage grows. Document results.
- [ ] **8.4 finish:** invoke superpowers:finishing-a-development-branch.

---

## Self-Review (done at authoring)

**Spec coverage:** §4.1→Inc1; §4.2→Inc2; §4.3→Inc3; §4.4+§4.8→Inc4; §4.5→Inc5; §4.7→Inc6; §4.6→Inc7; §6.8 docs→Inc8. All spec sections mapped. ✓
**Placeholders:** novel logic (Agent enum, hook writer, Codex parser, agent-host kill) has concrete code; mechanical edits have exact file:line + transformation. ✓
**Type consistency:** `Agent`, `Backend`, `backend()`, `hook_events()`, `CODEX_EVENTS`, `run_service_llm`, `resume_cmd` used consistently across tasks. ✓
**Known judgment call:** Inc 7.2 (Codex agent-host) is the riskiest/lowest-value slice; built last so it's deferrable without affecting Inc 0–6.
