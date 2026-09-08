# Native audio and window verification

## Evidence and limits

| Check | Executed path | Evidence |
| --- | --- | --- |
| Input device discovery | Actual CoreAudio HAL metadata outside the tool sandbox | `native_metadata_probe`: one input device, 69.8 ms. No AudioUnit, capture stream, or permission request. |
| Pending/denied microphone decisions | Deterministic Rust regression tests | `NotDetermined` cannot capture; releasing pending capture removes demand and returns permission guidance. TCC responses were not changed. |
| Cold meeting startup | Actual channels, delayed synthetic PCM, production startup decision | A delayed first frame survives startup. Timeout, denied permission, and cancellation cannot acknowledge recording. |
| Capture restart | Production fan-out with synthetic samples | Frames and state updates from the retired generation are rejected after stop/restart. |
| Cancelled native setup | Actual capture function with an already-cancelled stop flag | Returns before CPAL host/device discovery; later native setup stages and the realtime callback also check cancellation. No input stream was opened. |
| Meeting system-audio delegate | Actual Objective-C provider and CoreMedia buffers | Synthetic float PCM, sample content, PTS, unsupported format, cancellation, non-audio rejection, synchronized close, and late-callback suppression all passed. No SCStream was created. |
| Original dictation destination | Actual AX capture and CGEvent insertion into a separate AppKit helper | Four native checks passed: original field, rejection of a changed field in the same process, confirmed paste, and repeated-paste confirmation. [Report](native-insertion.json). |
| UI and native windows | Debug `.app` harness below | Uses actual WKWebView, Tauri IPC, and native macOS branches; results and screenshots are recorded per run. |
| Voice-history persistence | Real temporary files and concurrent Rust callers | 19 prompt/history tests passed. A failed atomic rename preserves the runtime record/setting; stale retranscription cannot overwrite a manual edit or recreate a deleted row. |

The focused Rust run after these changes passed **46 tests**, with the explicitly opt-in HAL probe ignored in that run. The HAL probe was also run separately outside the sandbox, as described above. Existing compiler warnings concern deprecated CPAL naming and unused code.

No user microphone audio, meeting conversation, or editor content was recorded. The later explicit insertion fixture used only a synthetic marker in its own editor; clipboard restoration is described below. No permission was granted or reset. A real microphone round trip, a permission-dialog interaction, mixed-DPI displays, and moving between user Spaces still need observations in an appropriately controlled environment; unit-test coverage is not a substitute for those observations.

## Why device discovery changed

CPAL 0.17.1 on macOS implements `input_devices()` by probing supported configurations. `Device::name()` delegates to the richer description, which also probes AudioUnits. The frozen application sample showed the main thread inside this path, blocked in CoreAudio while a microphone permission dialog was pending.

`stt/input_devices_macos.rs` now reads the HAL device list, the size of each input-scope stream array, and `kAudioObjectPropertyName`. It does not instantiate AudioUnits. The IPC wrapper runs discovery away from the UI thread with a bounded response deadline. Apple's [AudioObjectGetPropertyData documentation](https://developer.apple.com/documentation/coreaudio/audioobjectgetpropertydata(_:_:_:_:_:_:)) describes this metadata-query API; ownership of the returned name CFString was checked against the installed SDK's `AudioHardwareBase.h`.

## Isolated native UI runner

Build a debug `.app` through the normal project build. The runner intentionally **does not build or restart an existing application**:

```sh
node scripts/native-smoke.mjs \
  --app /absolute/path/to/Jarvis.app \
  --scenario /absolute/path/to/scripts/qa/native-modules.js \
  --timeout 90
```

It copies that bundle into a newly created `jarvis-native-smoke-*` temporary directory, gives the copy a separate bundle identity, signs only the copy, and launches its executable with `--native-smoke <profile>`. The original `.app` and running Jarvis processes remain intact. Only the child created by the runner can be terminated on a deadline.

The app accepts this mode only in a debug build, with the explicit launch argument, a validated temporary-directory marker, and `JARVIS_DIR` pointing to the profile's own `data` directory. Changing a production environment variable cannot enable it. The profile starts with muted audio, disabled wake word, no models or sessions, and no updater. The main WKWebView uses a nonpersistent WebKit data store.

The real `Daemon`, renderer, platform code, and IPC commands are used. These external effects are excluded and reported in every result:

