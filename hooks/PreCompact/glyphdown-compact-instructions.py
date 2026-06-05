#!/usr/bin/env python3
"""Glyphdown PreCompact hook — instruct the compaction summarizer to
emit GLYPHDOWN-MAX dense form, prefixed with a cache-stable preamble.

When Claude Code triggers context compaction (auto-compact at the
55% token threshold, slash-compact explicit), this hook injects a
`newCustomInstructions` block telling the model: produce the
compaction summary in GLYPHDOWN-MAX form starting with a fixed byte
preamble (internal-ref). The byte-stable preamble
lets the next turn cache-hit on the leading tokens at 0.1x rate
instead of paying 1.25x cache_creation.

Override: this hook's `newCustomInstructions` takes precedence over
the default compaction prompt per the Claude Code hook contract.
"""

from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path

# Cache-stable preamble (internal-ref).
# Every compacted summary MUST start with these exact bytes so that
# the next turn's cache_read rate (0.1x) applies to the preamble
# instead of cache_creation rate (1.25x). Drift here invalidates the
# cache project-wide; pinning hash test in tests/test_compact_preamble.py.
PREAMBLE = (
    "## GLYPHDOWN COMPACTED CONTEXT v1\n"
    "### Session-invariants\n"
    "- format: GLYPHDOWN-MAX dense form\n"
    "- schema: goal | state | delta | decisions | files | validation"
    " | blockers | next | restore\n"
    "- preamble-hash: glyphdown-compact-cache-stable-v1\n"
    "- cache-stability: preamble is byte-identical across compactions\n"
    "\n"
    "### Body\n"
)

INSTRUCTIONS = """\
STRUCTURE IS MANDATORY — overrides the default summary template.
Produce the compaction summary in GLYPHDOWN-MAX dense form, NOT prose.

MANDATORY PREAMBLE: your summary MUST begin with the following 8
lines verbatim before any content. Reproduce them byte-for-byte:

## GLYPHDOWN COMPACTED CONTEXT v1
### Session-invariants
- format: GLYPHDOWN-MAX dense form
- schema: goal | state | delta | decisions | files | validation | blockers | next | restore
- preamble-hash: glyphdown-compact-cache-stable-v1
- cache-stability: preamble is byte-identical across compactions

### Body

The preamble is required for prompt-cache prefix stability across
compactions; deviating costs roughly 5K tokens per long session. Do
not reword, reorder, omit lines.

TARGET: 75-85% fewer tokens than the default summary template.
HARD CAP: 6,000 characters total (including preamble).
NEVER drop restart-critical facts to hit the cap — shrink the
prose harder before sacrificing identifiers.

FORMAT:
- key=value, semicolon chains, pipe-separated lists. Fragments over
  sentences.
- Symbols: -> for chain/causes, <- for why/source, :: for tagged
  values, @ for scope, # for id, ok/~/x/? for state.
- Abbreviate aggressively: env, cfg, var, max, min, init, impl,
  req, resp, err, val, perf, sec, auth, db, fn, mod, dep, src,
  dst, tmp, prod, repro, AC, DoD.
- Code refs: path:line / path::symbol. Code bodies → signature
  only unless exact snippet is needed.
- Numbers compact: 5K, 1.2M, $3, 15s, 4/14 pass. Strip filler.
- Stable / cache-friendly: deterministic order, no fresh timestamps
  unless semantically required, no speculation.

ORDER: goal | state | delta | decisions | files | validation |
blockers | next | restore.

PRESERVE EXACT LITERALS (mandatory):
- The newest user instruction and the active goal.
- AC, DoD gates, blockers, the next action.
- File paths, line refs, function names, type names, constants.
- Commands run, test results, error strings, env / config flags.
- Issue / PR / commit IDs, SHAs, dates.
- Decisions and rejections that still constrain future work.
- Touched files, deployed / restarted services.

DROP / COMPRESS FIRST:
- Tool schemas, superseded tool outputs, repeated search listings,
  conversational acks, chatter.
- Resolved errors and failed attempts unless they explain the
  current state.
- Long command output → result line + the unresolved error line
  only.

STRUCTURED TOOL HISTORY rule: turn tool_use / tool_result history
into plain-text facts only. Stale / unavailable mcp__* names are
identifiers, not callable tool refs.

QUALITY BAR: Maximum density, but exact identifiers, errors,
commands remain unmodified.
"""

LAYER2_MAX_LINES = 200

LAYER2_DIRECTIVE_TEMPLATE = """\

MANDATORY LAYER-2 BLOCK (internal-ref):
After the preamble's "### Body" line and before any other body
content, reproduce the following lines verbatim inside a fenced code
block. Do not paraphrase, re-order, or summarize. The bytes are a
real PostToolUse arc-event ledger; treat them as exact data, not as
prose to compact.

```glyphdown-arc-events
{events_block}
```

Continue the body content after the closing fence.
"""


def _sanitize_session_id(sid: str) -> str:
    return re.sub(r"[^A-Za-z0-9_\-]", "_", sid)[:64] or "no-session"


def _glyphdown_data_dir() -> Path:
    env_dir = os.environ.get("GLYPHDOWN_DATA_DIR")
    if env_dir:
        return Path(env_dir).expanduser().resolve()
    return Path.home() / ".ultracos"


def _read_ledger_tail(session_id: str, arc_index: int = 0) -> list[str]:
    try:
        ledger_path = (
            _glyphdown_data_dir() / "arcs"
            / _sanitize_session_id(session_id)
            / f"{int(arc_index)}.jsonl"
        )
        if not ledger_path.is_file():
            return []
        with open(ledger_path, encoding="utf-8") as fh:
            raw = fh.readlines()
        tail = raw[-LAYER2_MAX_LINES:] if len(raw) > LAYER2_MAX_LINES else raw
        out: list[str] = []
        for line in tail:
            line = line.rstrip("\n")
            if not line:
                continue
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue
            ts = str(rec.get("ts_iso", ""))
            summ = str(rec.get("short_summary", ""))
            if ts and summ:
                out.append(f"{ts} {summ}")
        return out
    except Exception:
        return []


def _build_instructions(session_id: str | None) -> str:
    if not session_id:
        return INSTRUCTIONS
    events = _read_ledger_tail(session_id)
    if not events:
        return INSTRUCTIONS
    block = "\n".join(events)
    return INSTRUCTIONS + LAYER2_DIRECTIVE_TEMPLATE.format(events_block=block)


def main() -> int:
    session_id: str | None = None
    try:
        raw = sys.stdin.read()
        if raw:
            payload = json.loads(raw)
            if isinstance(payload, dict):
                sid = payload.get("session_id")
                if isinstance(sid, str) and sid:
                    session_id = sid
    except Exception:
        # Any parse failure → fall back to v0+v1-only output (no
        # Layer-2 injection). Compaction must never block on stdin
        # shape changes.
        session_id = None

    try:
        instructions = _build_instructions(session_id)
    except Exception:
        instructions = INSTRUCTIONS

    try:
        sys.stdout.write(json.dumps({
            "continue": True,
            "newCustomInstructions": instructions,
        }))
    except Exception as e:
        print(f"glyphdown-compact-instructions: {e}", file=sys.stderr)
        sys.stdout.write(json.dumps({"continue": True}))

    return 0


if __name__ == "__main__":
    sys.exit(main())
