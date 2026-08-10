# Jarvis React UI MVP Implementation Plan

> **For Codex:** execute this plan inline with `executing-plans`. The current
> checkout is already an isolated feature branch (`feat/turn-ai-analysis`).
> Preserve the unrelated staged `.claude/settings.json`.

**Goal:** Build a complete, redesigned, animated browser MVP of Jarvis on mock
data without losing any existing page or user-facing workflow.

**Architecture:** A standalone `ui-next/` Vite application uses React, strict
TypeScript, Motion and a typed `JarvisClient` boundary. UI features depend only
on domain types and client methods. `MockJarvisClient` supplies fixtures,
delays, streaming-like updates and failure states. The same components will be
reused when `TauriJarvisClient` replaces the mock during migration.

**Tech Stack:** React 19, TypeScript strict, Vite, Motion, Lucide React,
Zustand, Vitest, Testing Library, plain layered CSS with design tokens.

**Functional source of truth:**
`docs/superpowers/specs/2026-08-07-react-ui-mvp-functional-inventory.md`

---

## Visual direction

The application is one calm “paper instrument” rather than a dashboard. Golos
Text carries both dense lists and section titles; monospace is reserved for
terminal, paths and numeric telemetry. Six existing theme/paint combinations
remain, but the hierarchy becomes clearer through ink weight, spacing and one
animated live-signal rail. Lucide icons replace glyph placeholders.

Motion is functional: route changes use a restrained shared-axis transition,
drawers reveal their spatial origin, live state dots breathe, and VM boot
progress moves through named stages. Local setting changes do not replay the
whole page. Reduced-motion mode swaps movement for opacity only.

## Task 1: Scaffold the typed application

**Files:**
- Create: `ui-next/package.json`
- Create: `ui-next/vite.config.ts`
- Create: `ui-next/tsconfig.json`
- Create: `ui-next/tsconfig.app.json`
- Create: `ui-next/tsconfig.node.json`
- Create: `ui-next/index.html`
- Create: `ui-next/src/main.tsx`
- Create: `ui-next/src/app/App.tsx`
- Create: `ui-next/src/app/app-store.ts`
- Create: `ui-next/src/core/client/types.ts`
- Create: `ui-next/src/core/client/jarvis-client.ts`
- Create: `ui-next/src/core/client/mock-jarvis-client.ts`
- Create: `ui-next/src/core/mock/fixtures.ts`
- Copy: Golos Text font files into `ui-next/public/fonts/`

Steps:

1. Define discriminated unions for session, VM, question, task, notification,
   settings and async states.
2. Define the `JarvisClient` interface before writing components.
3. Implement a mock client with deterministic latency and mutable in-memory
   state.
4. Create the app store with narrow selectors and typed navigation state.
5. Add a smoke test that creates the mock client and reads the initial snapshot.
6. Run `npm run typecheck` and the smoke test.

## Task 2: Build tokens, primitives and the app shell

**Files:**
- Create: `ui-next/src/styles/fonts.css`
- Create: `ui-next/src/styles/tokens.css`
- Create: `ui-next/src/styles/global.css`
- Create: `ui-next/src/shared/ui/IconButton.tsx`
- Create: `ui-next/src/shared/ui/Button.tsx`
- Create: `ui-next/src/shared/ui/Segmented.tsx`
- Create: `ui-next/src/shared/ui/Switch.tsx`
- Create: `ui-next/src/shared/ui/EmptyState.tsx`
- Create: `ui-next/src/shared/ui/StatusDot.tsx`
- Create: `ui-next/src/shared/ui/Modal.tsx`
- Create: `ui-next/src/shared/ui/ToastHost.tsx`
- Create: `ui-next/src/shared/motion/transitions.ts`
- Create: `ui-next/src/app/AppShell.tsx`
- Create: `ui-next/src/app/Navigation.tsx`
- Create: `ui-next/src/app/GlobalBanners.tsx`

Steps:

1. Port the six theme/paint token sets from the current prototype.
2. Implement accessible shared controls and visible focus states.
3. Build the window shell, navigation, search, footer and live signal rail.
4. Add global limit/config banners and toast host.
5. Add keyboard navigation and direct hash routes.
6. Add shared route transitions and reduced-motion handling.
7. Verify shell rendering and keyboard route changes in tests.

## Task 3: Implement Chats

**Files:**
- Create: `ui-next/src/features/chats/SessionListPage.tsx`
- Create: `ui-next/src/features/chats/SessionRow.tsx`
- Create: `ui-next/src/features/chats/ChatPage.tsx`
- Create: `ui-next/src/features/chats/Transcript.tsx`
- Create: `ui-next/src/features/chats/TurnSummary.tsx`
- Create: `ui-next/src/features/chats/Composer.tsx`
- Create: `ui-next/src/features/chats/CommandPalette.tsx`
- Create: `ui-next/src/features/chats/QuestionPanel.tsx`
- Create: `ui-next/src/features/chats/TaskBoard.tsx`
- Create: `ui-next/src/features/chats/DocumentViewer.tsx`
- Test: `ui-next/src/features/chats/chats.test.tsx`

Steps:

