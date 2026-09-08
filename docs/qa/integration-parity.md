# Agent integrations and instance parity

Follow-up requested and verified 2026-09-05. This records the executed acceptance
scope and the remaining product boundaries. Integration changes follow the
handoff from the Projects/VM/avatars and Analytics tasks. The notification NSPanel/motion fix is already shipped and must
be preserved.

## Acceptance scope

| Area | Required result |
| --- | --- |
| Codex instances | Discover explicit homes, environment/default home and statically declared local wrappers; canonical-path deduplication, stable identity and editable label/enabled state. Never execute wrappers to discover an account or expose authentication data. |
| Current Desktop chat | Restore current session from real rollout lifecycle events even when hooks are absent. Keep history, live status, source instance and control capability distinct; do not label every recently modified file Working. |
| Account routing | History, live events, usage, models, launches/resumes and tmux environment retain the selected source instance. Summaries retain the source session identity; the configured service backend uses the explicit default Codex profile when Codex is selected. No silent fallback to a different account. |
| Hooks | Idempotent installation/repair for every enabled instance, preserve foreign hooks/settings, remove stale Jarvis registrations and report actual trust/delivery limitations. |
| VM | A managed VM can be connected to the event transport, with explicit connection/account state. Hook setup, transcript roots, lifecycle, questions, usage/analytics and smart notifications use the same source identity. |
| Questions | Current request identity, strict single/multiple/custom validation, multiline drafts, keyboard navigation, duplicate protection, explicit delivery states and no automatic replay after uncertain delivery. |
| Transport | Remote questions read the remote pane, never a local pane with the same number. Terminal input checks the actual prompt/cursor/selection. Structured responses use only an owned pending RPC connection. |
| Notification/statistics | No duplicate completion effects, no cross-account or cross-machine attribution; late events and reconnects preserve truthfulness. Remote session statistics are included with provenance. |
| Verification | Isolated local and SSH/VM acceptance fixtures, actual installed CLI/schema checks, browser/native UI, current Desktop chat visible under its real instance, release rebuilt from the integrated tree and relaunched. |

## Read-only findings before handoff

- `codex-work` opens the default Desktop app using `~/.codex`; `codex-personal`
  declares a separate home and Desktop user-data directory. The current root chat
  is already in production history, but not the live session store. Its rollout
  contains genuine `task_started`/`task_complete` boundaries.
- A personal `SessionEnd` hook points to the old development installation while
  other rules point to production. Hook-file health alone does not establish trust
  or prove runtime delivery.
- Hook envelopes do not carry provider-home identity. Plain `codex` launch/resume,
  models, summary auth and agent-host auth resolve homes differently. The tmux
  update-environment list omits `CODEX_HOME`/`CLAUDE_CONFIG_DIR`.
- Remote installation omits the managed terminal shim/environment, and node
  service configuration loses custom Codex roots. Managed VM inventory alone does
  not register a node connection. Existing saved remote configurations are empty.
- Remote screen-question detection calls local `tmux capture-pane`, making equal
  local/remote pane IDs a cross-machine routing risk.
- Question submissions have no request revision or duplicate guard. Toast
  multiselect submits one option immediately; free text is forbidden for Codex.
  Screen recognition discards cursor/checkbox state, and key planning assumes the
  initial cursor. Successful key injection is reported as acceptance without proof.
- A delayed prompt acknowledgment can trigger an automatic second written reply.
  That must be replaced by explicit uncertain-delivery state, retaining the draft.
- Remote sessions are excluded from the analytics report; ordinary usage scans are
  local. Remote Claude account quota fallback is not session usage/statistics.
- Personal Hermes is accessed through Teleport. The raw native node hostname does
  not match the existing OpenSSH ProxyCommand patterns; treating it as an ordinary
  SSH host routes incorrectly. Fresh Teleport access currently requests MFA, so
  no remote authentication or configuration was changed during this audit.

## Ownership after handoff

- Instance discovery agent: shared registry, discovery and focused identity tests.
- VM parity agent: remote installer/node protocol/environment, explicit transport
  and remote statistics fixtures.
- Questions agent: request/answer model, terminal planner/delivery, screen detector,
  wizard/toast UI and their regression fixtures.
- Root: integration coordination, Desktop live importer, registry consumers and
  settings, hooks/account routing, analytics integration and final acceptance.

No live user agent receives test replies. Synthetic fixtures own their terminals,
questions and provider requests. Existing account sandbox/approval policies are
outside this change.

## Completed practical checks

- Both parallel user-owned tasks (Projects/VM/chat and Analytics) completed their
  handoff before this integrated audit. The final build must use this current
  worktree, not the earlier notification-only release snapshot.
