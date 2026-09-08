# Machines and connections design QA

The standalone Machines launcher module renders the production inventory and
selected-machine inspector, without the Settings sidebar.
`scripts/qa/connections-design.cjs` replaces only the native bridge with strict
synthetic SSH/Teleport and VM responses. It never reaches a real machine, repairs
hooks, changes account settings, or installs software.

Run with `node scripts/qa/connections-design.cjs`. Screenshots and the result
manifest are written to `docs/qa/assets/connections-design/`. The manifest records
source SHA-256 hashes so screenshots can be tied to the tested UI files.

Latest browser run: **24 checks passed**, no browser errors and no unexpected
bridge methods. Evidence: `assets/connections-design/checks.json`, generated
2026-09-05 at 19:54 UTC. Screenshots were inspected at 1440, 1040 and 720 pixels,
including the compact inspector after selecting a card.
The manifest includes renderer, workspace, settings layout and VM stylesheet
hashes, as well as the connection controller and component styles.

Covered behavior:

- Launcher click, Cmd8, Escape and Settings/Back enter and leave the standalone
  module without retaining duplicate pane IDs or the Settings sidebar.
- Root search for VM and CmdK search for Teleport open Machines with Enter.
- A delayed Settings plugin response cannot clear an already active Machines
  module.
- Card selection, Enter activation, search and type filters, empty results.
- Read-only selection; explicit checks target the selected connection and recover
  from transport errors.
- Maintenance is separate from the overview. Codex hook repair retains both the
  machine name and exact provider source ID.
- Dark, light and compact layouts without horizontal overflow.
- Selecting a card on a compact screen brings its inspector into view.
- Add, Cancel and Escape preserve the SSH draft. Cancelling pending preflight
  invalidates its response and restores an enabled check action on reopen.
- Actual designed Teleport selectors retain proxy, cluster, SSH login and VM ID.
  Escape dismisses the picker without closing the connection editor.
- Probe success does not install anything until the explicit install action.
- Installation events sanitize terminal escapes, retain progress across module
  navigation, ignore completion for a different machine, update the inventory,
  and open a new-chat draft on the installed machine.
- A minimized installation resumes with its original connection and progress.
  Completed installation selects its machine once; later refreshes preserve the
  user's new selection.
- Expired Teleport access uses the SSO flow and observes renewed status without
  presenting an SSH password form.
- Local VM start/stop preserves selection and follows returned status.
- Removal requires confirmation and selects a surviving machine; an empty list
  retains a working Add entry point.

`ui/settings-connections.test.mjs` retains transport identity, stale-response,
authorization, reinstall, failed save and terminal-output regressions.
`ui/settings-machines.test.mjs` checks VM capability controls, multiple mounts,
failed VM actions, partial inventory and settings persistence in the new inspector.
These two scoped suites passed **35 tests** with the standalone controller.

`scripts/qa/native-connections.js` is a separate real WKWebView scenario for the
debug `native-smoke.mjs` harness. Its updated five-step scenario reads the real
bridge inventory, checks standalone launcher/Cmd8/Settings Back navigation, card
selection and search, edits an unsaved SSH draft, verifies Cancel/Escape and field
geometry, and captures dark/light screenshots. It makes no connection, login,
installation, hook or VM action requests.

Latest native Machines result: **5 checks passed**, no JavaScript errors, at
988×747 CSS pixels with DPR 2, on 2026-09-05 at 19:59 UTC.
Evidence and four reviewed screenshots are copied to
`assets/connections-design/native.json` and `native-connections-*.png`. The
isolated real inventory contained one local VM and no configured remotes; remote
transport workflows are therefore covered by the separate browser fixture.
These current native artifacts verify the standalone module and replace the
earlier four-step Settings-wrapper evidence.

