# Jarvis config health and onboarding polish

**Date:** 2026-07-28
**Status:** Approved design

## Context

Jarvis currently reads `$JARVIS_DIR/settings.json` as dynamic JSON and merges it
with defaults. If the file is invalid JSON, has the wrong root type, or contains
wrongly typed values, the loader silently falls back to defaults. This keeps the
daemon alive, but hides the configuration problem from the user.

The readiness onboarding has a second class of problems:

- its visual hierarchy changes from screen to screen;
- headings are oversized while list details and warnings are too small and dim;
- the footer can make long diagnostics difficult to read;
- disabled and hover button states lose contrast;
- the CSS shell radius and the native Tauri effect radius differ, producing a
  doubled or uneven outline at the corners;
- generic recovery copy hides the actual installer error.

During investigation, a second debug Jarvis process was found alongside the
normal release process. The debug process was a plugin-host smoke run using
`/tmp/jarvis-plugin-host-smoke.../profile`; its log reported that `HOME` was
unavailable and that agent CLIs were absent. It nevertheless opened the normal
onboarding window. The real dev profile simultaneously reported a healthy
Claude/Codex integration. Test and smoke runtimes must not surface production UI.

## Goals

1. Validate Jarvis's own `settings.json` at startup without preventing the daemon
   from starting.
2. Show concrete configuration issues inside Jarvis and offer a safe,
   user-initiated repair.
3. Preserve valid settings, unknown forward-compatible keys, plugin blocks, and
   secrets while repairing only invalid known values.
4. Make onboarding compact, readable, and visually consistent across every
   state.
5. Show the real sanitized installer failure instead of a generic panic message.
6. Keep headless and smoke processes from opening production Jarvis windows.
7. Use one canonical Claude/Codex resolver so readiness and runtime cannot
   disagree about whether an agent is installed.

## Non-goals

- Validating or repairing Claude, Codex, shell, macOS, or third-party
  configuration files.
- Testing network connectivity, account credentials, microphones, models, or
  external services as part of config validation.
- Automatically changing a user's file without an explicit click.
- Rejecting unknown fields or plugin-specific settings.
- Replacing the onboarding flow or introducing a new UI framework.

## Config health model

### Report

The backend exposes a serializable report:

```text
ConfigHealth
  status: healthy | warning | error
  path: absolute active settings path
  issues: ConfigIssue[]
  repairable: boolean
  restartRequired: boolean

ConfigIssue
  path: JSON path without a value
  code: stable machine-readable code
  severity: warning | error
  message: short Russian explanation
  repair: preserve | reset-default | recreate-file
```

Issue payloads never include the current value. This avoids exposing proxy
credentials, Claude secrets, tokens, custom commands, or other sensitive data in
the UI, logs, and tests.

### Validation boundary

Validation reads the raw file from disk, not the default-merged cache. Missing
`settings.json` is healthy because Jarvis can create it on first write.

The validator checks:

- file readability;
- valid JSON;
- an object at the root;
- supported `schemaVersion`;
- expected object shapes for known nested blocks;
- types of known fields;
- enums used by Jarvis, such as panel position, launch terminal, service backend,
  voice rate, and STT/wake engines;
- numeric ranges such as notification TTL and wake thresholds;
- shortcut syntax, selection-template syntax, and conflicts between known
  actions;
- cross-field requirements that affect runtime behavior.

Missing known fields are valid and receive defaults. Unknown root and nested
fields are preserved and do not produce issues.

### Code ownership

Pure validation and value repair live in a focused
`src-tauri/src/config_health.rs` module. It receives the current defaults and
schema version from `settings.rs`.

`settings::Store` remains the owner of disk access, cache invalidation, atomic
writes, permissions, and backup creation. It exposes:

```text
health() -> ConfigHealth
repair() -> Result<RepairOutcome, String>
```

This keeps schema reasoning independently testable while preserving one owner
for settings persistence.

## Safe repair

Repair is only executed after the user clicks **Исправить конфиг**.

1. Read the original file again under the store lock.
2. Write an exact timestamped backup beside it with mode `0600`.
3. If the root object parses, preserve all valid and unknown fields and replace
   only invalid known fields with their defaults.
4. If JSON or the root object is unusable, preserve the full original in the
   backup and write a fresh default object.
5. Persist through the existing atomic temp-file-and-rename path with mode
   `0600`.
6. Invalidate the settings cache and validate the resulting file again.
7. Return the backup path and fresh health report.

The UI shows only a home-shortened backup path. Because voice, STT, wake-word,
shortcuts, and service components may already have consumed the old effective
settings, successful repair offers **Перезапустить Jarvis** instead of attempting
partial hot reconfiguration.

If backup or atomic persistence fails, the original file remains untouched and
the exact sanitized filesystem error is shown.

## Startup and runtime flow

1. Acquire the active profile lock.
2. Run schema migrations.
3. Construct the daemon with the active settings store.
4. Read `ConfigHealth` before choosing the initial window.
5. Start runtime components with existing fail-safe defaults.
6. If config health contains errors, open onboarding directly in config recovery.
7. Otherwise keep the existing integration-health decision.

