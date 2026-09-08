# Jarvis workspace redesign

Base: `origin/master` at `6ab8f09`, branch `codex/jarvis-redesign`.

## Product and interaction

Jarvis has three everyday jobs: work with agents, dictate into an application,
and keep meeting recordings. These have separate entry points and clear state.
Projects continue to support local work and existing SSH remote nodes. Advanced
automation, multi-agent work and usage remain accessible in the navigation.

- New installations open a regular resizable window and follow the system theme.
  Existing appearance preferences are preserved. Window/overlay mode remains in
  Settings → Appearance.
- Translucent native window chrome surrounds a more opaque reading area; rounded
  surfaces and short transitions share one stylesheet. Reduced-motion and
  reduced-transparency preferences have explicit fallbacks.
- Phosphor Icons 2.1.2 replaces the navigation and settings/voice icon sets. Its
  WOFF2, stylesheet and MIT license ship in `ui/vendor/phosphor`; no CDN is used.
  Run `npm run icons:sync` after updating the package.
- The home screen is a searchable vertical list of modules and recent chats. Each module opens as the next screen; Escape returns through the stack and retains search, selection and drafts. No horizontal module tabs remain.
- `Cmd/Ctrl+K` searches sections and current chats. Arrow keys select, Enter opens,
  Escape closes and restores focus. `Cmd/Ctrl+Shift+K` opens contextual actions.
  Command-letter shortcuts also work with a Cyrillic keyboard layout. Editors,
  IME composition and shortcut recording own their key events.
- Settings search indexes individual parameters, including proxy and autostart, and focuses the matching row. Errors belong to the originating section. Failed persistence is returned through IPC; toggles and selectors revert, and failed saves never publish an audio engine/runtime change.

## Reliability changes

Dictation captures the original process and, when available, AX field identity
before showing its HUD. A changed application or field uses clipboard recovery.
Paste is never reported as confirmed without observing a change to the original
AX element. `pasteSent`, `inserted`, `copied` and `saved` are separate facts.
The former 120 ms clipboard restore race is removed; dictated text stays available
for manual paste. The success message reflects durable history write success.

Notifications are shown without activating Jarvis. The overlay chooses the
foreground window's monitor and applies Space/Stage Manager policy together with
positioning and display on AppKit's main thread. Voice phases reuse one animated
HUD; a meeting has a separate persistent recording indicator and Stop button.

The provider/event boundary validates hook envelopes and current turn identity.
Repeated terminal events and late tool output cannot reopen completed work;
old asynchronous summaries cannot overwrite a newer turn. Claude and Codex
streams terminate busy state on failure/EOF and preserve the session ID.
JSONL tailing retains raw partial bytes, preserving split UTF-8, uses the exact
history cursor, handles rewritten files and rejects stale chat-open requests.
Remote event pages are ordered and deduplicated before delivery.

This keeps the existing provider architecture. It does not claim a complete
migration from hooks to Codex app-server or a Claude SDK. Hooks without turn IDs
still cannot identify every historical event unambiguously.

## Meetings and MCP

The Meetings screen starts recording only after the user chooses a source and
presses Start. Sources are the configured microphone, or microphone plus system
audio on macOS 13+. Recording streams to local WAV files rather than keeping a
whole meeting in memory. Online mode preserves both source tracks, then mixes
them for transcription. Stopped recordings use bounded STT chunks and retain
recoverable audio on failure. There is no automatic insertion into another app.

The existing local `jarvis-mcp` bridge gains read-only `meetings.list` and
`meetings.get`, subject to the existing grant/authorization mechanism. It cannot
start a recording. See [Meeting implementation and verification](meetings.md)
for permissions, storage, limitations and manual checks.

## Reference decisions

- [T3 Code provider adapters](https://github.com/pingdotgg/t3code/blob/91c66ac43ddad4f8697887009746f4de11736cb9/apps/server/src/provider/Services/ProviderAdapter.ts)
  informed the provider boundary; [its lifecycle model](https://github.com/pingdotgg/t3code/blob/91c66ac43ddad4f8697887009746f4de11736cb9/docs/internals/overview.md)
  separates turn completion from later effects.
- [T3 keyboard handling](https://github.com/pingdotgg/t3code/blob/91c66ac43ddad4f8697887009746f4de11736cb9/apps/web/src/keybindings.ts)
  informed context-aware keys and non-Latin layout fallback.
- [Wispr Notetaker](https://docs.wisprflow.ai/articles/9238501024-recording-a-meeting-with-notetaker-beta)
  informed explicit microphone/system sources and the recording indicator.
- [Raycast's native window design](https://www.raycast.com/blog/a-technical-deep-dive-into-the-new-raycast)
  informed keyboard-first interaction and translucent chrome.
- [Phosphor Icons](https://phosphoricons.com/) provides the installed icon pack.

## Verification

The current acceptance record and reproducible commands are in [QA](qa/README.md).
It distinguishes real WKWebView/IPC, actual macOS audio/AX, real SSH/Linux/tmux,
real Codex/MCP, and browser fixtures. It also records checks that require external
credentials or hardware/permissions; a synthetic fixture is never reported as a
successful provider or live meeting.

## Native freeze found after launch

Opening Voice Input exposed a native deadlock missed by the browser fixture:
`stt_input_devices` ran on AppKit's main thread, and CPAL's input-device filtering
opened an AudioUnit while checking capabilities. CoreAudio waited for microphone
permission while Jarvis could no longer service the permission dialog.
The sampled process was blocked in `AudioUnitSetProperty` / `HALC_ProxyObject`.

macOS device discovery now reads CoreAudio device metadata without probing an
AudioUnit. The IPC also runs discovery in a blocking worker, limits outstanding
probes to one and returns a recoverable error after four seconds. The settings
screen renders available controls while discovery is pending and preserves the
selected device when discovery fails. Model inventory is also read off the UI
thread because computing directory sizes may be slow.

STT engine and wake-word changes also run in a serialized blocking worker; the
reservation stays with the worker if its IPC request is cancelled. Process waits
and Whisper/Metal teardown release status locks first.

The full-feature native journey now opens all eight modules and fourteen settings panes through actual WKWebView/IPC. Input-device discovery returned in 72 ms in the audio journey without capture. The original permission-dialog deadlock is no longer in this path. See [native module results](qa/native-modules.json) and [audio results](qa/native-audio.json).
