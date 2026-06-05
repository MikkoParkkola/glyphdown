#!/usr/bin/env python3
"""Glyphdown-Stats UserPromptSubmit hook.

Detects /glyphdown-stats trigger (including "glyphdown stats", "show glyphdown stats")
and emits aggregated compaction savings: session tokens, lifetime tokens, top-5 tools.

Behavior:
  - If trigger present → read ~/.ultracos/audit.jsonl, aggregate by session_id
    (or last 1h if CLAUDE_SESSION_ID unavailable), emit decision:block with markdown table.
  - If --share flag → emit tweetable one-liner.
  - Pre-renders to ~/.ultracos/statusline-suffix for future statusline integration.
  - If trigger absent → emit `{"continue": true}` (no-op).
  - Any exception → log to stderr, emit `{"continue": true}`, exit 0.
"""

from __future__ import annotations

import json
import os
import sys
import time
from pathlib import Path
from collections import defaultdict

TRIGGERS = (
    "/glyphdown-stats",
    "glyphdown stats",
    "show glyphdown stats",
)


def read_audit_jsonl() -> list[dict]:
    """Read ~/.ultracos/audit.jsonl, return list of compaction records."""
    audit_path = Path.home() / ".ultracos" / "audit.jsonl"
    if not audit_path.exists():
        return []

    records = []
    try:
        with open(audit_path) as f:
            for line in f:
                if line.strip():
                    try:
                        records.append(json.loads(line))
                    except (json.JSONDecodeError, ValueError):
                        pass
    except Exception as e:
        print(f"glyphdown-stats-handler: failed to read audit.jsonl: {e}", file=sys.stderr)

    return records


def aggregate_stats(records: list[dict], session_id: str | None = None) -> dict:
    """Aggregate compaction statistics from audit records.

    Returns: {
        'session_tokens_saved': int,
        'lifetime_tokens_saved': int,
        'estimated_usd': float,
        'top_5_tools': [(tool, tokens), ...],  # descending by tokens
    }
    """
    now = time.time()
    one_hour_ago = now - 3600

    session_records = []
    lifetime_records = records.copy()

    # Filter to session or last 1h
    for rec in records:
        ts = rec.get("ts", 0)
        if session_id:
            if rec.get("session_id") == session_id:
                session_records.append(rec)
        else:
            # Fallback: last 1h
            if ts >= one_hour_ago:
                session_records.append(rec)

    # Aggregate tokens saved
    session_saved = sum(r.get("saved_tokens", 0) for r in session_records)
    lifetime_saved = sum(r.get("saved_tokens", 0) for r in lifetime_records)

    # Top 5 tools by saved_tokens
    tool_totals = defaultdict(int)
    for rec in lifetime_records:
        tool = rec.get("tool", "unknown")
        saved = rec.get("saved_tokens", 0)
        tool_totals[tool] += saved

    top_5 = sorted(tool_totals.items(), key=lambda x: x[1], reverse=True)[:5]

    # Estimate USD: $3/M input
    rate = 3.0 / 1_000_000
    session_usd = session_saved * rate

    return {
        "session_tokens_saved": session_saved,
        "lifetime_tokens_saved": lifetime_saved,
        "estimated_usd": session_usd,
        "top_5_tools": top_5,
    }


def format_table(stats: dict, share: bool = False) -> str:
    """Format stats as markdown table (or one-liner for --share)."""
    if share:
        session = stats["session_tokens_saved"]
        lifetime = stats["lifetime_tokens_saved"]
        usd = stats["estimated_usd"]
        return f"Glyphdown saved {session:,}T this session, {lifetime:,}T lifetime (~${usd:.4f})"

    session = stats["session_tokens_saved"]
    lifetime = stats["lifetime_tokens_saved"]
    usd = stats["estimated_usd"]

    # Format lifetime USD (rough: $3/M = $0.000003 per token)
    lifetime_usd = lifetime * (3.0 / 1_000_000)

    top_5 = stats["top_5_tools"]
    top_5_str = "\n".join(f"  {tool}: {tokens:,}T" for tool, tokens in top_5) if top_5 else "  (none)"

    return f"""\
## Glyphdown Compaction Stats

| Metric | Tokens | Est. USD |
|--------|--------|----------|
| Session | {session:,} | ${usd:.4f} |
| Lifetime | {lifetime:,} | ${lifetime_usd:.4f} |

### Top 5 Tools (Lifetime)
{top_5_str}
"""


def main() -> int:
    try:
        raw = sys.stdin.read()
        try:
            payload = json.loads(raw) if raw.strip() else {}
        except Exception:
            payload = {}

        prompt = (payload.get("prompt") or payload.get("user_prompt") or "").lower()

        # Check for trigger
        if not any(trigger in prompt for trigger in TRIGGERS):
            sys.stdout.write(json.dumps({"continue": True}))
            return 0

        # Parse --share flag
        share = "--share" in prompt

        # Read audit
        records = read_audit_jsonl()
        session_id = os.environ.get("CLAUDE_SESSION_ID")

        # Aggregate
        stats = aggregate_stats(records, session_id)

        # Format
        reason = format_table(stats, share=share)

        # Pre-render to statusline-suffix
        try:
            status_dir = Path.home() / ".ultracos"
            status_dir.mkdir(parents=True, exist_ok=True)
            suffix_path = status_dir / "statusline-suffix"
            suffix_path.write_text(f"[Glyphdown: {stats['lifetime_tokens_saved']:,}T saved]\n")
        except Exception as e:
            print(f"glyphdown-stats-handler: failed to write statusline-suffix: {e}", file=sys.stderr)

        # Emit decision:block so model sees stats but doesn't process original prompt
        sys.stdout.write(json.dumps({
            "decision": "block",
            "reason": reason,
            "continue": True,
        }))
        return 0

    except Exception as e:
        print(f"glyphdown-stats-handler: {e}", file=sys.stderr)

    sys.stdout.write(json.dumps({"continue": True}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
