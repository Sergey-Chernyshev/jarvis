# Chat controls and capability states

Implemented and checked on 2026-09-05 as part of the settings and chat clarity update.

## Behavior

- Managed idle/completed sessions offer a model selector. Claude labels resolve to exact allowed values. Codex uses suggestions from the selected local account's model cache, preserving exact model identity.
- New tasks pass the selected model as `--model` before the initial prompt; the chosen account and permissions remain bound to that task. The same command-building path is used on local and remote machines.
- Codex model changes use the actual native picker. The implementation opens the fixed `/model` command, recognizes the exact model row, verifies the moved selection, recognizes its reasoning screen and verifies the final native confirmation. It does not send `/model <slug>` or optimistically pretend that a model changed.
- Model changes reject external sessions, busy sessions, custom agents and nonempty/unknown native composers. The model path never uses Ctrl-U to delete a Codex draft. An unrecognized picker stays open with a visible terminal and an explicit explanation.
- Working/waiting/limited sessions explain why configuration controls are unavailable. Codex does not expose Claude's unsupported standalone `/effort` command.
- External local and remote sessions display a blue read-only state and hide the composer and terminal controls, even if stale pane metadata exists. Statistics remain readable and source-scoped.
- A missing managed terminal is a distinct unavailable state. An offline managed machine permits drafting but prevents sending; it does not discard the draft or claim the chat was started externally.

## Verification

`node scripts/qa/chat-capabilities.cjs` passed 9 Chromium checks against production UI scripts with a synthetic bridge. Tests cover model selection, rejection rollback, in-flight refresh, working state, account-scoped model suggestions, local/remote external sessions, disconnected drafts, missing terminal and model/account launch parameters. No user session or live provider was controlled by these browser fixtures.

`python3 scripts/qa/codex-model-fixture.py --probe src-tauri/target/debug/deps/jarvis-4f55feb3d96caf77` ran the compiled production selector against real Codex CLI 0.153.1. The fixture used its own HOME, CODEX_HOME, tmux socket and localhost Responses API, with no account authentication or external provider. The negative case preserved the native draft and made no provider request. The positive case changed from gpt-5.6-terra to gpt-5.6-sol with high reasoning; the resulting actual provider requests contained `model: gpt-5.6-sol`. All fixture processes were stopped afterwards.

The exact-slug display change preserves the existing approximate GPT-5-family cost coefficient; it does not revise pricing or account quotas.

Evidence: [browser report](assets/chat-capabilities/report.json), [native model probe](assets/chat-capabilities/native-model-probe.json), [read-only chat](assets/chat-capabilities/readonly-dark.png), [new task model](assets/chat-capabilities/new-task-model.png).

The native model integration was exercised locally. Remote model selection uses the same checked picker and the existing node key/screen transport; this update did not run a live remote Codex model change. Unknown picker variants or placeholders are rejected with a terminal fallback rather than guessed.
