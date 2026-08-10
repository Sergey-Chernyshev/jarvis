# Plugin Manifest v2

`plugin.json` is the closed, declarative contract between a plugin package and
Jarvis. Manifest v2 uses JSON Schema Draft 2020-12 and Plugin API 2. Unknown
root or nested fields are rejected.

This increment validates manifests only. Package inspection, signature trust,
permission consent, installation, and execution are separate stages. Manifest
validation never starts a native entry.

## Minimal UI-only plugin

```json
{
  "schemaVersion": 2,
  "id": "dev.example.hello-page",
  "name": "Hello Page",
  "version": "1.0.0",
  "publisher": "example",
  "compatibility": {
    "jarvis": ">=0.4.0, <0.5.0",
    "pluginApi": 2
  },
  "runtime": {
    "kind": "ui-only",
    "protocol": 2,
    "activationEvents": ["onPage:home", "onCommand:dev.example.hello-page.open"]
  },
  "permissions": [],
  "state": {
    "schemaVersion": 1,
    "migrations": [],
    "rollbackCompatibleThrough": 1
  },
  "contributes": {
    "pages": [
      {
        "id": "home",
        "title": "Hello",
        "entry": "ui/pages/home/index.html",
        "placements": ["sidebar", "commandPalette"],
        "instancePolicy": "singleton"
      }
    ],
    "commands": [
      {
        "id": "dev.example.hello-page.open",
        "title": "Open Hello",
        "risk": "read",
        "placements": ["globalPalette"],
        "handler": {
          "type": "openPage",
          "page": "home"
        }
      }
    ],
    "actions": [],
    "hotkeys": [],
    "settings": [],
    "projectRuntimes": [],
    "dataContracts": []
  }
}
```

## Minimal verified-native source plugin

Only a publisher accepted for native distribution may ultimately install a
`verified-native` package. That trust decision is not inferred from the plugin
ID or from this manifest.

```json
{
  "schemaVersion": 2,
  "id": "dev.example.native",
  "name": "Example Native",
  "version": "1.2.3",
  "publisher": "example",
  "compatibility": {
    "jarvis": ">=0.4.0, <0.5.0",
    "pluginApi": 2
  },
  "runtime": {
    "kind": "verified-native",
    "lifecycle": "service-bridge",
    "bridgeEntry": "bin/${target}/example-bridge",
    "service": {
      "id": "example-controller",
      "manager": "launchd-user",
      "entry": "bin/${target}/example-controller",
      "survivesCoreExit": true
    },
    "protocol": 2,
    "activationEvents": [
      "onPage:manager",
      "onCommand:dev.example.native.open",
      "onDataContract:dev.example.native/runtime@1.0.0"
    ]
  },
  "permissions": [
    {
      "id": "projects.read",
      "scope": "selected"
    },
    {
      "id": "process.vm-provider"
    }
  ],
  "state": {
    "schemaVersion": 1,
    "migrations": [],
    "rollbackCompatibleThrough": 1
  },
  "contributes": {
    "pages": [
      {
        "id": "manager",
        "title": "Native Manager",
        "entry": "ui/pages/manager/index.html",
        "placements": ["sidebar"],
        "instancePolicy": "singleton"
      }
    ],
    "commands": [
      {
        "id": "dev.example.native.open",
        "title": "Open Native Manager",
        "risk": "read",
        "placements": ["globalPalette"],
        "handler": {
          "type": "openPage",
          "page": "manager"
        }
      }
    ],
    "actions": [
      {
        "id": "dev.example.native.open-action",
        "title": "Open Native Manager",
        "icon": "server-play",
        "locations": ["project.actions"],
        "command": "dev.example.native.open",
        "when": "plugin.enabled",
        "context": ["project.id"]
      }
    ],
    "hotkeys": [
      {
        "command": "dev.example.native.open",
        "default": "Cmd+Shift+N",
        "scope": "global"
      }
    ],
    "settings": [
      {
        "id": "dev.example.native.max-workers",
        "title": "Maximum workers",
        "type": "integer",
        "default": 2,
        "minimum": 1,
        "maximum": 8
      }
    ],
    "projectRuntimes": [],
    "dataContracts": [
      {
        "id": "dev.example.native/runtime@1.0.0",
        "kind": "entity",
        "schema": "schemas/runtime.schema.json",
        "visibility": "granted",
        "sensitivity": "internal"
      }
    ]
  }
}
```

Source validation substitutes `${target}` only in `runtime.bridgeEntry` and
`runtime.service.entry`. The supported target tokens are `darwin-arm64` and
`darwin-amd64`. A packaged manifest must contain the concrete token and cannot
contain `${...}` anywhere.

