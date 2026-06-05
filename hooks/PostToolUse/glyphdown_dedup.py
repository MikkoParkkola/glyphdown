#!/usr/bin/env python3
"""glyphdown PostToolUse dedup + summarize for Read/Grep/Glob/Monitor (internal-ref).

A8.1: Hooked into Read, Grep, Glob, Monitor via existing PostToolUse codec.
A8.2: Hash-keyed dedup. FNV-1a (32-bit) on normalized content. Repeat hits
      get full-text replacement with `[seen earlier this session: <ref>]`.
A8.3: Size threshold (default 8KB). Oversize outputs summarized as
      top-40 lines + error lines + tail-10 lines.
A8.4: Fail-open. Any exception in any helper returns the input untouched.

Session state lives at $GLYPHDOWN_STATE_DIR/dedup-<session_id>.json
(default $HOME/.ultracos/dedup-<session_id>.json). Atomic write via
tempfile+os.replace. No file locking — single agent per session.
"""

from __future__ import annotations

import json
import os
import re
import tempfile
from pathlib import Path
from typing import Optional

# Tools whose outputs are eligible for dedup/summarize.
DEDUP_TOOLS = {"Read", "Grep", "Glob", "Monitor"}

# A8.3 default threshold: outputs above this size get summarized.
DEFAULT_SUMMARIZE_BYTES = 8 * 1024

# Summarizer head/tail line caps.
SUMMARIZE_HEAD_LINES = 40
SUMMARIZE_TAIL_LINES = 10
SUMMARIZE_MAX_ERROR_LINES = 20

# Error-pattern heuristics for A8.3 preservation.
_ERROR_RE = re.compile(
    r"(?i)\b(error|warning|warn|fatal|exception|traceback|panic|failed|fail)\b"
)
_ANSI_RE = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07")
_WS_COLLAPSE_RE = re.compile(r"[ \t]+")

# internal-ref: log-line dedup via normalize-before-hash. Volatile fields (timestamps,
# PIDs) get replaced with sentinels so that the same logical log line — emitted at
# different wall-clock times or from different processes — hashes to the same key.
# Sentinels (rather than empty replacement) prevent two adjacent fields from
# silently collapsing into one (which would be a normalization collision risk).
_ISO_TS_RE = re.compile(
    r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?(?:Z|[+-]\d{2}:?\d{2})?"
)
_SYSLOG_TS_RE = re.compile(
    r"\b(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\s+\d{1,2}"
    r"\s+\d{2}:\d{2}:\d{2}(?:[.,]\d+)?\b"
)
_BRACKET_TIME_RE = re.compile(r"\[\d{2}:\d{2}:\d{2}(?:[.,]\d+)?\]")
_EPOCH_MS_RE = re.compile(r"\b1[5-9]\d{8}\d{3}\b|\b2[0-3]\d{8}\d{3}\b")
_PID_KV_RE = re.compile(r"\bpid[=:]\s*\d+\b", re.IGNORECASE)
_PID_BRACKET_RE = re.compile(r"\[pid[:\s]*\d+\]", re.IGNORECASE)
_PROCESS_BRACKET_RE = re.compile(r"\[\d{3,7}\]")

# internal-ref G16: cross-platform path resolution. Best-effort import — fail-open.
try:
    import glyphdown_paths as _paths  # noqa: F401
    _PATHS_AVAILABLE = True
except Exception:  # noqa: BLE001
    _paths = None  # type: ignore
    _PATHS_AVAILABLE = False


def _state_dir() -> Path:
    """Resolve dedup state directory. Tests override via GLYPHDOWN_STATE_DIR."""
    override = os.environ.get("GLYPHDOWN_STATE_DIR", "").strip()
    if override:
        return Path(override)
    if _PATHS_AVAILABLE:
        try:
            return _paths.glyphdown_data_dir()  # type: ignore
        except Exception:  # noqa: BLE001 — fail-open to legacy layout
            pass
    return Path.home() / ".ultracos"


def _state_path(session_id: str) -> Path:
    safe = re.sub(r"[^A-Za-z0-9_-]", "_", session_id)[:128] or "default"
    return _state_dir() / f"dedup-{safe}.json"


def _normalize(text: str) -> str:
    """Normalize for hashing: strip ANSI, LF-normalize, collapse runs of WS.

    internal-ref: also strip volatile log-line fields (timestamps, PIDs) so the
    same logical line — emitted at different times or from different processes —
    hashes to the same dedup key. Volatile fields collapse to sentinels
    (`<TS>`, `<PID>`) rather than empty strings to avoid two distinct adjacent
    fields silently merging into one.
    """
    text = _ANSI_RE.sub("", text)
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    # Volatile-field stripping BEFORE whitespace collapse so sentinels remain
    # word-bounded for the negative-case test.
    text = _ISO_TS_RE.sub("<TS>", text)
    text = _SYSLOG_TS_RE.sub("<TS>", text)
    text = _BRACKET_TIME_RE.sub("<TS>", text)
    text = _EPOCH_MS_RE.sub("<TS>", text)
    text = _PID_KV_RE.sub("pid=<PID>", text)
    text = _PID_BRACKET_RE.sub("[pid=<PID>]", text)
    text = _PROCESS_BRACKET_RE.sub("[<PID>]", text)
    text = _WS_COLLAPSE_RE.sub(" ", text)
    return text.strip()


