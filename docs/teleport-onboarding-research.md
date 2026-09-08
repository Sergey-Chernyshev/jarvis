# Teleport onboarding: verified CLI contract

Inspected on 2026-09-05, with the installed macOS client and official Teleport documentation/source. Only version/help/local status and one bounded node inventory request ran. No login, logout, SSH session, provisioning, or remote file changes were performed. This document contains whitelisted connection metadata; no credentials, certificates, or SSO claims.

## Local evidence

- Executable: `/opt/homebrew/bin/tsh`, client version `18.10.0`, runtime `go1.26.5`.
- `tsh --version` is invalid here. Use `tsh version --client --format=json`, which returned `{ "version": "18.10.0", "gitref": "", "runtime": "go1.26.5" }`.
- `tsh status --client --format=json` succeeds without contacting the server. Snapshot checked at `2026-09-05T13:44:06Z`:

| Profile | Cluster | Allowed SSH logins | Certificate expiry | State at inspection |
| --- | --- | --- | --- | --- |
| `https://tl.ticksly.ru:443` (active) | `tl.ticksly.ru` | `coder`, `root` | `2026-09-05T19:24:29+03:00` | Valid |
| `https://teleport.tcsbank.ru:443` | `teleport.tcsbank.ru` | `se.chernyshev` | `2026-09-03T01:00:58+03:00` | Expired |

Expiry describes the cached certificate, not proof that a particular server or OS account is reachable. Recompute it on every status response; never hardcode these observations.

Current `~/.jarvis/settings.json` and `~/.jarvis-dev/settings.json` have empty `remotes`. Jarvis reads `<jarvis_dir>/settings.json`. `~/.ssh/config` includes `~/.ssh/tl-ticksly.conf`, whose Teleport wildcard rules use proxy `tl.ticksly.ru:443`, cluster `tl.ticksly.ru`, and SSH port `3022`. They do not identify a concrete Hermes server or OS user. An SSH alias/config and a native Teleport connection are distinct choices in the UI.

At `2026-09-05T13:52:24Z`, a 15-second-bounded request with null stdin succeeded in under one second:

```sh
tsh --browser-login=none --proxy=tl.ticksly.ru:443 ls --cluster=tl.ticksly.ru --format=json
```

It returned 11 nodes: `agents-0.htz.ticksly.host`, `agents-1.htz.ticksly.host`, `agents-2.htz.ticksly.host`, `compute-vm-2-1-20-hdd-1779475825706`, `ticksly-dev-department.play2go.cloud`, `ticksly-oneuptime.play2go.cloud`, `ticksly-posthog.ticksly.host`, `ticksly-runner-2.play2go.cloud`, `ticksly-runner-4.ticksly.host`, `ticksly-tp`, and `ticksly-wiki`.

The screenshot's `ticksly-runner-2.play2go.cloud` exists, with node ID `4829bdbd-43bc-43e1-b8ec-c9f4894ca785` and labels `need=runner`, `type=vm`. No node name/label identifies Hermes. The screenshot establishes a candidate, not authority to assume the Hermes process owner or install into it.

## Commands and parsing

| Purpose | Verified installed-client command | Result/handling |
| --- | --- | --- |
| Installed version | `tsh version --client --format=json` | Read `version`; tolerate unknown extra fields. |
| Cached profiles | `tsh status --client --format=json` | Object with optional `active`, array `profiles`, optional `environment`. |
| Explicit sign-in | `tsh --proxy=PROXY login [CLUSTER]` | Interactive terminal/browser operation. `CLUSTER` is positional here. Afterwards re-read status. |
| Nodes | `tsh --browser-login=none --proxy=PROXY ls --cluster=CLUSTER --format=json` | Array of node resources, empty list is `[]`. |
| Clusters | `tsh --browser-login=none --proxy=PROXY clusters --format=json` | Source-defined array containing `cluster_name`, `status`, `cluster_type`, `labels`, `selected`; not run against the account. |
| SSH command/tunnel | `tsh ssh --no-forward-agent --no-relogin --request-mode=off --proxy=PROXY --cluster=CLUSTER LOGIN@NODE ...` | These suppression flags are accepted for `ssh`, not `ls`. |

Status: retain only `profile_url`, `username`, `cluster`, `logins`, and `valid_until`, plus a derived active/expired state. Real output includes a `traits` object containing SSO claims; do not pass the original JSON or arbitrary stderr through IPC or into logs. Empty/unparseable status is different from an expired profile. Select the profile matching the requested proxy instead of assuming the active profile matches.

