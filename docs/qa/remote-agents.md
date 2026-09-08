# Remote nodes and provider reliability

Validation date: 2026-09-05 (Europe/Moscow). Branch: `codex/jarvis-redesign`.
The evidence below distinguishes a real transport with synthetic content from
a real provider request. The original SSH run did not start a user VM; the later Linux VM follow-up below did.

## Environment and isolation

The host has OpenSSH 10.3, tmux 3.6b, Python 3.10, Claude CLI 2.1.258 and
Codex CLI 0.152.0. Docker is installed but its daemon is stopped; Podman is
absent. No daemon or user VM was started or modified during that initial SSH run; the later Linux follow-up below has a separate scope.

The SSH fixture starts a localhost-only `sshd` with newly generated keys and a
dedicated configuration, then runs the actual compiled `jarvis-node` through
SSH. A second SSH connection forwards a localhost TCP port to the node's Unix
socket. `TMUX_TMPDIR` isolates the `-L jarvis` tmux server. All repositories,
messages and transcripts in this fixture are synthetic. The fixture never
calls `/projects` or reads the user's transcript roots. Its daemons and panes
are stopped in `finally`; temporary result JSON and logs remain for inspection.

The final SSH run is recorded at
`/private/tmp/jarvis-remote-qa-6dmc1rwo/{protocol,lifecycle}-results.json`.
Earlier baseline failures were recorded at
`/private/tmp/jarvis-remote-qa-9zr3xogx/protocol-results.json`.

## Reproduce

```sh
cargo build --manifest-path src-tauri/Cargo.toml -p jarvis-node
cargo test --manifest-path src-tauri/Cargo.toml -p jarvis-node
python3 scripts/qa/remote-ssh-fixture.py src-tauri/target/debug/jarvis-node
node --test ui/agent-chat.test.mjs
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --bin jarvis remote::
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --bin jarvis backend::
```

Localhost binding and SSH may require permission outside a restricted sandbox.
The script does not alter `~/.ssh/config`, known hosts, or authorized keys.

## Results and fixed failures

| Scenario | Evidence | Result |
| --- | --- | --- |
| SSH authentication and TCP-to-Unix forwarding | Real sshd, ssh and node `/hello` | Pass |
| Long-poll delivery of exact Unicode hook envelope | POST over actual tunnel wakes pending `/events` | Pass |
| Launch a process and paste a reply | Real tmux pane runs synthetic Python receiver; Unicode and literal shell syntax preserved | Pass |
| Transcript access under live pane cwd | Baseline HTTP 403 because tmux emitted underscores in place of metadata tabs under `LC_ALL=C`; force `tmux -u` in node and local helpers | Fixed, actual rerun passes |
| Concurrent launches with identical names | Baseline one HTTP 502 `duplicate session`; use milliseconds, PID and atomic sequence | Fixed, both actual panes launch |
| Duplicate node process | Baseline second node silently unlinked live socket; OS file lock now claimed before stale-socket cleanup | Fixed, duplicate exits 1 and original socket inode/HTTP remain intact |
| Partial UTF-8 writes | A chunk containing only the first bytes of an emoji previously returned replacement text and advanced its cursor | Fixed, cursor remains zero until character is complete; actual `/file` rerun passes |
| File truncation and denied roots | Smaller `from`/`next` advertises rewind; path outside roots returns 403 | Pass |
| Delivery after SSH disconnect | Three synthetic hooks written directly to node.sock while forward is down; reconnect returns exact order | Pass |
| Ring overflow | Eight-item ring receives ten events; explicit `gap:true` returned | Pass |
| Node crash/restart | SIGKILL of fixture node, restart while SSH forward stays alive; old cursor reports gap and new stream starts at zero | Pass |
| Restart after new counter catches up | New process identity differs even when numeric cursor already exceeds the saved offset | Fixed, actual protocol rerun returns gap/zero |
| Client page boundaries | Actual node page has event cursor 0 and next cursor 1. Old client filter `> previous` dropped the first event of every page | Fixed to `[previous, next)`; regression uses observed protocol shape |
| Saved node incarnation | Cursor file now stores cursor and process identity together, atomically; old numeric files remain readable | Unit regression passes; upgrade deliberately refreshes transcript state |
| Removed remote with queued start | A delayed blocking task could re-arm `active` after stop | Fixed: only a new Tunnel object can re-arm an explicitly stopped remote |
| Codex in temporary runtime directory | Real CLI rejected old argv before any answer: `Not inside a trusted directory and --skip-git-repo-check was not specified` | Fixed for the internal agent-host runtime; read-only sandbox retained |
| Custom Codex account home | Agent used hardcoded `~/.codex/auth.json` while history and CLI use configured `CODEX_HOME` | Fixed to use the same configured source; synthetic account tests verify original files unchanged |
| Provider selection | Installed but expired Claude made Codex unreachable through the automatic-only host choice | Explicit Auto/Claude/Codex selector; installation availability is shown separately from authentication |
| Cross-provider resume and subscription readiness | A Codex session must never be passed to Claude; sending before listeners attach can lose completion | Selector resets session, remains locked during a turn, and send waits for listeners; four DOM tests pass |
| Confirmation keyboard access and expired requests | Controls were non-focusable divs; UI displayed approval before receiving the IPC result | Semantic buttons now support Tab/Enter and an expired nonce displays its actual state; fifth DOM regression passes |

