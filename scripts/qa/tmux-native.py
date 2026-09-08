#!/usr/bin/env python3
"""Disposable tmux smoke. Never connects to a named/user tmux server."""
import ast
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
TMUX = shutil.which("tmux")
assert TMUX, "tmux is required"
checks = []


def check(label, condition):
    assert condition, label
    checks.append(label)


with tempfile.TemporaryDirectory(prefix="jarvis-native-") as temporary:
    directory = Path(temporary)
    socket = str(directory / "socket")
    env = {**os.environ, "TERM": "xterm-256color", "LC_ALL": "en_US.UTF-8"}
    env.pop("TMUX", None)

    def tmux(*args):
        result = subprocess.run([TMUX, "-u", "-S", socket, *map(str, args)],
                                env=env, capture_output=True, text=True, timeout=10)
        assert result.returncode == 0 and not result.stderr, (args, result.stdout, result.stderr)
        return result.stdout

    def stop():
        subprocess.run([TMUX, "-S", socket, "kill-server"], capture_output=True, timeout=5)

    def option(name, scope="-gv"):
        return tmux("show-options", scope, name).strip()

    def wait_for(predicate, description):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(.05)
        raise AssertionError(description)

    try:
        # The real config path contains an apostrophe and spaces, just as an
        # installed JARVIS_DIR may. No shell interpolation of paths is allowed.
        config = directory / "Jarvis user's tmux.conf"
        config.write_text((ROOT / "bin/jarvis-tmux.conf").read_text() + "\nset -g @jarvis-fixture first\n")
        producer = directory / "output.py"
        control = directory / "control"
        os.mkfifo(control)
        producer.write_text("import sys\nfor i in range(100000):\n print(f'row-{i:06d} · Привет 中文 🐈 é')\nprint('INITIAL-DONE', flush=True)\nwith open(sys.argv[1]) as control:\n for line in control:\n  for i in range(100): print(f'live-{i:04d}')\n  print('LIVE-DONE-' + line.strip(), flush=True)\n")
        pane = tmux("-f", config, "new-session", "-d", "-P", "-F", "#{pane_id}", "-x", "100", "-y", "40", "-s", "fixture", shutil.which("python3"), "-u", producer, control).strip()
        check("isolated launch returned a pane", pane.startswith("%"))
        wait_for(lambda: "INITIAL-DONE" in tmux("capture-pane", "-pt", pane), "100k output did not finish")
        check("actual Jarvis config loaded", option("@jarvis-fixture") == "first")
        check("history limit is 100000", option("history-limit") == "100000")
        check("mouse enabled", option("mouse") == "on")
        check("clipboard external", option("set-clipboard", "-sv") == "external")
        history = int(tmux("display-message", "-pt", pane, "#{history_size}").strip())
        check("large scrollback retained", 90000 <= history <= 100000)

        alternate = directory / "do not load.conf"
        alternate.write_text("set -g history-limit 7\nset -g mouse off\nset -g @jarvis-fixture overwritten\n")
        tmux("-f", alternate, "new-session", "-d", "-s", "second", "/bin/sleep", "120")
        check("existing server settings preserved", option("@jarvis-fixture") == "first" and option("history-limit") == "100000" and option("mouse") == "on")

        control_writer = open(control, "w")
        for mode, table in [("emacs", "copy-mode"), ("vi", "copy-mode-vi")]:
            binding = tmux("list-keys", "-T", table, "MouseDragEnd1Pane")
            check(f"{mode} release binding retains selection", "copy-selection-no-clear" in binding and "-and-cancel" not in binding)
            tmux("set-window-option", "-t", pane, "mode-keys", mode)
            tmux("copy-mode", "-t", pane)
            for command in ["history-top", "start-of-line", "begin-selection", "end-of-line", "copy-selection-no-clear"]:
                tmux("send-keys", "-t", pane, "-X", command)
            copied = tmux("save-buffer", "-").rstrip("\n")
            check(f"{mode} exact Unicode copy", re.fullmatch(r"row-\d{6} · Привет 中文 🐈 é", copied) is not None)
            check(f"{mode} selection stays active", tmux("display-message", "-pt", pane, "#{pane_in_mode}:#{selection_present}").strip() == "1:1")
            scroll_before = int(tmux("display-message", "-pt", pane, "#{scroll_position}").strip())
            control_writer.write(mode + "\n")
            control_writer.flush()
            wait_for(lambda: f"LIVE-DONE-{mode}" in tmux("capture-pane", "-pt", pane), "live output did not finish")
            tmux("send-keys", "-t", pane, "-X", "copy-selection-no-clear")
            check(f"{mode} live output keeps copied bytes", tmux("save-buffer", "-").rstrip("\n") == copied)
            check(f"{mode} remains in scrollback", int(tmux("display-message", "-pt", pane, "#{scroll_position}").strip()) >= scroll_before)
            tmux("send-keys", "-t", pane, "-X", "cancel")

        control_writer.close()

        stop()
        source = (ROOT / "src-tauri/node/src/node/tmux.rs").read_text()
        literal = re.search(r'const FALLBACK_LAUNCH_CONFIG: &str = ("(?:\\.|[^"\\])*");', source, re.S).group(1)
        fallback = ast.literal_eval(literal)
        fallback_path = directory / "fallback.conf"
        fallback_path.write_text(fallback)
        tmux("-f", fallback_path, "new-session", "-d", "-s", "fallback", "/bin/sleep", "120")
        check("missing-config fallback loads", option("history-limit") == "100000" and option("mouse") == "on" and option("set-clipboard", "-sv") == "external")
        stop()
        # Exercise the compatibility branches on the installed parser. This is
        # not a substitute for an actual old-tmux binary compatibility run.
        for version, clipboard in [("2.4", "off"), ("2.6", "external"), ("2.9a", "external")]:
            compatibility = directory / f"compat-{version}.conf"
            compatibility.write_text(fallback.replace("#{version}", version))
            tmux("-f", compatibility, "new-session", "-d", "-s", "compat", "/bin/sleep", "120")
            check(f"{version} compatibility branch", option("set-clipboard", "-sv") == clipboard and "copy-selection-no-clear" not in tmux("list-keys", "-T", "copy-mode", "MouseDragEnd1Pane"))
            stop()
        print(json.dumps({"tmux": subprocess.check_output([TMUX, "-V"], text=True).strip(), "checks": checks, "passed": len(checks)}, ensure_ascii=False, indent=2))
    finally:
        stop()
