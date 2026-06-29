<p align="center"><a href="README.md">Русский</a> · <b>English</b></p>

<h1 align="center">Jarvis</h1>

<p align="center">
  A macOS menu-bar command center for every Claude&nbsp;Code agent at once&nbsp;— it tells you the moment one needs you.
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/macOS-11%2B%20·%20universal-black?logo=apple" alt="macOS 11+, universal">
  <img src="https://img.shields.io/github/v/release/Sergey-Chernyshev/jarvis?include_prereleases&sort=semver&label=release" alt="Latest release">
  <img src="https://img.shields.io/badge/status-pre--1.0-orange" alt="Status: pre-1.0">
</p>

> This is an English translation. The canonical version is the Russian [README.md](README.md); if they diverge, the Russian version is authoritative.

<p align="center"><i>🖼 Screenshot and demo coming soon. What to capture and how to wire it in: <a href="docs/assets/README.md">docs/assets/README.md</a>.</i></p>
<!-- <p align="center"><img src="docs/assets/hero.png" alt="Jarvis: the ⌘J panel, menu-bar counters, a toast over fullscreen" width="760"></p> -->

## The problem

Running many Claude&nbsp;Code agents at once turns **you** into the bottleneck. Spread across terminals, tabs and macOS Spaces, you can't see at a glance which agent is **blocked on a permission prompt**, which **finished and is now idle** burning your wall-clock, and which **hit a rate limit**. Claude&nbsp;Code's native notifications are per-session and terminal-scoped (they don't even fire in the VS&nbsp;Code extension), with no single pane for the aggregate state. And a sleeping Mac **silently freezes** claude processes and severs in-flight requests, killing long overnight runs — and on Apple&nbsp;Silicon, closing the lid forces a sleep that plain `caffeinate` can't bypass.

**Jarvis is mission-control for your Claude Code agents, right in the menu bar:** see, hear, and reply to every session so none stalls waiting on you, and a sleeping Mac never kills a long run.

## What it does

- **🖥 Multi-session menu-bar monitor** — every Claude Code session across every terminal at once, live **⏸ waiting · ⚙ working** counters.
- **🔔 Toasts that render over fullscreen** — you see "needs permission" even inside a fullscreen app.
- **🎛 Always-on-top ⌘J panel** — open, glance, act, dismiss (Raycast-style); never steals focus.
- **↩️ Reply straight into any session** — type back into a session via tmux even if its window is minimized or on another Space.
- **⚙️ Remote-control model & effort** — switch Opus/Sonnet/Haiku and reasoning effort from the panel.
- **🗣 Jarvis speaks** — a local TTS engine reads out, in Russian, what a session did or what it needs.
- **🎙 Voice input (dictation)** — push-to-talk (F8); your speech is transcribed and inserted into the active session.
- **☕ Keep the Mac awake** — anti-sleep (a `caffeinate` equivalent) plus a guarded closed-display mode for overnight runs.
- **✅ Read-only task board** — live `TodoWrite` progress (done / in-progress / queued) per session.
- **🔒 Event-driven & private** — built on Claude Code's own hooks (no screen scraping), everything local, no telemetry.

> **Boundary:** Jarvis is a **monitor and remote, not an orchestrator.** It does not spawn agents or own the plan; it complements orchestrators (Claude Squad, Conductor, Crystal, native Agent Teams) and the sessions you already run.

## Install

### From a release (recommended)

1. Download `Jarvis_x.y.z_universal.dmg` from the [releases](https://github.com/Sergey-Chernyshev/jarvis/releases) page.
2. Open the DMG, drag **Jarvis** into **Applications**, launch from Launchpad.
3. On first launch Jarvis offers to install the Claude Code integration and voice — click **"Настроить"** ("Set up"); progress is shown step by step in the window.

The app checks for updates itself (built-in updater). Reinstall the integration anytime: menu bar → "Reinstall integration…".

**Requirements:** macOS 11+ (Big Sur), Apple Silicon or Intel — a universal binary, Developer&nbsp;ID signed and **notarized**. The remote and reply-into-session features need tmux (`brew install tmux`).

<details>
<summary>Build from source / for developers</summary>

You need: Rust stable (rustup; the minimum is the `rust-version` field in [`src-tauri/Cargo.toml`](src-tauri/Cargo.toml)), tmux (optional).

```bash
npm run setup     # install hooks into ~/.claude/settings.json (backed up, idempotent)
npm start         # build and launch the daemon + menu bar (◇ top-right)
```

(npm here is just a familiar runner: under the hood it's `cargo run --release`. Without npm: `cargo run --release --manifest-path src-tauri/Cargo.toml`.)

Then **restart any active** `claude` sessions — hooks are snapshotted at session start. Remove everything: `npm run teardown`. Check the status of each component: `npm run status`. Build a DMG locally (unsigned, host arch): `npm run bundle`.

**Try it without Claude Code** (the daemon must be running) — manually, the way a real hook does it:

```bash
echo '{"session_id":"t1","cwd":"'$PWD'"}' | ~/.jarvis/bin/jarvis-hook claude session-start
echo '{"session_id":"t1","cwd":"'$PWD'","prompt":"добавь тесты"}' | ~/.jarvis/bin/jarvis-hook claude prompt
echo '{"session_id":"t1","cwd":"'$PWD'","message":"Claude needs your permission to use Bash"}' | ~/.jarvis/bin/jarvis-hook claude notification
echo '{"session_id":"t1","cwd":"'$PWD'"}' | ~/.jarvis/bin/jarvis-hook claude stop
```

Check the daemon is alive: `curl -s --unix-socket ~/.jarvis/run.sock http://jarvis/`

</details>

## Features

### 🖥 Session monitoring

A registry of every Claude Code session: status (idle / working / waiting / finished / hit the limit), project, model, activity. The menu-bar counter (**◇ ⏸N ⚙M**) is the cross-terminal summary.

<details>
<summary>How it works</summary>

No screen parsing — only structured events from Claude Code itself, via hooks: `SessionStart` → idle, `UserPromptSubmit` → working, `Notification` → waiting (+a notification), `Stop` → done (+a notification), `SessionEnd` → the session disappears. The registry-reducer lives in the Rust daemon; state survives a daemon restart (`~/.jarvis/state.json`). The pane's screen is read only by the interactive-prompt detector and the remote (regex, not screenshots).

</details>

### 🔔 Notifications & 🎛 the ⌘J panel

Jarvis's own toast notifications render **over fullscreen**; the panel (`alwaysOnTop`) does not steal focus. The global **⌘J** hotkey opens the panel centered on screen — Esc, a click outside, or ⌘J again closes it.

<details>
<summary>Details</summary>

Click ◇ for the panel. Right-click for the menu (test notification, autostart, quit). The hotkey, panel position, notification toggles and autostart are in settings (⚙ in the panel header), stored in `~/.jarvis/settings.json`. Note: the global ⌘J intercepts that key across all apps (Chrome "downloads", VS Code "panel") — change the shortcut in settings if it gets in the way.

</details>

### ↩️ Reply into a session (tmux) & ⚙️ the remote

In a session's chat the **"Reply"** field inserts text straight into the session's terminal — even if the window is minimized or on another Space. Below it sits the **remote**: **Model** segments (Opus / Sonnet / Haiku) and **Effort** (auto / low / med / high / xhigh) send a slash command (`/model sonnet`, `/effort high`) into the live session.

<details>
<summary>How it works</summary>

`npm run setup` installs a PATH shim `~/.jarvis/shims/claude` (pyenv pattern, a managed block in `~/.zshrc` between jarvis markers). After `exec zsh`, every interactive `claude` launch is transparently wrapped in tmux on a **separate server** (`-L jarvis`, config `~/.jarvis/tmux.conf`): no status bar, mouse scrolls, your personal tmux untouched. In iTerm2 — control mode (`-CC`), native tabs. Headless runs (`-p`, pipes, `$TMUX`) are not wrapped. Insertion: `set-buffer → paste-buffer -p → send-keys Enter` (multi-line prompts arrive as one chunk). The command palette (`/` in the reply field) reaches the session's other slash commands.

**Codex CLI** is wrapped by the same mechanism: if `codex` is found during `npm run setup`, Jarvis installs hooks into `~/.codex/hooks.json` (label `codex`) and a shim `~/.jarvis/shims/codex` (one `agent-shim` script, behavior chosen by `basename "$0"`). Interactive `codex` sessions appear in the panel with a `codex` badge: status, toasts, voice, chat, reply-in-session, the resume command `codex resume <session_id>`. Model and reasoning are changed via Codex's `/model` picker (there is no separate `/effort`). Voice activation "Hey Jarvis" works as usual — it doesn't depend on which agent is running. **Headless `codex exec` does NOT fire hooks** — such runs aren't monitored (by design, like `claude -p`). On a fresh machine Codex hooks require trust (`~/.codex/config.toml [hooks.state]`); the Codex shim adds `--dangerously-bypass-hook-trust` (if the installed `codex` supports the flag — the installer checks `codex --help`) — this **disables trust verification for all Codex hooks** on interactive launch, so keep that trade-off in mind.

- A session outside tmux is flagged "outside tmux" — it can't be controlled; the panel shows how to bring it in (`claude --resume <session_id>`).
- Close the terminal window and the agent lives on: the tmux session detaches, Jarvis keeps watching. Reattach: `tmux -L jarvis attach -t <name>`.
- **Model** is free to read (written to the transcript on every assistant turn). **Effort** can't be read from outside — the panel keeps optimistic state, highlighting what it set itself.

</details>

### ☕ Keep awake · ⌒ Clamshell (power plugins)

A sleeping Mac means frozen claude processes and severed API requests. Two pluggable plugins (toggles in settings), UX modeled on Raycast Coffee and Amphetamine.

- **☕ Keep awake** — vetoes idle sleep via a power assertion (IOPMAssertion, like `caffeinate`): indefinitely, for a duration (15m…8h), while a process lives, or auto — while agents are working.
- **⌒ Clamshell** — closed-display mode (`pmset disablesleep`): the Mac runs with the lid shut. On Apple Silicon this is exactly what plain `caffeinate` can't do.

<details>
<summary>Safety and fail-safes</summary>

The assertion lives in the daemon process: a crash auto-releases it, a "stuck" sleep block is impossible. Check: `pmset -g assertions | grep -i jarvis`.

Clamshell means root and thermal risk, so the plugin **detects and suggests rather than silently sudo-ing**: after an interrupted sleep it offers closed-display mode; with an external display it advises native clamshell (no root). The manual toggle is an honest admin dialog; for silent switching there's "Set up silent mode" (`/etc/sudoers.d/jarvis-pmset`, exactly two commands via `visudo -c`). Fail-safes: the `~/.jarvis/clamshell.json` marker, sleep restored on daemon start/exit, a battery guard (≤15% → restore sleep). A fanless MacBook Air throttles under a closed lid — the plugin warns about it.

</details>

### 🗣 Voice (TTS) — Jarvis speaks

After a session event Jarvis briefly says, in Russian, what happened, via a local TTS engine. The text is assembled from structural signals by template; numbers are spelled out with Russian agreement.

- turn finished → "Pixela: four of six tasks, now docker-compose" (board present) · "Recru is done, three files changed" (diff present) · "Ticketing finished";
- waiting on you → **priority**: "Pixela is waiting — needs permission for Bash"; hit the limit → "Pixela hit the limit, resets in two hours".

<details>
<summary>Engine, config, boundary</summary>

The engine is **Silero** (a local Python sidecar, FastAPI, model held in memory; binds to `127.0.0.1` only), which the daemon manages itself: starts it, restarts on crash, stops it when idle/on exit. Speakers: `aidar` · `baya` · `kseniya` · `xenia` · `eugene`. Utterances don't overlap: the queue is serialized, "waiting" jumps ahead of "done", a pile-up of `Stop` lines is coalesced. Config lives in `~/.jarvis/settings.json`:

```json
"voice": { "engine": "silero", "mute": false,
           "events": { "stop": true, "notification": true, "stopFailure": true } }
```

Menu bar: a **"Mute"** toggle and **"Test voice"**. If the engine is unavailable the daemon runs as before, Claude Code is untouched, the reason is in the log.

⚠️ **Voice license:** the default Russian Silero voice `v4_ru` is **non-commercial** (CC BY-NC-SA 4.0). For commercial use see the [License](#license) section.

</details>

### 🎙 Voice input (dictation)

Push-to-talk: hold **F8**, speak, release — your speech is transcribed locally and inserted into the active session. The default engine is Qwen3 via an MLX sidecar; Whisper is optional (native, behind the `whisper-native` feature flag).

### ✅ Task board (observer)

Sessions running a multi-step plan (`TodoWrite`) show an "N/M tasks" ring → a "Tasks" slide-over: an aggregate, a progress bar, a list with statuses, and where possible the model and time of the correlated subagent. The board is live: the next `TodoWrite` redraws it on its own.

<details>
<summary>Why read-only</summary>

The source of truth for tasks is the **orchestrator inside the session**. The `TodoWrite` list has no external "mark done" API — it can't be mutated from outside. Jarvis only **reads** the board (`PostToolUse`, last-write-wins) and **displays** it. A task action (go to / skip / restart) is an **instruction to the orchestrator**: it pre-fills the reply field with editable text that you review and send yourself. The board changes only when the next real `TodoWrite` arrives — so you see what the agent actually does, not what we asked for.

</details>

## Jarvis vs neighbors

| | **Jarvis** | Menu-bar monitors | Orchestrators | Claude Code (native) |
|---|:---:|:---:|:---:|:---:|
| State detection | hooks (events) | process polling | spawn their own | itself |
| All sessions in one place | ✅ | ✅ | partial | ❌ |
| Reply into a session | ✅ (tmux) | ❌ | ✅ | — |
| Remote model/effort | ✅ | ❌ | partial | manual |
| Voice (TTS) | ✅ | ❌ | ❌ | ❌ |
| Anti-sleep + clamshell | ✅ | ❌ | ❌ | ❌ |
| Task board | ✅ read-only | ❌ | ✅ owns it | ✅ TodoWrite |
| Spawns agents | ❌ (by design) | ❌ | ✅ | — |
| License | MIT | varies | varies | proprietary |

The closest in spirit are menu-bar monitors; their edge is zero setup. Jarvis answers with accuracy (hooks, not guessing), reply-into-session plus a remote, voice, and power management.

## How it works

```
claude (any terminal)
  └─ hooks from ~/.claude/settings.json   ← installed by npm run setup
       └─ ~/.jarvis/bin/jarvis-hook       ← fail-silent shim, 0.3s curl
            └─ unix socket ~/.jarvis/run.sock
                 └─ Rust daemon (Tauri) = the session registry
                      ├─ its own toast notifications (over fullscreen)
                      ├─ the panel (alwaysOnTop, doesn't steal focus)
                      └─ the menu-bar counter: ⏸ waiting · ⚙ working
```

The frontend (panel `ui/index.html` + `ui/renderer.js`, toasts `ui/toast.*`) runs in the system WKWebView; the `window.jarvis` contract is implemented by a thin adapter `ui/bridge.js` over Tauri IPC. The main process is Rust (`src-tauri/src/`): a unix socket on axum, the registry-reducer with effects, tmux, transcripts, usage/history, the power plugins. State, settings and stats live in `~/.jarvis/` in stable formats.

**Principles:** no screen reading (structured events only) · fail-silent hooks (they never break Claude Code) · everything local, no telemetry · remove everything with one command (`npm run teardown`). The increment-by-increment development history is visible in [`docs/superpowers/`](docs/superpowers/).

## Status and limitations

A pre-1.0 MVP. What's stable is in the "Features" section above. Deliberate boundaries and known issues:

- **macOS-only**; **Claude Code (CLI)** and **Codex (CLI)** are supported — **interactive** sessions are monitored (headless `claude -p` / `codex exec` don't fire hooks — not monitored); the remote and reply need **tmux**.
- **Effort** can't be read from outside → the panel keeps optimistic state.
- **Claude Code's hook schema drifts between versions.** If events stop arriving after a `claude` update, compare against the current hooks docs and fix `EVENTS`/the format in `src-tauri/src/bin/setup.rs`.
- A hard-killed terminal (no `SessionEnd`) leaves a session hanging — the panel's "Clear" button removes done/idle ones.
- **Wake-word** ("Hey Jarvis") — voice activation (openWakeWord/`ort`), enabled in release builds (the DMG, `npm run bundle` / `start:prod`); **off by default** in the panel, the detector model (non-commercial — see License) is fetched on demand. No speaker verification yet (v1 is a seam). The dev build `npm start` omits the feature — the detector is inert.
- **Agent chat** (the capability platform, MCP bridge `jarvis-mcp`) — the bridge exists, the UI isn't shipped. *In progress.*

## Versioning

Pre-1.0 (**SemVer 0.x**): the hook contract and the on-disk formats (`~/.jarvis/`) may change. `0.MINOR` bumps for features/breaking changes, `0.x.PATCH` for fixes. `1.0` will freeze the hook contract and the state format.

## Contributing

Issues and PRs are welcome! See [CONTRIBUTING.en.md](CONTRIBUTING.en.md) for how to build, test, and open a PR. By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md). Report vulnerabilities privately via [SECURITY.md](SECURITY.md). Specs and per-increment plans live in [`docs/superpowers/`](docs/superpowers/).

Direct pushes to `master` are disabled — changes land only via Pull Request with green CI.

## License

Jarvis's code is licensed under the **[MIT License](LICENSE)** © 2026 Sergey Chernyshev.

**Model weights are downloaded, not part of the repo** and keep their own licenses — these restrict **use** of the corresponding feature independently of the MIT code:

| Artifact | Used for | License | Commercial |
|---|---|---|:---:|
| Silero `v4_ru` | **default voice** | CC BY-NC-SA 4.0 | ❌ |
| Silero `v5_cis_base` / `_nostress` | voice | MIT | ✅ |
| openWakeWord `hey_jarvis_v0.1` | **default wake-word** | CC BY-NC-SA 4.0 | ❌ |
| whisper.cpp `ggml-*` | speech-to-text | MIT | ✅ |
| Qwen3 (`mlx-community`) | speech-to-text | Apache-2.0 | ✅ |

> ⚠️ The default voice (`v4_ru`) and wake-word (`hey_jarvis`) are **non-commercial**. For commercial use: voice → Silero `v5_cis_base` (MIT); wake-word → train your own or disable it; STT → Whisper or Qwen3. The full breakdown and third-party obligations are in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) (including the BSD-3 attribution for `mediaremote-adapter`).

**Non-affiliation.** Jarvis is an independent open-source project. It is **not affiliated with, or endorsed by, Anthropic, PBC**; "Claude" and "Claude Code" are trademarks of Anthropic, used **nominatively** (to describe compatibility). Jarvis works on top of Claude Code's **official hooks**. The project is also unaffiliated with Marvel/Disney; any resemblance to the fictional "J.A.R.V.I.S." is unintentional.
