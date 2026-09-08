# Projects and VM validation — 2026-09-05

## Backend

Focused Rust checks compiled the desktop command registrations and passed:

- Projects registry: 9 tests — durable metadata persistence, failure handling, concurrent updates, unchanged project files, canonical local paths, per-machine identity, offline entries, deduplicated sessions and Claude/Codex counts.
- VM inventory and command contracts: 7 tests — legacy/modern detection, multiple mounts, Lima JSONL/array parsing, partial state, unrelated VM restrictions and exact action targeting.
- Remote Claude/Codex project indexing: 5 tests.
- Folder browsing: 2 checks, including read-only enumeration of filesystem root.

These checks use disposable files and synthetic VM records. The real `avm` installation was inspected read-only; no real VM was started, stopped, recreated or provisioned.

## UI

`node --test ui/projects.test.mjs ui/settings-machines.test.mjs`: **23/23**.

Projects coverage includes machine/path identity, filters and pinning, failed saves retaining drafts, metadata-only removal, exact launch correlation with events before IPC replies, unrelated launch events, not navigating after leaving the project, and stale directory responses after closing/changing the browser target. VM/settings coverage includes capability gating, partial inventory, collapsed advanced settings and a single Docker image editor.

Production HTML/scripts rendered in Chrome against strict synthetic bridges:

- `node scripts/qa/projects-workspace.cjs`: **16 recorded scenarios/artifacts**, no page errors or unknown bridge calls. Includes route back/search preservation, remote new-chat handoff without starting an agent, offline history, directory selection, failed-save retry/removal, early resume events, footer semantics, direct Machines/VM navigation and compact overflow/title visibility.
- `node scripts/qa/session-workspace.cjs`: **23 checks**, no page errors or unknown bridge calls after the composer adopted saved project locations.
- `node scripts/qa/panel-browser.cjs`: **44 records**, no page errors or unknown bridge calls; includes modern multi-project VM rows, action failure/retry, config action, unavailable/partial inventory and compact settings.

Screenshots use synthetic project names and data:

- [Projects, dark desktop](assets/projects-workspace/projects-dark-desktop.png)
- [Remote project](assets/projects-workspace/project-remote-dark.png)
- [Offline project](assets/projects-workspace/project-offline-dark.png)
- [Preserved draft after save failure](assets/projects-workspace/project-save-error-draft.png)
- [Projects, compact light](assets/projects-workspace/projects-light-compact.png)
- [Project, compact light](assets/projects-workspace/project-light-compact.png)
- [Machines and VM](assets/panel/machines-modern-dark-desktop.png)
- [Machines and VM, compact light](assets/panel/machines-modern-light-compact.png)

`git diff --check` and syntax checks for modified UI scripts passed. The shared tree is under concurrent development. The previously reported five adjacent analytics form test failures were resolved; the full UI suite passed 247/247 after avatar integration. This feature was not included in the neighbor's earlier frozen HUD release snapshot and is not claimed as installed until a subsequent build includes it.


## Avatar extension

`node --test ui/project-avatars.test.mjs ui/projects.test.mjs`: **42/42**. Covers custom priority, one-versus-multiple discovery, exact machine/directory cache identity, coalescing and two-request concurrency, offline cache reuse, image limits and decode errors, retained drafts on save failure, resetting to automatic artwork, pin preservation, and stale picker/upload results.

The Chrome Projects harness includes actual SVG decoding and canvas PNG conversion, a 400×200 upload becoming 128×64, multiple thumbnails, automatic favicon display without metadata writes, a failed upload/save retry, avatar reset and saved artwork propagation into the chat sidebar. The avatar picker automatically scrolls into view. The existing Chat harness still passes 23 checks.

- [Choose a detected project icon](assets/projects-workspace/avatar-candidates-dark.png)
- [Uploaded avatar in the project and sidebar](assets/projects-workspace/avatar-uploaded-dark.png)

Automatic discovery reads project files locally or over existing SSH; it does not fetch a website. UI browser checks use explicit synthetic files/data and never connect to real remote hosts.

Avatar backend validation: **8/8 scanner tests** passed, covering one/multiple/Next.js app icons, bounded reads, dependency/deep-path exclusion, symlink containment under concurrent directory changes, inert SVG validation, and raster/data URL validation. The shipped SSH Python script was executed locally against disposable fixtures and matched Rust scanner results; shell metacharacters in the root path remained literal. No real remote host or user project was modified by these tests.

The updated Projects backend suite passed **11/11**, including durable avatar persistence across reloads, preservation on pin updates, explicit reset to `null`, and rejection without overwriting saved data. Desktop IPC/main registrations compile with the new scanner. The shared Cargo target and all avatar/project file ownership were released to the coordinating integration task after these checks.
