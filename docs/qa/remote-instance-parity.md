# Remote provider instances and hook parity — 2026-09-05

The node protocol is version 2. `/hello` includes an opaque process `instance`,
capabilities and provider sources. Event cursors belong to that process epoch;
reconnect keeps the source timestamp and marks backlog events as replay.

`GET /sources` exposes registered provider homes and stable source IDs, without
reading credentials. `GET /sessions` returns raw provider session IDs together
with `sourceId`, `instanceId`, `providerHome`, transcript path, size, modification
time and file identity. The desktop adds its own remote prefix. Two Codex homes
containing the same raw session ID remain independent sources.

Transcript discovery is bounded: 32 sources, 40,000 directory entries, depth 9,
12,000 files per traversal and at most 4,000 returned sessions. `/file` checks
canonical transcript roots and existing terminal working directories, limits a
chunk to 512 KiB and preserves UTF-8 character boundaries. A configured provider
home does not make its `auth.json` readable.

The installer merges only Jarvis hook entries, preserves foreign settings and
disabled hooks, installs managed shims and marked PATH blocks, and stores explicit
provider roots in its private `provider-roots.json`. Identical writes do not
produce another backup. Remote shims pass `node.sock` to tmux explicitly, so an
existing server's stale desktop `run.sock` cannot redirect the hooks.

Current Codex hook trust uses the official `hooks/list` and `config/batchWrite`
RPCs. Only exact installed Jarvis commands in the selected user `hooks.json`
receive their provider-computed trusted hash. Other hooks, account credentials,
approvals and sandbox policies are untouched. `POST /sources/repair {sourceId}`
and `jarvis-node --repair-hooks` use the same shared implementation. An unknown
source, incomplete/disabled hook set or failed trust verification is an error.

Explicit connection metadata supports OpenSSH `sshConfigFile`, Teleport proxy
and loopback TCP port, and an optional `runAsUser` distinct from the transport
login. Native Lima connections reuse Lima's actual SSH configuration, including
vsock ProxyCommand. Teleport never falls back to plain SSH or assumes support
for forwarding a Unix socket. User data operations and git use the same transport.

## Practical evidence

- [macOS node fixture](assets/node-parity/report.json): 21 checks passed with a
  real node, installed hook, interactive managed shim, existing tmux server,
  source-selected launch, literal Russian answers through `/reply` and `/keys`,
  transcript boundaries and actual Codex 0.153.1 trust RPC. Two temporary homes
  prove that repairing work does not trust personal. A third profile added after
  startup immediately supports transcript reads and canonical hook identity.
- [Linux node fixture](assets/linux-node-parity/node-fixture.json): the same 20
  checks passed on aarch64 Ubuntu in an existing Lima VM. Rust and Codex were
  installed only in the guest fixture's `/tmp`, without changing user profiles.
- [Linux transport/service report](assets/linux-node-parity/report.json): the
  standalone embedded source layout compiled, native Lima SSH forwarded the
  node socket, and restarting a uniquely named transient user service produced
  a new event epoch through the existing tunnel.
- [Production installer component test](assets/linux-node-parity/installer-components.log):
  actual `install_sources`, `remote_hooks` and `install_transport` ran twice on
  Linux, preserving foreign hooks/profile, permissions, file hashes and backup
  count. The real `unit_text` started a separate runtime unit and `verify_node`
  confirmed delivery from the installed hook. Paths containing spaces were used.
- [Foreground tunnel regression](assets/linux-node-parity/foreground-tunnel.json):
  Lima's shared ControlMaster previously let `ssh -N` return success and exit,
  misleading the desktop supervisor. An explicitly owned foreground SSH child
  now remains alive, forwards the Unix socket and preserves command exit status.

The fixtures use synthetic conversations and provider homes, no provider login,
and no additions to the user's saved remote connections. Linux logs are in
`assets/linux-node-parity/linux-build.log`. The VM was restored to its original
stopped state after the coordinated installer/analytics checks; the final Lima
inventory reports `t-bank: Stopped`.

Real Hermes/Teleport account operation still requires an existing authenticated
Teleport session; this run does not claim that its MFA or account configuration
was exercised. Terminal control remains available only for a verified live pane
on the owning node; transcript discovery does not imply control of an external
Desktop conversation. Codex official account quota is reported as unsupported
by the legacy `/usage` endpoint; transcript token/cost analytics use source-scoped
data instead of another account's quota.