Both native scenarios use a stable debug bundle copied to
`/private/tmp/jarvis-machines-qa-bundle/Jarvis.app`; its signature passed
`codesign --verify --deep --strict`. Executable SHA-256 is
`f3cbb88bebbf7e81aba30275f0b985621da8b6907b8a55c014be4db4d9f578f3`.
The full Machines run output is `assets/connections-design/native-run.log`;
the complete native report remains in the profile linked by `native.json`.

The initial native run interacted with a retained pane before it was stably
visible during startup. The scenario now waits for visible navigation and a
bounded initial-route stability check, reopening only through the actual launcher
control if startup returns to Home. The current successful run needed no retry.
No product CSS, route or visibility was forced by the test.

Latest `native-modules.js` result: **30 checks passed**, no JavaScript errors, on
the same stable bundle at 20:03 UTC. It opened all nine modules and navigated all
13 Settings panes; verified Machines Cmd8, filter-clearing Escape and return to
the previous Settings pane; saved a setting through real IPC and reopened it;
checked a real missing-model error; and restored the light launcher.
Evidence is `assets/connections-design/native-modules.json` and
`native-modules-run.log`, with screenshots prefixed `modules-`.

The first modules run passed 26 steps before stopping at the settings-toggle
test: the backend exposed the persisted value while the UI's saving request
was still busy. The fixture now waits up to five seconds for that control to
leave its busy state before checking the saved value; the subsequent run passed
without a product change. The initial result remains in
`assets/connections-design/native-modules-initial.json` and
`native-modules-initial-run.log`, and the full original report and screenshots
remain in its recorded temporary profile.

## Standalone production application

The common signed release bundle was restarted and inspected through native
accessibility and screenshots. Launcher → Machines and Cmd8 open the inventory
without Settings navigation. Two registered remotes and one local VM are shown.
An imported Codex Personal chat exposes the explicit Continue in Jarvis action;
this check did not start a continuation or submit messages.

Evidence: `assets/connections-design/standalone-production.json`, including the
running PID and binary hash. At the initial inspection both remote cards reported
a 15-second SSH tunnel timeout. The integration task is verifying their reconnect;
this visual check does not claim that remote transport was healthy.

## Previous local application validation

The following production and native evidence predates the standalone Machines
module. It does not verify the current module build.

The common local application bundle was built with `wakeword-ort,whisper-native,stt-vad`
and launched on 2026-09-05 at 19:14 UTC. `codesign --verify --deep --strict`
passed. The running application returned a valid `/state` response with 219
sessions. The UI source hashes matched the then-current 20-check browser manifest.
Executable hash, PID, node source fingerprint and source comparisons are in
`assets/connections-design/production.json`. No version publication was performed.

The final common UI suite passed **322 tests**. Terminal native verification
passed **6 checks** with actual top and bottom visibility of the output; its
separate report is `assets/terminal-native-open/native.json`. That check found and
fixed short terminal output being scrolled above the outer viewport. Shared
terminal decoder and remote environment work was coordinated with the chat and
terminal task before this common build. The connection design validation above
does not imply complete VM or account integration coverage.

The running production page was also inspected through native accessibility and
a screenshot: three machine cards, both remote cards online, selected coder
inspector, and 477 sessions after background imports. The current external Codex
chat was visible with an explicit read-only state. A blank Claude draft was then
prepared for `ticksly-runner-2-coder` and `/home/coder/workspace/ticksly`; no prompt
was submitted.

The live source-discovery follow-up was fixed: automatic node and installer
discovery skip complete backup/backups/bak name tokens while preserving explicit
homes, old accounts and canonical IDs. Default and named profiles now have
readable labels. Six node and two installer Rust regressions passed. Both remote
nodes were updated and their running executable hashes and `/hello` sources
verified: only Claude and Codex remain. The single automatically added backup
manifest entry and its ten generated Jarvis hooks were removed with backups;
credentials and real agent processes were untouched. Evidence is
`assets/connections-design/profile-discovery.json`. The installer source fix is
coordinated for the next common desktop bundle with the active chat-continuation
task; the already launched design application was kept running.
