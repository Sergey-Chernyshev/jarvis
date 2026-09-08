# Designed select controls

The request is to replace the system dropdowns visible in the new-chat composer with controls that fit Jarvis: clear selection, considered spacing, rounded menus, color accents, and smooth opening and dismissal. The same interaction should apply to the other single-choice fields throughout the application.

## Implementation review requirements

- Preserve the original select's ID, value, options, disabled state, `change` listeners, and form validation. Display one accessible combobox instead of a duplicate native control.
- Synchronize programmatic value writes, asynchronous catalogs, option selection and disabled states, and field visibility. Codex's unavailable planning permission must remain unavailable.
- Present the menu outside scrolling/clipping panels; fit it within the viewport and close it when its owning control is removed, hidden, or disabled.
- Arrow keys highlight choices without submitting the surrounding form. Enter commits once; Escape closes only the popup and restores focus. Respect composition input and shortcut recording.
- Preserve visible form labels, required-field feedback, focus restoration after a rerender, and drafts during periodic module updates.
- Keep local and remote task routing, profile configuration, and provider model catalogs unchanged by cosmetic changes.

## Verification status

The shared picker is implemented in `ui/select-control.js` and `ui/select-control.css`. Its native select remains the authority for values and form behavior; the visible trigger and portal reflect that state.

Independent Chromium boundary verification passed **9/9** cases with `node scripts/qa/select-boundaries.cjs`:

- Popup placement at all four viewport corners across 32 combinations of 800×540 / 420×360 viewports and 0.8× / 1× / 1.25× / 1.5× root CSS zoom.
- Unique accessible IDs during rapid close/reopen, including the outgoing animation.
- A disabled fieldset closes its popup and disables the trigger.
- Native form reset and direct option selection update the visible value without extra change events.
- Disabled optgroups cannot be chosen with the keyboard.
- Hiding the owning panel, removing it, or removing only the native select clears the popup and any orphaned trigger.
- Required-field validation focuses the visible control.
- Frame samples show intermediate opacity values during both opening and closing, followed by the delayed popup removal.

The audit found and verified fixes for two defects: duplicate outgoing IDs on fast reopen and an orphaned trigger after removing only the original select. Existing loop/bundle regression tests also passed **24/24** after adapting polling and modal focus handling to comboboxes.

The complete application Chromium run passed **12/12** groups with no page errors (`scripts/qa/select-controls.cjs`). It used the production page and synthetic bridge responses to exercise provider/profile/model selection, search, disabled permissions and offline machines, keyboard/typeahead/Tab/outside click, failed settings-save rollback, successful settings and project rerender focus, required analytics fields, and 650/900 px dark/light layouts. These interactions never launched a real agent or wrote account settings.

`scripts/qa/native-selects.js` passed **6/6** groups in the isolated native smoke harness using the rebuilt, signed debug bundle. It uses actual WKWebView rendering, the production bridge, and registered profile catalogs (8 choices for the local personal Codex model selector). It changes only new-task draft selections; it does not launch an agent, save a profile, install hooks, capture audio, or connect to a remote machine.

The native scenario covers the provider popup, profile/model async loading, keyboard selection and single change delivery, disabled planning, machine cancellation, settings label focus, Escape ownership, clipping, and dark/light snapshots. Keyboard events in this scenario are DOM-dispatched, not hardware input. When an unfocused WebKit compositor suspends finite animations, the harness finishes those animations for static screenshots and records that policy; those screenshots do not establish native motion behavior.

All existing UI unit tests passed **299/299**. Debug app signature verification passed. No Rust backend behavior changed in this pass.

Reports, source hashes, and reviewed screenshots are in `docs/qa/assets/select-controls/`. `native-report.json` intentionally excludes the captured full DOM and retains only the relevant checks, runtime metadata, and catalog evidence. `report.json` records the full-page browser checks; boundary assertions are executable in `scripts/qa/select-boundaries.cjs`.

The release bundle was rebuilt and its signature verified, then the local production app was restarted gracefully. Verification found one process (PID 77197) and a responsive state endpoint with 221 discovered sessions. Executable SHA-256: `abb4df679b09a6ff4654124e742b09332333ffd406bd8112d341d9f20bd1cca8`. The picker/integration source hashes stayed unchanged through the final build. See `production.json` for the executable path and health snapshot.
