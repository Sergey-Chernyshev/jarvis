# Jarvis product validation

Final local acceptance record for `codex/jarvis-redesign`, 2026-09-05. The full-feature
macOS app was built, signed, launched with the normal user profile, and sampled:
the main thread was waiting in the AppKit run loop rather than blocked in CoreAudio.
A passing check establishes its stated scope; it is not a claim that every possible
hardware, account, or external service combination is bug-free.

## Acceptance matrix

| Area | Result and actual evidence | Remaining practical limit |
| --- | --- | --- |
| Navigation | Vertical searchable module list replaces top tabs; all eight modules, keyboard selection, nested Escape/back, search and selection restoration. [29-step native journey](native-modules.json), [39 browser records](assets/panel/report.json). | Browser data is synthetic; native profile is isolated. |
| Appearance/settings | All 14 settings panes in actual WKWebView; dark/light, small/large browser layouts, empty/error/loading states, scoped errors, parameter search, focus preservation, real settings write and failed-model rollback. | Native snapshots show WebView rendering, not the wallpaper behind the window. |
| Preparation/icon | Animated preparation, voice-only path, stage/retry/error fixtures and keyboard handling. [13 state regressions and browser audit](onboarding.md); [actual 560×660 native window](native-onboarding.json). Original [SVG icon](../../src-tauri/icons/jarvis.svg) generates packaged icons. | Fresh model downloads and account login were not performed. |
| Agents | Actual Codex request, streaming completion, session resume and MCP call passed. Claude expired-auth failure is surfaced correctly. [Provider and event audit](remote-agents.md). | A successful Claude turn/resume requires working Claude authorization. |
| Remote/VM | Actual SSH/tmux/node reconnect, events, reply and Unicode/file cases; actual Linux arm64 node build, 38 unit tests and nine protocol checks inside a Lima VM. [Evidence](remote-agents.md#actual-linux-vm-follow-up). | Fresh provisioning, systemd upgrades, cloud VM and provider login inside the VM remain unverified. |
| Automation/teams | Cancellation reaches child processes, resumed state is retained, concurrent operations serialized, dirty worktrees preserved, explicit Claude/Codex selection. [Actual Git/tmux fixtures and regressions](loops-bundle.md). | Provider executables are synthetic in loop/team fixtures; full remote team creation/merge is unverified. |
| Dictation/insertion | HAL discovery stays responsive; actual Whisper/Metal decodes synthetic speech twice. [Four actual AX/CGEvent checks](native-insertion.json) confirm paste into the original external editor field, repeated paste, and rejection after field change. [Audio details](audio-native.md). | A microphone-to-recognition-to-paste round trip and new TCC permission-dialog interaction were not performed. |
| Notifications | A real nonactivating NSPanel hosts a persistent morphing HUD. [Fullscreen follow-up](fullscreen-notifications.md): 24 native steps across Regular/Accessory modes, four real pastes, 441 independent WindowServer samples and actual intermediate WebKit animation frames. [Motion/transparent-edge audit](toast-motion.md). | Multiple physical displays and the global-hotkey/microphone pipeline are outside this HUD fixture. |
| Meetings | Disk streaming/recovery/cancellation regressions, production CoreMedia delegate with synthetic buffers and concurrent callbacks; normal and UBSan runs passed. [Evidence](audio-native.md). | A live microphone + ScreenCaptureKit meeting recording and long real meeting transcription remain unverified. |
| MCP | [Seven actual daemon/stdio/gate checks](assets/native-mcp/report.json): negotiation, registry, archive read, missing/invalid data, denied mutation and absent token. | Archive was empty. No live recording or privileged mutation was authorized by this fixture. |
| Usage | Unsupported headless `/usage` output no longer blocks remote fallback or masquerades as quota; cancelled refresh releases its reservation; failed refresh clears stale values. | Installed Claude 2.1.258 returned a cost summary instead of limits. Jarvis reports unavailable limits; it cannot infer subscription percentages from spend. |
| Packaging | Full `wakeword-ort,whisper-native,stt-vad` debug and release bundles built; offline icon resources packaged; strict code signature verification and launch passed. | Local ad-hoc signed build, not a published/notarized release. |

## Final regression run

```sh
cargo test --manifest-path src-tauri/Cargo.toml \
  --features wakeword-ort,whisper-native,stt-vad --workspace
node --test ui/*.test.mjs
git diff --check
```

The final production-feature workspace run passed **1,028 tests**, with six
explicitly opt-in tests ignored in the ordinary run: application 915, MCP 8,
setup 67, node 38. The UI suite passed **165 tests**. Focused/integration reruns
below overlap parts of these suites and are not added to that total.
Machine-readable counts: [regressions.json](regressions.json).

The final release replaced the previous process through graceful SIGTERM and
`open` after `codesign --verify --deep --strict` passed. One release process was
observed (PID 15091 for this run). Its two-second sample recorded 1,742 main-thread
samples in the normal AppKit event loop, with 1,713 waiting on the run-loop Mach
port. No synchronous audio configuration wait appeared on that thread. The new
quota error wording also appeared in the actual release log after launch.

Native runs use `scripts/native-smoke.mjs` with the normal debug `.app` and
separate temporary profiles. They execute the actual WKWebView, Tauri IPC,
settings persistence and macOS window code. Background compositor suspension
when the user returns to another app can pause animations; static native snapshots
finish finite animations explicitly in that case. Browser checks exercise motion
without that snapshot policy. See the scope fields in each JSON report.

The insertion helper is a separate AppKit process containing only two synthetic
fields. It restores the previous clipboard formats from memory if no intervening
copy occurred. The Linux VM was initially stopped, started for the audit, and
returned to stopped state. Fixtures do not overwrite user conversations or
repositories. Live meeting audio was not collected.

## Runtime decision

Keep Tauri for this revision. The reproduced freeze was a synchronous CoreAudio
configuration probe on the main thread while microphone permission was pending.
Read-only HAL metadata discovery now runs off that thread with a bounded IPC
response. The actual full-feature WKWebView journey and release main-thread sample
passed afterward. Rewriting the entire application would not itself address that
native boundary and is not justified by this reproduction.

The design is a searchable vertical command list, a module screen, and a retained
back stack. Dialogs/pickers and local searches consume Escape before navigation.
There is no horizontal module row. Neutral translucent chrome, rounded surfaces,
Phosphor icons, restrained color and short transitions are shared across modules.

## Reproduction references

- [Native audio, insertion, Whisper and window commands](audio-native.md)
- [Fullscreen notifications and animated transitions](fullscreen-notifications.md)
- [Remote SSH, Linux VM and real provider commands](remote-agents.md)
- [Automation and team fixtures](loops-bundle.md)
- [Preparation and icon generation](onboarding.md)
- [Voice archive/dictionary/transform audit](voice-history.md)
- [Design and implementation notes](../redesign.md)

References: [Raycast search](https://manual.raycast.com/search-bar),
[keyboard behavior](https://manual.raycast.com/keyboard-shortcuts),
[Tauri testing](https://v2.tauri.app/develop/tests/).
