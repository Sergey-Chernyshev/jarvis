#!/usr/bin/env python3
"""Execute the ignored Rust lifecycle test with fake CLIs and private real tmux.
Usage: python3 scripts/qa/loops-bundle-fixture.py /absolute/path/to/jarvis-test-binary
No credentials, user repos, user tmux server or microphone are used.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

binary = Path(sys.argv[1]).resolve()
if not binary.is_file():
    raise SystemExit("test binary does not exist")
root = Path(tempfile.mkdtemp(prefix="jarvis-loops-bundle-qa-", dir="/private/tmp"))
root.chmod(0o700)
for name in ["bin", "home", "tmux", "data", "codex"]:
    (root / name).mkdir(mode=0o700)
head = f"#!{sys.executable}\n"
cli = head + r'''
import json, os, pathlib, sys
args = sys.argv[1:]
pathlib.Path("fake-change.txt").write_text("deterministic fixture edit\n")
if "QA_FAIL" in args:
    print(json.dumps({"type":"result", "is_error":True, "result":"deterministic failure"}))
    sys.exit(7)
if pathlib.Path(sys.argv[0]).name == "codex":
    assert "--json" in args and "--" in args, args
    print(json.dumps({"type":"item.completed", "item":{"type":"agent_message","text":"deterministic edit"}}))
    print(json.dumps({"type":"turn.completed", "usage":{"input_tokens":9,"cached_input_tokens":4,"output_tokens":3}}))
else:
    assert "--output-format" in args and "json" in args, args
    print(json.dumps({"type":"result","result":"deterministic edit","usage":{"input_tokens":9,"output_tokens":3}}))
'''
tui = head + r'''
import os, pathlib, sys, termios, tty
old = termios.tcgetattr(0)
tty.setraw(0)
sys.stdout.write("QA TUI READY\r\n")
sys.stdout.flush()
buf = bytearray()
try:
    while True:
        byte = os.read(0, 1)
        if not byte:
            break
        if byte == b"\x1b":
            pathlib.Path("interrupted.txt").write_text("Escape received\n")
        elif byte == b"\x15":
            buf.clear()
        elif byte in [b"\r", b"\n"]:
            if buf:
                pathlib.Path("reply.txt").write_bytes(buf)
                sys.stdout.write("\r\nRECEIVED\r\n")
                sys.stdout.flush()
                buf.clear()
        else:
            buf.extend(byte)
finally:
    termios.tcsetattr(0, termios.TCSANOW, old)
'''
for name, source in [("claude", cli), ("codex", cli), ("fake-tui", tui)]:
    path = root / "bin" / name
    path.write_text(source)
    path.chmod(0o700)
env = os.environ.copy()
env.update({"JARVIS_QA_LOOPS_FIXTURE":str(root), "JARVIS_DIR":str(root / "data"),
    "HOME":str(root / "home"), "CODEX_HOME":str(root / "codex"),
    "TMUX_TMPDIR":str(root / "tmux"), "PATH":str(root / "bin") + os.pathsep + env.get("PATH", ""), "JARVIS_IGNORE":"1"})
env.pop("TMUX", None)
for name in list(env):
    if any(key in name.upper() for key in ["API_KEY", "AUTH_TOKEN", "ACCESS_TOKEN", "REFRESH_TOKEN"]):
        env.pop(name)
try:
    result = subprocess.run([str(binary), "--exact", "loops::qa::isolated_cli_and_tmux_round_trip", "--ignored", "--nocapture", "--test-threads=1"], env=env, capture_output=True, text=True, timeout=70)
    (root / "test-output.txt").write_text(result.stdout + result.stderr)
    print(result.stdout + result.stderr)
    print(json.dumps({"fixture":str(root), "exit_code":result.returncode, "evidence":"actual Git/shell/tmux; deterministic fake Claude/Codex CLI"}))
    sys.exit(result.returncode)
finally:
    # Same server name as production, distinct private socket directory.
    subprocess.run([shutil.which("tmux") or "tmux", "-u", "-L", "jarvis", "kill-server"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