def fnv1a_32(data: str) -> str:
    """FNV-1a 32-bit hash. offset basis 0x811c9dc5, prime 0x01000193."""
    h = 0x811C9DC5
    prime = 0x01000193
    for byte in data.encode("utf-8", errors="replace"):
        h ^= byte
        h = (h * prime) & 0xFFFFFFFF
    return f"{h:08x}"


def _load_state(session_id: str) -> dict:
    path = _state_path(session_id)
    try:
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
        if not isinstance(data, dict):
            return {"seen": {}, "counters": {}}
        data.setdefault("seen", {})
        data.setdefault("counters", {})
        return data
    except (OSError, json.JSONDecodeError, ValueError):
        return {"seen": {}, "counters": {}}


def _save_state(session_id: str, state: dict) -> None:
    """Atomic write: tmp file then os.replace. Fail-open on any I/O error."""
    path = _state_path(session_id)
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        # tempfile in same dir guarantees same-filesystem replace.
        fd, tmp = tempfile.mkstemp(
            prefix=path.name + ".", dir=str(path.parent)
        )
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                json.dump(state, f, separators=(",", ":"))
            os.replace(tmp, path)
        except Exception:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise
    except OSError:
        pass  # fail-open


def summarize_large(text: str) -> tuple[str, bool]:
    """A8.3: collapse to head + error lines + tail. Returns (out, changed)."""
    lines = text.splitlines()
    if len(lines) <= SUMMARIZE_HEAD_LINES + SUMMARIZE_TAIL_LINES + 5:
        return text, False

    head = lines[:SUMMARIZE_HEAD_LINES]
    tail = lines[-SUMMARIZE_TAIL_LINES:]
    head_set = set(range(SUMMARIZE_HEAD_LINES))
    tail_set = set(range(len(lines) - SUMMARIZE_TAIL_LINES, len(lines)))
    middle_errors: list[tuple[int, str]] = []
    for idx in range(SUMMARIZE_HEAD_LINES, len(lines) - SUMMARIZE_TAIL_LINES):
        if _ERROR_RE.search(lines[idx]):
            middle_errors.append((idx, lines[idx]))
            if len(middle_errors) >= SUMMARIZE_MAX_ERROR_LINES:
                break

    pieces: list[str] = []
    pieces.extend(head)
    hidden_lines = len(lines) - len(head) - len(tail) - len(middle_errors)
    if middle_errors:
        pieces.append(
            f"[glyphdown:summarize-v1 kept={len(middle_errors)} error-lines"
            f" hidden={hidden_lines} lines]"
        )
        for idx, ln in middle_errors:
            pieces.append(f"L{idx + 1}: {ln}")
    else:
        pieces.append(
            f"[glyphdown:summarize-v1 hidden={hidden_lines} lines, no error pattern]"
        )
    pieces.extend(tail)
    out = "\n".join(pieces)
    # Discard `head_set`/`tail_set` (kept for future scoring if needed).
    del head_set, tail_set
    return out, True


def maybe_dedup_or_summarize(
    tool_name: str,
    text: str,
    session_id: str,
    *,
    summarize_bytes: int = DEFAULT_SUMMARIZE_BYTES,
) -> Optional[tuple[str, str]]:
    """Apply A8.1..A8.3 to a single text item.

    Returns (new_text, mode) where mode is "dedup", "summarize", or None
    (no rewrite). Caller stays untouched on None.

    A8.4: fail-open. Any exception caught here returns None.
    """
    try:
        if tool_name not in DEDUP_TOOLS or not isinstance(text, str) or not text:
            return None

        state = _load_state(session_id)
        norm = _normalize(text)
        if not norm:
            return None
        digest = fnv1a_32(norm)
        seen = state.get("seen", {})
        counters = state.get("counters", {})

        prior = seen.get(digest)
        if isinstance(prior, dict) and "ref" in prior:
            # A8.2: dedup hit — return reference placeholder (full replacement).
            return (
                f"[seen earlier this session: {prior['ref']}]",
                "dedup",
            )

        # First occurrence: assign a ref like Read#3.
        next_n = int(counters.get(tool_name, 0)) + 1
        counters[tool_name] = next_n
        ref = f"{tool_name}#{next_n}"
        seen[digest] = {"ref": ref, "bytes": len(text)}
        state["seen"] = seen
        state["counters"] = counters
        _save_state(session_id, state)

        # A8.3: if oversize, summarize.
        if len(text.encode("utf-8", errors="replace")) > summarize_bytes:
            out, changed = summarize_large(text)
            if changed:
                tag = (
                    f"[glyphdown:dedup-ref ref={ref}]\n"
                )
                return tag + out, "summarize"
        return None
    except Exception:  # noqa: BLE001 — A8.4 fail-open
        return None
