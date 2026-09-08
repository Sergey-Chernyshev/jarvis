# Settings and chat controls

The refinement separates everyday choices from configuration details. Profiles show identity, observation status and hook health; paths, trust diagnostics and renaming open on demand. Additional CLI agents use labeled forms with an explicit save action. Settings navigation is grouped by purpose and remains searchable.

Visual direction: macOS system type for labels and prose; monospace only where a technical value benefits from it. Graphite `#191D25`, raised slate `#202630`, mint `#70DFAD`, blue `#8BBFFF`, amber `#F2BD68`, foreground `#F2F5FA`. Accent choice remains user configurable; semantic colors distinguish connections, successful setup and attention states. Main content is constrained in width; editing panels have visible labels, spaced fields and one primary action.

Review criteria:

- Default views should answer what is connected and what needs attention without displaying implementation paths.
- Form errors preserve input and existing configuration; refresh failure after a successful save must not enable stale overwrites.
- Escape closes an inner editor or menu before navigating away from settings.
- Read-only and disconnected chats do not offer actions that cannot be delivered.
- A model change must be delivered to the intended provider and verified, never sent as a user prompt or marked successful optimistically.

Validation evidence is collected under `docs/qa/assets/settings-clarity`, `settings-refinement`, and the dedicated profile/chat browser reports. Browser fixtures render the production UI with synthetic bridges; they do not by themselves prove native IPC, SSH or agent behavior. Native acceptance and an isolated real Codex model-selection probe cover those separate boundaries.

## Final acceptance — 2026-09-05

| Boundary | Result |
| --- | --- |
| Combined Rust application tests | 1153 passed; 12 opt-in tests excluded |
| UI tests | 299 passed |
| Broad browser chat flow | 25 recorded checks/snapshots, no JavaScript errors or unknown IPC |
| Focused profile / custom CLI / keyboard / chat browser checks | 17 / 7 / 5 / 9 passed |
| Light/dark settings at 650, 900 and 1280 px | 7 checks passed |
| Native WKWebView modules/settings | 29 passed |
| Native real profiles, current Desktop chat and model catalog | 5 passed, including select control sizing |
| Real isolated Codex 0.153.1 | Model change verified in actual provider requests; terminal draft preserved by refusal |
| Final bundle | Strict signature verification passed; source hashes unchanged during build |

The verified release was launched locally from `src-tauri/target/release/bundle/macos/Jarvis.app`. SHA-256: `e041f0025cc5acb31556d4c8b3df1a4b29152ab1a53dcbb0bcce17ee383db9ec`. Production acceptance confirms this task is associated with `codex-personal` and is read-only. The initial session count is a startup snapshot while background discovery continues.

Final reports: [native profiles](assets/settings-clarity/native-profiles.json), [native modules](assets/settings-clarity/native-modules.json), [running application](assets/settings-clarity/production.json), [source manifest](assets/settings-clarity/source-manifest.json), [actual model-selection probe](assets/chat-capabilities/native-model-probe.json).

Native test profiles are disposable and do not repair real hooks or launch real agent tasks. Their static screenshots can finish finite animations when WebKit is occluded; they are layout evidence, not a motion benchmark. Remote model switching uses the same checked picker and existing SSH transport, but this pass did not exercise a live remote model switch or install a new VM.