After onboarding, the main panel requests health when it is shown. A compact,
persistent banner appears above the content only when issues exist:

```text
Конфигурация Jarvis требует внимания · 3 проблемы
[Подробнее] [Исправить]
```

The banner is not an expiring toast. It disappears only after a healthy
revalidation.

## Onboarding config recovery

Config recovery has precedence over agent installation failures because those
checks may consume the invalid settings.

The screen contains:

- a small status icon, not the large animated orbit;
- title **Проверь конфигурацию Jarvis**;
- one concise row per issue, using JSON paths but never values;
- a note that valid and plugin settings will remain unchanged;
- primary action **Исправить конфиг**;
- secondary action **Продолжить без исправления**.

After success it shows the backup location and changes the primary action to
**Перезапустить Jarvis**. Continuing without repair is allowed because the
runtime remains fail-safe, but the persistent main-panel banner remains.

## Honest integration diagnostics

Readiness uses the same executable resolvers as the actual Claude and Codex
backends. The resolver:

- searches the process `PATH` plus known Homebrew, local-bin, and installed nvm
  `bin` directories;
- ignores Jarvis's own shims when looking for the real CLI;
- returns the resolved path internally while the UI receives only presence and a
  safe, home-shortened location.

Installer boundaries return `Result` for expected filesystem/environment
failures. The onboarding job stores the concrete sanitized error. `catch_unwind`
remains only as a last-resort safety boundary and extracts a panic string instead
of replacing it with “Core installer аварийно остановился”.

Headless and smoke runners set `JARVIS_HEADLESS=1`. In this mode Jarvis does not
create panel, toast, tray, or onboarding windows and does not run interactive
repair flows. The socket and explicitly requested test services can still run.
The plugin-host smoke harness must set this flag.

## Visual system

The existing dark Jarvis identity remains, but all onboarding screens share one
compact scale.

### Window and corners

- Use the same radius for the Tauri window effect, `html`, `body`, and `.shell`.
- Keep one visible shell border and the native window shadow.
- Remove the CSS shadow layer that currently creates a second outer contour.
- Clip at the root so footer and animated effects cannot leak through corners.

### Type and spacing

- Reduce display headings and constrain them to two short lines.
- Increase list details, notices, and error text to a readable body size.
- Raise secondary-text contrast.
- Use one content width and one vertical rhythm across welcome, agents,
  capabilities, recovery, and ready screens.
- Reduce welcome/recovery top padding, hero size, and decorative gaps.
- Keep long issue lists in the scrollable content area with bottom breathing
  room above the footer.
- Change **Нужен корпоративный прокси?** to **Нужен прокси?**.

### Lists and recovery

- Use compact status rows with a single divider and no redundant badges.
- Prefer short Russian explanations over mixed operational jargon.
- Replace the large recovery orbit with a small inline status mark.
- Group related warnings and avoid repeating the same cause in the lead, list,
  and notice.

### Actions

Buttons receive explicit independent states:

- normal;
- hover;
- keyboard focus;
- pressed;
- disabled;
- busy.

Disabled primary actions use a subdued surface and readable muted text rather
than lowering opacity on a bright blue button. Hover never changes text to a
near-background color. The footer is shorter and uses stable one- and two-action
layouts.

## Testing

### Rust

- missing file is healthy;
- malformed JSON reports a repairable root error;
- non-object root reports an error;
- wrong known types, invalid enums, ranges, and shortcut conflicts are reported;
- missing and unknown fields remain valid;
- issue payloads contain paths but no values or secrets;
- repair preserves valid, unknown, plugin, and secret fields;
- repair resets only invalid known fields;
- unparseable files are backed up and recreated;
- backup and repaired file modes are `0600`;
- failed backup/write leaves the original untouched;
- post-repair validation is healthy;
- canonical CLI resolution finds nvm binaries and skips Jarvis shims;
- expected installer failures return concrete errors instead of panicking;
- headless startup does not create application windows.

### JavaScript

- config recovery takes precedence in onboarding state derivation;
- successful repair transitions to restart;
- continuing without repair preserves the warning state;
- button labels and enabled/disabled states match each recovery phase;
- the proxy label is **Нужен прокси?**.

### Visual verification

Use the real `480 × 600` CSS-pixel onboarding viewport with mocked snapshots for:

- welcome;
- agent readiness with warnings;
- config recovery;
- generic installer recovery;
- capabilities;
- ready;
- primary hover, focus, disabled, and busy states.

Capture screenshots at device scale 2 and inspect all four corners, text
contrast, footer overlap, scrolling, and button states. Finally run a real dev
launch and verify the socket plus integration `→ OK` log before opening
onboarding from the running app.

## Acceptance criteria

- A broken Jarvis settings file is visible in-app on the same startup.
- Repair never occurs without a click and always creates a private backup.
- Valid, unknown, plugin, and secret settings survive repair.
- The repaired file validates successfully before Jarvis offers restart.
- A smoke/headless runtime cannot open production onboarding.
- The real dev runtime detects the installed Claude and Codex CLIs.
- Installer failures show their actual sanitized cause.
- All onboarding screens use the same readable hierarchy and button states.
- No footer content is clipped, and no doubled corner outline is visible.
