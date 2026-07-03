<p align="center"><b>English</b> · <a href="README.ru.md">Русский</a></p>

<h1 align="center">Jarvis</h1>

<p align="center">
  Mission control for your coding agents, right in the macOS menu bar.<br>
  Jarvis watches every Claude&nbsp;Code and Codex session you run — and tells you, even out loud, the moment one needs you.
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/macOS-11%2B%20·%20Apple%20Silicon-black?logo=apple" alt="macOS 11+, Apple Silicon">
  <img src="https://img.shields.io/github/v/release/Sergey-Chernyshev/jarvis?include_prereleases&sort=semver&label=release" alt="Latest release">
  <img src="https://img.shields.io/github/actions/workflow/status/Sergey-Chernyshev/jarvis/ci.yml?branch=master&label=CI" alt="CI">
  <img src="https://img.shields.io/badge/status-pre--1.0-orange" alt="Status: pre-1.0">
</p>

<p align="center"><i>🖼 Screenshot and demo coming soon. What to capture and how to wire it in: <a href="docs/assets/README.md">docs/assets/README.md</a>.</i></p>
<!-- <p align="center"><img src="docs/assets/hero.png" alt="Jarvis: the ⌘J panel, menu-bar counters, a toast over fullscreen" width="760"></p> -->

## Why Jarvis

Running several coding agents at once turns **you** into the bottleneck. Sessions are spread across terminals, tabs and macOS Spaces, and you can't see at a glance which agent is **blocked on a permission prompt**, which **finished half an hour ago and is idling** while you think it's working, and which **hit a rate limit**. The agents' own notifications are per-session and terminal-scoped (they don't even fire in the VS Code extension) — there is no single pane showing the aggregate state. And a sleeping Mac **silently freezes** agent processes and severs in-flight API requests, killing long overnight runs; on Apple Silicon, closing the lid forces a sleep that plain `caffeinate` can't prevent.

**Jarvis is that missing single pane.** It sits in the menu bar, watches every interactive Claude Code and Codex CLI session on your machine, and makes sure no agent ever waits on you silently:

- you **see** the aggregate state at all times (live counters in the menu bar, a Raycast-style ⌘J panel);
- you **hear** it — toast notifications that render over fullscreen apps, and an optional local voice that speaks session events out loud;
- you **act** without hunting for the right terminal — reply straight into any session, switch its model or reasoning effort, and let Jarvis keep the Mac awake through the night.

## Highlights

- **🖥 Multi-session monitor** — every Claude Code and Codex session across every terminal, with live **⏸ waiting · ⚙ working** counters in the menu bar.
- **🔔 Toasts over fullscreen** — "needs permission" is visible even inside a fullscreen app, with customizable content (branch, model, effort, tokens, duration).
- **🎛 Always-on-top ⌘J panel** — open, glance, act, dismiss (Raycast-style); never steals focus.
- **↩️ Reply into any session** — type back into a session via tmux even if its window is minimized or on another Space; a `/` command palette included.
- **⚙️ Remote control** — switch model (Opus / Sonnet / Haiku) and reasoning effort from the panel; answer multi-choice agent questions with native pickers.
- **📊 Usage, costs and limits** — token and cost tracking per model and project; when a session hits the usage limit, Jarvis shows when it resets and can auto-resume it.
- **🗣 Jarvis speaks** — a local TTS voice reads out what a session did or what it's waiting for (Russian-first for now).
- **🎤 "Hey Jarvis" voice assistant** *(experimental)* — say the wake word and talk to your sessions: route a reply by voice, ask what an agent did, control media/volume, ask a general question.
- **🎙 Dictation** — push-to-talk (F8): speech is transcribed locally (Whisper / Qwen3) and inserted into the active session; full dictation history with re-transcription.
- **☕ Keep the Mac awake** — anti-sleep (a `caffeinate` equivalent) plus a guarded closed-lid mode for overnight runs.
- **✅ Read-only task board** — live `TodoWrite` progress (done / in-progress / queued) per session.
- **📦 Model manager** — download, delete and hot-swap the local TTS/STT/wake-word models from settings; guided first-run onboarding.
- **🔒 Event-driven & private** — built on the agents' own hooks (no screen scraping), everything runs locally, no telemetry, removable with one command.

