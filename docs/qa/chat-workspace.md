# Chat workspace verification — 2026-09-05

## Automated checks

- `node --test ui/*.test.mjs`: 175 passed, including 5 new project/attention state tests.
- `node scripts/qa/panel-browser.cjs`: 39 existing panel checks/captures, no page errors or unknown bridge calls.
- `node scripts/qa/session-workspace.cjs`: 23 checks/captures, no page errors or unknown bridge calls. [Report](assets/session-workspace/report.json).
- `cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --bin jarvis`: 930 passed, 4 explicitly ignored. Later launch binding changes also passed the targeted 18-test launch suite.
- Targeted terminal suite: 12 passed.
- Disposable real SSH → jarvis-node → isolated tmux fixture: 12 protocol scenarios and 7 lifecycle checks passed, including screen read, explicit key acknowledgement and an ended pane error. No real agent prompt or user terminal was used.
- `git diff --check`: clean at completion of this task.

The browser checks use a strict synthetic bridge. They verify the actual HTML,
CSS and JS, but do not prove native Tauri IPC or live provider authentication.
The SSH fixture verifies the node transport separately. Packaging and native
fullscreen verification are coordinated with the parallel Jarvis redesign task.

## Regressions covered

Project identity includes machine and directory. A local same-path session cannot
satisfy a pending remote launch. A `launchId` waits for the corresponding initial
message delivery event; that event can arrive before the launch response.
Failed delivery retains the original task. Retrying a failed launch is explicit.

Chat drafts survive session switches without sending. Late terminal responses
cannot overwrite another chat. Terminal reads begin only after disclosure and
keyboard actions are separate calls. Token detail starts collapsed and labels
cost as an estimate. Attention survives repeated snapshots and does not resurrect
read questions after reconnect. A failed remote chat open does not mark it read.

## Visual checks

Desktop: 1280×850. Narrow window: 600×700. Light and dark themes inspected.
These screenshots contain synthetic QA sessions, not user conversations.

- [New chat, dark](assets/session-workspace/welcome-dark-desktop.png)
- [Conversation, dark](assets/session-workspace/chat-dark-desktop.png)
- [Conversation, narrow](assets/session-workspace/chat-dark-narrow.png)
- [Conversation, light](assets/session-workspace/chat-light-desktop.png)
- [Remote terminal](assets/session-workspace/terminal-remote-desktop.png)
- [Usage disclosure](assets/session-workspace/usage-details-desktop.png)
- [Attention inbox](assets/session-workspace/attention-inbox-desktop.png)

## Composer and navigation regression checks — 2026-09-05

- Full UI suite: 331 passed after the composer changes.
- `node scripts/qa/chat-input.cjs`: 9 browser scenarios passed with no page errors.
  Covers stable recent button identity across polling, PDF on a new chat, immediate
  startup feedback, retained attachment after launch failure, remote DOCX host
  routing, immediate reply feedback, draft/file restoration, image paste and file drop.
- `cargo test --manifest-path src-tauri/Cargo.toml --bin jarvis attachments::tests`:
  3 passed: confined names, local document bytes and extension, binary stdin through
  the remote upload shell script (executed locally in the test).
- Additional DOM tests exercise delayed chat responses after navigation, immediate
  sends before IPC resolves, selected model persistence, and a delivery failure
  arriving after the launch has already connected.

These checks do not send prompts to a real agent. The upload shell test does not
establish a real SSH connection. Native packaging and app restart are coordinated
with the chat/terminal task.

## Loading and recovery states

The chat workspace now distinguishes initial loading, confirmed empty results,
errors with retry, and successful content. History uses a transcript-shaped
skeleton; an eight-second wait adds explanatory copy without cancelling the
request. Sidebar/recent skeletons are initial-load only, so background refreshes
preserve content and focus. Startup shows preparation, agent launch and connection
stages from actual operations. Attachment reads have immediate placeholders;
upload counts use completed files. Send buttons and message status show delivery
progress, failures preserve drafts and expose retry, and usage has loading,
empty, error and retry states. Animations respect reduced-motion preferences.

- `npm run test:ui`: 335 passed.
- `node scripts/qa/chat-input.cjs`: 14 scenarios passed, no page errors; includes
  initial list/history skeletons, startup stages, busy send controls, dark/light
  themes, 600px width and reduced motion.
- Batch upload failure waits for in-flight siblings, preventing late progress
  callbacks from replacing an error with a stale loading indicator.

Deployment verification: the production app bundle was rebuilt with voice features,
passed `codesign --verify --deep --strict`, and was restarted through the app.
The combined build also contains the Teleport startup timeout fix. Targeted
`cargo test --bin jarvis remote::`: 83 passed, 2 ignored. Both configured node
health endpoints answered successfully after restart.

## Default project location

New-chat location preferences persist the last selected machine and a separate
absolute project path per machine. Manual input, project actions and launch
capture the source directory; the launch result's worktree directory does not
replace it. A missing/offline saved host retains its own path and cannot launch
on an implicit local fallback. Existing-chat continuation remains source-bound.

Validation: 337 UI tests passed; chat-input browser suite passed 15 scenarios,
including restoring the selected machine and project path after page reload.

## First message before transcript synchronization

The launch task is registered as an outgoing message before opening its bound
session. Outgoing records and their delivery state belong to the session, so
navigation cannot discard them. Transcript echoes replace their pending copies;
previously observed identical messages cannot acknowledge a newer reply. A working
session with no synchronized transcript shows a synchronization state.

Validation: 337 UI tests passed; 19 headless browser scenarios passed without
page errors, including an empty remote transcript at launch, navigation before
acknowledgment, and a queued repeated-text reply. Screenshot:
`output/chat-input/first-message-before-sync.png`.

## Artifact previews and annotations

File selection now holds the quick panel across the native focus transition;
selection/cancellation releases the hold. Draft and sent attachments, assistant
file links and file actions open an in-app preview. Image/PDF/text/Markdown/static
HTML and macOS Word text previews share the session-bound local/remote reader.
Comments support image points and selected text, durable editing/deletion, and
explicit insertion into the source chat draft. See `docs/artifacts.md` for format
limits and storage behavior.

- Full UI suite: 337 passed.
- Headless artifact suite: 18 scenarios passed, no page errors.
- Headless chat-input suite: 19 scenarios passed, no page errors.
- Native artifact tests: 5 passed, including real DOCX extraction.
- Native upload tests: 4 passed, including EOF delivery through async stdin.
- Image/Markdown/PDF/light-narrow screenshots: `output/artifacts/`.

The upload regression exposed an existing wait-for-EOF deadlock: closing the
writer explicitly before awaiting the remote process now lets `cat` finish.
Native WKWebView dialog behavior and a live SSH host were not exercised by these
headless fixture checks; no user-facing windows were opened for testing.
