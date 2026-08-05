# Plugin development quick start

Jarvis plugins live in ordinary source folders, but a folder is never an
implicit installation or activation source. Putting code under `plugins/` only
keeps first-party plugin sources in the monorepo.

## Minimal UI plugin

Create `plugins/dev.example.hello/plugin.json` and
`plugins/dev.example.hello/ui/index.html`. The manifest uses Plugin API v2 and
declares its UI pages explicitly:

```json
{
  "schemaVersion": 2,
  "id": "dev.example.hello",
  "publisher": "example",
  "name": "Hello",
  "version": "0.1.0",
  "compatibility": {
    "jarvis": ">=0.4.0, <0.5.0",
    "pluginApi": 2
  },
  "runtime": {
    "kind": "ui-only",
    "bridgeEntry": null,
    "service": null,
    "activationEvents": []
  },
  "permissions": [],
  "contributes": {
    "pages": [
      {
        "id": "hello",
        "title": "Hello",
        "entry": "ui/index.html"
      }
    ],
    "actions": [],
    "projectActions": [],
    "chatActions": [],
    "settings": []
  },
  "state": {
    "schemaVersion": 1,
    "rollbackCompatibleThrough": 1,
    "migrations": []
  }
}
```

Use the repository fixtures and
[`schemas/plugin-manifest-v2.schema.json`](../../schemas/plugin-manifest-v2.schema.json)
as the exact field reference while the public authoring guide is still being
expanded.

Validate the source without installing it:

```sh
jarvis plugin validate ./plugins/dev.example.hello
```

Build a deterministic local developer archive:

```sh
jarvis plugin pack ./plugins/dev.example.hello
```

The output is marked `developer-unverified`. It is useful for reproducibility
checks, but it is not a trusted catalog release and cannot bypass publisher
signature verification.

## Immutable Developer Mode

Developer Mode is off by default. Enable it explicitly and link the source:

```sh
jarvis plugin developer-mode enable
jarvis plugin link ./plugins/dev.example.hello
```

Jarvis validates, packages and extracts the source into an immutable
digest-addressed snapshot. Runtime activation never uses the mutable source
folder. If permissions or the digest changed, inspect the returned plan and
repeat with explicit consent:

```sh
jarvis plugin link ./plugins/dev.example.hello --accept-permissions
jarvis plugin reload dev.example.hello --accept-permissions
```

Verified-native developer plugins additionally require the exact digest printed
by the plan:

```sh
jarvis plugin reload dev.example.native \
  --accept-permissions \
  --trust-native-digest sha256:EXACT_DIGEST
```

Native consent is intentionally invalid after every Jarvis restart. Unlinking
retains plugin-owned data:

```sh
jarvis plugin unlink dev.example.hello
jarvis plugin developer-mode disable
```

Disabling Developer Mode revokes active developer generations before the
setting is persisted.

## Trusted installation and diagnostics

Catalog installs are two-phase. Preparation downloads and verifies the signed
release, then prints its exact digest and permission diff:

```sh
jarvis plugin install dev.example.hello@1.0.0
```

Commit only the printed operation:

```sh
jarvis plugin install --commit OPERATION_ID --accept-permissions
```

For native code, also pass the exact `--trust-native-digest` printed by
preparation. An irreversible state migration additionally requires
`--approve-irreversible-migration`. Missing flags fail with exit code 2; the CLI
does not silently approve from non-interactive stdin.

Inspect durable manager state and redacted runtime logs with:

```sh
jarvis plugin list
jarvis plugin doctor dev.example.hello
jarvis plugin logs dev.example.hello
```

`doctor` reports recoverable interrupted operations and receipt problems.
Plugin logs redact lines likely to contain tokens, authorization values,
passwords or secrets.