> **Boundary:** Jarvis is a **monitor and a remote, not an orchestrator.** It does not spawn agents and does not own the plan; it complements orchestrators (Claude Squad, Conductor, Crystal, native Agent Teams) and the sessions you already run.

## Who it's for

Jarvis is a power-user tool. It pays off once you cross the pain threshold of **3+ parallel agent sessions**:

- **Solo developers running an agent fleet** — several Claude Code / Codex sessions across projects, where checking each terminal by hand eats the very time the agents were supposed to save.
- **Overnight and long unattended runs** — big refactors, test-fix loops, batch migrations on a MacBook: Jarvis keeps the Mac awake (even lid-closed) and auto-resumes sessions when the usage limit window resets.
- **People who like to stay heads-down** — you keep writing code or reading while agents grind; Jarvis interrupts you only when one of them actually needs a decision, with a toast or a spoken line.
- **Small teams standardizing agent workflows** — Jarvis is fully local per machine (no server, no accounts), so every developer just installs it and gets the same visibility.

It's probably **not** for you if you run one session in one terminal (native notifications may be enough), or if you're looking for something to *spawn and plan* agents — pair Jarvis with an orchestrator for that.

## Install

### From a release (recommended)

1. Download `Jarvis_x.y.z_aarch64.dmg` from the [releases](https://github.com/Sergey-Chernyshev/jarvis/releases) page.
2. Open the DMG, drag **Jarvis** into **Applications**, launch it.
3. First launch: the build is **ad-hoc signed** (no Apple Developer ID yet, not notarized), so macOS will warn about an unidentified developer. Right-click the app → **Open** → **Open**. If macOS claims the app is "damaged", clear the quarantine flag:

   ```bash
   xattr -dr com.apple.quarantine /Applications/Jarvis.app
   ```

4. On first launch Jarvis offers to set up the Claude Code / Codex integration and download voice models — click **«Настроить»** ("Set up"); progress is shown step by step in the window.

The app checks for updates itself (built-in updater). You can reinstall the integration anytime: menu bar → "Reinstall integration…".

**Requirements:**

- an **Apple Silicon** Mac (M1 or newer), macOS 11+ — the prebuilt DMG is aarch64-only; Intel Macs can build from source (below);
- **tmux** for the reply-into-session and remote-control features: `brew install tmux`;
- **Claude Code** (CLI) and/or **Codex** (CLI) — the agents Jarvis monitors.

<details>
<summary>Build from source / for developers</summary>

You need: Rust stable ([rustup](https://rustup.rs/); the minimum version is the `rust-version` field in [`src-tauri/Cargo.toml`](src-tauri/Cargo.toml)), Node.js 20+, CMake (`brew install cmake`), tmux (optional).

```bash
npm ci
npm run setup     # install hooks into ~/.claude/settings.json (backed up, idempotent)
npm start         # build and launch the daemon + menu bar (◇ top-right) against the dev profile (~/.jarvis-dev)
```

(npm here is just a familiar runner: under the hood it's `cargo build --release` plus an ad-hoc `codesign` needed for microphone access.)

Then **restart any active** `claude` sessions — hooks are snapshotted at session start. Remove everything: `npm run teardown`. Check the status of each component: `npm run status`. Build a DMG locally (unsigned, host arch): `npm run bundle`. Run the prod profile: `npm run start:prod`.

**Try it without Claude Code** (the daemon must be running) — manually, the way a real hook does it:

```bash
echo '{"session_id":"t1","cwd":"'$PWD'"}' | ~/.jarvis/bin/jarvis-hook claude session-start
echo '{"session_id":"t1","cwd":"'$PWD'","prompt":"add tests"}' | ~/.jarvis/bin/jarvis-hook claude prompt
echo '{"session_id":"t1","cwd":"'$PWD'","message":"Claude needs your permission to use Bash"}' | ~/.jarvis/bin/jarvis-hook claude notification
echo '{"session_id":"t1","cwd":"'$PWD'"}' | ~/.jarvis/bin/jarvis-hook claude stop
```

Check the daemon is alive: `curl -s --unix-socket ~/.jarvis/run.sock http://jarvis/`

</details>

## Features

### 🖥 Session monitoring

A registry of every interactive Claude Code / Codex session: status (idle / working / waiting / finished / hit the limit), project, branch, model, activity. The menu-bar counter (**◇ ⏸N ⚙M**) is the cross-terminal summary; the ⌘J panel is the detailed view.

<details>
<summary>How it works</summary>

No screen parsing — only structured events from the agents themselves, via their native hooks: `SessionStart` → idle, `UserPromptSubmit` → working, `Notification` → waiting (+a notification), `Stop` → done (+a notification), `SessionEnd` → the session disappears. The registry-reducer lives in the Rust daemon; state survives a daemon restart (`~/.jarvis/state.json`) and live tmux sessions are re-adopted. The pane's screen is read only by the interactive-prompt detector and the remote (a regex, not screenshots).

</details>

### 🔔 Notifications & 🎛 the ⌘J panel

Jarvis's own toast notifications render **over fullscreen**; the panel (`alwaysOnTop`) does not steal focus. The global **⌘J** hotkey opens the panel centered on screen — Esc, a click outside, or ⌘J again closes it.

<details>
<summary>Details</summary>

Click ◇ for the panel. Right-click for the menu (test notification, autostart, quit). The hotkey, panel position, notification toggles and autostart live in settings (⚙ in the panel header), stored in `~/.jarvis/settings.json`. Notification content is composable from segments (branch · model · effort · tokens · duration) with a live preview; a separate toggle speaks notifications only when a Bluetooth headset is connected. Note: the global ⌘J intercepts that key across all apps (Chrome "downloads", VS Code "panel") — change the shortcut in settings if it gets in the way.

</details>

### ↩️ Reply into a session & ⚙️ the remote

In a session's chat the **Reply** field inserts text straight into the session's terminal — even if the window is minimized or on another Space. Below it sits the **remote**: **Model** segments (Opus / Sonnet / Haiku) and **Effort** (auto / low / med / high / xhigh) send a slash command (`/model sonnet`, `/effort high`) into the live session. When an agent asks a multiple-choice question, the panel renders native pickers (multi-select included) instead of making you type numbers into a terminal.

<details>
<summary>How it works</summary>

`npm run setup` installs a PATH shim `~/.jarvis/shims/claude` (pyenv pattern, a managed block in `~/.zshrc` between jarvis markers). After `exec zsh`, every interactive `claude` launch is transparently wrapped in tmux on a **separate server** (`-L jarvis`, config `~/.jarvis/tmux.conf`): no status bar, mouse scrolls, your personal tmux untouched. In iTerm2 — control mode (`-CC`), native tabs. Headless runs (`-p`, pipes, `$TMUX`) are not wrapped. Insertion: `set-buffer → paste-buffer -p → send-keys Enter` (multi-line prompts arrive as one chunk). The command palette (`/` in the reply field) reaches the session's other slash commands.

- A session outside tmux is flagged "outside tmux" — it can't be controlled; the panel shows how to bring it in (`claude --resume <session_id>`).
- Close the terminal window and the agent lives on: the tmux session detaches, Jarvis keeps watching. Reattach: `tmux -L jarvis attach -t <name>`.
- **Model** is free to read (written to the transcript on every assistant turn). **Effort** can't be read from outside — the panel keeps optimistic state, highlighting what it set itself.

</details>

### 🤝 Claude Code and Codex, side by side

If `codex` is found during setup, Jarvis wires it up too: hooks in `~/.codex/hooks.json` and a `codex` shim using the same tmux mechanism. Interactive Codex sessions appear in the panel with a `codex` badge and get the same treatment — status, toasts, voice, chat, reply-into-session, and a resume command (`codex resume <session_id>`).

<details>
<summary>Codex specifics</summary>

Model and reasoning are changed via Codex's own `/model` picker (there is no separate `/effort`). **Headless `codex exec` does not fire hooks** — such runs aren't monitored (by design, same as `claude -p`). On a fresh machine Codex hooks require trust (`~/.codex/config.toml [hooks.state]`); the Codex shim adds `--dangerously-bypass-hook-trust` when the installed `codex` supports it — this **disables trust verification for all Codex hooks** on interactive launches, so keep that trade-off in mind. Codex usage/cost numbers are estimates.

</details>

### 📊 Usage, costs and limits

Jarvis tracks token usage and estimated cost per session, model and project, and understands the rolling usage-limit window. When a session hits the limit, the panel and the voice line tell you when it resets — and, with auto-resume enabled, Jarvis picks the session back up the moment the window opens.

### 🗣 Voice: Jarvis speaks (TTS)

After a session event Jarvis briefly says out loud what happened, via a local TTS engine — currently in Russian. The text is assembled from structural signals by template; numbers are spelled out with correct grammatical agreement.

- turn finished → "Pixela: four of six tasks, now docker-compose" (board present) · "Recru is done, three files changed" (diff present);
- waiting on you → **priority**: "Pixela is waiting — needs permission for Bash"; hit the limit → "Pixela hit the limit, resets in two hours".

<details>
<summary>Engine, config, boundary</summary>

The engine is **Silero** (a local Python sidecar, FastAPI, model held in memory; binds to `127.0.0.1` only), managed by the daemon: started on demand, restarted on crash, stopped when idle and on exit. Speakers: `aidar` · `baya` · `kseniya` · `xenia` · `eugene`. Utterances don't overlap: the queue is serialized, "waiting" jumps ahead of "done", a pile-up of `Stop` lines is coalesced. Config lives in `~/.jarvis/settings.json`:

```json
"voice": { "engine": "silero", "mute": false,
           "events": { "stop": true, "notification": true, "stopFailure": true } }
```

Menu bar: a **"Mute"** toggle and **"Test voice"**. If the engine is unavailable the daemon runs as before, the agents are untouched, the reason is in the log.

⚠️ **Voice license:** the default Russian Silero voice `v4_ru` is **non-commercial** (CC BY-NC-SA 4.0). For commercial use see the [License](#license) section.

</details>

### 🎤 "Hey Jarvis" — the voice assistant *(experimental)*

Say the wake word and talk — no keyboard, no window switching:

- **route a reply by voice** — Jarvis figures out which session you're addressing by content (and shows a picker when unsure), stages the text, and sends it;
- **ask what happened** — "what did the ticketing session do?" gets a spoken summary;
- **quick OS commands** — media play/pause/next, system volume, open an app;
- **general questions** — handed to a separate local assistant agent that can search the web and answers out loud.

Replies are half-duplex: Jarvis pauses listening while it speaks, and notifications wait while you're talking. Wake-word detection (openWakeWord) is included in release builds but **off by default** — enable it in the panel; the detector model is downloaded on demand (non-commercial license — see [License](#license)). There is no speaker verification yet — anyone in the room can say the wake word.

### 🎙 Dictation (push-to-talk STT)

Hold **F8**, speak, release — your speech is transcribed locally and inserted into the active session. The default engine is Qwen3-ASR via an MLX sidecar; Whisper (whisper.cpp, Metal) is built in as an alternative. Mixed Russian/English speech — including inline code terms — is handled.

<details>
<summary>Dictation history and anti-hallucination</summary>

The panel keeps a full dictation history: search, day grouping, stats, transcript enhancement, export, delete — and the last dictations keep their compressed audio, so you can **re-transcribe** them with a different engine. Against STT hallucinations on silence/noise there is a VAD gate (Silero, alpha toggle, off by default), a phrase blocklist, and tuned decoding parameters.

</details>

### ☕ Keep awake · ⌒ Clamshell (power plugins)

A sleeping Mac means frozen agent processes and severed API requests. Two pluggable power plugins (toggles in settings), UX modeled on Raycast Coffee and Amphetamine:

- **☕ Keep awake** — vetoes idle sleep via a power assertion (IOPMAssertion, like `caffeinate`): indefinitely, for a duration (15m…8h), while a process lives, or auto — while agents are working.
- **⌒ Clamshell** — closed-display mode (`pmset disablesleep`): the Mac keeps running with the lid shut. On Apple Silicon this is exactly what plain `caffeinate` can't do.

<details>
<summary>Safety and fail-safes</summary>

The assertion lives in the daemon process: a crash auto-releases it, so a "stuck" sleep block is impossible. Check: `pmset -g assertions | grep -i jarvis`.

Clamshell means root and thermal risk, so the plugin **detects and suggests rather than silently sudo-ing**: after an interrupted sleep it offers closed-display mode; with an external display it advises native clamshell (no root needed). The manual toggle is an honest admin dialog; for silent switching there's "Set up silent mode" (`/etc/sudoers.d/jarvis-pmset`, exactly two commands, validated via `visudo -c`). Fail-safes: the `~/.jarvis/clamshell.json` marker, sleep restored on daemon start/exit, a battery guard (≤15% → restore sleep). A fanless MacBook Air throttles under a closed lid — the plugin warns about it.

</details>

### ✅ Task board (observer)

Sessions running a multi-step plan (`TodoWrite`) show an "N/M tasks" ring → a "Tasks" slide-over: an aggregate, a progress bar, a list with statuses, and where possible the model and duration of the correlated subagent. The board is live: the next `TodoWrite` redraws it on its own.

<details>
<summary>Why read-only</summary>

The source of truth for tasks is the **orchestrator inside the session**. The `TodoWrite` list has no external "mark done" API — it can't be mutated from outside. Jarvis only **reads** the board (`PostToolUse`, last-write-wins) and **displays** it. A task action (go to / skip / restart) is an **instruction to the orchestrator**: it pre-fills the reply field with editable text that you review and send yourself. The board changes only when the next real `TodoWrite` arrives — so you see what the agent actually does, not what we asked for.

</details>

### 📦 Model manager

All local models — TTS voices, STT engines, the wake-word detector — are managed from a single "Models" section in settings: status, size on disk, download, delete, and hot-swapping the active STT engine without restarting the daemon. First-run onboarding offers a model checklist with unified download progress. Nothing is bundled with the app; weights are fetched from their upstream sources into `~/.jarvis/`.

## Jarvis vs neighbors

| | **Jarvis** | Menu-bar monitors | Orchestrators | Claude Code (native) |
|---|:---:|:---:|:---:|:---:|
| State detection | hooks (events) | process polling | spawn their own | itself |
| All sessions in one place | ✅ | ✅ | partial | ❌ |
| Reply into a session | ✅ (tmux) | ❌ | ✅ | — |
| Remote model/effort | ✅ | ❌ | partial | manual |
| Voice (TTS + assistant) | ✅ | ❌ | ❌ | ❌ |
| Usage & limit tracking | ✅ | partial | partial | per-session |
| Anti-sleep + clamshell | ✅ | ❌ | ❌ | ❌ |
| Task board | ✅ read-only | ❌ | ✅ owns it | ✅ TodoWrite |
| Spawns agents | ❌ (by design) | ❌ | ✅ | — |
| License | MIT | varies | varies | proprietary |

The closest in spirit are menu-bar monitors; their edge is zero setup. Jarvis answers with accuracy (hooks, not guessing), reply-into-session plus a remote, voice, and power management.

## How it works

```
claude / codex (any terminal)
  └─ native hooks (~/.claude/settings.json · ~/.codex/hooks.json)  ← installed by setup
       └─ ~/.jarvis/bin/jarvis-hook        ← fail-silent shim, 0.3 s curl
            └─ unix socket ~/.jarvis/run.sock
                 └─ Rust daemon (Tauri) = session registry + effects
                      ├─ toasts over fullscreen · ⌘J panel (always-on-top)
                      ├─ tmux reply & remote · TTS/STT sidecars · power plugins
                      └─ menu-bar counter: ⏸ waiting · ⚙ working
```

The frontend (panel `ui/index.html` + `ui/renderer.js`, toasts `ui/toast.*`) runs in the system WKWebView; the `window.jarvis` contract is implemented by a thin adapter `ui/bridge.js` over Tauri IPC. The main process is Rust (`src-tauri/src/`): a unix socket on axum, the registry-reducer with effects, tmux, transcripts, usage/history, voice, STT, wake-word, the power plugins. State, settings and stats live in `~/.jarvis/` in stable formats.

**Principles:** no screen reading (structured events only) · fail-silent hooks (they never break the agents) · everything local, no telemetry · remove everything with one command (`npm run teardown`). The increment-by-increment development history — specs, plans, mockups — is in [`docs/superpowers/`](docs/superpowers/).

## Tips & recommended setup

- **Install tmux** (`brew install tmux`) — without it Jarvis still monitors and notifies, but can't reply into sessions or drive the remote. In iTerm2 sessions get native tabs (tmux control mode).
- **Pair with an orchestrator.** Claude Squad, Conductor, Crystal, native Agent Teams — they spawn and plan; Jarvis watches everything they (and you) run interactively, in one place.
- **Overnight runs:** enable **Keep awake → auto** (awake while agents work) and let the clamshell guard handle the lid; enable auto-resume so a limit reset doesn't strand the run.
- **Office-friendly voice:** the "speak only into a Bluetooth headset" toggle keeps summaries out of the room's speakers.
- **⌘J clashes** with Chrome's Downloads and VS Code's panel toggle — rebind it in settings if you use those.
- **Commercial use:** the default voice and wake-word models are non-commercial; switch to the commercial-clean set (Silero `v5_cis_base`, Whisper/Qwen3, own or no wake-word) — see [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
- A session started outside the shim shows as "outside tmux" — restart it via `claude --resume <session_id>` to make it controllable.

## Roadmap

Jarvis is a solo-maintained pre-1.0 project; this is a direction, not a promise. The best way to influence it is an [issue](https://github.com/Sergey-Chernyshev/jarvis/issues).

**Voice assistant → hands-free mission control**

- Create a *new* session by voice ("Jarvis, start an agent in ~/myproject") — today voice only routes into existing sessions.
- Acoustic barge-in: interrupt Jarvis mid-sentence without the mic picking up its own speech (echo cancellation via macOS `VoiceProcessingIO`).
- Streaming word-by-word dictation (today transcription happens on release).
- Persistent assistant memory across conversations; a dedicated conversation-history view.
- Speaker verification for the wake word (today anyone in the room can trigger it).
- A full voice HUD.

**Agent platform**

- Ship the agent-chat UI on top of the capability platform — the security gate and the `jarvis-mcp` MCP bridge are already in place.
- A plugin system (manifests, sandboxing, signing) and deeper security layers (span-level provenance, egress control).
- Exposing Jarvis capabilities as an MCP server for external clients.

**Engines & models**

- Fully native TTS — dropping the Python sidecar (Piper via `ort`, or Silero via `ort` with native Russian stress handling).
- Quantized STT models (4-bit Qwen3) once accuracy is validated; production-grade noise suppression (the current toggle is alpha).

**Codex parity** — limit tracking with auto-resume and exact (not estimated) usage numbers for Codex sessions.

**Distribution & polish**

- Proper Developer ID signing and notarization (today's builds are ad-hoc signed).
- A hero demo GIF and screenshots; a prebuilt Intel DMG if there's demand.
- `1.0`: freeze the hook contract and the on-disk formats.

**Exploring** — English UI and English spoken summaries (the interface and voice are currently Russian-first).

**Explicitly out of scope:** spawning/orchestrating agents (by design — see the boundary note), Windows/Linux (deeply macOS-native: IOPMAssertion, `pmset`, WKWebView, MLX), and the Mac App Store (its sandbox is incompatible with hooks and tmux).

## Status & limitations

A pre-1.0 MVP, developed in the open. Deliberate boundaries and known issues:

- **macOS-only**; **Claude Code (CLI)** and **Codex (CLI)** are supported — **interactive** sessions only (headless `claude -p` / `codex exec` don't fire hooks — not monitored); the remote and reply need **tmux**.
- **The UI and voice are currently Russian-first.** Monitoring, notifications, dictation and the remote work regardless of your language; English localization is on the roadmap.
- **Effort** can't be read from outside → the panel keeps optimistic state.
- **Hook schemas drift between agent versions.** If events stop arriving after a `claude` update, compare against the current hooks docs and fix `EVENTS`/the format in `src-tauri/src/bin/setup.rs`.
- A hard-killed terminal (no `SessionEnd`) leaves a session hanging — the panel's "Clear" button removes done/idle ones.
- **Wake-word** ("Hey Jarvis") is experimental: no speaker verification, detector model is non-commercial (see [License](#license)), off by default. The dev build (`npm start`) includes the feature flag but the detector stays inert until enabled in the panel.
- **Agent chat** (the capability platform, MCP bridge `jarvis-mcp`) — the bridge exists, the UI isn't shipped. *In progress.*

## Versioning

Pre-1.0 (**SemVer 0.x**): the hook contract and the on-disk formats (`~/.jarvis/`) may change. `0.MINOR` bumps for features/breaking changes, `0.x.PATCH` for fixes. `1.0` will freeze the hook contract and the state format. Updates never touch your data: everything user-owned lives in `~/.jarvis/`, outside the app bundle — see [docs/release/versioning-and-migration.md](docs/release/versioning-and-migration.md).

## Contributing

Issues and PRs are welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for how to build, test, and open a PR. By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md). Report vulnerabilities privately via [SECURITY.md](SECURITY.md). Specs and per-increment plans live in [`docs/superpowers/`](docs/superpowers/).

Direct pushes to `master` are disabled — changes land only via Pull Request with green CI.

## License

Jarvis's code is licensed under the **[MIT License](LICENSE)** © 2026 Sergey Chernyshev.

**Model weights are downloaded, not part of the repo**, and keep their own licenses — these restrict **use** of the corresponding feature independently of the MIT code:

| Artifact | Used for | License | Commercial |
|---|---|---|:---:|
| Silero `v4_ru` | **default voice** | CC BY-NC-SA 4.0 | ❌ |
| Silero `v5_cis_base` / `_nostress` | voice | MIT | ✅ |
| openWakeWord `hey_jarvis_v0.1` | **default wake-word** | CC BY-NC-SA 4.0 | ❌ |
| whisper.cpp `ggml-*` | speech-to-text | MIT | ✅ |
| Qwen3 (`mlx-community`) | speech-to-text | Apache-2.0 | ✅ |

> ⚠️ The default voice (`v4_ru`) and wake-word (`hey_jarvis`) are **non-commercial**. For commercial use: voice → Silero `v5_cis_base` (MIT); wake-word → train your own or disable it; STT → Whisper or Qwen3. The full breakdown and third-party obligations are in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) (including the BSD-3 attribution for `mediaremote-adapter`).

**Non-affiliation.** Jarvis is an independent open-source project. It is **not affiliated with, or endorsed by, Anthropic, PBC**; "Claude" and "Claude Code" are trademarks of Anthropic, used **nominatively** (to describe compatibility). It is likewise not affiliated with OpenAI; "Codex" is referenced nominatively. Jarvis works on top of the agents' **official hooks**. The project is also unaffiliated with Marvel/Disney; any resemblance to the fictional "J.A.R.V.I.S." is unintentional.
