#!/usr/bin/env python3
"""ultracos schema-tag A/B effectiveness monitor (internal-ref).

Reads ~/.ultracos/audit.jsonl, aggregates per-session compact-token deltas
between with-tag and no-tag variants, appends rollup rows to
~/.ultracos/schema-tag-ab.jsonl, and writes a disable-flag file when N>=20
sessions per variant have accumulated with no measurable effect.

Honesty note: the "output-token delta" we measure here is the *compact_tokens*
the model sees as input on its next turn, NOT downstream model output verbosity
(that's internal-ref). The hypothesis being tested: does emitting the `[ultracos:
compact-v1 ...]` schema tag change subsequent input-token volume (e.g. via the
model echoing it or compensating with verbosity that re-bloats the next call)?
If across N>=20 sessions per variant the mean compact-token-per-event delta is
under 5% relative, we conclude "no effect" and write a disable flag.

Auto-disable semantics: the flag tells the codec to stop emitting the tag
(default to no-tag). It is SEPARATE from ULTRACOS_AB_DISABLE (which forces
with-tag and is used to short-circuit the experiment). See `decide_variant`
in ultracos_ab.py for the consultation order.

Fail-open: every public function returns a safe value on error and never raises.
"""

from __future__ import annotations

import json
import os
from collections import defaultdict
from pathlib import Path
from typing import Optional

try:
    from ultracos_paths import ultracos_data_dir as _data_dir  # type: ignore
except Exception:  # noqa: BLE001
    def _data_dir() -> Path:
        env = os.environ.get("ULTRACOS_DATA_DIR")
        if env:
            p = Path(env).expanduser()
        else:
            p = Path.home() / ".ultracos"
        p.mkdir(parents=True, exist_ok=True)
        return p


MIN_SESSIONS_PER_VARIANT = 20
MIN_RELATIVE_DELTA = 0.05  # 5%

VARIANT_WITH_TAG = "with-tag"
VARIANT_NO_TAG = "no-tag"


def _audit_path() -> Path:
    raw = os.environ.get("ULTRACOS_AUDIT_FILE")
    if raw:
        return Path(raw).expanduser()
    return _data_dir() / "audit.jsonl"


def _rollup_path() -> Path:
    raw = os.environ.get("ULTRACOS_AB_ROLLUP_FILE")
    if raw:
        return Path(raw).expanduser()
    return _data_dir() / "schema-tag-ab.jsonl"


def _disable_flag_path() -> Path:
    raw = os.environ.get("ULTRACOS_AB_DISABLE_FLAG")
    if raw:
        return Path(raw).expanduser()
    return _data_dir() / "schema-tag-ab.disabled"


