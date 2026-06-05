#!/usr/bin/env python3
"""glyphdown per-tool learned policy (internal-ref SIL-2).

Tracks rolling per-(tool, command-prefix) compaction savings and auto-skips
low-entropy buckets where the codec would burn CPU for no gain. Periodically
re-samples skipped buckets so we notice when behavior drifts.

Contract (fail-open on all I/O):
- bucket_key(tool_name, content_preview) -> str
- should_skip(bucket) -> bool          (in-process cached, <2ms after first hit)
- update_policy(bucket, saved_tokens)  (persists rolling mean to disk)
- cooldown_resample(bucket) -> bool    (True == force re-sample this call)

Storage: ~/.ultracos/tool_policy.json (atomic replace).
Schema:
  {"version": 1,
   "buckets": {"<key>": {"n": int, "mean_saved": float,
                          "last_seen": ts, "skip_streak": int}}}
"""

from __future__ import annotations

import json
import os
import sys
import tempfile
import time
from pathlib import Path
from typing import Optional

POLICY_VERSION = 1
SKIP_THRESHOLD = float(os.environ.get("GLYPHDOWN_SKIP_THRESHOLD", "5"))
MIN_SAMPLES = int(os.environ.get("GLYPHDOWN_MIN_SAMPLES", "50"))
COOLDOWN_SKIPS = int(os.environ.get("GLYPHDOWN_COOLDOWN_SKIPS", "100"))

# internal-ref G16: cross-platform path resolution. Best-effort import — fail-open.
try:
    import glyphdown_paths as _paths  # noqa: F401
    _PATHS_AVAILABLE = True
except Exception:  # noqa: BLE001
    _paths = None  # type: ignore
    _PATHS_AVAILABLE = False


def _policy_dir() -> Path:
    """Get policy dir via glyphdown_paths if available, else fallback."""
    if _PATHS_AVAILABLE:
        return _paths.glyphdown_data_dir()  # type: ignore
    return Path.home() / ".ultracos"


def _policy_file() -> Path:
    """Get policy file via glyphdown_paths if available, else fallback."""
    if _PATHS_AVAILABLE:
        return _paths.tool_policy_file()  # type: ignore
    return _policy_dir() / "tool_policy.json"


_POLICY_DIR = _policy_dir()
_POLICY_FILE = _policy_file()

# In-process cache. Loaded lazily on first access so cold path is one JSON read
# and every subsequent should_skip/update_policy in the same process is O(1).
_cache: Optional[dict] = None
_cache_dirty = False


def _empty_state() -> dict:
    return {"version": POLICY_VERSION, "buckets": {}}


def _load() -> dict:
    """Read policy file once per process. Fail-open to empty state."""
    global _cache
    if _cache is not None:
        return _cache
    try:
        with open(_POLICY_FILE, "r", encoding="utf-8") as f:
            data = json.load(f)
        if not isinstance(data, dict) or "buckets" not in data:
            data = _empty_state()
        elif not isinstance(data.get("buckets"), dict):
            data["buckets"] = {}
        data.setdefault("version", POLICY_VERSION)
        _cache = data
    except (OSError, ValueError, json.JSONDecodeError):
        _cache = _empty_state()
    return _cache


def _persist() -> None:
    """Atomic write of cached state. Fail-open."""
    global _cache_dirty
    if _cache is None or not _cache_dirty:
        return
    try:
        _POLICY_DIR.mkdir(parents=True, exist_ok=True)
        fd, tmp = tempfile.mkstemp(
            dir=str(_POLICY_DIR), prefix=".tool_policy.", suffix=".json"
        )
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                json.dump(_cache, f, separators=(",", ":"))
            os.replace(tmp, _POLICY_FILE)
            _cache_dirty = False
        except OSError:
            try:
                os.unlink(tmp)
            except OSError:
                pass
    except OSError:
        pass


