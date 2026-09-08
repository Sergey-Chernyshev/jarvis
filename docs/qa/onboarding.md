# Onboarding and app icon QA

The preparation window now separates agent integration from local voice capabilities. It supports a complete voice-only path without Claude Code or Codex installed, while keeping native `coreReady` false until agent integration is actually healthy. No models are selected automatically.

The UI uses the native job snapshot as its source of truth. Percentages describe an actual installer stage only; stages without a percentage use an indeterminate bar. Progress events update the same screen without resetting the proxy field, selection or caret. A three-second snapshot fallback recovers a missing completion event. Installation errors, IPC errors and a missing application bridge have explicit recovery states. Back preserves the selection; Escape closes the window and does not cancel an active native job.

The visual treatment is a rounded, translucent neutral surface with a quiet lavender accent, compact step navigation and a custom folded metallic J mark. Motion is limited to screen transitions, an active preparation indicator and completion; `prefers-reduced-motion` removes these animations. The fixed footer remains accessible while long content scrolls.

## Evidence

`node --test ui/onboarding-state.test.mjs` passes 13 state regression tests. They cover stale-job ordering, terminal-state protection, real stage progress, disabled/installed model filtering, explicit voice-only navigation and failure recovery.

`node scripts/qa-onboarding.cjs` launches real headless Chrome against a loopback static server and injects a strict test bridge. Unknown commands fail. It exercises the actual onboarding HTML/CSS/JavaScript and captures light/dark layouts at 560×660, 480×600 and 360×520. The report and screenshots are in `docs/qa/assets/onboarding/`.

The browser scenarios include core and model preparation, a network failure followed by changing the selection and retry, rejected IPC, a failed first read, a missing event subscription recovered by polling, proxy credential masking, proxy persistence before model installation, focus/caret preservation, Enter inside an input, repeated Escape, models unavailable in the build, no CLI installed, voice-only model preparation, reopening a completed voice-only job, and deferring both agents and models. No browser exceptions or unknown fixture commands are permitted.

Browser results validate rendering and UI command contracts using fixtures. They do not validate actual downloads, agent login, native window blur/Spaces or live audio. No native install, recording, login or user settings mutation was invoked by this harness.

Rust regression tests in `src-tauri/src/onboarding.rs` and `src-tauri/src/install/mod.rs` cover native job duplication, stage updates, readiness independent of thread completion, cached artifacts unavailable in the current build, independent voice readiness without CLI, the support matrix and explicit Qwen runtime planning. The model installation entry points reject unsupported builds/platforms before any download or environment creation.

## Review images

- `assets/onboarding/welcome-dark.png`: first-run introduction and original mark.
- `assets/onboarding/preparing-dark.png`: actual 53% stage fixture; the number is test data from the bridge, not a native installation.
- `assets/onboarding/failure-dark.png`: network recovery with masked credentials.
- `assets/onboarding/capabilities-light-360x520.png`: narrow layout with accessible footer and scrollable choices.
- `assets/onboarding/voice-only-ready-light.png`: voice prepared with agents explicitly deferred.

## Reproduction

```sh
node --test ui/onboarding-state.test.mjs
node scripts/qa-onboarding.cjs
cargo test --manifest-path src-tauri/Cargo.toml --bin jarvis onboarding:: -- --test-threads=1
cargo test --manifest-path src-tauri/Cargo.toml --bin jarvis model_support_tests -- --test-threads=1
```

The browser script accepts `JARVIS_PLAYWRIGHT_PATH`; otherwise it tries a local Playwright installation and the bundled Codex runtime. It requires Chrome. It binds only to 127.0.0.1 and blocks non-local requests. Browser launch may need a sandbox exception in an automated environment.

## Original SVG and native icon generation

The source is `src-tauri/icons/jarvis.svg`, an original vector drawing with a graphite rounded tile, folded silver J and a small lavender signal. It is not a symbol from an icon pack. `ui/onboarding-mark.svg` is generated from that source.

```sh
node scripts/generate-app-icon.mjs
```

This uses the installed Tauri icon CLI and `rsvg-convert` (librsvg). The script regenerates the existing desktop PNG, ICO and ICNS files and the onboarding SVG from one source, uses a temporary output directory, and does not build or restart Jarvis. The native window uses 560×660 with a 22 px radius; actual window behavior is tested separately by the native smoke harness.


## Actual native window follow-up

The full-feature debug `.app` also opened the preparation screen through actual
Tauri IPC in the isolated native harness. The observed NSWindow was visible, key,
on the active Space, and exactly 560×660. The single native scenario passed in
1.181 seconds: [report](native-onboarding.json),
[snapshot](assets/native/native-onboarding-welcome.png). This adds actual window
and WKWebView evidence; it does not turn simulated download progress into a real
installer/download test.
