#!/usr/bin/env python3
"""glyphdown SessionStart restart-batcher auto-clear (internal-ref).

A fresh process restart proves the queued config changes are now in
effect, so the pending-restart queue can be cleared automatically.

Critical: SessionStart fires with four possible `source` values —
`startup`, `resume`, `clear`, `compact`. Only `startup` is an actual
new process that reloads ~/.claude/settings.json and plugin config.
`resume` reuses an existing process; `clear` and `compact` are
in-session events that do NOT reload plugin config. Auto-clearing
on those would erase a legitimately-pending restart signal and break
the batcher's core invariant. We gate strictly on `source == startup`.

Emits one advisory context line when entries are cleared so the
operator sees the loop close; silent in every other case (empty queue,
non-startup source, module-load failure) so we add zero context tax
on every other session start.

Fail-open: any exception → {"continue": true}.
"""
from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path

_PLUGIN_ROOT = Path(__file__).resolve().parents[2]
_MODULE_PATH = _PLUGIN_ROOT / "policy" / "restart_batcher.py"


def _load_rb():
    spec = importlib.util.spec_from_file_location("restart_batcher", _MODULE_PATH)
    if spec is None or spec.loader is None:
        return None
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _read_source() -> str:
    try:
        raw = sys.stdin.read()
        if not raw:
            return ""
        payload = json.loads(raw)
        return str(payload.get("source") or "")
    except Exception:
        return ""


def main() -> int:
    try:
        source = _read_source()
        # Only `startup` reloads plugin config / settings.json.
        # `resume` reuses the existing process; `clear` and `compact`
        # are in-session and do NOT pick up queued restart-required
        # changes. Clearing on those would silently drop the signal.
        if source != "startup":
            print(json.dumps({"continue": True}))
            return 0
        rb = _load_rb()
        if rb is None:
            print(json.dumps({"continue": True}))
            return 0
        if rb.pending_count() == 0:
            print(json.dumps({"continue": True}))
            return 0
        cleared = rb.clear(reason="session-start-auto-clear")
        ctx = (f"[restart-batcher] auto-cleared {cleared} pending "
               f"entr{'y' if cleared == 1 else 'ies'} on session start")
        print(json.dumps({"continue": True, "additionalContext": ctx}))
        return 0
    except Exception:
        print(json.dumps({"continue": True}))
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
