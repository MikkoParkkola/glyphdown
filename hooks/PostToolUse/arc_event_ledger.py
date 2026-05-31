#!/usr/bin/env python3
"""ultracos arc-event ledger writer — PostToolUse append-only log
(internal-ref Layer-2 substrate).

Records one line per tool invocation into an append-only JSONL ledger
keyed by (session_id, arc_index). The PreCompact hook will inject the
ledger verbatim into compaction directives so the model reproduces
real tool history byte-stably instead of paraphrasing it (cache hit
on the structural-history region; zero model-output cost; zero
hallucination on file paths / commands).

Contract (LOAD-BEARING):
- APPEND-ONLY. Existing lines must never be re-ordered, edited, or
  truncated; the file grows monotonically. The cache discipline
  depends on the prefix being byte-identical across compactions; any
  in-place rewrite breaks every cache hit on subsequent turns.
- DETERMINISTIC `short_summary`. Built by a tool-specific formatter
  (NOT the model). Same inputs always emit same bytes.
- ARGS HASHED, not echoed. The `key_args_hash` field captures
  argument identity for diffing; the rendered `short_summary` is the
  formatter's job to redact / shorten. Secrets-shaped args (api keys,
  tokens) MUST NOT land in `short_summary`.
- FAIL-OPEN. Any I/O / encoding / lock-contention error → silently
  return without blocking the tool. A missing ledger entry is a
  cache miss next compaction, never a tool failure.

Storage: ULTRACOS_DATA_DIR / arcs / <session_id> / <arc_index>.jsonl
fcntl-locked append (same discipline as restart_batcher).
"""
from __future__ import annotations

import hashlib
import json
import os
import re
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

# fcntl is POSIX-only; on Windows we fall back to best-effort append
# without an advisory lock. Append-only `open(..., "a")` is itself
# atomic for small writes (<PIPE_BUF) on most filesystems, which the
# ledger lines comfortably satisfy.
try:
    import fcntl  # type: ignore[import-not-found]
    _HAS_FCNTL = True
except ImportError:
    fcntl = None  # type: ignore[assignment]
    _HAS_FCNTL = False

try:
    from ultracos_paths import ultracos_data_dir
except ImportError:  # tests / standalone runs
    def ultracos_data_dir() -> Path:
        if env_dir := os.environ.get("ULTRACOS_DATA_DIR"):
            p = Path(env_dir).expanduser().resolve()
            p.mkdir(parents=True, exist_ok=True)
            return p
        p = Path.home() / ".ultracos"
        p.mkdir(parents=True, exist_ok=True)
        return p


SHORT_SUMMARY_MAX = 80
# Conservative secret-shaped pattern: long alphanumeric runs that
# look like keys/tokens. The formatter still owns redaction; this is
# the last-line defence inside `_short_summary` itself.
_SECRET_RX = re.compile(r"\b[A-Za-z0-9_\-]{32,}\b")


def _iso_now() -> str:
    return datetime.now(tz=timezone.utc).isoformat(timespec="seconds").replace(
        "+00:00", "Z"
    )


def _hash_args(tool_input: Any) -> str:
    """Deterministic 12-char hex of canonical-JSON tool_input.

    Same input dict → same hash (sort_keys + compact separators).
    """
    try:
        canonical = json.dumps(
            tool_input, sort_keys=True, separators=(",", ":"), ensure_ascii=False
        )
    except (TypeError, ValueError):
        canonical = repr(tool_input)
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()[:12]


def _redact(s: str) -> str:
    return _SECRET_RX.sub("<redacted>", s)


def _short_summary(tool_name: str, tool_input: Any) -> str:
    """Deterministic ≤80-char render. NOT an LLM call.

    Per-tool formatters cover the common cases; the default falls back
    to `<tool> <hashed-args>`. Output is redacted as a defence in
    depth even after the per-tool formatter ran.
    """
    if not isinstance(tool_input, dict):
        body = f"{tool_name} {_hash_args(tool_input)}"
    elif tool_name in {"Read", "Edit", "Write"}:
        p = str(tool_input.get("file_path", "")) or "<no-path>"
        body = f"{tool_name} {p}"
    elif tool_name == "Bash":
        cmd = str(tool_input.get("command", "")).split("\n", 1)[0]
        body = f"Bash {cmd}"
    elif tool_name == "Grep":
        body = f"Grep {tool_input.get('pattern', '')!s}"
    elif tool_name == "Glob":
        body = f"Glob {tool_input.get('pattern', '')!s}"
    else:
        body = f"{tool_name} {_hash_args(tool_input)}"
    body = _redact(body)
    if len(body) > SHORT_SUMMARY_MAX:
        body = body[: SHORT_SUMMARY_MAX - 1] + "…"
    return body


def _ledger_path(session_id: str, arc_index: int) -> Path:
    safe_sid = re.sub(r"[^A-Za-z0-9_\-]", "_", session_id)[:64] or "unknown"
    d = ultracos_data_dir() / "arcs" / safe_sid
    d.mkdir(parents=True, exist_ok=True)
    return d / f"{int(arc_index)}.jsonl"


def append_event(
    session_id: str,
    arc_index: int,
    tool_name: str,
    tool_input: Any,
    *,
    ts_iso: str | None = None,
) -> Path | None:
    """Append one event line. Returns the ledger path, or None on
    fail-open. Caller does not need to check the return value.
    """
    try:
        rec = {
            "ts_iso": ts_iso or _iso_now(),
            "tool": tool_name,
            "key_args_hash": _hash_args(tool_input),
            "short_summary": _short_summary(tool_name, tool_input),
        }
        line = json.dumps(rec, ensure_ascii=False, sort_keys=True) + "\n"
        path = _ledger_path(session_id, arc_index)
        # flock + flush is sufficient for the append-only contract;
        # fsync per write would block the tool path on a hot stream.
        # The contract treats a crash-lost trailing line as a cache
        # miss on the next compaction, not a correctness failure.
        with open(path, "a", encoding="utf-8") as fh:
            if _HAS_FCNTL:
                fcntl.flock(fh.fileno(), fcntl.LOCK_EX)
            try:
                fh.write(line)
                fh.flush()
            finally:
                if _HAS_FCNTL:
                    fcntl.flock(fh.fileno(), fcntl.LOCK_UN)
        return path
    except Exception:
        return None  # fail-open by contract


def _hook_main(stdin_data: dict) -> dict:
    """PostToolUse hook entrypoint. Reads payload, appends one line,
    always returns `{"continue": true}` (fail-open by contract).
    """
    session_id = str(stdin_data.get("session_id", "")) or "no-session"
    # arc_index sourcing is a separate concern (SessionStart-bounded
    # counter); until that ships we key on a per-session stash file
    # that defaults to 0. A future arc-boundary detector writes
    # increments into the same stash.
    arc_index = int(stdin_data.get("arc_index", 0) or 0)
    tool_name = str(stdin_data.get("tool_name", "") or "unknown")
    tool_input = stdin_data.get("tool_input", {})
    append_event(session_id, arc_index, tool_name, tool_input)
    return {"continue": True}


def main(argv: list[str] | None = None) -> int:
    try:
        payload = json.load(sys.stdin)
    except Exception:
        print(json.dumps({"continue": True}))
        return 0
    if not isinstance(payload, dict):
        print(json.dumps({"continue": True}))
        return 0
    print(json.dumps(_hook_main(payload)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
