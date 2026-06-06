#!/usr/bin/env python3
"""glyphdown PreToolUse history-dedup ring (internal-ref).

Detects consecutive duplicate tool calls within 5 minutes. Two modes:

  - WARN (default): emit advisory via additionalContext, never block.
  - DEDUP-SERVE (opt-in, GLYPHDOWN_DEDUP_SERVE=1): for idempotent Read calls
    only, deny the redundant re-read and point the model at its prior result.
    The prior tool result is still in context (median dup gap ~19s), so a
    ~30-token pointer replaces a multi-KB re-injection. Bash and all
    write-capable tools are EXCLUDED; any write-capable tool observed after
    the prior read invalidates the serve (Read-after-Edit stays correct).

Rolling 10-call ring per session_id, persisted to ~/.ultracos/history_ring.json.
Fail-open: any error → allow the call.

Audit trail: ~/.ultracos/audit.jsonl event="history-dedup-warn" (always) plus
event="history-dedup-serve" when a re-read is actually short-circuited.
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

# Dedup-serve scope: only tools whose result is a pure function of their args
# (no side effects, no time dependence) are safe to short-circuit.
_IDEMPOTENT_READ_TOOLS = frozenset({"Read"})

# Any of these observed AFTER the prior read means the read's result may have
# changed — invalidate the serve and let the re-read proceed. Bash is included
# because it can write files invisibly (e.g. `echo > f`); correctness over
# savings.
_WRITE_CAPABLE_TOOLS = frozenset(
    {"Edit", "Write", "MultiEdit", "NotebookEdit", "Bash"}
)


def _dedup_serve_enabled() -> bool:
    """Opt-in gate. Default OFF preserves byte-identical warn-only behavior."""
    return os.environ.get("GLYPHDOWN_DEDUP_SERVE", "").strip() in {"1", "true", "yes"}


def _data_dir() -> Path:
    """Resolve glyphdown data dir. Cross-platform (internal-ref G16).

    Priority: GLYPHDOWN_DATA_DIR env override > Path.home()/.ultracos.
    Path.home() is cross-platform (Linux/macOS=~, Windows=%USERPROFILE%).
    """
    override = os.environ.get("GLYPHDOWN_DATA_DIR", "").strip()
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
        prior_ts = None
        seconds_ago = 0
        for i, entry in enumerate(reversed(ring)):
            ts = entry.get("ts", 0)
            if now - ts > _DEDUP_WINDOW_SECS:
                break
            if entry.get("tool") == tool_name and entry.get("hash") == args_hash:
                prior_ts = ts
                seconds_ago = int(now - ts)
                warn_msg = (
                    f"[glyphdown] You called {tool_name} with these same args "
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

        # Dedup-serve eligibility (opt-in). Only short-circuit an idempotent
        # Read whose result cannot have changed since the prior read: no
        # write-capable tool may have run after prior_ts.
        serve = False
        if (
            prior_ts is not None
            and _dedup_serve_enabled()
            and tool_name in _IDEMPOTENT_READ_TOOLS
        ):
            mutated_after = any(
                e.get("tool") in _WRITE_CAPABLE_TOOLS
                and e.get("ts", 0) > prior_ts
                for e in ring
            )
            serve = not mutated_after

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

        # Emit response. Dedup-serve denies the redundant read with a pointer;
        # otherwise allow (with advisory context if a warn fired).
        if serve:
            reason = (
                f"[glyphdown] Redundant Read: you read these exact args "
                f"{seconds_ago}s ago and no write has touched it since, so the "
                f"result is unchanged and already in your context above. Reuse "
                f"that prior result instead of re-reading."
            )
            _write_audit({
                "ts": now,
                "event": "history-dedup-serve",
                "tool": tool_name,
                "session_id": session_id,
                "seconds_ago": seconds_ago,
            })
            resp = {
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason,
                }
            }
            print(json.dumps(resp))
            return 0

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