- Automatic hook reconciliation and custom shims.
- Remote connections, global hotkeys, background timers, and updater.
- Sidecar processes and startup voice synthesis. Sidecar health checks use port zero instead of the user's 8731/8732 services.
- Live microphone/system-audio capture. An attempted recording returns an explicit test-mode error.

The harness does not claim these excluded integrations work. Scenarios do not invoke destructive installation commands or launch external agents. Ordinary journeys do not write the shared clipboard. The explicit `--editor` journey below additionally exercises production paste into its owned synthetic editor and conditionally restores the clipboard. The root journey covers navigation/settings and read-only device discovery. The native MCP companion uses the actual daemon server in the same isolated profile.

Scenario files are JavaScript bodies executed inside an async function in the actual WKWebView. They can use `window.jarvis`, `document`, and:

```js
await t.step('Device list remains responsive', async () => {
  const result = await window.jarvis.sttInputDevices();
  t.assert(Array.isArray(result.devices), 'Device list is malformed');
  t.evidence('deviceCount', result.devices.length);
});
await t.waitFor(() => document.querySelector('#launcher'));
await t.screenshot('home');
```

`t.invoke(command, args)` calls real Tauri IPC. `t.geometry(selector)` returns DOM rectangles and visibility. Exceptions, failed assertions, browser errors, and unhandled rejections fail the report. WebKit error reports include both the message and stack, because its stack alone can omit the assertion reason. The runner preserves `report.json`, `process.log`, page-load `stages.log`, and screenshots in the temporary profile for review.

