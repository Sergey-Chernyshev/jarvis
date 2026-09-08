# Projects and machines

Jarvis keeps projects as working locations: **machine + absolute directory**. Local and remote folders with the same path are separate projects. A project can be saved before it has a chat; saving and removing its catalog entry only changes Jarvis metadata.

The Projects page combines saved locations, transcript history and current sessions. It supports names, pinning, machine/search filters, active/saved filters, recent/name ordering, and local or remote folder selection. An unavailable machine retains its saved projects and history; starting and continuing chats are disabled until it reconnects. A failed save keeps the editable form.

Opening a project shows its conversations and current attention/work counts. “New chat” carries its machine and directory into the shared chat composer, where provider and task options remain in one place. Continuing a historical conversation uses the existing launch service and correlates its exact `launchId`, including when an event precedes the IPC response. Navigating elsewhere prevents a later launch completion from unexpectedly opening a chat. The Back action restores the catalog search/filter and also keeps a form draft when returning from another page.

## Machines and VM settings

The stable `remotes` settings route is now named “Машины и VM”. It owns:

- SSH connections and their existing connection wizard; maintenance details are collapsed.
- Agent VM inventory, reconciled against Lima runtime state. Recognized managed VM rows expose start, stop and opening their existing spec according to capabilities. VM mount/configuration details stay collapsed until requested.
- A task isolation explanation and the single Docker image editor.

“Локальный запуск” owns local terminal preferences. Legacy global permissions and proxy settings are under its advanced disclosure. VM lifecycle, a Git worktree and a Docker container have distinct meanings and are explained alongside their controls.

The installed `avm` is detected by command capabilities, including legacy builds whose version is `dev`. Modern configurations may mount several projects into one VM. See [the upstream research and exact source revisions](project-vm-research.md). Jarvis preserves existing VM specs; this change does not upgrade `avm`, recreate VMs, change mounts, or provision a new machine. To use a VM as a chat machine, connect its SSH endpoint through the existing SSH wizard. VM lifecycle inventory alone does not establish a chat transport.

## Data contracts

- `projects_list(machine?)`: `{ok, projects, warnings}`; no scope means all known machines. Partial remote failures retain available entries.
- `projects_save({machine,cwd,name?,pinned?})`: atomically updates `settings.projects`, acknowledges durable writes, emits `projects_changed`.
- `projects_remove(machine,cwd)`: removes only saved metadata; discovered chats remain.
- `vm_status()`: CLI availability/generation/version, VM inventory, explicit per-row capabilities, and partial-state warnings.
- `vm_action(name,action)`: `start`, `stop`, `open-config`; rechecks exact managed identity and runtime state before execution.

The remote node now indexes stopped Codex rollouts as well as Claude transcripts. Existing remote nodes need the updated node binary to return that additional Codex history; their live sessions remain supported. No human-formatted CLI tables are parsed.

Implementation: `ui/projects.js`, `ui/projects.css`, `ui/settings2.js`, `src-tauri/src/projects.rs`, `src-tauri/src/vm.rs`, and `src-tauri/node/src/node/projects.rs`. [Validation and screenshots](qa/projects-vm.md).

## Project avatars

Projects show their avatar in the catalog, project header and chat sidebar. Click the project image to choose artwork. Jarvis searches the project's files for favicon, apple-touch-icon, icon and logo variants, including common app/public/static/assets directories and shallow monorepo layouts. A single candidate appears automatically; several candidates show a count and open a thumbnail chooser with file paths. Automatic discovery does not save or overwrite project metadata.

The project form also accepts an uploaded image. Uploaded and chosen artwork is decoded and reduced to a PNG with a maximum side of 128 pixels before saving. A custom choice takes priority over discovery; removing it returns to automatic artwork. Failed decoding or saving keeps the previous avatar/draft. Pinning and renaming preserve an existing avatar. Saved images remain available even while the project's machine is offline; previously discovered images can also be reused from the in-memory cache without contacting the machine.

Discovery is read-only, does not start project tools or fetch websites, and excludes dependencies, build outputs and symlinks. The UI discovers visible avatars lazily with at most two concurrent requests and caches results by machine and directory. The desktop can inspect remote project files through existing SSH using Python 3; this does not require a remote jarvis-node upgrade. Own-file upload remains available if remote discovery is unavailable.

`projects_icon_candidates(machine,cwd)` returns `{ok,candidates:[{path,name,dataUrl}],truncated?}`. The optional `avatar` field in project metadata holds a bounded raster data URL plus its source and optional relative path/label. An omitted field preserves the existing value; `null` resets it. SVG candidates are restricted drawings displayed only as image sources and rasterized before persistence.
