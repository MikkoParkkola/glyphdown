#!/usr/bin/env python3
"""glyphdown cache-aware compression (internal-ref SIL-5).

Back off compression on cache-hot prefixes so the Anthropic native prompt
cache stays warm. Compressing a payload that starts with a long stable
prefix that has already been cached upstream would mutate the cache key
and force a fresh fetch -- net-negative for token cost and latency even
when the codec saves a few hundred tokens.

This module ships the *heuristic* fallback path described in
docs/SIL5_CACHE_AWARE.md: until the Anthropic SDK exposes
``usage.cache_read_input_tokens`` and a PostMessageReceive hook fires, we
infer cache-hotness from repeated long-prefix observations across hook
calls. A prefix that recurs N+ times within a TTL window is treated as
cache-hot and bypasses compression.

Contract (fail-open on every I/O):
- ``prefix_signature(text)``      -> str | None  (None when text is too short)
- ``observe(text, *, now=None)``  -> None        (records one sighting)
- ``is_cache_hot(text, *, now=None)`` -> bool    (True when bypass desired)
- ``should_bypass_for_cache(text)`` -> bool      (observe+probe convenience)

Storage: ``<glyphdown_data_dir>/cache_state.json``
Schema (v1):
    {
      "version": 1,
      "prefixes": {
        "<sig>": {"hits": int, "first_seen": ts, "last_seen": ts}
      }
    }

Tunables (env):
- ``GLYPHDOWN_CACHE_AWARE``        bool   (default FALSE; opt-in flag)
- ``GLYPHDOWN_CACHE_PREFIX_BYTES`` int    (default 1024; min prefix length to track)
- ``GLYPHDOWN_CACHE_HOT_HITS``     int    (default 2;  hits to declare hot)
- ``GLYPHDOWN_CACHE_TTL_SECONDS``  int    (default 7 * 86400)
- ``GLYPHDOWN_CACHE_MAX_ENTRIES``  int    (default 2048; LRU cap)
"""

from __future__ import annotations

import hashlib
import json
import os
import tempfile
import time
from pathlib import Path
from typing import Optional

POLICY_VERSION = 1


def _int_env(name: str, default: int) -> int:
    raw = os.environ.get(name)
    if raw is None or raw == "":
        return default
    try:
        return int(raw)
    except ValueError:
        return default


def _bool_env(name: str, default: bool) -> bool:
    raw = os.environ.get(name)
    if raw is None or raw == "":
        return default
    return raw.strip().lower() in ("1", "true", "yes", "on")


def _config() -> dict:
    """Resolve config each call so test env overrides are honored."""
    return {
        # Default OFF: SIL-5 ships behind a flag so existing audit/test
        # invariants stay intact until the operator opts in. Enablement
        # happens once we have the Anthropic SDK ``cache_read_input_tokens``
        # signal (docs/SIL5_CACHE_AWARE.md) confirming the heuristic
        # converges on real cache hits.
        "enabled": _bool_env("GLYPHDOWN_CACHE_AWARE", False),
        "prefix_bytes": max(64, _int_env("GLYPHDOWN_CACHE_PREFIX_BYTES", 1024)),
        "hot_hits": max(2, _int_env("GLYPHDOWN_CACHE_HOT_HITS", 2)),
        "ttl_seconds": max(60, _int_env("GLYPHDOWN_CACHE_TTL_SECONDS", 7 * 86400)),
        "max_entries": max(16, _int_env("GLYPHDOWN_CACHE_MAX_ENTRIES", 2048)),
    }


# Best-effort import of glyphdown_paths so we share the same data dir as the
# rest of the codec stack. Falls back to ~/.ultracos to keep the module
# usable in dev / test contexts where the helper isn't importable.
try:
    import glyphdown_paths as _paths  # type: ignore
    _PATHS_AVAILABLE = True
except Exception:  # noqa: BLE001
    _paths = None  # type: ignore
    _PATHS_AVAILABLE = False


def _state_dir() -> Path:
    if _PATHS_AVAILABLE:
        try:
            return _paths.glyphdown_data_dir()  # type: ignore
        except Exception:  # noqa: BLE001
            pass
    return Path.home() / ".ultracos"


def _state_file() -> Path:
    return _state_dir() / "cache_state.json"


def prefix_signature(text: str, *, prefix_bytes: Optional[int] = None) -> Optional[str]:
    """Hash the first ``prefix_bytes`` of ``text`` to a stable signature.

    Returns ``None`` when the text is shorter than ``prefix_bytes`` -- short
    payloads cannot meaningfully participate in cache reuse, and tracking
    them would just pollute the state map with one-shot entries.
    """
    if not isinstance(text, str) or not text:
        return None
    cfg = _config()
    limit = prefix_bytes if prefix_bytes is not None else cfg["prefix_bytes"]
    raw = text.encode("utf-8", errors="ignore")
    if len(raw) < limit:
        return None
    head = raw[:limit]
    return hashlib.blake2b(head, digest_size=16).hexdigest()


