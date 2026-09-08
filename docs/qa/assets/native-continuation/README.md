# Native continuation QA

Actual WKWebView (`tauri://localhost`) in a disposable debug app/profile: **4/4 passed**, no JavaScript errors or blocked bridge calls.

Every Jarvis bridge method was replaced after startup by synthetic responses or a fail-closed stub. This verifies frontend navigation, event handling, rendering, composer state and terminal routing/geometry. It does not verify real agent continuation, authentication, shell processes or remote transport. No real command or message was sent.

The fixture follows an external Codex chat through Continue, explicitly opens its initially collapsed launch terminal, delivers a ready event before the child exists, then publishes the child session state. The managed child exposes its composer and an independent terminal; the original source remains unchanged and read-only. Both short terminal outputs are visible within the viewport.

`result.json` contains compact evidence and hashes; `report.json` is the complete native harness report. Source hashes describe the working files after the run; the debug and executed signed binary hashes identify the tested build. The executed scenario hash matches the source script.

`environment-failure-before-scenario.json` records an earlier sandbox-only AppKit registration abort. It happened before the scenario ran; an authorized launch of the same debug bundle passed. It is not counted as a product or scenario failure.
