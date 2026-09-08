# Voice workspace QA

The five voice subsections now use acknowledged native state. Reads that fail show a retryable error rather than an empty successful list. Edits, deletion, smart-mode settings and scratch saves retain the previous state on failure. There is no fallback to temporary JavaScript arrays, fabricated built-in settings or localStorage.

## Functional changes

- History preserves `hasAudio`, distinguishes a preview from a saved transformation, and supports actual text editing through `transcript_update`. Saving clears the applied-style annotation in the native store. Delete and regeneration errors leave the original row available. Clipboard writes use the native plugin's fulfilled promise contract.
- Statistics use the saved dictation list only. They do not invent an application source, token usage or a time-saving estimate. A failed history read also makes statistics unavailable rather than showing zeroes.
- Dictionary entries have an explicit recognized phrase and replacement. Native `voice-dictionary.json` stores the rules atomically with private file permissions. Unicode whole-word/phrase matching is case-insensitive, longest matches win, and replacements never cascade. The common STT service applies one dictionary snapshot to both full text and segments, for Whisper and Qwen; conflicting segmentation from a phrase spanning segments is omitted. This changes recognized text after recognition, rather than claiming to train the model.
- Built-in transformations are read-only because the runtime does not support individual switches or custom styles. The supported smart-mode switch waits for native persistence. Copy explains that Claude processes the selected text.
- Drafts use native `voice-scratch.json`, not browser storage. Saves are serialized and newer input is coalesced; the status shows pending, confirmed or failed persistence. A failed initial read disables editing to avoid overwriting an unreadable draft. Failed writes retain the draft in the window for retry. The maximum draft size is 2 MB.

Escape closes a transform menu, cancels an inline history/dictionary edit, clears history search, or leaves the draft field without discarding it. A subsequent Escape returns to History before the application's outer navigation handles another Escape. Native buttons make all actions reachable from the keyboard; history actions are not hover-only. Re-entering the module preserves an active editor and draft.

All file I/O and synchronization for the new dictionary/draft commands run in the blocking pool. UI commands report actual errors. The STT path retains successful recognized text if a dictionary file cannot be read; the dictionary UI exposes the read error. Broken dictionary JSON is never overwritten by an add/remove operation.

## Verification

`node --test ui/voice-history.test.mjs ui/panel-smoke.test.mjs` passes the focused voice regressions and the current whole-panel smoke tests. The focused tests cover failed edits/deletes, dictionary failures, smart-mode persistence, native audio metadata, serialized draft writes, failed initial reads and absence of duplicate host IDs.

`node scripts/qa-voice-history.cjs` passes 18 browser scenario groups and produces 39 screenshots at 1040×720, 680×560 and 480×620 in dark and light themes. It renders the actual voice JavaScript plus the application's styles in Chrome, with an isolated strict native-contract fixture. The scenarios include populated states of all five subsections, read and write failures, edit/save/cancel, previews, copy, regeneration, dictionary editing/deletion, smart-mode rollback, serialized draft writes, reopening persisted fixture data and Escape navigation. Geometry assertions reject horizontal overflow and a viewport-clipped main area. No browser exceptions occurred.

The browser test uses only synthetic transcripts and a fixture's in-memory representation of native files. It does not invoke a microphone, speech model, real Claude CLI, authentication, real clipboard or user data files. Its screenshots demonstrate rendering and command behavior, not live transcription accuracy or native permission behavior. Native read/edit/delete smoke evidence is collected separately by the application's native smoke harness.

Rust tests in `stt/voice_data.rs` use temporary files and explicit rules rather than the developer's real dictionary. They cover exact Unicode boundaries, case, punctuation, longest phrases, non-cascading replacement, empty dictionaries, full-text/segment consistency, rules spanning segments, private file permissions, round-trip/upsert/remove, malformed JSON protection, scratch round-trip, size bounds and write failures. Existing native history and smart-mode persistence tests were extended by the native agent.

## Reproduction

```sh
node --test ui/voice-history.test.mjs ui/panel-smoke.test.mjs
node scripts/qa-voice-history.cjs
cargo test --manifest-path src-tauri/Cargo.toml --bin jarvis voice_data:: -- --test-threads=1
```

Browser fixtures and screenshots are in `docs/qa/assets/voice/`; `report.json` records the exact geometry and scenario groups. The harness accepts `JARVIS_PLAYWRIGHT_PATH`, otherwise using local Playwright or the bundled Codex runtime. It requires Chrome, serves loopback only, and blocks non-local requests.
