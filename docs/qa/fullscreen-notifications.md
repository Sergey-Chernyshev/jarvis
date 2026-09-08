# Fullscreen notifications — 2026-09-05

The previous native acceptance check only inspected `isVisible`, key-window state
and collection-behavior flags on an ordinary NSWindow. That was insufficient:
starting the dictation HUD could still move the user's fullscreen Space.

## Actual reproduction

A disposable AppKit editor enters fullscreen through `toggleFullScreen` and waits
for the real will/did-enter delegate lifecycle. An independent 40 ms native
observer records its foreground PID, AX input identity, active Space and actual
WindowServer surface. Only the fixture's own processes are inspected; no user
window content or display screenshot is collected.

The old NSWindow failed after the production Listening event. Its fullscreen
editor's actual WindowServer X coordinate moved from 0 to 79 and then 277 while
AppKit still reported the original frame at X=0. The HUD's WindowServer surface
was at X=-9831, outside the active display. `isVisible` and the requested fullscreen
flags did not establish correct Space behavior. [Before-fix evidence](native-fullscreen-before.json).

## Fix

The notification WebView now lives in a real `NSPanel` created with the
`NonactivatingPanel` style at initialization. It cannot become key or main and is
shown without activating Jarvis. The original Tauri window remains hidden to
retain IPC and WebView ownership. Its empty placeholder view satisfies Tao's raw
window-handle contract; the complete Wry view hierarchy is retained and transferred
into the panel. No Objective-C class or private Tao instance-variable layout is
modified. All toast geometry, visibility and hover operations use the actual panel.
Space membership is assigned once at creation, and an already-visible panel is
not reordered during content resizing. This also avoids unnecessary native
window operations for the recording → processing transition.
The notification uses `NSStatusWindowLevel` (25), above fullscreen content,
instead of the launcher's screen-saver level (1000).

The resize IPC completes after AppKit commits the canvas geometry and visibility.
The renderer can therefore begin a transition with the required drawing area
already available. Hiding happens after the content finishes its exit animation.

The rectangular halo was a separate CSS shadow bug: hover restored a 48px shadow
from the main panel theme outside the much smaller transparent gutters. Every
notification state now uses the same contained shadow. See the [motion audit](toast-motion.md)
for nine browser scenarios, intermediate morph geometry and pixel-alpha evidence.

## Reproduce the actual native check

```sh
node scripts/native-smoke.mjs --app /absolute/path/to/debug/Jarvis.app \
  --scenario scripts/qa/native-fullscreen.js --editor \
  --fullscreen-policy accessory --timeout 60 --wait-idle 12
# Repeat with --fullscreen-policy regular for normal window mode.
```

The check uses two Listening → Analyzing → Empty → Dismiss cycles, including
native position, foreground/AX ownership, actual NSPanel type and WindowServer
order/physical overlap. It uses real WebKit animations without finishing them
artificially. The helper accounts for the display's native notch-safe area,
leaves fullscreen and closes after the run. It never resets TCC or records audio.
The global hotkey and microphone pipeline are separate from this HUD presentation
regression; multiple physical displays are not part of this local fixture.

## Follow-up evidence

Both activation policies passed all 12 steps each: two complete HUD cycles and
two real CGEvent insertions into the retained original AX field per mode.
The independent observer collected 219 samples in Regular mode and 222 in
Accessory mode, with no fullscreen, focus or WindowServer-surface violation.
Jarvis did not activate during either monitored HUD lifecycle.
[Regular result](native-fullscreen-regular.json),
[Accessory result](native-fullscreen-accessory.json),
[actual WebKit result card](assets/toast-motion/native-empty.png).

The native WKWebView recorded six transitions in each mode. In the first Regular
cycle, the card expanded through widths 225 → 296.89 → 339.56 → 373 → 382.67 → 392px
while shell opacity stayed 1. Entry opacity also passed through actual intermediate
values. System Reduced Motion was false and was never overridden. Both modes
completed their own native animations; the test did not force animation clocks.

Later diagnostic runs also record application activation and active-Space
notifications with timestamps. External application switches during intentional
fullscreen entry invalidate a run; the fixture never refocuses the editor or
repairs a Space switch to make assertions pass. On failure it observes for another
1.8 seconds before cleanup so that delayed Workspace notifications are retained.
The early baseline establishes the observed surface movement, but without those
later event observers it cannot conclusively attribute every movement to Jarvis.

The UI suite for the frozen source snapshot passed 182 tests. The native geometry
unit tests passed two cases. The nine rendering scenarios verify transparent
edges and actual intermediate animation frames; the native scenario additionally
samples intermediate WebKit opacity and geometry without forcing animations to
finish. Concurrent project/analytics work is outside this snapshot's evidence.
The shipped notification source is recorded by [SHA-256 manifest](toast-release-source.json).
The full-feature release passed strict code-signature verification and replaced
the old process through graceful SIGTERM and `open`. Exactly one release process
was observed (PID 1757). A two-second sample after launch recorded all 1,580
main-thread samples in the normal AppKit event loop; 1,579 were waiting on its
Mach port. No synchronous audio-permission/configuration wait appeared there.

Primary references: Apple's [nonactivating panel style](https://developer.apple.com/documentation/appkit/nswindow/stylemask-swift.struct/nonactivatingpanel),
[NSPanel](https://developer.apple.com/documentation/appkit/nspanel), and
[Wispr Flow Bar states](https://docs.wisprflow.ai/articles/1790396454-move-and-dock-the-flow-bar-on-desktop).