An expired active profile can produce valid JSON and a nonzero exit code; parse the metadata before classifying the failure. With no profiles, this client exits with an error before emitting JSON.

Node mapping, verified against the real response: `metadata.name` → stable node ID; `spec.hostname` → display name; `metadata.labels` → optional bounded display labels. A record may omit labels. Keep the selected proxy and cluster alongside the node ID. Do not confuse a node's SSH port with Jarvis's own TCP listening port. The [official client guide](https://goteleport.com/docs/connect-your-client/teleport-clients/tsh/) distinguishes Teleport identity (`--user`), remote OS login (`login@host`), cluster selection, and local port forwarding.

Use the node ID for an exact inventory selection while displaying its hostname. Teleport uses UUIDs to distinguish duplicate hostnames; enrolled SSH host certificates include the UUID. Keep a manual hostname target available for existing setups. See [official OpenSSH enrollment documentation](https://goteleport.com/docs/enroll-resources/server-access/openssh/openssh-manual-install/). A profile's allowed logins are selection candidates, not a guarantee of access to every listed node. An optional `runAsUser` such as Hermes's owner remains a separate explicitly configured OS account.

## Noninteractive discovery caveat

The v18.10 client implements node and cluster listing through `RetryWithRelogin`. Null stdin alone does not prevent SSO from opening a browser. The `NonInteractive` client option short-circuits re-login, but the CLI sets it when using an identity file; do not export/copy credentials merely to enable discovery. This behavior is visible in [client/api.go](https://github.com/gravitational/teleport/blob/v18.10.0/lib/client/api.go).

For this installed version, the hidden global `--browser-login=none` is accepted and maps to `TELEPORT_LOGIN_BROWSER=none`. `--browser=none` is login-scoped and is rejected by `ls`. The SSO redirector receives the client's browser setting, including during retry: [client/sso.go](https://github.com/gravitational/teleport/blob/v18.10.0/lib/client/sso.go). Suppressing browser opening does not disable every authentication attempt; a failed request may still print an SSO link and wait. Do not expose or automatically open that link as a discovery error.

Jarvis should gate discovery on a valid matching cached profile, use the verified browser suppression flag, null stdin, a timeout, and bounded output. Convert auth failures/timeouts to a concise status with an explicit sign-in action. Keep discovery user-triggered or deduplicated, and never silently choose a different proxy. Probe flags when supporting other client versions; fail visibly rather than dropping suppression on an unknown version. Avoid `ls --all`, which queries additional profiles/clusters outside the selected connection.

The pinned [CLI implementation](https://github.com/gravitational/teleport/blob/v18.10.0/tool/tsh/common/tsh.go) confirms command scoping and JSON serialization. These installed-client help checks all succeeded without network/authentication:

```sh
tsh help version
tsh help status
tsh help login
tsh help ls
tsh help clusters
tsh --browser-login=none ls --help
tsh ssh --no-relogin --request-mode=off --help
```

The following are invalid in this version: `tsh --version`, `tsh ls --no-relogin`, `tsh ls --request-mode=off`, and `tsh ls --browser=none`. Also, `login --format` selects an exported identity format, not a JSON status format. The current [CLI reference](https://goteleport.com/docs/reference/cli/tsh/) can describe newer flags (for example login `--force`); installed help takes precedence for capability detection.

## Suggested Jarvis onboarding flow

1. Show native SSH and Teleport as clear transport choices. For Teleport, detect `tsh`, show cached profiles and expiry, and prefill the selected proxy/cluster from safe status metadata.
2. Missing/expired profile: an explicit “Войти через Teleport” action opens `tsh login` in the chosen local terminal. Returning from the terminal is not proof of success; refresh status.
3. Valid profile: “Показать серверы” loads a bounded inventory for that profile/cluster. Preserve manual target entry and explain an empty list separately from network/auth errors.
4. Select server and OS login; show optional process-owner settings separately. Save the explicit proxy, cluster, login, and node identity so later changes to the active tsh profile cannot redirect a saved connection.
5. Check Jarvis readiness only for the selected connection. Distinguish authentication, SSH reachability, remote executable availability, and Jarvis daemon state. Discovery alone establishes none of the latter three.

This flow is applicable to the app's macOS/Linux scope. Windows-specific terminal and credential handling was intentionally not assumed or tested.

For missing-client help, link to [the official client guide](https://goteleport.com/docs/connect-your-client/teleport-clients/tsh/) or [macOS client installation](https://goteleport.com/docs/installation/macos/). The latter provides the signed PKG client tools option. Opening installation documentation is separate from executing an installer.
