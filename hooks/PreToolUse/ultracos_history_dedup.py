#!/usr/bin/env python3
"""ultracos PreToolUse history-dedup ring (internal-ref).

Detects consecutive duplicate tool calls within 5 minutes and emits advisory
warning via additionalContext. Rolling 10-call ring per session_id, persisted
to ~/.ultracos/history_ring.json. Fail-open: never blocks.

Audit trail: ~/.ultracos/audit.jsonl event="history-dedup-warn"
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
import time
from pathlib import Path

_RING_SIZE = 10
_DEDUP_WINDOW_SECS = 300  # 5 minutes


def _data_dir() -> Path:
    """Resolve ultracos data dir. Cross-platform (internal-ref G16).

    Priority: ULTRACOS_DATA_DIR env override > Path.home()/.ultracos.
    Path.home() is cross-platform (Linux/macOS=~, Windows=%USERPROFILE%).
    """
    override = os.environ.get("ULTRACOS_DATA_DIR", "").strip()
    if override:
        return Path(override).expanduser()
    return Path.home() / ".ultracos"


# Module-level capture preserved so existing tests that monkeypatch
# _AUDIT_FILE / _RING_FILE continue to work. _data_dir() is called per
# attribute below at import time, mirroring the historical layout but
# routing through the cross-platform resolver.
_RING_FILE = _data_dir() / "history_ring.json"
_AUDIT_DIR = _data_dir()
_AUDIT_FILE = _AUDIT_DIR / "audit.jsonl"


def _fnv1a_hash(data: str) -> str:
    """FNV-1a hash of canonicalized JSON string."""
    return hashlib.md5(data.encode("utf-8")).hexdigest()[:16]


def _normalize_args(tool_input: dict) -> str:
    """Canonicalize tool_input: sorted keys, no whitespace."""
    try:
        return json.dumps(tool_input, sort_keys=True, separators=(",", ":"))
    except (TypeError, ValueError):
        return ""


def _write_audit(row: dict) -> None:
    """Append-only audit JSONL. Fail-open on any I/O error."""
    try:
        _AUDIT_DIR.mkdir(parents=True, exist_ok=True)
        line = json.dumps(row, separators=(",", ":")) + "\n"
        with open(_AUDIT_FILE, "a", encoding="utf-8") as f:
            f.write(line)
    except OSError:
        pass


def _load_ring(session_id: str) -> list[dict]:
    """Load ring from file. Return empty list on any error."""
    try:
        if not _RING_FILE.exists():
            return []
        with open(_RING_FILE, "r", encoding="utf-8") as f:
            data = json.load(f)
        if not isinstance(data, dict):
            return []
        ring = data.get(session_id, [])
        return list(ring) if isinstance(ring, list) else []
    except (OSError, json.JSONDecodeError):
        return []


def _save_ring(rings: dict[str, list[dict]]) -> None:
    """Persist all session rings. Fail-open on any I/O error."""
    try:
        _RING_FILE.parent.mkdir(parents=True, exist_ok=True)
        with open(_RING_FILE, "w", encoding="utf-8") as f:
            json.dump(rings, f, separators=(",", ":"))
    except OSError:
        pass


def main() -> int:
    try:
        raw = sys.stdin.read()
        if not raw:
            print(json.dumps({"continue": True}))
            return 0

        payload = json.loads(raw)
        tool_name = payload.get("tool_name", "")
        session_id = payload.get("session_id") or os.environ.get(
            "CLAUDE_SESSION_ID", f"pid-{os.getpid()}"
        )
        tool_input = payload.get("tool_input") or {}
        now = time.time()

        # Normalize args to hash
        norm_args = _normalize_args(tool_input)
        args_hash = _fnv1a_hash(norm_args)

        # Load ring for this session
        ring = _load_ring(session_id)

        # Check for recent duplicate
        warn_msg = None
        for i, entry in enumerate(reversed(ring)):
            ts = entry.get("ts", 0)
            if now - ts > _DEDUP_WINDOW_SECS:
                break
            if entry.get("tool") == tool_name and entry.get("hash") == args_hash:
                seconds_ago = int(now - ts)
                warn_msg = (
                    f"[ultracos] You called {tool_name} with these same args "
                    f"{seconds_ago} seconds ago. Reusing the prior result avoids cost."
                )
                _write_audit({
                    "ts": now,
                    "event": "history-dedup-warn",
                    "tool": tool_name,
                    "session_id": session_id,
                    "seconds_ago": seconds_ago,
                })
                break

        # Update ring: append new entry, prune old, cap at _RING_SIZE
        ring.append({
            "tool": tool_name,
            "hash": args_hash,
            "ts": now,
        })

        # Prune entries older than _DEDUP_WINDOW_SECS
        ring = [e for e in ring if now - e.get("ts", 0) <= _DEDUP_WINDOW_SECS * 2]

        # Cap at _RING_SIZE; drop oldest if over
        if len(ring) > _RING_SIZE:
            ring = ring[-_RING_SIZE:]

        # Save all rings back to file
        all_rings = {}
        try:
            if _RING_FILE.exists():
                with open(_RING_FILE, "r", encoding="utf-8") as f:
                    all_rings = json.load(f) or {}
        except (OSError, json.JSONDecodeError):
            all_rings = {}

        all_rings[session_id] = ring
        _save_ring(all_rings)

        # Emit additionalContext if warning fired
        resp = {"continue": True}
        if warn_msg:
            resp["additionalContext"] = warn_msg

        print(json.dumps(resp))
        return 0

    except Exception:  # noqa: BLE001 — fail-open is the contract
        print(json.dumps({"continue": True}))
        return 0


if __name__ == "__main__":
    sys.exit(main())
