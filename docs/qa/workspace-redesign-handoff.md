# Workspace / dictation integration handoff

Thread: `01a06ef8-5cc9-7141-9437-495731f49bca`.
Central release owner: sibling `01a06e5a-e84b-7d72-98d7-f33e6ee938d2`.

Status: **READY — RUNTIME SOURCE FROZEN** (5 September, about17:38 MSK).
All agents in this task have stopped editing runtime/UI source. Central owner
may take the final source snapshot, compile, run final gates and restart.
This runtime has no callable thread send/read tools (exhausted ALL_TOOLS lookup).
Please use this file for the return path and send your status into this thread.

Owned changes in this task:
- Independent quick `main` and native `workspace` / `workspace-*` windows;
  routes, per-window tail manager and targeted append, own-window close cleanup,
  native main-surface clipping, shared theme/install progress data events.
- Compact persistent project/chat sidebar, stable project order, six quick recents,
  safe titles, chat/project detach actions. Current capabilities changes by the
  sibling task are preserved, including readonly usage in collapsed details.
- Codex display context/guardian filtering, live/history cache repairs.
- Separate dictation permission banner, cancellable insertion, original transcript,
  formatting safeguards, separate language-preserving service prompt.
- `docs/chat-workspace.md`, `docs/speech-dictation.md`.

Completed: codex_titles.rs batched local provider title metadata, main.rs module
registration, native attachment title extraction and in-app-browser context
attributes. No root Cargo/build is running. Voice backend is done.
Title verification is complete:70 current isolated tests passed (plus7 baseline
comparison tests), including a real read-only SQLite fixture test. Read-only
local sample for today:27 ordinary sessions,31 technical excluded;6 native
titles plus21 agent nickname/role labels,0 unnamed in this sample.
No pending work or running agents remain in this task. Final combined Cargo,
bundle and native restart are delegated to the central release owner.

Latest: UI299/299 passed in `/tmp/jarvis-workspace-redesign-final-ui.log`.
Native Reopen is now gated on `has_visible_windows: false` in main.rs, preserving
the current detached window when the application merely becomes active.
`show_application` prioritizes existing workspaces if a launch does need one.
Speech IPC now uses `run_service_text_transform`; UI says AI-форматирование and
describes selected-provider fallback. Root will make no further runtime changes.

Last completed checks BEFORE pending title helper and service prompt wiring:
- Full featured workspace Cargo: 1144 app + 8 MCP + 102 setup + 44 node passed.
  Local HTTP tests require outside-sandbox localhost sockets. 16 ignored total.
- UI276 passed; later focused title/sidebar tests45 passed.
- After installer event routing, onboarding5 passed, release bundle compiled.
- Speech isolated:46 core tests; later27 service/prompt tests passed.
- Native production workspace opens with real traffic lights and rounded frame;
  folder collapse works and remains after snapshots; actual external Codex chat
  opens without disabled composer/tmux instructions and receives appended events.
- Native smoke uncovered cached title prefixes during bounded replay and missing
  provider-generated titles; these are the pending title work.
- Native app focus/reopen covered workspace with quick panel because launch mode
  is overlay. Root has now fixed show_application to prioritize an existing
  workspace; needs final native verification.

Production was restarted at17:16 local with process61687, bundle
`src-tauri/target/release/bundle/macos/Jarvis.app`, SHA256
`aa38654ad381deed82d0f77cd084d2b6e6287c751e7774935f05ffcc98e58a3e`.
This running bundle predates the last title/service changes. Do not call it final.
No active meetings at restart; no live microphone/TCC grant/insertion tested.
Root will not restart again; central owner should do so after READY.

Runtime fingerprints at freeze: `docs/qa/workspace-redesign-source.json`.
Runtime source has not been edited by this task after these fingerprints.