The standalone node unit suite passes 38 tests. The first application test build
passed 47 remote-related tests and 31 backend tests, with the real provider smoke
explicitly ignored in the ordinary suite. These scopes overlap with other
filters; their counts must not be added together.

The full no-default-features application suite then passed **884 tests**, with
**3 explicitly ignored**, out of **887 total**, in 23.27 seconds. Its binary is
`src-tauri/target/debug/deps/jarvis-a20ad7e74bb273fd`. This checks Rust behavior;
it does not establish production-feature audio or native UI behavior.

## Real provider evidence

A real Codex request in an empty disposable Git repository, with user config,
rules and hooks disabled for that invocation, exited 0 in 4.99 seconds and
returned `JARVIS_CODEX_QA_OK`. Observed events were `thread.started`,
`turn.started`, `item.completed` and `turn.completed`. This is a real provider
result; it is separate from the synthetic hook envelopes in the SSH fixture.

A real Claude request used no tools, no MCP servers, no session persistence and
empty settings sources. It exited 1 in 0.40 seconds because its OAuth session
was expired and could not be refreshed. Claude emitted `subtype: "success"`
with `is_error: true`; the regression verifies this is a terminal failure.
This does **not** count as a successful Claude completion.

An opt-in test, `real_codex_agent_runtime_and_resume_with_synthetic_capability`,
uses the production argument builder, parser and session lifecycle with the
real Codex CLI and real `jarvis-mcp` binary. The daemon capability endpoint is
explicitly synthetic (`qa_echo`); application permissions are not tested by
that endpoint. It makes two small requests: call `qa_echo`, then resume the same
session. It runs in an empty temporary directory and uses a private temporary
copy of existing authorization so provider refresh cannot rewrite the source
auth file. The copy and temporary session are removed on completion or failure.
Set `JARVIS_QA_CODEX_BIN` and `JARVIS_QA_MCP_BIN` to explicitly selected real
binaries, then run the test by exact name with `--ignored --nocapture`.