Screenshots use [WKWebView.takeSnapshot](https://developer.apple.com/documentation/webkit/wkwebview/takesnapshot(with:completionhandler:)) and PNG encoding on the app's own view. They neither capture other applications nor require Screen Recording permission. They verify WebKit layout and rendering; they do not photograph the wallpaper or the native material behind the webview.

The isolated main window explicitly disables background WebKit throttling and receives focus on launch. The first run exposed suspended WebContent processes while an unbounded animation-frame wait stalled the scenario; the wait is now bounded. Scenarios also wait for their finite CSS animations and final opacity before visual snapshots. Ordinary application background throttling is unchanged.

`scripts/qa/native-audio-windows.js` adds current native permission observations, read-only HAL discovery, a fixed synthetic history row with actual IPC/file round trips, and a synthetic HUD through production event/layout code. It verifies the HUD's actual NSWindow focus, current-Space membership, and native display coordinates; `t.screenshot(name, 'toast')` snapshots its own WebView. These observations do not prove a manual Space switch or a second monitor.

Its AX step only runs when the isolated process already has Accessibility permission and owns foreground focus. Two synthetic textareas exercise the production retained-element `CFEqual` comparison. If authorization or AX identity is unavailable, the report explicitly records `axVerification.verified: false`; no permission is requested and no paste or clipboard operation is performed. An overall UI pass must not be cited as proof of AX in that case.

## Native CoreMedia fixture

This compiles the exact production Objective-C delegate into a separate executable. It constructs synthetic `CMSampleBuffer` values and invokes the delegate directly. In addition to PCM and timestamp checks, two actual dispatch queues verify that `close` cannot return while a callback holds the context, and that callbacks after close cannot deliver data.

```sh
xcrun clang -fobjc-arc -fblocks -Wall -Wextra -Wno-unused-parameter \
  -framework Foundation -framework CoreMedia -framework ScreenCaptureKit \
  scripts/qa/system-audio-fixture.m -o /private/tmp/jarvis-system-audio-fixture
/private/tmp/jarvis-system-audio-fixture
```

Executed successfully on this Mac, including a second build with `-fsanitize=undefined` and no UBSan findings. This fixture never constructs an `SCStream`, calls `SCShareableContent`, requests a permission, or acquires live audio. It proves the native buffer/lifetime handling, not end-to-end capture from a meeting application.

The AddressSanitizer build did not reach `main`: a native sample showed recursive ASan initialization through `get_dyld_hdr` / `_Block_copy`, spinning in `StaticSpinMutex` on this macOS/Command Line Tools combination. That isolated fixture was terminated; ASan is not counted as a passed check. The regular and UBSan executions are the native results above.

Current official [Tauri testing documentation](https://v2.tauri.app/develop/tests/webdriver/) also supports macOS through an embedded `tauri-plugin-wdio-webdriver` server with WebdriverIO. Direct `tauri-driver` alone still lacks a macOS WKWebView driver. The lightweight local harness provides immediate isolated testing without adding that dependency stack; the same isolation rules would apply if WDIO is added later.

## Recording lifecycle rules checked

- `NotDetermined` means permission is pending, not authorization to open capture. Explicit dictation/meeting actions ask the user to retry after answering the system dialog.
- A wake subscription with continuing demand can start on a later supervisor tick after authorization. A released/cancelled request has no demand and cannot start later.
- `starting` is distinct from `listening`; a meeting acknowledges readiness only after receiving and saving its first frame.
- A stop invalidates the generation before detaching the capture threads. Late callbacks from the old generation cannot alter the new recording's state or data.
- No capture-thread joins occur under the lifecycle lock. CoreAudio teardown can be slow; it cannot hold the application's audio controls hostage.

## Repeatable Rust checks

```sh
cargo test --manifest-path src-tauri/Cargo.toml -p jarvis --bin jarvis \
  --no-default-features -- stt::hub:: stt::mic_permission:: stt::dictation:: \
  meetings::tests:: --test-threads=1

# Explicit read-only HAL query; no input stream or permission request.
cargo test --manifest-path src-tauri/Cargo.toml -p jarvis --bin jarvis \
  --no-default-features -- native_metadata_probe --ignored --nocapture
```

In ordinary Rust tests macOS AX and TCC are deliberately stubbed. Native UI reports come from a non-test debug executable and therefore exercise the actual macOS implementations. Always state which kind of result is being cited.


## Final native journeys and actual external insertion

The final production-feature debug bundle passed all **29 module/settings steps**
([report](native-modules.json)), including all eight modules and 14 settings panes,
actual persistent settings changes, failed-model rollback, and scoped parameter
search. The **five-step HUD/history journey** ([report](native-audio.json)) observed
an authorized microphone, existing Accessibility permission, one HAL input device
in 72 ms, actual history edit/delete IPC, and native HUD focus/Space/geometry. It
requested no new permission and started no capture.

The same-process WKWebView AX observation was unavailable and is recorded as such.
A separate AppKit helper then exercised the real external application boundary:

```sh
node scripts/native-smoke.mjs --app /absolute/path/to/debug/Jarvis.app \
  --scenario /absolute/path/to/scripts/qa/native-insertion.js --editor --timeout 45
```

All **four checks passed in 1.127 seconds** ([report](native-insertion.json)). The
production AX code retained the first text field. Focusing the second field in
the same PID refused the stale target without sending paste. Returning to the
first field allowed actual CGEvent Cmd-V, with AX confirming the exact synthetic
marker `Jarvis native paste QA 👋`. A second insertion confirmed two occurrences;
the other field remained empty. No unrelated application's AX field is accepted
by the helper commands.

`native-editor.m` retains previous clipboard formats only in memory and restores
them on exit only if no intervening user copy changed the clipboard. It receives
only fixed focus/state/stop commands and self-exits after 45 seconds. The debug
runner owns and cleans up this child. The production release cannot enable this
fixture mode.

## Actual local Whisper/Metal decode

An explicit opt-in test decoded a six-second, 16 kHz mono PCM WAV twice through the
production Whisper engine and its cached Metal context. The existing
`ggml-large-v3-turbo-q5_0.bin` was read without downloading or changing it. macOS
`say` generated the synthetic English fixture; no microphone or speaker was used.
Both decodes passed in **9.50 seconds**, producing:

> Hello. This is a test of local speech recognition. The quick brown fox jumps over the lazy dog.

```sh
JARVIS_QA_WHISPER_MODEL=/absolute/path/to/ggml-large-v3-turbo-q5_0.bin \
JARVIS_QA_WHISPER_WAV=/absolute/path/to/synthetic.wav \
JARVIS_QA_WHISPER_EXPECT='local speech recognition' \
cargo test --manifest-path src-tauri/Cargo.toml \
  --features wakeword-ort,whisper-native,stt-vad --bin jarvis \
  stt::engine_whisper::tests::real_whisper_decodes_explicit_synthetic_speech \
  -- --exact --ignored --nocapture
```

An earlier ordinary unit test implicitly loaded the user's model and attempted GPU
allocation under the restricted tool sandbox, crashing in Metal. That test now
uses an explicit missing-model fixture to check engine switching; real model/GPU
testing is opt-in with explicit paths. The permitted actual Metal run above passed.
This is real recognition of synthetic speech, not a live microphone round trip.
