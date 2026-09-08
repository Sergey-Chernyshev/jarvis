# Agent workspace: upstream research and Jarvis integration

Research date: 2026-09-05. This document records inspected public source code and
the existing Jarvis implementation. Recommendations are design proposals, not
claims that every upstream capability has been implemented or tested in Jarvis.
The supplied screenshot is a visual reference: its model names and permission
labels are not a source of provider capabilities.

## Sources examined

| Project | Revision examined | Useful idea |
| --- | --- | --- |
| [T3 Code](https://github.com/pingdotgg/t3code/tree/1963ca0abe07f9b9aac67820bca45ea3b46ce023) | `1963ca0abe07f9b9aac67820bca45ea3b46ce023` | Environment ownership, provider adapters, durable events, reconnect behavior |
| [Happy](https://github.com/slopus/happy/tree/ee5bf71c48dcdb16c198f9fd115d8388b8d3596e) | `ee5bf71c48dcdb16c198f9fd115d8388b8d3596e` | Hook plus transcript capture, deduplication, structured questions |
| [CloudCLI / Claude Code UI](https://github.com/siteboon/claudecodeui/tree/c1be241bc41586478f3d15f4dc6a5a6399d40aa1) | `c1be241bc41586478f3d15f4dc6a5a6399d40aa1` | Project chat, bounded event replay, terminal reattachment, notification policy |
| [ccusage](https://github.com/ccusage/ccusage/tree/7ae9dd66bfa9cbe8d6e30029f57a95101be17e5f) | `7ae9dd66bfa9cbe8d6e30029f57a95101be17e5f` | Transcript accounting, stream corrections, cache categories, fork deduplication |

The findings below describe architectural ideas. No upstream implementation is
vendored by this research.

## 1. T3 Code: the workspace machine owns execution

T3's provider adapter exposes start, send, interrupt, approval response, user-input
response, read, rollback, and a normalized event stream. Capabilities explicitly
describe whether model switching or conversation rollback is supported. A UI
therefore need not guess CLI flags or pretend that every provider implements the
same controls. [ProviderAdapter.ts](https://github.com/pingdotgg/t3code/blob/1963ca0abe07f9b9aac67820bca45ea3b46ce023/apps/server/src/provider/Services/ProviderAdapter.ts)

Its orchestration engine checks a command receipt before dispatch, checks that
the command ID belongs to the correct aggregate, and commits events, projections,
and the receipt in one SQL transaction. Subscribers receive events after commit.
This makes retrying an accepted command distinct from executing it again.
[OrchestrationEngine.ts](https://github.com/pingdotgg/t3code/blob/1963ca0abe07f9b9aac67820bca45ea3b46ce023/apps/server/src/orchestration/Layers/OrchestrationEngine.ts)

The connection runtime separates transport readiness from thread/terminal data
freshness. One owner controls retries per environment. Cached data survives an
involuntary disconnect; explicit removal clears the environment scope. A state
snapshot and its replay cursor are retained together only after application.
Reconnect does not automatically retry mutations.
[Connection runtime](https://github.com/pingdotgg/t3code/blob/1963ca0abe07f9b9aac67820bca45ea3b46ce023/docs/internals/connection-runtime.md)

**Jarvis application:** use the existing local/remote target abstraction. Put
machine identity beside project path in both the composer and chat header. A
remote connection failure must preserve the draft and visible conversation;
retrying a read must never paste the last prompt a second time. Distinguish
“machine disconnected”, “history loading”, and “agent working”. Existing terminal
sessions should show their observed provider configuration; unsupported model or
permission controls must not be fabricated from the reference image.

## 2. Happy: hooks locate the session; transcripts recover the conversation

Happy's session scanner validates JSONL entries, ignores known internal bookkeeping
records, and deduplicates messages by UUID. It watches both current and older
session files because resumed work can still write to an earlier transcript.
Watchers are supplemented by a three-second sync; missing transcripts eventually
stop their watcher. When the caller already has history, existing entries can be
marked processed to avoid replaying them as fresh messages.
[sessionScanner.ts](https://github.com/slopus/happy/blob/ee5bf71c48dcdb16c198f9fd115d8388b8d3596e/packages/happy-cli/src/claude/utils/sessionScanner.ts)

The inspected main-branch launch flow also combines SDK output with a JSONL
scanner for user prompts typed in a parallel resumed terminal. It suppresses
app-originated prompt echoes with a short-lived content queue. This demonstrates
why live output and recovery history need explicit ownership; text matching alone
is not a universal message identity.
[runClaude.ts](https://github.com/slopus/happy/blob/main/packages/happy-cli/src/claude/runClaude.ts)

Happy identifies `AskUserQuestion` using actual assistant `tool_use` blocks and
their IDs. This is a useful signal for an actionable notification rather than
guessing whether prose contains a question mark.
[questionNotification.ts](https://github.com/slopus/happy/blob/ee5bf71c48dcdb16c198f9fd115d8388b8d3596e/packages/happy-cli/src/claude/utils/questionNotification.ts)

**Jarvis application:** retain the existing hook envelope and transcript readers.
Drive chat bubbles from normalized conversation items, and status/attention from
lifecycle events. A reconnect snapshot must reconcile with already displayed
items instead of appending the entire transcript. Preserve an explicit terminal
entry point for providers whose output is not understood.

## 3. CloudCLI: chat and terminal are related views with separate lifetimes

CloudCLI's run registry maps provider IDs to a stable app session ID, assigns a
monotonic sequence to each outgoing event, and retains a bounded replay buffer
(5,000 events). Completed runs remain for five minutes. A subscriber that missed
the buffer must reload authoritative history. The registry rejects concurrent
duplicate sends and suppresses a second completion after an abort/exit race.
[chat-run-registry.service.ts](https://github.com/siteboon/claudecodeui/blob/c1be241bc41586478f3d15f4dc6a5a6399d40aa1/server/modules/websocket/services/chat-run-registry.service.ts)

Its terminal service uses a PTY registry keyed by project/session, reattaches a
socket to a live PTY, replays bounded output, and later cleans up detached
processes. Chat events remain a distinct protocol from terminal byte output.
[shell-websocket.service.ts](https://github.com/siteboon/claudecodeui/blob/c1be241bc41586478f3d15f4dc6a5a6399d40aa1/server/modules/websocket/services/shell-websocket.service.ts)

Notifications have normalized kinds (action required, stop, error), per-kind and
per-channel preferences, and a 20-second deduplication window. Provider-native
session IDs are normalized before building a deep link. The title uses the
conversation name and the payload retains provider/session metadata.
[notification-orchestrator.service.js](https://github.com/siteboon/claudecodeui/blob/c1be241bc41586478f3d15f4dc6a5a6399d40aa1/server/modules/notifications/services/notification-orchestrator.service.js)

**Jarvis application:** group conversations by `(machine, cwd)` rather than folder
basename; preserve the selected chat while refreshing the list. Make questions,
approval requests, failures, and unread completions visible in an attention
filter. Clicking one should open that exact conversation. Native toasts and the
attention list should consume the same lifecycle facts, with per-chat mute and
duplicate suppression. An embedded terminal preview must identify whether it is
a refreshed tmux screen or a full interactive PTY.

## 4. ccusage: reconcile usage before aggregating it

The current Rust Claude adapter scopes normal deduplication by message ID,
request ID, and session. It detects sidechain replay even if a copied message
has a different request ID. When duplicate records disagree, it prefers the
parent record over a sidechain and otherwise the record with the larger token
total. This avoids counting streaming snapshots repeatedly while still accepting
later, more complete usage.
[Claude adapter](https://github.com/ccusage/ccusage/blob/7ae9dd66bfa9cbe8d6e30029f57a95101be17e5f/rust/adapters/claude/src/lib.rs)

The Codex parser uses `last_token_usage` only when the cumulative total advances;
otherwise it derives a saturating delta from `total_token_usage`. Cached input is
clamped to input and cache creation to the remaining input. Fork replay is handled
separately from ordinary per-file token parsing.
[Codex parser](https://github.com/ccusage/ccusage/blob/7ae9dd66bfa9cbe8d6e30029f57a95101be17e5f/rust/adapters/codex/src/parser.rs),
[Fork replay](https://github.com/ccusage/ccusage/blob/7ae9dd66bfa9cbe8d6e30029f57a95101be17e5f/rust/adapters/codex/src/replay.rs)

ccusage supports separate token categories, explicit pricing overrides, offline
pricing, and timezone-aware grouping. Its cost modes distinguish reported costs
from calculated estimates.
[README](https://github.com/ccusage/ccusage/blob/7ae9dd66bfa9cbe8d6e30029f57a95101be17e5f/README.md),
[Cost modes](https://ccusage.ryoppippi.com/guide/cost-modes)

**Jarvis application:** expose input, output, cache read, and cache write counts
beside the total. Label computed money as an API-equivalent estimate; token
estimates do not establish subscription quota. Unknown model pricing should
remain visibly unknown or approximate. Preserve the source machine/account for
official limits and the time at which they were fetched. Later stream corrections
must adjust aggregates once rather than being discarded or counted as another
request. Synthetic fixture coverage should precede any claim of accurate billing.

## Existing Jarvis building blocks

These observations are from the working tree, which already contained unrelated
in-progress changes when research began:

| Existing code | Relevant behavior |
| --- | --- |
| `src-tauri/src/model.rs` | Session includes cwd, project, provider, remote, tmux pane, title, lifecycle revision, question, and last prompt |
| `src-tauri/src/backend/events.rs` | Validates hook envelopes, rejects stale turn/process events, suppresses duplicate stop |
| `src-tauri/src/remote.rs` | SSH target, node incarnation, persisted cursor, explicit gap handling, ordered/deduplicated event pages |
| `src-tauri/src/tail.rs`, `transcript.rs`, `backend/codex_transcript.rs` | Provider-normalized chat items and incremental transcript reading |
| `src-tauri/src/ipc.rs` | Existing chat open, terminal focus, session launch, project/machine routing, terminal ping, slash delivery |
| `src-tauri/src/usage.rs` | Incremental local Claude/Codex accounting, hourly/project/session aggregates, official usage source identity |
| `ui/renderer.js`, `bridge.js` | Existing session chat rendering, attachments, history launch and machine selection |
| `docs/qa/remote-agents.md` | Previous real localhost SSH/tmux transport tests; explicit limits on provider and remote-VM coverage |

At inspection, `usage.rs` used a first-seen message-ID ring and coarse model-family
prices, including a guessed Fable price. These are concrete accounting follow-up
points, not evidence of exact model pricing. `for_session` returned only total,
cost, billing, and model, so a richer usage strip needs additional source fields.

## Integration priorities and useful regression scenarios

1. Build the project/chat workspace over existing session and target APIs: project
   navigation, persistent per-chat drafts, visible machine, readable conversation,
   large composer, provider status, resume and terminal actions.
2. Carry state freshness and target connectivity into the UI. Preserve selection
   and draft across updates; disable or reject sends to stale/offline targets.
3. Derive an attention view and notification controls from actual lifecycle
   transitions. Reopening old history must not create a fresh notification.
4. Expose session usage categories and provenance; reconcile repeated usage
   snapshots before adding more analytics charts.

Regression cases: same folder name on two machines; chat switch during a pending
snapshot; disconnect after paste but before acknowledgement; cursor gap/restart;
duplicate completion; stale permission event after a new turn; repeated identical
user prompts; two snapshots of one assistant message with increasing output;
Codex cache included within input; fork history replay; unknown price; retained
draft after remote failure. These test observable user outcomes rather than
mirroring implementation details.
