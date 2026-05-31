#!/usr/bin/env python3
"""ultracos SIL-1 self-improving loop: codec break-even auto-tuner.

internal-ref SIL-1: read the local audit log (~/.ultracos/audit.jsonl), fit a
per-tool break-even threshold from observed savings distributions, and persist
it to ~/.ultracos/tuned_thresholds.json for the next session.

Design contract:
- Cold start (no audit): caller falls back to static DEFAULT_BREAK_EVEN_TOKENS.
- Threshold per tool = max(p10(saved_tokens), FLOOR) where FLOOR=10.
- Threshold per shape = max(p10(saved_tokens grouped by row["shape"]), FLOOR).
  Codec resolves shape-key first (json/yaml/code/text/...), then tool-key,
  then DEFAULT. Shape granularity is a strictly tighter signal than tool
  because the same tool (Bash) produces wildly different shapes
  (json vs ansi-laden text); the absolute-token guard should track shape.
- ULTRACOS_BREAK_EVEN_TOKENS env var wins over tuned thresholds (codec-side).
- ULTRACOS_NO_LEARN=1 disables loading (codec-side: skip load_thresholds()).
- Fail-open: any I/O / parse error returns sane empty defaults.
- Atomic write: tempfile + os.replace.
- Tuned file shape:
  {"version": 1, "computed_at": ts,
   "per_tool": {tool: int}, "per_shape": {shape: int}}.
- Periodic refresh: this module is cron-runnable as a one-shot CLI —
  ``*/15 * * * * python3 ultracos_tuned.py refresh`` — so the
  break-even floor tracks the observed savings distribution without
  blocking the PostToolUse hot path.
"""

from __future__ import annotations

import json
import os
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

FLOOR = 10
VERSION = 1
DEFAULT_LOOKBACK_DAYS = 7
DEFAULT_PERCENTILE = 0.10  # p10

# internal-ref G16: cross-platform path resolution. Best-effort import — fail-open.
try:
    import ultracos_paths as _paths  # noqa: F401
    _PATHS_AVAILABLE = True
except Exception:  # noqa: BLE001
    _paths = None  # type: ignore
    _PATHS_AVAILABLE = False


def _audit_dir() -> Path:
    """Resolve audit dir each call so HOME changes (tests) are honored."""
    if _PATHS_AVAILABLE:
        return _paths.ultracos_data_dir()  # type: ignore
    return Path(os.path.expanduser("~")) / ".ultracos"


def _audit_file() -> Path:
    if _PATHS_AVAILABLE:
        return _paths.audit_file()  # type: ignore
    return _audit_dir() / "audit.jsonl"


def _tuned_file() -> Path:
    if _PATHS_AVAILABLE:
        return _paths.tuned_thresholds_file()  # type: ignore
    return _audit_dir() / "tuned_thresholds.json"


