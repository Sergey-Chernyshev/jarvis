# Projects and Agent VM integration

Inspected on 2026-09-05. The public upstream is [MikD1/agent-vm](https://github.com/MikD1/agent-vm). Its actual release names are **v0.2** and **v0.3**, not separate public v2/v3 repositories. The current main revision is `fb8325a3a78bd9223fc4927a6d1a5db0a221e5cf`, tagged **v.0.10** (including that dot). Discovery included the owner's public repositories, branches, tags and releases; a private or differently named successor cannot be ruled out.

## What changed upstream

| Contract | v0.2 (`e11870c3881716ecfdae3dd32efe1f534cc2d7aa`) | v0.3 (`a8a7cf76fcb89e3420d331e90aaa31f18dfab7e8`) and current |
| --- | --- | --- |
| VM ownership | One VM per project, mount or clone workspace | One VM per domain of work, with multiple mounted projects |
| Human configuration | Project `.agent-vm.yaml` | Dedicated VM directory containing `agent-vm.yaml` |
| Recorded workspace | `workspace.mode`, `hostPath`, `guestPath` | `configDir`, guest `home`, `mounts[{hostPath,guestPath}]` |
| Tools | Selected shell provisioning modules | mise tool references and optional versions, scalar or single-key YAML map |
| Configuration resources | Shared `~/.config/agent-vm` | VM-owned files/scripts/certificates alongside its spec |

Sources: [v0.2 README](https://github.com/MikD1/agent-vm/blob/e11870c3881716ecfdae3dd32efe1f534cc2d7aa/README.md), [v0.3 README](https://github.com/MikD1/agent-vm/blob/a8a7cf76fcb89e3420d331e90aaa31f18dfab7e8/README.md), [current record schema](https://github.com/MikD1/agent-vm/blob/fb8325a3a78bd9223fc4927a6d1a5db0a221e5cf/internal/registry/record.go), [resolved mount schema](https://github.com/MikD1/agent-vm/blob/fb8325a3a78bd9223fc4927a6d1a5db0a221e5cf/internal/config/resolve.go).

Current `avm mount`/`unmount` can change project mounts; applying changes to a running VM requires restart. `recreate` rebuilds the VM, so it must not masquerade as an ordinary settings save. Omitted modules and an explicit empty list are different. Jarvis should preserve the upstream configuration rather than generate guessed defaults. [CLI commands](https://github.com/MikD1/agent-vm/blob/fb8325a3a78bd9223fc4927a6d1a5db0a221e5cf/internal/cli/root.go), [spec parser](https://github.com/MikD1/agent-vm/blob/fb8325a3a78bd9223fc4927a6d1a5db0a221e5cf/internal/config/spec.go).

## Actual local installation

The local source checkout is v0.1 (`4912f1d4d8c55084ebc0d0b915936ec8a82b4afe`) with existing user edits. The installed `~/.local/bin/avm --version` reports `dev`; its help advertises the legacy one-project interface and no mount/unmount commands. The registry contains a legacy mount record. Therefore a numeric version alone cannot select an adapter generation.

An external `~/.jarvis/plugins/agent-vm` binary advertises chat/resume/cancel/files/shell-command in a v0.1 manifest. The current Rust application has no corresponding runtime plugin host; the repository contains a design specification. A manifest's declared capabilities are not proof of an operational Jarvis transport. Lifecycle inventory can be integrated directly now; VM chat transport needs its own actual adapter.

## Integration decisions

- Projects are persistent workspace identities with a machine and directory. They may exist before the first chat. The same VM can serve multiple projects; a project is not a VM.
- One settings area owns machine connections and managed VM inventory. Local launch preferences, per-task worktrees and Docker images retain their own meanings instead of becoming duplicate VM configuration forms.
- Neither inspected legacy nor current `avm list` offers JSON. Read typed metadata from `${XDG_CONFIG_HOME:-~/.config}/agent-vm/vms/*.yaml`, and reconcile live state with `limactl list --json` (JSON objects separated by newlines). Do not scrape the decorative table. [Registry store](https://github.com/MikD1/agent-vm/blob/fb8325a3a78bd9223fc4927a6d1a5db0a221e5cf/internal/registry/store.go), [list implementation](https://github.com/MikD1/agent-vm/blob/fb8325a3a78bd9223fc4927a6d1a5db0a221e5cf/internal/cli/list.go).
- Discover the CLI's advertised commands. Enable `avm start <name>`/`avm stop <name>` only for a fresh recognized managed VM and known runtime state. Do not normalize a user-supplied name into a different target. [Lifecycle implementation](https://github.com/MikD1/agent-vm/blob/fb8325a3a78bd9223fc4927a6d1a5db0a221e5cf/internal/cli/lifecycle.go).
- Keep unrelated Lima instances read-only. A failed Lima query means unknown state, not an empty inventory or missing VMs. Show orphaned registry entries separately.
- Open an existing, recorded spec in a text editor. Do not read provisioning files or credentials into settings. Keep mounts and paths in expanded details; show VM name, status and useful actions first.

The new `vm.rs` implements this bounded inventory/lifecycle layer. Validation uses fixture records and command-contract tests; the real local installation was queried read-only. No user VM was started, stopped, recreated or provisioned during implementation.