The first real MCP attempts reached terminal completion but did not return the
synthetic marker. A diagnostic rerun returned `MCP tool call requires approval,
but approval policy is never`, with zero calls to the capability endpoint.
The host now sets the documented server-specific
`mcp_servers.jarvis.default_tools_approval_mode = "approve"`: CLI approval is
delegated only for Jarvis's gated bridge, while the daemon's grant and native
confirmation still authorize actual actions. The socket is also passed directly
through `mcp_servers.jarvis.env.JARVIS_SOCK`, avoiding reliance on inherited MCP
environment variables. See the [official configuration reference](https://developers.openai.com/codex/config-reference/).
The next real attempt reached the synthetic endpoint once but the answer was
only `{"provenance":"trusted"}`. The bridge's `structuredContent` had contained
only provenance while its text content held the tool result. The bridge now
includes the actual `value` (or `error`) in structured content as well, preserving
provenance and the untrusted-text marker. Unit assertions cover both channels.

After these fixes the real pipeline test **passed in 16.45 seconds**. Exactly one
`qa_echo` call reached the synthetic daemon; Codex returned
`JARVIS_CODEX_MCP_QA_OK`. A second real `exec resume` returned
`JARVIS_CODEX_RESUME_QA_OK`, preserved the same session ID, and emitted exactly
one terminal `Done`. The actual `jarvis-mcp` binary passed its eight unit tests.

```sh
JARVIS_QA_CODEX_BIN=/absolute/path/to/real/codex \
JARVIS_QA_MCP_BIN=/absolute/path/to/jarvis-mcp \
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --bin jarvis \
  backend::codex_agent::tests::real_codex_agent_runtime_and_resume_with_synthetic_capability \
  -- --exact --ignored --nocapture
```

## Remaining gaps

- Fresh provisioning, systemd installation/upgrades and a cloud VM remain
  unverified. The later existing Linux VM node/protocol check below passed.
- The SSH fixture exercises real node transport and tmux, but synthetic hook
  payloads; it does not prove installed CLI hook interception on a user VM.
- Native UI rendering of real provider streams remains separate from provider
  subprocess checks. The actual daemon MCP registry and denial gate subsequently
  passed seven checks: [native MCP report](assets/native-mcp/report.json).
- Successful Claude completion/resume requires working Claude authorization.
- Legacy nodes without process identity retain numerical-cursor fallback and
  cannot distinguish every fast restart; update the node to gain that fix.

Reference architecture checked against the official T3 Code source:
[provider adapter](https://github.com/pingdotgg/t3code/blob/91c66ac43ddad4f8697887009746f4de11736cb9/apps/server/src/provider/Services/ProviderAdapter.ts),
[connection runtime](https://github.com/pingdotgg/t3code/blob/91c66ac43ddad4f8697887009746f4de11736cb9/docs/internals/connection-runtime.md).


## Actual Linux VM follow-up

The existing Jarvis Lima machine `jarvis-227cb228cd9d` was initially stopped. It
started successfully, reported Linux aarch64, and had Rust, tmux and Python
available. `scripts/qa/linux-vm-fixture.py` copied only node sources, Cargo metadata
and the synthetic protocol probe into a new VM `/tmp/jarvis-remote-qa-*` directory.
It pruned the copied workspace lockfile offline, built the actual Linux node,
passed **38 node tests**, and passed **nine actual HTTP/tmux protocol checks**.
SSH commands originated on macOS; the server and test receiver ran inside Linux.

```sh
python3 scripts/qa/linux-vm-fixture.py \
  --vm jarvis-227cb228cd9d --lima-home /absolute/path/to/lima-home
```

Evidence: [environment](assets/linux-vm/environment.json),
[protocol results](assets/linux-vm/protocol-results.json). Unicode/literal input,
partial UTF-8 file writes, access boundaries, concurrent session names and event
long polling passed. The fixture stopped only its own node/tmux processes. The
VM was returned to its original stopped state after the run; other VMs were not
changed. Existing cargo cache is a prerequisite for the offline lockfile step.

This confirms actual Linux compatibility and node transport in an existing VM.
It does not establish fresh VM creation, systemd provisioning, provider login
inside the VM, or a full remote team merge. Claude's expired local authorization
also remains an external prerequisite for a successful real Claude turn.