def _reset_for_tests() -> None:
    """Test helper: drop in-process cache so a fresh HOME is honored."""
    global _cache, _cache_dirty
    global _POLICY_DIR, _POLICY_FILE
    _POLICY_DIR = Path.home() / ".ultracos"
    _POLICY_FILE = _POLICY_DIR / "tool_policy.json"
    _cache = None
    _cache_dirty = False


# ── Public API ─────────────────────────────────────────────────────────────

def bucket_key(tool_name: str, content_preview: str) -> str:
    """Compute a stable bucket key.

    Bash: tool + first token of the command (so `rg foo` and `rg bar` share a
    bucket but `ls` lives in its own). Other tools: just the tool name —
    Read/Glob/Grep payloads are uniform enough that command-prefix splitting
    would over-shard.
    """
    if not isinstance(tool_name, str) or not tool_name:
        return "unknown"
    if tool_name == "Bash":
        preview = content_preview or ""
        # Strip leading whitespace and take the first whitespace-delimited token.
        token = preview.strip().split(None, 1)[0] if preview.strip() else ""
        # Drop trailing punctuation that occasionally trails a command word.
        token = token.rstrip(":;,").lower()
        if not token:
            return "Bash"
        return f"Bash:{token}"
    return tool_name


def should_skip(bucket: str) -> bool:
    """Return True iff codec should bypass compaction for this bucket.

    Skip when n >= MIN_SAMPLES AND mean_saved < SKIP_THRESHOLD, but every
    COOLDOWN_SKIPS consecutive skips force one re-sample so we can detect drift.
    """
    try:
        state = _load()
        rec = state.get("buckets", {}).get(bucket)
        if not isinstance(rec, dict):
            return False
        n = int(rec.get("n", 0) or 0)
        mean_saved = float(rec.get("mean_saved", 0.0) or 0.0)
        if n < MIN_SAMPLES or mean_saved >= SKIP_THRESHOLD:
            return False
        # Bucket is a skip candidate — consult cooldown.
        if cooldown_resample(bucket):
            return False
        return True
    except Exception:  # noqa: BLE001 — fail-open
        return False


def cooldown_resample(bucket: str) -> bool:
    """Increment skip_streak; on every COOLDOWN_SKIPS-th call force a re-sample.

    Side-effects: bumps skip_streak in the cached state. When the streak hits
    COOLDOWN_SKIPS we reset it to 0 and return True so the caller re-samples.
    """
    global _cache_dirty
    try:
        state = _load()
        buckets = state.setdefault("buckets", {})
        rec = buckets.setdefault(
            bucket,
            {"n": 0, "mean_saved": 0.0, "last_seen": time.time(), "skip_streak": 0},
        )
        streak = int(rec.get("skip_streak", 0) or 0) + 1
        if streak >= COOLDOWN_SKIPS:
            rec["skip_streak"] = 0
            _cache_dirty = True
            _persist()
            return True
        rec["skip_streak"] = streak
        _cache_dirty = True
        _persist()
        return False
    except Exception:  # noqa: BLE001
        return False


def update_policy(bucket: str, saved_tokens: int) -> None:
    """Update rolling mean for `bucket` with this sample. Resets skip_streak."""
    global _cache_dirty
    try:
        state = _load()
        buckets = state.setdefault("buckets", {})
        rec = buckets.get(bucket)
        if not isinstance(rec, dict):
            rec = {"n": 0, "mean_saved": 0.0, "last_seen": time.time(),
                   "skip_streak": 0}
            buckets[bucket] = rec
        n = int(rec.get("n", 0) or 0)
        mean = float(rec.get("mean_saved", 0.0) or 0.0)
        n_new = n + 1
        # Incremental mean. Robust to large n.
        mean_new = mean + (float(saved_tokens) - mean) / n_new
        rec["n"] = n_new
        rec["mean_saved"] = mean_new
        rec["last_seen"] = time.time()
        rec["skip_streak"] = 0
        _cache_dirty = True
        _persist()
    except Exception:  # noqa: BLE001
        return


if __name__ == "__main__":  # pragma: no cover — operator introspection
    state = _load()
    json.dump(state, sys.stdout, indent=2)
    sys.stdout.write("\n")