def load_audit(days: int = DEFAULT_LOOKBACK_DAYS) -> list[dict[str, Any]]:
    """Read compact rows from the audit log within the lookback window.

    Returns a list of dicts. Fail-open: returns [] on any I/O / parse error.
    """
    try:
        path = _audit_file()
        if not path.exists():
            return []
        cutoff = time.time() - (days * 86400)
        rows: list[dict[str, Any]] = []
        with open(path, "r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    row = json.loads(line)
                except (json.JSONDecodeError, ValueError):
                    continue
                if not isinstance(row, dict):
                    continue
                if row.get("event") != "compact":
                    continue
                ts = row.get("ts")
                if isinstance(ts, (int, float)) and ts < cutoff:
                    continue
                rows.append(row)
        return rows
    except OSError:
        return []


def _percentile(sorted_vals: list[float], p: float) -> float:
    """Nearest-rank percentile on a pre-sorted list. p in [0, 1].

    Uses the canonical nearest-rank formula: rank = ceil(p * N), then index = rank-1.
    For p=0.10 and N=10 → rank=1, index=0 (smallest value).
    """
    import math
    if not sorted_vals:
        return 0.0
    n = len(sorted_vals)
    if n == 1:
        return float(sorted_vals[0])
    rank = max(1, math.ceil(p * n))
    idx = max(0, min(n - 1, rank - 1))
    return float(sorted_vals[idx])


def compute_per_tool_threshold(rows: list[dict[str, Any]]) -> dict[str, int]:
    """Fit per-tool break-even threshold from observed saved_tokens.

    threshold = max(p10(saved_tokens for that tool), FLOOR)
    Returns {} if no usable rows.
    """
    by_tool: dict[str, list[float]] = {}
    for row in rows:
        tool = row.get("tool")
        saved = row.get("saved_tokens")
        if not isinstance(tool, str) or not tool:
            continue
        if not isinstance(saved, (int, float)):
            continue
        if saved <= 0:
            continue
        by_tool.setdefault(tool, []).append(float(saved))

    thresholds: dict[str, int] = {}
    for tool, vals in by_tool.items():
        vals.sort()
        p10 = _percentile(vals, DEFAULT_PERCENTILE)
        thresholds[tool] = int(max(p10, FLOOR))
    return thresholds


def compute_per_shape_threshold(rows: list[dict[str, Any]]) -> dict[str, int]:
    """Fit per-shape break-even threshold from observed saved_tokens.

    Mirrors :func:`compute_per_tool_threshold` but keys on ``row["shape"]``
    (json/yaml/toml/code/text/html/binary/empty). Shape is a strictly
    tighter signal than tool — Bash output is sometimes JSON, sometimes
    ANSI-laden text, and the absolute-token guard should track that.

    threshold = max(p10(saved_tokens for that shape), FLOOR)
    Returns {} if no usable rows.
    """
    by_shape: dict[str, list[float]] = {}
    for row in rows:
        shape = row.get("shape")
        saved = row.get("saved_tokens")
        if not isinstance(shape, str) or not shape:
            continue
        if not isinstance(saved, (int, float)):
            continue
        if saved <= 0:
            continue
        by_shape.setdefault(shape, []).append(float(saved))

    thresholds: dict[str, int] = {}
    for shape, vals in by_shape.items():
        vals.sort()
        p10 = _percentile(vals, DEFAULT_PERCENTILE)
        thresholds[shape] = int(max(p10, FLOOR))
    return thresholds


def persist_thresholds(
    thresholds: dict[str, int],
    *,
    per_shape: dict[str, int] | None = None,
) -> bool:
    """Atomically write tuned thresholds to disk. Fail-open: returns False on error.

    Writes per-tool thresholds (legacy positional argument) plus optional
    per-shape thresholds (internal-ref extension). Both keys are always present
    in the persisted payload so the loader does not need to feature-detect.
    """
    try:
        d = _audit_dir()
        d.mkdir(parents=True, exist_ok=True)
        payload = {
            "version": VERSION,
            "computed_at": time.time(),
            "per_tool": {k: int(v) for k, v in thresholds.items()},
            "per_shape": {
                k: int(v) for k, v in (per_shape or {}).items()
            },
        }
        # atomic write: tempfile in same dir + os.replace
        fd, tmp_path = tempfile.mkstemp(prefix=".tuned_thresholds.", suffix=".tmp", dir=str(d))
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                json.dump(payload, f, separators=(",", ":"))
            os.replace(tmp_path, str(_tuned_file()))
            return True
        except OSError:
            try:
                os.unlink(tmp_path)
            except OSError:
                pass
            return False
    except OSError:
        return False


def _load_payload() -> dict[str, Any] | None:
    """Read the tuned-thresholds payload. Fail-open: None on any error."""
    try:
        path = _tuned_file()
        if not path.exists():
            return None
        with open(path, "r", encoding="utf-8") as f:
            payload = json.load(f)
        if not isinstance(payload, dict):
            return None
        return payload
    except (OSError, json.JSONDecodeError, ValueError):
        return None


def _coerce_int_map(raw: Any) -> dict[str, int]:
    if not isinstance(raw, dict):
        return {}
    out: dict[str, int] = {}
    for k, v in raw.items():
        if not isinstance(k, str):
            continue
        try:
            out[k] = int(v)
        except (TypeError, ValueError):
            continue
    return out


def load_thresholds() -> dict[str, int]:
    """Load persisted per-tool thresholds. Fail-open: returns {} on any error."""
    payload = _load_payload()
    if payload is None:
        return {}
    return _coerce_int_map(payload.get("per_tool", {}))


def load_shape_thresholds() -> dict[str, int]:
    """Load persisted per-shape thresholds. Fail-open: returns {} on any error.

    Returns {} when the file pre-dates internal-ref (no ``per_shape`` key) so
    the codec gracefully falls back to per-tool / DEFAULT.
    """
    payload = _load_payload()
    if payload is None:
        return {}
    return _coerce_int_map(payload.get("per_shape", {}))


def refresh_thresholds(days: int = DEFAULT_LOOKBACK_DAYS) -> dict[str, Any]:
    """Convenience: load_audit → compute (tool + shape) → persist → return.

    Return shape changed in internal-ref from ``dict[str, int]`` (per-tool only)
    to ``{"per_tool": {...}, "per_shape": {...}}`` so callers can inspect
    both granularities. The on-disk file format also carries both keys.
    """
    rows = load_audit(days=days)
    if not rows:
        return {"per_tool": {}, "per_shape": {}}
    per_tool = compute_per_tool_threshold(rows)
    per_shape = compute_per_shape_threshold(rows)
    if per_tool or per_shape:
        persist_thresholds(per_tool, per_shape=per_shape)
    return {"per_tool": per_tool, "per_shape": per_shape}


# CLI entry point (manual refresh / debugging / cron)
if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "refresh":
        t = refresh_thresholds()
        print(json.dumps({"refreshed": t, "file": str(_tuned_file())}, indent=2))
        sys.exit(0)
    print(json.dumps({
        "per_tool": load_thresholds(),
        "per_shape": load_shape_thresholds(),
    }, indent=2))
    sys.exit(0)
