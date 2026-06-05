#!/usr/bin/env python3
"""Glyphdown-Mode UserPromptSubmit hook.

Detects when the user's prompt activates GLYPHDOWN dual-mode (dense
internal reasoning + plain external output). When triggered, injects
an additionalContext block that the model treats as a system
directive for the rest of the turn.

Triggers (case-insensitive, substring match):
  - "use glyphdown"
  - "glyphdown mode"
  - "glyphdown thinking"
  - "compact thinking"
  - "dense reasoning"
  - "think compact"
  - "reasoning in glyphdown"
  - "internal glyphdown"
  - "/!:glyphdown-mode"

Behavior:
  - If trigger present → output JSON with hookSpecificOutput
    .additionalContext containing the dual-register directive.
  - If trigger absent → emit `{"continue": true}` (no-op).
  - Any exception → log to stderr, emit `{"continue": true}`,
    exit 0. Never block user input.
"""

from __future__ import annotations

import json
import sys

TRIGGERS = (
    "use glyphdown",
    "glyphdown mode",
    "glyphdown thinking",
    "compact thinking",
    "dense reasoning",
    "think compact",
    "reasoning in glyphdown",
    "internal glyphdown",
    "/!:glyphdown-mode",
    "stop glyphdown",  # negative trigger; suppresses
)

DIRECTIVE = """\
# Glyphdown Dual-Mode Active

For this turn (and the rest of the conversation until explicitly
disabled), split your token economy into two registers:

INTERNAL register — chain-of-thought, scratchpads, tool-call
reasoning, intermediate plans, self-critique, debugging notes:
use GLYPHDOWN-MAX dense form.

  VOC: ASCII 1-tok arrows: -> : - @ # = | (avoid 3-tok unicode like
       triangle, identical, forall, exists, checkmark)
  STATE: ok healthy | ~ degraded | x failed | ? unknown
  PRE: V=verify A=analyze I=impl R=read W=write F=fix E=error
  ABBR: env=environment cfg=config var=variable max=maximum min=minimum
  GRAM: -> chain a->b->c | state name(ok|~|x) | subj: action | k=v lines

  Example internal: `R:file -> locate:fn -> I:patch edge_case @user_req`

EXTERNAL register — the final message the human reads, commit
messages, code blocks, file paths, tables shown to user, error
messages quoted to user: normal human-readable prose. Full
sentences. No symbols unless they improve clarity (markdown
tables, code blocks, paths).

  Example external: "Patched `foo` at line 42. The edge case now
  returns `None` instead of panicking."

Boundary:
- If unsure which register a piece of content belongs to, default
  to EXTERNAL when the user will read it directly.
- Code, file paths, commands STAY in their natural form regardless
  of register — never compress what's executable.
- Numbers, identifiers, file:line refs STAY exact.

This directive overrides the default verbose chain-of-thought
style for internal reasoning only. The user-visible output remains
unchanged in quality and clarity.
"""

STOP_DIRECTIVE = """\
# Glyphdown Dual-Mode DEACTIVATED

For this turn forward, use normal prose in both internal reasoning
and user-visible output. Disregard any prior Glyphdown dual-register
directive.
"""


def main() -> int:
    try:
        raw = sys.stdin.read()
        try:
            payload = json.loads(raw) if raw.strip() else {}
        except Exception:
            payload = {}
        prompt = (payload.get("prompt") or payload.get("user_prompt") or "").lower()

        if "stop glyphdown" in prompt:
            sys.stdout.write(json.dumps({
                "continue": True,
                "hookSpecificOutput": {
                    "hookEventName": "UserPromptSubmit",
                    "additionalContext": STOP_DIRECTIVE,
                },
            }))
            return 0

        if any(trigger in prompt for trigger in TRIGGERS):
            sys.stdout.write(json.dumps({
                "continue": True,
                "hookSpecificOutput": {
                    "hookEventName": "UserPromptSubmit",
                    "additionalContext": DIRECTIVE,
                },
            }))
            return 0
    except Exception as e:
        print(f"glyphdown-mode-detector: {e}", file=sys.stderr)

    sys.stdout.write(json.dumps({"continue": True}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