1. Implement searchable, keyboard-navigable active sessions.
2. Implement the transcript and summary/full-feed modes.
3. Implement composer, mock attachments and queued send state.
4. Implement slash commands, model and effort selection.
5. Implement question flows including multi-select/custom answer.
6. Implement task board with prefill-only actions.
7. Implement document and diff viewer.
8. Test opening a waiting session, answering a question and sending a reply.

## Task 4: Implement Projects and Agent VM workspace

**Files:**
- Create: `ui-next/src/features/projects/ProjectCatalogPage.tsx`
- Create: `ui-next/src/features/projects/ProjectChatsPage.tsx`
- Create: `ui-next/src/features/projects/NewChatMenu.tsx`
- Create: `ui-next/src/features/vm/VmWorkspacePage.tsx`
- Create: `ui-next/src/features/vm/VmControlPanel.tsx`
- Create: `ui-next/src/features/vm/VmStateScene.tsx`
- Create: `ui-next/src/features/vm/TerminalSurface.tsx`
- Test: `ui-next/src/features/projects/projects-vm.test.tsx`

Steps:

1. Port the project catalog and dated project history.
2. Implement Claude/Codex and Mac/VM selection.
3. Implement the live VM terminal workspace and composer.
4. Implement sleep → boot stages → running state.
5. Implement failure and retry flows.
6. Implement VM control panel and stop confirmation.
7. Test direct routes and the automatic VM boot on send.

## Task 5: Implement environment management

**Files:**
- Create: `ui-next/src/features/environments/EnvironmentListPage.tsx`
- Create: `ui-next/src/features/environments/EnvironmentRow.tsx`
- Create: `ui-next/src/features/environments/MirrorCard.tsx`
- Create: `ui-next/src/features/environments/EnvironmentFilesPage.tsx`
- Create: `ui-next/src/features/environments/FileBrowser.tsx`
- Create: `ui-next/src/features/environments/EnvironmentSettings.tsx`
- Test: `ui-next/src/features/environments/environments.test.tsx`

Steps:

1. Implement all VM rows, telemetry, actions and consequence tooltips.
2. Implement stop confirmation and cache cleanup feedback.
3. Implement mirror details and searchable skills.
4. Implement mounts and file browsing with breadcrumbs.
5. Implement environment defaults and memory scope.
6. Test restart/stop, skills search and folder navigation.

## Task 6: Implement Statistics and Voice

**Files:**
- Create: `ui-next/src/features/stats/StatsPage.tsx`
- Create: `ui-next/src/features/stats/UsageLedger.tsx`
- Create: `ui-next/src/features/stats/LimitMeter.tsx`
- Create: `ui-next/src/features/voice/VoicePage.tsx`
- Create: `ui-next/src/features/voice/VoiceHistory.tsx`
- Create: `ui-next/src/features/voice/VoiceInsights.tsx`
- Create: `ui-next/src/features/voice/Dictionary.tsx`
- Create: `ui-next/src/features/voice/Transforms.tsx`
- Create: `ui-next/src/features/voice/Scratchpad.tsx`
- Test: `ui-next/src/features/voice/voice.test.tsx`

Steps:

1. Build period-aware usage and limit views without generic metric cards.
2. Build voice history search and transform actions.
3. Build insights/heatmap, dictionary CRUD and transforms CRUD.
4. Build scratchpad autosave state.
5. Test voice navigation, dictionary add/remove and mock transform.

## Task 7: Implement Settings, onboarding and overlays

**Files:**
- Create: `ui-next/src/features/settings/SettingsPage.tsx`
- Create: `ui-next/src/features/settings/AppearanceSettings.tsx`
- Create: `ui-next/src/features/settings/settings-sections.tsx`
- Create: `ui-next/src/features/settings/HotkeyRecorder.tsx`
- Create: `ui-next/src/features/onboarding/OnboardingFlow.tsx`
- Create: `ui-next/src/features/overlays/NotificationPreview.tsx`
- Create: `ui-next/src/features/overlays/VoiceHud.tsx`
- Test: `ui-next/src/features/settings/settings.test.tsx`

Steps:

1. Put Appearance first with themes, paints and error demo.
2. Implement every settings navigation group from the parity checklist.
3. Make primary controls mutable in mock state; simulate downloads/tests.
4. Integrate full environment management under `Среды`.
5. Implement onboarding states and system overlay gallery/preview.
6. Test theme persistence and config-error repair.

## Task 8: Polish, performance and verification

**Files:**
- Modify: all `ui-next/src/features/**`
- Modify: `docs/superpowers/specs/2026-08-07-ui-v2-atomic-migration-design.md`
- Modify: `docs/superpowers/specs/2026-08-07-react-ui-mvp-functional-inventory.md`
- Create: `ui-next/README.md`

Steps:

1. Add final shared-layout, drawer and list animations.
2. Audit buttons for visible results and remove dead controls.
3. Verify all routes at 900×650 and 1280×820.
4. Run typecheck, tests and production build.
5. Walk every checklist section and record known gaps explicitly.
6. Update the migration spec: `ui-next` is Phase 0 and the source of truth.
7. Start the browser prototype at `http://localhost:8777/`.
8. Capture/compare the completed page in Figma as an auxiliary design artifact.

