# SSH / Teleport onboarding QA

Tested 2026-09-05. The wizard uses production `settings2.js`, `bridge.js` and
the existing remote installer; browser tests substitute a strict native bridge
and never connect to a real machine.

## Verified

Final checks: 24 targeted UI tests and 11 Chrome scenarios passed. The combined
application Rust suite passed 1,144 tests, with 12 explicit environment-dependent
tests ignored. The combined native debug check passed 34 steps. Release packaging
and restart are coordinated with the main UI task.

- SSH remains the default and does not trigger Teleport discovery.
- During the initial wizard checks, the installed `tsh 18.10` profile and JSON
  inventory were inspected read-only. Eleven machines were returned. No login,
  logout, remote shell or installation was run during that initial check.
- Teleport selection passes the exact proxy, cluster, OS login and node resource
  UUID to preflight and installation. Password helpers are reserved for SSH.
- Official SSO login remains pending until a valid profile is observed. Profile
  discovery does not open a browser or return identity claims, keys or stderr.
- The setup summary explains automatic installation of missing `tmux` / `curl`.
  Connection controls and technical preflight details collapse after checking.
- Installer completion renders a connected machine. Desktop and compact layouts
  have no horizontal overflow or browser errors.
- SSH password bootstrap preserves the selected SSH config. Teleport rejects
  this path before a key or helper is created.
- Tunnel setup preserves cluster and explicit TCP port. Manual Teleport entries
  default to loopback port 7717; ordinary SSH keeps Unix socket forwarding.

## Reproduce

```sh
node --test ui/settings-connections.test.mjs ui/settings-machines.test.mjs
node scripts/qa/remote-onboarding.cjs
cargo test --manifest-path src-tauri/Cargo.toml --bin jarvis teleport::
cargo test --manifest-path src-tauri/Cargo.toml --bin jarvis remote::
```

The browser harness needs Chrome and Playwright and permission to bind its
temporary loopback HTTP server. The Rust HTTP client integration test also needs
loopback permission. In a restricted sandbox it reports `Operation not permitted`;
its isolated rerun with loopback access passed. Real SSH/VM fixture tests remain
explicitly ignored unless their target is configured.

Browser captures and check results:

- [Teleport access](assets/remote-onboarding/teleport-login-dark.png)
- [Machine selection](assets/remote-onboarding/teleport-machine-dark.png)
- [Ready to configure](assets/remote-onboarding/teleport-ready-dark.png)
- [Compact layout](assets/remote-onboarding/teleport-ready-compact.png)
- [Check results](assets/remote-onboarding/checks.json)

CLI flags and compatibility sources: [Teleport research](../teleport-onboarding-research.md).

## Zsh preflight regression — 2026-09-05

The selected VM used `/usr/bin/zsh`. Executing the POSIX probe through its login
shell failed on an unmatched `.codex-*` glob, despite valid Teleport access.
Remote commands now explicitly invoke `/bin/sh -c`, including password SSH,
while preserving stdin for uploaded payloads and any explicit run-as user.

- Reproduced the old failure and verified the fixed probe on the selected VM
  using the same Teleport profile, cluster, node and `coder` login. The fixed
  read-only probe returned Linux/x86_64 and found Claude Code and Codex.
- After rebuilding and restarting the signed local release app, repeated the
  selection and preflight in its native UI. It displayed **Машина проверена**,
  **Claude Code · Codex** and the enabled **Настроить автоматически** action.
  No remote installation was started.
- Focused Rust checks: 32 passed, 1 environment-dependent test ignored. These
  cover shell quoting, missing profile globs, stdin payloads and error cleanup.
  Targeted UI checks: 24 passed. App signature verification passed.
- Terminal control sequences are removed from displayed errors. A remote probe
  failure no longer unconditionally tells the user to renew Teleport access.

## Missing release node regression — 2026-09-05

The v0.3.3 release did not include Linux node assets, so a successful Teleport
preflight was followed by a 404 during installation. Desktop bundles now embed
both Linux musl architectures from the current source tree. The release workflow
builds those nodes before packaging the desktop app and publishes standalone
assets from the same verified bundle.

Local regression checks: 40 Rust checks passed (one explicit environment test
ignored), 27 wizard UI checks passed, and 15 offline bundle checks passed.
These cover source/lockfile freshness, SHA-256, static ELF/architecture validation,
bundled delivery without curl/Cargo, atomic activation preserving an old node on
failure, root systemd service selection, and distinguishing managed shims from
real CLIs on repeat installation. UI progress and final failures are also
sanitized independently of the native backend.

The signed local `.app` was rebuilt successfully with both verified artifacts,
then restarted (PID 82283). App executable SHA-256:
`519b57f517d5b6fa5d91690eeab870142b5cce360a053b69acd83fd31e4a5c36`.
Read-only preflight on the selected root VM confirmed a working system systemd
manager, no user manager, Claude CLI and a Codex data directory without Codex CLI.
Live installation verification is pending renewal of the Teleport session,
which expired at 19:24 MSK. No remote installation was performed by this check.
