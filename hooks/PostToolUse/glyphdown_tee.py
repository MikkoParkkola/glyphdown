#!/usr/bin/env python3
"""glyphdown tee-on-failure raw-payload preservation (internal-ref).

Writes raw payload to local cache BEFORE transform so agent can Read
the tee path if downstream error references missing content (avoids 50K+ token retry cost).

Fail-open on all I/O errors.
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

try:
    from glyphdown_paths import glyphdown_data_dir
except ImportError:
    def glyphdown_data_dir() -> Path:
        if env_dir := os.environ.get("GLYPHDOWN_DATA_DIR"):
            path = Path(env_dir).expanduser().resolve()
            path.mkdir(parents=True, exist_ok=True)
            return path
        path = Path.home() / ".ultracos"
        path.mkdir(parents=True, exist_ok=True)
        return path


def tee_payload(tool_name: str, payload: str, timestamp: float | None = None) -> Path | None:
    """Write raw payload to local cache before transform.

    Args:
        tool_name: Name of tool that produced payload
        payload: Raw text/JSON to preserve
        timestamp: Unix timestamp (defaults to now)

    Returns:
        Path to tee file, or None on I/O failure (fail-open)
    """
    try:
        if timestamp is None:
            timestamp = time.time()

        tee_dir = glyphdown_data_dir() / "tee"
        tee_dir.mkdir(parents=True, exist_ok=True)

        # ISO timestamp + tool name + payload hash
        iso_ts = datetime.fromtimestamp(timestamp, tz=timezone.utc).isoformat(
            timespec="milliseconds"
        ).replace("+00:00", "Z")
        payload_hash = hashlib.sha1(payload.encode("utf-8")).hexdigest()[:8]
        filename = f"{iso_ts}_{tool_name}_{payload_hash}.log"
        tee_path = tee_dir / filename

        # Atomic write: write to temp, rename
        temp_path = tee_dir / f".{filename}.tmp"
        with open(temp_path, "w", encoding="utf-8") as f:
            f.write(payload)
        temp_path.replace(tee_path)

        return tee_path
    except OSError:
        return None  # fail-open: I/O failure must never block hook


def tee_on_failure(
    payload: str,
    *,
    tool_name: str = "",
    error: BaseException | str | None = None,
    timestamp: float | None = None,
) -> Path | None:
    """Tee the raw payload to local cache when codec raises (internal-ref).

    Writes ``~/.ultracos/failed-payloads/<ts>.json`` (or under
    ``GLYPHDOWN_DATA_DIR`` when set) so the failing payload can be replayed
    against the codec for debugging. Fail-open: any I/O error returns None
    without raising — the codec hook contract is "never block the tool call".

    Args:
        payload: Raw stdin / text payload that triggered the codec exception.
        tool_name: Optional originating tool name (recorded in the envelope).
        error: Optional exception or error string (str(error) recorded).
        timestamp: Unix timestamp; defaults to now.

    Returns:
        Path to the written JSON file, or None on I/O failure.
    """
    try:
        if timestamp is None:
            timestamp = time.time()

        fail_dir = glyphdown_data_dir() / "failed-payloads"
        fail_dir.mkdir(parents=True, exist_ok=True)

        # Filename: <unix-ts-ms>.json — monotonic, sortable, collision-resistant
        # under sub-millisecond bursts via short payload-hash suffix.
        ts_ms = int(timestamp * 1000)
        payload_hash = hashlib.sha1(payload.encode("utf-8", errors="replace")).hexdigest()[:8]
        filename = f"{ts_ms}_{payload_hash}.json"
        fail_path = fail_dir / filename

        envelope = {
            "ts": timestamp,
            "tool": tool_name or "",
            "error": str(error) if error is not None else "",
            "payload": payload,
        }

        # Atomic write: temp + rename
        temp_path = fail_dir / f".{filename}.tmp"
        with open(temp_path, "w", encoding="utf-8") as f:
            json.dump(envelope, f, ensure_ascii=False)
        temp_path.replace(fail_path)

        return fail_path
    except OSError:
        return None  # fail-open
    except Exception:  # noqa: BLE001 — codec contract: never raise
        return None


def prune_old_tees(
    max_age_days: int = 7,
    max_total_mb: int = 100,
    invocation_count: int | None = None,
) -> int:
    """Delete oldest tee files when retention exceeded.

    Runs on every Nth invocation (e.g. mod 100) to avoid filesystem spam.

    Args:
        max_age_days: Delete tee files older than this many days
        max_total_mb: Delete oldest until total size < this MB
        invocation_count: Current invocation count; prune every 100th call

    Returns:
        Number of files deleted
    """
    try:
        # Optional: skip pruning unless invocation_count % 100 == 0
        if invocation_count is not None and invocation_count % 100 != 0:
            return 0

        tee_dir = glyphdown_data_dir() / "tee"
        if not tee_dir.exists():
            return 0

        now = time.time()
        max_age_secs = max_age_days * 86400
        max_bytes = max_total_mb * 1024 * 1024
        deleted = 0

        # Collect all .log files with mtime
        files = []
        for path in tee_dir.glob("*.log"):
            try:
                stat = path.stat()
                files.append((stat.st_mtime, stat.st_size, path))
            except OSError:
                pass

        # Sort by mtime (oldest first)
        files.sort()

        # Delete by age
        for mtime, size, path in files:
            if now - mtime > max_age_secs:
                try:
                    path.unlink()
                    deleted += 1
                except OSError:
                    pass

        # Delete by total size
        remaining = files[deleted:] if deleted else files
        total_size = sum(size for _, size, _ in remaining)
        if total_size > max_bytes:
            target_size = int(max_bytes * 0.9)  # Delete until 90% of limit
            for mtime, size, path in remaining:
                if total_size <= target_size:
                    break
                try:
                    path.unlink()
                    total_size -= size
                    deleted += 1
                except OSError:
                    pass

        return deleted
    except OSError:
        return 0  # fail-open
