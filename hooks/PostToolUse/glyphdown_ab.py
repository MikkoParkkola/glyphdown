#!/usr/bin/env python3
"""glyphdown SIL-3 schema-tag A/B effectiveness monitor (internal-ref).

The codec emits `[glyphdown:compact-v1 ...]` on every compacted payload to
prevent rtk#582 verbose-compensation regression. We don't know if the model
in front of any given operator actually responds to it. This module decides
whether a given session should get the tag (90%) or have it stripped (10%
shadow) so downstream audit analysis can measure output-token deltas.

Determinism: variant is a stable hash of session_id, NOT random — same
session always gets the same variant, so model behavior is consistent
within a session.

Fail-open: every public function returns the safe ("with-tag") variant on
any error; record_variant never raises.
"""

from __future__ import annotations

import hashlib
import os
from typing import Optional


VARIANT_WITH_TAG = "with-tag"
VARIANT_NO_TAG = "no-tag"

# Default 10% no-tag samples. Tunable per-deployment.
DEFAULT_NO_TAG_RATE = 0.10

# Stable hash salt — bumping this re-buckets all sessions.
_HASH_SALT = "glyphdown-ab-v1"


def _no_tag_rate() -> float:
    """Read no-tag rate from env or fall back to default. Clamped [0.0, 1.0]."""
    raw = os.environ.get("GLYPHDOWN_AB_NO_TAG_RATE")
    if raw is None or raw == "":
        return DEFAULT_NO_TAG_RATE
    try:
        rate = float(raw)
    except (TypeError, ValueError):
        return DEFAULT_NO_TAG_RATE
    if rate < 0.0:
        return 0.0
    if rate > 1.0:
        return 1.0
    return rate


def _ab_disabled() -> bool:
    """GLYPHDOWN_AB_DISABLE=1 forces with-tag always."""
    raw = os.environ.get("GLYPHDOWN_AB_DISABLE", "").strip().lower()
    return raw in ("1", "true", "yes", "on")


def _monitor_auto_disabled() -> bool:
    """True if internal-ref monitor wrote an auto-disable flag (proven no effect).

    Consulted AFTER GLYPHDOWN_AB_DISABLE. When set, default to no-tag.
    Fail-open: any error → False (stay in experiment).
    """
    try:
        import glyphdown_ab_monitor as _mon  # local import; fail-open if absent
        return _mon.is_monitor_disabled()
    except Exception:  # noqa: BLE001
        return False


def decide_variant(session_id: Optional[str], seed: Optional[str] = None) -> str:
    """Return "with-tag" or "no-tag" deterministically from session_id.

    Hash bucket: blake2b(salt + seed + session_id) mod 10000 < rate * 10000.
    Same session_id always yields the same variant. Empty/None session_id
    or any exception → fail-open to "with-tag".

    Consultation order:
    1. GLYPHDOWN_AB_DISABLE=1  → force with-tag (operator kill switch)
    2. monitor auto-disable   → force no-tag (experiment concluded "no effect")
    3. deterministic hash bucket on session_id
    """
    try:
        if _ab_disabled():
            return VARIANT_WITH_TAG
        if _monitor_auto_disabled():
            return VARIANT_NO_TAG
        if not session_id:
            return VARIANT_WITH_TAG
        key = f"{_HASH_SALT}|{seed or ''}|{session_id}".encode("utf-8")
        digest = hashlib.blake2b(key, digest_size=8).digest()
        bucket = int.from_bytes(digest, "big") % 10000
        threshold = int(_no_tag_rate() * 10000)
        return VARIANT_NO_TAG if bucket < threshold else VARIANT_WITH_TAG
    except Exception:  # noqa: BLE001 — fail-open
        return VARIANT_WITH_TAG


def record_variant(audit_row: dict, variant: str) -> dict:
    """Add `variant` field to an audit row. Mutates and returns the dict.

    Fail-open: if audit_row is not a dict, return it untouched.
    """
    try:
        if isinstance(audit_row, dict):
            audit_row["variant"] = variant
    except Exception:  # noqa: BLE001
        pass
    return audit_row


def strip_tag_prefix(output: str, tag_prefix: str) -> str:
    """Remove the schema-tag line from `output` if it starts with tag_prefix.

    Strips everything up through (and including) the first newline. Returns
    the input unchanged if it doesn't start with tag_prefix.
    """
    try:
        if not isinstance(output, str) or not output.startswith(tag_prefix):
            return output
        nl = output.find("\n")
        if nl < 0:
            # No newline — entire string is the tag line; return empty body
            return ""
        return output[nl + 1:]
    except Exception:  # noqa: BLE001
        return output
