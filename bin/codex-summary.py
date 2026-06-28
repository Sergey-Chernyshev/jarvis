#!/usr/bin/env python3
"""codex-summary.py — one-shot Codex sidecar for Jarvis.

Reads ONE JSON object from stdin, runs it through the official OpenAI Codex
Python SDK (`openai-codex`, import `openai_codex`) as a single read-only turn,
and prints ONE JSON object to stdout:

    {"ok": true,  "text": "<final assistant text>"}            (success)
    {"ok": false, "error": "<message>"}                        (failure)

Input schema (stdin, single JSON object):
    {
      "prompt": "<required: the task text>",
      "model":  "gpt-5.5",          # optional; SDK/account default if omitted
      "effort": "low",              # optional: none|minimal|low|medium|high|xhigh
      "instructions": "...",        # optional: system/developer steering text
      "timeout": 120                # optional: seconds (default 120)
    }

Design:
  * Read-only sandbox  -> sandbox=Sandbox.read_only (no file writes).
  * No approval prompts -> approval_mode=ApprovalMode.deny_all (never blocks).
  * Web search disabled -> config={"web_search": "disabled"} at thread start.
  * cwd = a fresh temp dir so the thread can't touch the real project tree.
  * ephemeral=True       -> thread is not persisted to the on-disk session DB.
  * Auth: the bundled codex runtime reuses $CODEX_HOME/auth.json
    (default ~/.codex/auth.json) — the existing `codex login`. No API key needed.
  * stdout discipline: the SDK can be chatty, so we redirect any library
    stdout writes to stderr and emit exactly one JSON line on real stdout.

Requires: pip install openai-codex   (Python >=3.10; pulls openai-codex-cli-bin).
"""

from __future__ import annotations

import contextlib
import json
import sys
import tempfile
import threading


def _fail(error: str) -> "NoReturn":  # type: ignore[name-defined]
    """Emit a single failure JSON object on stdout and exit non-zero."""
    sys.stdout.write(json.dumps({"ok": False, "error": str(error)}) + "\n")
    sys.stdout.flush()
    raise SystemExit(1)


def _read_request() -> dict:
    raw = sys.stdin.read()
    if not raw.strip():
        _fail("empty stdin: expected a single JSON object")
    try:
        req = json.loads(raw)
    except json.JSONDecodeError as exc:
        _fail(f"invalid JSON on stdin: {exc}")
    if not isinstance(req, dict):
        _fail("stdin JSON must be an object")
    if not isinstance(req.get("prompt"), str) or not req["prompt"].strip():
        _fail("missing required string field 'prompt'")
    return req


def _run_with_timeout(fn, timeout_s: float):
    """Run fn() in a daemon thread; raise TimeoutError if it overruns.

    The Codex transport runs the bundled `codex app-server` subprocess; if it
    hangs we abandon the worker thread (daemon) and let the process exit.
    """
    result: dict = {}

    def _worker() -> None:
        try:
            result["value"] = fn()
        except BaseException as exc:  # noqa: BLE001 - surfaced to caller
            result["error"] = exc

    t = threading.Thread(target=_worker, daemon=True)
    t.start()
    t.join(timeout_s)
    if t.is_alive():
        raise TimeoutError(f"codex turn exceeded {timeout_s:g}s")
    if "error" in result:
        raise result["error"]
    return result.get("value")


def _sdk_via() -> str:
    """Доказательство транспорта для лога: версии официального SDK + bundled-
    бинаря codex, через который SDK гонит app-server (JSON-RPC). Видно в jarvis.log
    как `[codex-sdk] ← (via openai_codex … · app-server) …`."""
    import importlib.metadata as _m

    def _v(pkg: str) -> str:
        try:
            return _m.version(pkg)
        except Exception:  # noqa: BLE001
            return "?"

    return f"openai_codex {_v('openai-codex')} · codex-cli-bin {_v('openai-codex-cli-bin')} · app-server"


def main() -> None:
    req = _read_request()

    prompt: str = req["prompt"]
    model = req.get("model") or None            # None -> SDK/account default
    effort = req.get("effort") or None          # None -> model default
    instructions = req.get("instructions") or None
    try:
        timeout_s = float(req.get("timeout", 120))
    except (TypeError, ValueError):
        timeout_s = 120.0

    # Import lazily so a missing dependency yields a clean JSON error, not a
    # raw traceback on stdout.
    try:
        from openai_codex import ApprovalMode, Codex, Sandbox
        from openai_codex.types import ReasoningEffort
    except Exception as exc:  # noqa: BLE001
        _fail(
            "openai-codex SDK not importable "
            f"({exc}); install with: pip install openai-codex"
        )

    # Validate effort against the real enum (none|minimal|low|medium|high|xhigh).
    effort_value = None
    if effort is not None:
        try:
            effort_value = ReasoningEffort(effort)
        except ValueError:
            allowed = ", ".join(e.value for e in ReasoningEffort)
            _fail(f"invalid effort {effort!r}; allowed: {allowed}")

    # A throwaway working directory: nothing in the real tree is reachable, and
    # there is no git repo here so no repo-trust path is exercised.
    with tempfile.TemporaryDirectory(prefix="codex-summary-") as workdir:

        def _do_turn() -> str:
            # Redirect any incidental library stdout chatter to stderr so our
            # single JSON result stays the only thing on real stdout.
            with contextlib.redirect_stdout(sys.stderr):
                with Codex() as codex:
                    thread = codex.thread_start(
                        cwd=workdir,
                        model=model,
                        sandbox=Sandbox.read_only,      # no writes
                        approval_mode=ApprovalMode.deny_all,  # never prompt/block
                        ephemeral=True,                 # don't persist to session DB
                        developer_instructions=instructions,
                        # config keys mirror ~/.codex/config.toml. Disable the
                        # web search tool for a hermetic, deterministic run.
                        config={"web_search": "disabled"},
                    )
                    result = thread.run(
                        prompt,
                        model=model,
                        effort=effort_value,
                        sandbox=Sandbox.read_only,
                    )
            text = result.final_response
            if text is None:
                # No final-answer assistant message arrived (e.g. refusal or an
                # empty turn). Surface a deterministic error rather than null.
                raise RuntimeError(
                    f"no final assistant text (turn status={getattr(result, 'status', '?')})"
                )
            return text.strip()

        try:
            text = _run_with_timeout(_do_turn, timeout_s)
        except Exception as exc:  # noqa: BLE001
            _fail(f"{type(exc).__name__}: {exc}")

    sys.stdout.write(
        json.dumps({"ok": True, "text": text, "via": _sdk_via()}, ensure_ascii=False) + "\n"
    )
    sys.stdout.flush()


if __name__ == "__main__":
    main()