def _load_audit(path: Path) -> list[dict]:
    rows: list[dict] = []
    if not path.exists():
        return rows
    try:
        with open(path, "r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    row = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if isinstance(row, dict):
                    rows.append(row)
    except OSError:
        return []
    return rows


def aggregate_sessions(audit_path: Optional[Path] = None) -> list[dict]:
    """Group compact events by session_id; one summary row per session.

    Returns list of dicts: {session_id, variant, n_events,
    sum_original_tokens, sum_compact_tokens, mean_compact_tokens}.
    Sessions where variant is mixed or unknown are dropped (defensive).
    """
    path = audit_path or _audit_path()
    rows = _load_audit(path)
    per_sid: dict[str, dict] = {}
    for r in rows:
        if r.get("event") != "compact":
            continue
        sid = r.get("session_id")
        variant = r.get("variant")
        if not sid or variant not in (VARIANT_WITH_TAG, VARIANT_NO_TAG):
            continue
        entry = per_sid.setdefault(sid, {
            "session_id": sid,
            "variant": variant,
            "n_events": 0,
            "sum_original_tokens": 0,
            "sum_compact_tokens": 0,
            "_variants_seen": set(),
        })
        entry["_variants_seen"].add(variant)
        entry["n_events"] += 1
        entry["sum_original_tokens"] += int(r.get("original_tokens", 0) or 0)
        entry["sum_compact_tokens"] += int(r.get("compact_tokens", 0) or 0)

    out: list[dict] = []
    for sid, entry in per_sid.items():
        if len(entry["_variants_seen"]) != 1:
            continue  # session-id collision across variants; drop
        entry.pop("_variants_seen", None)
        n = entry["n_events"]
        entry["mean_compact_tokens"] = (
            entry["sum_compact_tokens"] / n if n > 0 else 0.0
        )
        out.append(entry)
    return out


def evaluate(
    sessions: list[dict],
    min_sessions: int = MIN_SESSIONS_PER_VARIANT,
    min_rel_delta: float = MIN_RELATIVE_DELTA,
) -> dict:
    """Compute summary verdict from aggregated session rows.

    Returns: {with_tag_n, no_tag_n, with_tag_mean, no_tag_mean,
              relative_delta, sufficient, no_effect, should_disable}.
    """
    wt = [s for s in sessions if s["variant"] == VARIANT_WITH_TAG]
    nt = [s for s in sessions if s["variant"] == VARIANT_NO_TAG]
    wt_n, nt_n = len(wt), len(nt)
    wt_mean = (
        sum(s["mean_compact_tokens"] for s in wt) / wt_n if wt_n else 0.0
    )
    nt_mean = (
        sum(s["mean_compact_tokens"] for s in nt) / nt_n if nt_n else 0.0
    )
    denom = max(wt_mean, 1e-9)
    rel_delta = abs(wt_mean - nt_mean) / denom if wt_mean > 0 else 0.0
    sufficient = wt_n >= min_sessions and nt_n >= min_sessions
    no_effect = sufficient and rel_delta < min_rel_delta
    return {
        "with_tag_n": wt_n,
        "no_tag_n": nt_n,
        "with_tag_mean_compact_tokens": round(wt_mean, 4),
        "no_tag_mean_compact_tokens": round(nt_mean, 4),
        "relative_delta": round(rel_delta, 6),
        "min_sessions_per_variant": min_sessions,
        "min_relative_delta": min_rel_delta,
        "sufficient_signal": sufficient,
        "no_effect": no_effect,
        "should_disable": no_effect,
    }


def should_auto_disable(
    audit_path: Optional[Path] = None,
    min_sessions: int = MIN_SESSIONS_PER_VARIANT,
    min_rel_delta: float = MIN_RELATIVE_DELTA,
) -> bool:
    """True iff N>=min_sessions per variant AND relative delta < min_rel_delta."""
    try:
        sessions = aggregate_sessions(audit_path)
        verdict = evaluate(sessions, min_sessions, min_rel_delta)
        return bool(verdict.get("should_disable"))
    except Exception:  # noqa: BLE001
        return False


def is_monitor_disabled() -> bool:
    """True if the auto-disable flag file exists (codec consults this)."""
    try:
        return _disable_flag_path().exists()
    except Exception:  # noqa: BLE001
        return False


def _write_disable_flag(verdict: dict) -> None:
    try:
        path = _disable_flag_path()
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            json.dumps({"reason": "no-effect", "verdict": verdict}) + "\n",
            encoding="utf-8",
        )
    except Exception:  # noqa: BLE001
        pass


def _append_rollup(rollup_path: Path, payload: dict) -> None:
    try:
        rollup_path.parent.mkdir(parents=True, exist_ok=True)
        with open(rollup_path, "a", encoding="utf-8") as f:
            f.write(json.dumps(payload, sort_keys=True) + "\n")
    except OSError:
        pass


def run_monitor(
    audit_path: Optional[Path] = None,
    rollup_path: Optional[Path] = None,
    min_sessions: int = MIN_SESSIONS_PER_VARIANT,
    min_rel_delta: float = MIN_RELATIVE_DELTA,
) -> dict:
    """Compute verdict, append to rollup, write disable flag if triggered.

    Returns the verdict dict. Fail-open: any error returns
    {"error": "...", "should_disable": False}.
    """
    try:
        import time
        sessions = aggregate_sessions(audit_path)
        verdict = evaluate(sessions, min_sessions, min_rel_delta)
        verdict["ts"] = time.time()
        verdict["event"] = "ab-rollup"
        _append_rollup(rollup_path or _rollup_path(), verdict)
        if verdict.get("should_disable"):
            _write_disable_flag(verdict)
        return verdict
    except Exception as exc:  # noqa: BLE001
        return {"error": str(exc)[:200], "should_disable": False}


def main(argv: Optional[list[str]] = None) -> int:
    import argparse
    p = argparse.ArgumentParser(
        description="ultracos schema-tag A/B effectiveness monitor (internal-ref)",
    )
    p.add_argument("--audit-file", type=Path, default=None)
    p.add_argument("--rollup-file", type=Path, default=None)
    p.add_argument("--min-sessions", type=int, default=MIN_SESSIONS_PER_VARIANT)
    p.add_argument("--min-rel-delta", type=float, default=MIN_RELATIVE_DELTA)
    p.add_argument("--json", action="store_true")
    args = p.parse_args(argv)
    verdict = run_monitor(
        args.audit_file, args.rollup_file,
        args.min_sessions, args.min_rel_delta,
    )
    if args.json:
        print(json.dumps(verdict, indent=2, sort_keys=True))
    else:
        for k, v in sorted(verdict.items()):
            print(f"{k:32s} {v}")
    return 0


if __name__ == "__main__":
    import sys
    sys.exit(main())
