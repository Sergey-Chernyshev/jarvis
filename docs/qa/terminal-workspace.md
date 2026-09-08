# Terminal workspace QA

## Regression fixes verified on 2026-09-05

- tmux `capture-pane -C` grid data doubles literal backslashes, whereas
  `%output` and pending input use octal encoding. Separate decoders now restore
  OSC 8 hyperlinks and literal paths without weakening live-frame validation.
  The fixture opens a real isolated tmux pane with both, reads history and
  closes the stream while leaving its process alive. All three real tmux stream
  tests and ten pure stream tests passed. See the tmux 3.6
  [grid encoder](https://github.com/tmux/tmux/blob/3.6/grid.c) and
  [pending-input encoder](https://github.com/tmux/tmux/blob/3.6/cmd-capture-pane.c).
- Following the current cursor row keeps short output visible within the
  remote screen. Native WKWebView checks assert both top and bottom bounds;
  all six passed. Chromium checks cover short/long output, retained history,
  selection and zoom. Scrolling to the physical bottom of a mostly blank
  screen was the cause of the empty viewport.
- Hook parent PIDs can change between notifications. Desktop bindings retain
  session, pane, provider and machine identity; actual process/server PIDs stay
  pinned by the stream backend. All eleven desktop terminal tests passed.
- Synthetic API-error models no longer replace the last real Claude model or
  become model-picker entries. Unknown models show an explicit placeholder;
  custom model IDs remain intact. Two Rust and eighteen chat UI checks passed.

A read-only check against the affected live Claude pane over the app's Teleport
tunnel returned a 2,545-byte initial screen and successful history/poll/close
responses. No input or resize was sent; agent and tmux server PIDs were preserved.

The terminal has two independent checks. Neither uses an account, native IPC,
SSH, the system clipboard, a live agent, or an existing tmux server.

```sh
python3 scripts/qa/tmux-native.py
node scripts/qa/terminal-workspace.cjs
```

The Python smoke requires Python 3 and tmux. It creates a unique temporary
directory, a `tmux -S <temporary socket>` server and a synthetic output process.
Its `finally` cleanup kills only that socket and removes the directory. macOS
sandboxing may need approval to create the temporary Unix socket.

The browser smoke requires Chrome and Playwright (the bundled Codex runtime is
discovered automatically; `JARVIS_PLAYWRIGHT_PATH` can override it). It serves
production xterm, terminal UI and CSS on loopback, with a strict synthetic bridge.
Clipboard calls are recorded in memory. Browser profiles and processes are
disposable. Screenshots and results go to `docs/qa/assets/terminal-workspace/`.

## Native launch and selection

The node's `launch()` passes `-f $JARVIS_DIR/tmux.conf` before `new-session`.
If that file is missing, a private temporary config provides mouse support,
100,000 lines of history, clipboard and selection defaults. The temporary file
is removed after launch, including errors. An invalid existing config path is
reported instead of silently replaced.

tmux reads `-f` only when starting a server. A subsequent launch therefore does
not change an established server's options or key bindings. The smoke verifies
this with two conflicting configs and a config path containing spaces and an
apostrophe. See the [tmux manual](https://man.openbsd.org/tmux).

For tmux 3.0 and newer, both `copy-mode` and `copy-mode-vi` bind mouse release to
`copy-selection-no-clear`: selection and scrollback position remain after copy.
On tmux 2.4–2.9, `copy-selection` keeps copy-mode but clears the selection;
preserving the highlight requires the command introduced in 3.0. Clipboard
`external` is used from 2.6 onward; 2.4/2.5 use `off`. See the
[tmux changelog](https://github.com/tmux/tmux/blob/master/CHANGES) and
[clipboard guide](https://github.com/tmux/tmux/wiki/Clipboard). OSC 52 also needs
support and permission in the external terminal; this smoke checks tmux's buffer,
not the user's operating system clipboard. Versions older than 2.4 predate the
copy-mode command API already used here and are outside this compatibility range.

The current smoke passes **21 checks on tmux 3.6b**, including:

- 100,000 synthetic lines and a large retained scrollback.
- Exact Cyrillic, CJK, emoji and combining-character copy in both key tables.
- New output arriving while selection and scrollback remain active.
- Installed config, fallback config, and preservation of established options.
- Simulated 2.4, 2.6 and 2.9a conditional branches on the current parser.

The simulated branches do not claim execution on older tmux binaries. An actual
2.4/2.6/2.9 VM matrix remains a separate compatibility check. Four Rust unit tests
cover config precedence, temporary file permissions/cleanup, invalid path handling
and config/path argv boundaries; run them with the node's targeted test suite.

## Browser coverage

The synthetic bridge supplies 100,000 numbered lines and deferred stream replies.
Initial attach requests only 2,000 lines; loading 100,000 requires an explicit
action. The harness checks bounded DOM size, physically visible old-history
search matches and live prompt, exact Unicode copy, real mouse drag/wheel,
selection and viewport stability during output, duplicate sequence rejection,
split UTF-8, read-only input and resize gating, explicit input, expanded layout,
offline readability, reconnect and stale open responses after changing session.
Delayed input tests verify that off/on discards old queued bytes, failures never
replay them, rapid key events coalesce behind an in-flight request in exact order,
and a multiline Unicode paste stays one `paste:true` payload. A disappeared session
disables input immediately while every remaining tail page is still consumed.
Desktop dark and narrow light screenshots are generated from the real renderer.

The final browser run passes **35 checks** with no browser exceptions, including
input coalescing across a delayed request, cancellation after input is disabled,
no replay after an uncertain delivery, one-request multiline paste, and draining
the remaining output pages after input becomes unavailable. The 100,003-row
fixture used 236 terminal DOM nodes; the final observed load was about 3.2 s.
The final browser run passed **35 checks with zero browser exceptions**; the
current `results.json` records the check names and observed geometry/timing.

Observed runs retained 100,003 buffer rows with **236 terminal DOM nodes**. The
explicit large-history load varied from 3.39 to 14.78 seconds while other work ran
on the same host. These timings need a quiet, fixed benchmark host before making
performance claims. The full chat integration harness also passed 25 scenarios
with no browser exceptions or unknown bridge calls, using visible picker controls.

The duration reported as `initial100kMs` is an observed headless-browser load time
on one machine. It is not a native display frame-rate benchmark. This fixture also
does not measure SSH throughput, native Tauri clipboard integration, or remote
PTY parity; those require the isolated end-to-end transport fixture.

## Verified remote installation

On 2026-09-05 the current embedded x86_64 musl node was installed through the
shared Teleport installer on an authorized Linux VM with tmux 3.6. Systemd
startup, protocol 2 and delivery from the installed hook passed. A temporary,
uniquely named test session verified the initial screen, exact Unicode stream,
bracketed multiline paste, single-pane resize, history text and preservation of
the pane after closing its stream. Only that test session was removed afterward.

A separate real `tsh ssh -N -L` connection passed the capability check. After
restarting the signed macOS app, its own Teleport tunnel also returned a healthy
node advertising `terminal.stream.v1`. Native clipboard interaction was not part
of that VM check. The installed root account has Claude and Codex history but no
Codex CLI; the installer reports that limitation instead of claiming working
Codex notifications.

## Five release benchmark gates

Use one fixed machine/browser build and record its versions, display refresh rate,
window size, renderer, CPU/RSS and transport latency. Keep numbered line and byte
hash fixtures; never use agent output with secrets.

| Gate | Exact workload | Pass condition |
| --- | --- | --- |
| History and fidelity | 100,000 numbered 80-cell lines, then 1,000,000; include Cyrillic, CJK, emoji, combining marks, tabs, CRLF, ANSI colors and soft wraps. | Retained range is explicit. IDs and SHA-256 of copied/exported retained text match the fixture; no replacement characters or duplicates. DOM stays bounded by viewport rather than total history. |
| Scrolling under output | Scroll to line 20,000 while 1,000 lines/s arrive for 30 s; wheel up/down at 60 Hz. | The visible anchor stays on the same retained line, with no forced jump to live output. Wheel-to-paint p95 ≤33 ms on a 60 Hz reference display; record worst frame and long tasks. |
| Selection and copy | Select 2,000 lines with a real mouse drag, keep output running for 10 s, release and copy. Repeat vi/emacs natively and read/interactive modes in the UI. | Selected bytes and anchor survive until explicit action or documented eviction. Local clipboard bytes match the fixture, including Unicode and soft wraps. |
| Throughput and resize | Feed 10 MiB/s for 30 s across four disposable panes; resize 120×40 →80×24 →160×50; issue small input after each resize. | No UI lockup or unbounded queue/RSS growth; input-to-ack p95 ≤100 ms locally. VT geometry and cursor remain consistent; UI reading alone does not resize unrelated attached clients. |
| Reconnect and identity | Inject 100 ms RTT, disconnect for 15 s while output continues, reconnect, then switch sessions with old open/poll/resize replies still pending. | Gaps are reported or reconstructed without silent loss/duplication. No input replay, no cross-session output, stale handles are closed, and offline output remains copyable. |

A capped history cannot retain arbitrary old lines forever. A test that crosses
the configured cap must assert visible truncation and the exact retained range,
rather than incorrectly treating the deliberate cap as lossless archival storage.