def _load_state() -> dict:
    path = _state_file()
    try:
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, ValueError, json.JSONDecodeError):
        return {"version": POLICY_VERSION, "prefixes": {}}
    if not isinstance(data, dict) or data.get("version") != POLICY_VERSION:
        return {"version": POLICY_VERSION, "prefixes": {}}
    prefixes = data.get("prefixes")
    if not isinstance(prefixes, dict):
        data["prefixes"] = {}
    return data


def _save_state(state: dict) -> None:
    """Atomic write via tempfile + replace. Fail-open on any I/O error."""
    try:
        directory = _state_dir()
        directory.mkdir(parents=True, exist_ok=True)
        path = _state_file()
        # NamedTemporaryFile in same dir so os.replace is atomic on POSIX/Win.
        with tempfile.NamedTemporaryFile(
            mode="w", encoding="utf-8", dir=str(directory),
            prefix=".cache_state.", suffix=".tmp", delete=False,
        ) as tmp:
            json.dump(state, tmp, separators=(",", ":"))
            tmp_name = tmp.name
        os.replace(tmp_name, path)
    except OSError:
        # Best-effort cleanup; never raise out of the cache module.
        try:
            if "tmp_name" in locals():
                os.unlink(tmp_name)
        except OSError:
            pass


def _prune(state: dict, *, now: float, ttl: int, max_entries: int) -> None:
    """Evict expired entries; cap the map at ``max_entries`` by oldest last_seen."""
    prefixes = state.get("prefixes", {})
    cutoff = now - ttl
    expired = [k for k, v in prefixes.items()
               if not isinstance(v, dict) or float(v.get("last_seen", 0)) < cutoff]
    for k in expired:
        prefixes.pop(k, None)
    if len(prefixes) > max_entries:
        # Drop oldest-last-seen until under cap.
        sorted_keys = sorted(
            prefixes.items(),
            key=lambda kv: float(kv[1].get("last_seen", 0))
            if isinstance(kv[1], dict) else 0.0,
        )
        overflow = len(prefixes) - max_entries
        for k, _ in sorted_keys[:overflow]:
            prefixes.pop(k, None)


def observe(text: str, *, now: Optional[float] = None) -> Optional[str]:
    """Record a sighting of ``text`` prefix. Returns the signature or None."""
    sig = prefix_signature(text)
    if sig is None:
        return None
    cfg = _config()
    if not cfg["enabled"]:
        return sig
    ts = float(now) if now is not None else time.time()
    state = _load_state()
    prefixes = state.setdefault("prefixes", {})
    entry = prefixes.get(sig)
    if not isinstance(entry, dict):
        entry = {"hits": 0, "first_seen": ts, "last_seen": ts}
    entry["hits"] = int(entry.get("hits", 0)) + 1
    entry["last_seen"] = ts
    entry.setdefault("first_seen", ts)
    prefixes[sig] = entry
    _prune(state, now=ts, ttl=cfg["ttl_seconds"], max_entries=cfg["max_entries"])
    _save_state(state)
    return sig


def is_cache_hot(text: str, *, now: Optional[float] = None) -> bool:
    """True when ``text``'s prefix has been observed >= hot_hits within TTL."""
    cfg = _config()
    if not cfg["enabled"]:
        return False
    sig = prefix_signature(text)
    if sig is None:
        return False
    ts = float(now) if now is not None else time.time()
    state = _load_state()
    entry = state.get("prefixes", {}).get(sig)
    if not isinstance(entry, dict):
        return False
    last_seen = float(entry.get("last_seen", 0))
    if (ts - last_seen) > cfg["ttl_seconds"]:
        return False
    return int(entry.get("hits", 0)) >= cfg["hot_hits"]


def should_bypass_for_cache(text: str, *, now: Optional[float] = None) -> bool:
    """Convenience wrapper used by the codec: record THEN probe.

    Record-then-probe semantics mean the *first* sighting bumps the counter
    to 1 (still below the default hot_hits=2 threshold so we let compaction
    proceed and gather a baseline measurement); the *second* sighting bumps
    to 2 and trips the threshold, so the codec bypasses to preserve the
    Anthropic native prompt cache key. Observation runs first so a clean
    state file converges on "hot" after exactly ``hot_hits`` invocations.
    """
    try:
        observe(text, now=now)
    except Exception:  # noqa: BLE001 — observation must never break codec
        pass
    return is_cache_hot(text, now=now)