- Real Codex 0.153.1 and Claude 2.1.258 TUIs, isolated credentials/settings and fake
  provider APIs: single choices, preserved multiselections, custom Russian
  multiline answers and provider tool outputs verified. No user session received
  a test answer. See [question evidence](agent-questions.md).
- Real Codex app-server: only nine exact Jarvis commands from the intended
  canonical hooks file were trusted. Foreign hooks and existing approval/sandbox
  policies remained unchanged; the operation was repeated to prove idempotence.
- Real Linux ARM64 VM: complete node build, two independent Codex homes, same-SID
  separation, hook receipt, terminal question controls, restart/replay, systemd
  installation twice and correct scoped analytics. The VM was returned to its
  initial Stopped state. See `assets/linux-node-parity/report.json` and
  `assets/linux-node-parity/foreground-tunnel.json`.
- Real local node: a newly added source can be listed, read and attributed without
  a restart; transcript access never grants access to authentication files.
- Browser scenarios exercise profile/default changes, targeted hook repair,
  native VM SSH configuration and profile switching across machines. UI unit
  suite passes 252 tests.
- Regression checks cover late prompt/question/completion handling, hook/rollout
  deduplication, cursor restart, separate same-SID profiles, selected-profile launch
  binding and account-scoped limit/retry generations.

## Product boundaries

- External Codex Desktop sessions expose no owned answering RPC connection here.
  Jarvis observes their lifecycle/transcript and opens their correct Desktop
  profile. It does not claim that a Desktop question was answered. Managed local
  and VM CLI sessions support verified interactive answers.
- Source identity and session statistics are distinct from account quota. Unknown
  or offline statistics remain unavailable/cached with provenance, not zero.
  Unknown quota reset times never start a guessed automatic retry.
- Remote transcript analytics work through the node. Remote Git analysis is
  explicitly reported as not inspected; local filesystem paths are never used
  for guest files.
- The actual Personal Hermes Teleport connection still requires interactive MFA;
  its live integration was not changed or claimed verified. Transport and Linux
  installation were exercised using isolated fixtures and the actual Lima VM.
- The disk filled during final compilation. Only Jarvis's reproducible
  `target/debug/incremental` build cache, inactive marked native-test app copies
  and temporary `jarvis-*.o` files were removed before retrying. Reports remain.

Final native application verification and installed release identity are
recorded below.

### Final fork correction

The first native current-chat check failed: six actual child rollouts repeated
parent `session_meta`, so their completed state replaced the root chat. This was
fixed using immutable first-header ownership and the explicit
`subagent_history_start_ordinal` boundary. Local/remote observation, history and
usage/analytics now share that interpretation. Observation/history cache revisions
and usage v5 invalidate earlier derived errors. An actual 10.2 MB child rollout
probe preserved the child SID and excluded 16 inherited records with no truncation
or invalid lines. Regression fixtures also cover inherited token totals,
aggregate-only baselines and a 1.6 MB compacted parent-context record.

Final full Rust suite: **1106 passed, 11 opt-in tests ignored**. The relevant real
CLI, node/VM and actual-fork opt-in probes were separately executed above.
Installer: **98 passed, 3 ignored**, plus latest remote installer subset
**22 passed, 1 ignored**. MCP binary: **8 passed**. UI: **252 passed**.

### Installed acceptance

- Native WKWebView: four real-profile/current-chat checks passed. The 43.8 MB
  current rollout was imported in 19 seconds during an isolated cold start;
  switching to its chat took 249 ms and profile settings rendered in 520 ms.
  The initial deep link to Agents now survives settings initialization.
- Native modules/settings: **29 checks passed**, no console errors. Static
  screenshots finish finite animations when WebKit is occluded; they are layout
  evidence, not a new motion proof. See the existing fullscreen/motion report.
- Real installed Codex CLI **0.152.0** also passed isolated exact-hook trust,
  foreign-hook preservation and approval/sandbox preservation checks.
- Production `jarvis-setup repair` completed for Claude and both Codex homes.
  Both `codex-personal` and `codex-work` returned **trusted** through their actual
  CLI. The stale development SessionEnd registration was replaced.
- Full-feature release signature verified with `codesign --deep --strict`.
  Old PID 1757 exited through SIGTERM; new PID **91552** was the only release
  Jarvis process. The actual production socket returned both profile labels and
  this current chat as **working**, owned by Personal, with its own transcript.
  [Production evidence](assets/instances/production-acceptance.json).
- A two-second process sample found the main thread in the normal AppKit event
  loop in all 155 samples (153 waiting on its Mach port), with no blocking audio
  permission or configuration call. Existing official Claude account-quota
  retrieval reported unavailable; it is not represented as zero. Session token
  statistics remain separate from that account-quota endpoint.
- Source hashes and executable identity: [manifest](integration-release-source.json).

The source remains in the shared `codex/jarvis-redesign` worktree. No release was
published and no other task's work was reset or committed by this audit.
