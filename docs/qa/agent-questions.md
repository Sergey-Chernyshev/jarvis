# Agent question delivery

Question answers carry the request ID, content revision, submission ID, question IDs and option IDs. The backend validates cardinality, custom-answer capability and exact ownership before input. One in-flight sequence is allowed per session; an unknown result cannot be replayed. A correlated provider acknowledgment wins a concurrent timeout. Drafts remain in memory per session/request/revision and survive closing or returning through the wizard.

The question drawer supports single selection, multiple selections, multiline text, Back, review/edit, keyboard navigation and explicit submission. Enter inserts a newline in the text field; Cmd/Ctrl+Enter advances. Pending and unknown delivery disable submission. The notification is dismissed after confirmation. Multi-question, multi-select and text-only notifications open the complete drawer.

Terminal delivery reads the current screen from the session's owning `Target`, including a remote node. It compares the question and fingerprint before each page, moves from the actual cursor, and changes only checkbox states that differ from the requested answer. Claude's `Type something` and `Chat about this` actions are distinct. Existing terminal drafts stop delivery. Review is observed before submitting; asynchronously rendered tabs never receive a complete blind macro. A screen-observed page transition is a page acknowledgment; hooked questions wait for the correlated tool result.

Codex Desktop questions observed through another app-server connection remain explicitly read-only, with **Open in Codex**. The public request/response schema adapter preserves provider IDs and multiline responses. Owning a rollout file does not grant ownership of the original RPC request. Managed Codex CLI questions support text through their actual terminal picker.

## Executed checks

- `node --test ui/question-answer.test.mjs`: 12 helper cases, including exact identity, per-question capability, multiline text and isolated drafts.
- `node scripts/qa/question-browser.cjs`: 7 real Chromium scenarios against production HTML/renderer and a synthetic bridge: single/multi/custom, Back/review, state-push focus/caret preservation, pending duplicate protection, failure retry, unknown no-retry, new revision, late answer from another machine and external transport fallback. Report: [browser results](assets/questions/results.json).
- Rust filters `question_delivery::tests`, `screen_prompt::tests`, and `tmux::answer_keys_tests`: strict input validation, changed/stale identity, in-flight/duplicate/unknown protection, timeout/ack race, current cursor and checkbox state, real Claude picker structure, terminal draft protection and Codex idle recognition.
- Actual **Codex CLI 0.153.1** with an isolated localhost Responses API: the production Rust planner moved from a deliberately changed cursor to Local, then submitted a two-line Russian Other answer. The CLI returned both answers through its real `request_user_input` tool output. [Evidence](assets/questions/codex-cli-results.json).
- Actual **Claude Code 2.1.258** with an isolated localhost Anthropic API: the production Rust planner selected Local, retained an already checked Unit option while adding Integration, then submitted a two-line Russian Other answer and the observed review. The CLI returned all answers through its real `AskUserQuestion` tool result; Claude represents the line separator as `\r`. [Evidence](assets/questions/claude-cli-results.json).
- `node scripts/qa/instance-launch-browser.cjs`: 4 Chromium scenarios confirm programmatic local/VM project switching, correct profile lists, late-response isolation, failed profile refresh blocking launch while retaining the task draft, and recovery. [Evidence](assets/questions/profile-launch-results.json).

## Observation integration review

The follow-up review added an explicit observation state clock, protected prompt/reply ordering, stale-turn/question guards, matching Codex answers-output acknowledgment, and a pure observation-to-session merge used by the daemon importer. Managed terminals receive tmux capability; Desktop-only observations remain external. Rejected stale states do not publish question notifications. A new completed turn is distinguished from a previous Done row, while metadata never supplies the completion timestamp. Generic parallel tool activity preserves Waiting while a question is pending.

Explicit provider homes always receive an account namespace, including before the next discovery refresh. Conflicting ID/home pairs and disabled profiles are rejected. Identity normalization is idempotent for remote envelopes. Dedicated regression filters are `codex_live::tests` (10 cases), `session_identity::tests` (3 cases), and `daemon::tests::unrelated_tool_activity_keeps_an_unanswered_question_waiting`. These final follow-up tests are included in the integrated build verification; the earlier executed question test counts above do not include them.

## Reproduction and scope

The CLI fixtures create disposable `/tmp/jq-*` profiles, empty working directories, dedicated tmux sockets and local API servers. They do not use an account, a real model or an existing user terminal. Codex uses `env -i` and its own HOME/CODEX_HOME. Claude uses its own HOME/CLAUDE_CONFIG_DIR, safe mode, no personal setting sources or MCP servers, and an explicitly fake API key directed to localhost. Claude `--bare` cannot exercise this test because that version omits AskUserQuestion in bare mode.

Build the application test executable and pass its absolute path:

```sh
cargo test --manifest-path src-tauri/Cargo.toml --bin jarvis --no-run
python3 scripts/qa/codex-question-fixture.py --probe /absolute/path/to/jarvis-test-executable
python3 scripts/qa/claude-question-fixture.py --probe /absolute/path/to/jarvis-test-executable
```

The opt-in Rust probe only reads the fixture's captured screen and writes the production plan; Python plays that plan into its own terminal and checks the actual provider tool output. This proves the parser/planner with both real CLIs. It does not itself prove the full running GUI → daemon → remote node → hook path; remote node runtime checks and native application smoke checks are separate. Unknown delivery remains visible instead of claiming success when a provider acknowledgment is unavailable.
