#!/usr/bin/env python3
"""ultracos PreToolUse restart-batcher detect (internal-ref).

Detects tool invocations that imply a Claude Code session restart is
needed for the change to take effect, and enqueues a row in the
pending-restart batch queue so operator can schedule one batched
restart instead of paying ~38K cache_creation tokens per individual
change.

Watched patterns:
  - Bash: `claude plugin install`, `claude plugin marketplace add`
  - Edit/Write to ~/.claude/settings.json with a "hooks" key change
  - Edit/Write to ~/.claude/plugins/installed_plugins.json
  - Edit/Write to any .mcp.json or mcp-config file

Fail-open: never blocks the tool call. On any exception, emits
{"continue": true} and exits 0.
"""

from __future__ import annotations

import importlib.util
import json
import os
import sys
from pathlib import Path

# Direct-load the policy module to avoid site-packages collisions.
_PLUGIN_ROOT = Path(__file__).resolve().parents[2]
_MODULE_PATH = _PLUGIN_ROOT / "policy" / "restart_batcher.py"

_RESTART_FILE_PATTERNS = (
    (str(Path.home() / ".claude" / "settings.json"), "settings-edit", "medium"),
    (str(Path.home() / ".claude" / "plugins" / "installed_plugins.json"),
     "plugin-state-edit", "medium"),
)

_BASH_INSTALL_PATTERNS = (
    ("claude plugin install", "plugin-install", "low"),
    ("claude plugin marketplace add", "marketplace-add", "low"),
    ("claude plugin uninstall", "plugin-uninstall", "low"),
)


def _load_rb():
    spec = importlib.util.spec_from_file_location("restart_batcher", _MODULE_PATH)
    if spec is None or spec.loader is None:
        return None
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _classify(payload: dict) -> tuple[str, str, str] | None:
    tool = payload.get("tool_name") or payload.get("tool") or ""
    inp = payload.get("tool_input") or payload.get("input") or {}
    if tool == "Bash":
        cmd = (inp.get("command") or "")[:500]
        for needle, kind, urgency in _BASH_INSTALL_PATTERNS:
            if needle in cmd:
                return (kind, cmd[:120], urgency)
        return None
    if tool in ("Edit", "Write", "MultiEdit"):
        path = inp.get("file_path") or inp.get("path") or ""
        for prefix, kind, urgency in _RESTART_FILE_PATTERNS:
            if path == prefix or path.startswith(prefix):
                return (kind, path, urgency)
        # .mcp.json anywhere
        if path.endswith(".mcp.json") or path.endswith("/mcp-config.json"):
            return ("mcp-config-edit", path, "medium")
    return None


def main() -> int:
    try:
        raw = sys.stdin.read()
        payload = json.loads(raw) if raw else {}
        hit = _classify(payload)
        if hit is None:
            print(json.dumps({"continue": True}))
            return 0
        kind, detail, urgency = hit
        rb = _load_rb()
        if rb is None:
            print(json.dumps({"continue": True}))
            return 0
        rb.enqueue(kind=kind, detail=detail, urgency=urgency)
        # Non-blocking advisory context so operator sees the batch state
        summary = rb.pending_summary()
        ctx = (f"[restart-batcher] queued {kind}; {summary}"
               if summary else f"[restart-batcher] queued {kind}")
        print(json.dumps({"continue": True, "additionalContext": ctx}))
        return 0
    except Exception:
        # Fail-open: never block a tool call.
        print(json.dumps({"continue": True}))
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