## Identity and versions

Community plugin IDs are lowercase, namespaced IDs such as
`dev.example.hello-page`. Short IDs are reserved for the owner publisher
`jarvis-owner`; publisher signatures and catalog policy provide the actual
ownership proof in later install stages. Contribution IDs are unique across a
plugin.

`version` and every data-contract ID use a complete semantic version. A
contract ID has the form `namespace/contract@1.2.3`; ranges are not accepted in
contract IDs. A dotted plugin may declare contracts only in the namespace equal
to its plugin ID. An owner short ID uses `dev.jarvis.<plugin-id>`; for example,
`agent-vm` declares `dev.jarvis.agent-vm/...`. External provider and core
contract references are not restricted by this declaration rule. The
`jarvis-owner` text alone is not trust: the signed catalog and publisher
entitlement establish ownership during installation.

`compatibility.jarvis` uses the SemVer requirement grammar implemented by the
Rust `semver` crate. Multiple comparators must be comma-separated:

```json
{ "jarvis": ">=0.4.0, <0.5.0", "pluginApi": 2 }
```

The whitespace-only form `">=0.4.0 <0.5.0"` is invalid. A structurally valid
manifest still fails with `manifest_incompatible` when the running Jarvis
version or Plugin API does not match.

## Paths, references, and limits

Every declared package path is relative, NFC-normalized UTF-8, control-free,
and safe to embed in a `jarvis-plugin:` URL. Absolute paths, colons,
backslashes, percent encoding, query or fragment delimiters, empty components,
`.` and `..` components, repeated separators, trailing separators, and control
bytes are rejected. File existence and archive membership are checked by the
package layer after manifest validation.

State migration `from` and `to` versions start at 1 and each edge must move
strictly forward.

Page, command, action, hotkey, runtime, activation-event, and data-contract
references must resolve to declarations in the same manifest where the
contract requires a local reference. Project runtime lifecycle commands must
reference declared command contracts, and its extension contracts must
reference declared data contracts.

Host validation rejects manifests over 256 KiB, JSON deeper than 64 levels,
more than 20,000 JSON nodes, and individual strings or object keys over 64 KiB.
Duplicate object keys and non-local JSON Schema `$ref` values are rejected.

The current schema intentionally describes a small closed v2 surface. A field
or contribution kind not present in the schema is unsupported, not an
extension point.

## Permissions and updates

The manifest permission list is a requested ceiling, not an ambient grant.
The package manager compares it with the current install receipt. New or
broader permissions require explicit consent before activation; removed
permissions are no longer granted. Native execution also requires
exact-package-digest consent and cannot happen during manifest validation.

`admin` and arbitrary shell permissions do not exist. Filesystem mounts must
declare one or both of the finite `read` and `write` modes.

The accepted shapes are capability-specific:

| Permission                                                                                       | Required scope                                    | Modes                            |
| ------------------------------------------------------------------------------------------------ | ------------------------------------------------- | -------------------------------- |
| `projects.read`                                                                                  | `"selected"`                                      | forbidden                        |
| `filesystem.mount`                                                                               | `"selected"`                                      | unique subset of `read`, `write` |
| `memory.read`, `memory.propose-write`                                                            | non-empty array from `global`, `selected-project` | forbidden                        |
| `credentials.request`                                                                            | non-empty array from `claude`, `codex`            | forbidden                        |
| `chat.composer.text.read`                                                                        | `"invocation"`                                    | forbidden                        |
| `notifications.publish`, `process.vm-provider`, `chat.compose.contribute`, `projects.contribute` | forbidden                                         | forbidden                        |

Missing required scopes, unexpected scopes or modes, and duplicate scope or
mode values are rejected.

## Validate the schema

From the repository root, validate a manifest's closed JSON shape with:

```sh
npx --yes ajv-cli@5 validate --spec=draft2020 \
  -s schemas/plugin-manifest-v2.schema.json \
  -d path/to/plugin.json
```

The public typed parser runs first so SDK and host return identical stable error
codes. The bundled schema is then a defense-in-depth structural pass. Together
they enforce semantic versions, canonical paths, cross-references,
compatibility, duplicate keys, templates, and quotas. The repository gate for
both passes is:

```sh
cargo test --manifest-path crates/jarvis-plugin-protocol/Cargo.toml manifest
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  plugins::manifest_v2::tests
```

## Manifest v1 transition

Manifest v1 is accepted only for the bundled `agent-vm` migration transition.
Task A2 deliberately leaves the existing v1 parser and Agent VM runtime
untouched; it does not make arbitrary v1 plugins trusted or installable.
Receipt-backed resolution removes that compatibility path in the later Agent
VM migration increment.
