# Loops and bundle backend QA — 2026-09-05

The test environment used disposable local Git repositories and an isolated real tmux server. Agent executables in this audit were deterministic **fake Claude and Codex CLIs**. No network requests, credentials, user repositories, user tmux sessions, microphone capture or app restart were involved. Real provider authentication/streaming evidence is recorded separately in [remote-agents.md](remote-agents.md).

## Reproduced defects and fixes

| Defect | Resulting behavior | Evidence |
| --- | --- | --- |
| Stop changed only the journal; a waiting shell/agent continued and could publish `Running` again. | A watch notification cancels the current engine future. Every runner invocation owns a process group; cancellation and timeout terminate shell/tool descendants. Manual stop and deletion reject stale worker writes. | A real nested shell scheduled a delayed file write. Stop returned within one second; no file appeared after the original deadline. A separate timeout test verifies descendant cleanup. |
| Answer/resume constructed a fresh `Run`, losing history, counters, question step and outputs. | Resume keeps run number, start time, sandbox, journal and token totals. Pipeline outputs and exact next step persist in the journal. The scheduler slot is claimed before changing an answer or limit. | Real shell → human question → reload → answer → shell. The first command ran once; final output contained both previous output and answer. A separate iteration-limit resume ran only the next step. |
| Two loops with the same name could reuse another repository's worktree. | Worktree directories include the loop identity. Existing/resumed directories must match the original Git common directory and branch. | Two real repositories with identical loop names created distinct worktrees; cross-repository validation failed. |
| Login shell profile diagnostics contaminated command outputs. | Runner commands inherit the daemon environment through `sh -c`, avoiding login profile output/side effects. | The first real tests failed because a local profile printed a missing-file diagnostic; this corrupted a question and Git identity comparison. Both reproductions pass after the fix. |
| Codex loop calls used bare shell `codex` and did not account for usage. | Resolve the actual executable, pass structured argv including the selected model, consume JSONL completion/error events and real input/output usage. Incomplete or empty results fail. | Parser regressions plus a fake Codex executable exercised production command construction and engine token accounting. This audit does not claim a new real Codex billing/usage test. |
| Empty/error Claude output could pass; failures lost diagnostics. | Empty output and error subtypes fail; nonzero exit preserves CLI result/stderr. Critic success requires the exact verdict and a successful CLI result. | Parser regressions and a real fake-CLI subprocess failure. |
| Concurrent store updates could lose human edits or race on a shared `.tmp` filename. | Journal edits are atomic under the store mutex; snapshots use private unique atomic replacement and serialize while holding the relevant mutex. Human interventions/reviews survive stale worker snapshots. | Store save/reload and stale-write regressions. |
| Multiple bundle starts/merges/ticks could operate on the same checkout concurrently. | A per-bundle RAII operation reservation serializes start, add, save, merge, remove and maintenance. Early returns/cancellation release it. | Reservation/exclusion/drop regression; native IPC button-level concurrency remains a UI integration scenario. |
| Saving an older bundle form overwrote live hand states and event history. | Form saving merges configuration with current runtime metadata and preserves omitted live hands. A live bundle retains its original host, repository and provider. | Save → runtime update → stale form → reload regression. |
| `bundle_remove` forcibly removed every worktree, including live and dirty trees. | Working trees remain. Cleanup attempts only finished/failed trees with ordinary Git removal; uncommitted files prevent deletion. Branches remain. | Real dirty worktree cleanup was rejected and the unsaved file remained intact. The IPC predicate is code-reviewed; this fixture does not construct a live Tauri daemon. |
| Creating a deeply nested new bundle folder failed before `mkdir -p`. | Directory creation starts from an existing root/current directory. | Real nested-directory initialization, initial commit and worktree creation. |
| tmux launch reported success after an agent died, or when no usable pane ID was returned. | Require a valid pane ID and live pane while waiting for readiness; an exited CLI fails promptly. Readiness timeout stops its pane and returns an error. | Actual isolated tmux start → reply → Escape → kill, plus an immediately exiting command. The sandbox also reproduced a tmux invocation that printed a socket error with exit code zero and empty pane ID. |
| Bundle launching was hard-coded to Claude, even when Codex was selected/available. | `Bundle.agent` explicitly selects `claude` or `codex`; old files default to Claude. Preflight checks the chosen host's CLI before creating a repository/worktree. Presence does not promise working authentication. | Local command/preflight checks with both fake executables. Remote provider selection is code-covered but a full remote bundle launch is not covered by this fixture. |

## Commands and results

```sh
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --bin jarvis loops:: -- --nocapture
# 77 passed, 1 ignored; the ignored integration requires its explicit fixture.

src-tauri/target/debug/deps/jarvis-a20ad7e74bb273fd bundle:: --nocapture
# 24 passed, including real Git initialization, worktree, merge, conflict/rebase and dirty-tree checks.

python3 scripts/qa/loops-bundle-fixture.py \
  src-tauri/target/debug/deps/jarvis-a20ad7e74bb273fd
# One ignored integration executed explicitly: PASS, 3.40 s.
```

The tmux fixture needs permission to create a Unix socket in its private temporary directory. The first sandboxed attempt was blocked by the OS; it is not counted as a product pass. The subsequent permitted invocation passed. It created a private `TMUX_TMPDIR`, separate home/data directories and fake CLI executables; its `finally` block stopped that isolated tmux server. Sanitized result: [fixture-result.txt](assets/loops-bundle/fixture-result.txt).

## Limits of this evidence

- The real tmux fixture calls production launch/reply/interrupt helpers and the production loop engine. It does not instantiate the full native Tauri daemon or prove every IPC/UI interaction.
- The synthetic TUI confirms transport and lifecycle. It does not emulate real Claude/Codex auth screens, trust screens or every provider UI revision.
- Remote SSH/node reliability has separate real evidence in `remote-agents.md`; remote bundle creation/merge with a provider is not claimed here.
- User-requested removal keeps worktrees with uncommitted files. Their later manual cleanup is deliberately outside this audit.
- No additional MCP meeting-capability scenario was run in this bounded audit.
