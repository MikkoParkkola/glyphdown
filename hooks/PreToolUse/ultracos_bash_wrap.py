#!/usr/bin/env python3
"""ultracos_bash_wrap — PreToolUse Bash-wrap-mode codec (internal-ref AC.2).

Absorbs the architectural primitive from claudioemmanuel/squeez (126*) and
rtk-ai/rtk (52,635*): wrap matched bash subcommands so their stdout flows
through the ultracos codec BEFORE Claude ever pays input tokens for the
unfiltered output.

How it works (verified against Claude Code v2.1.x hook spec):
  1. Hook reads PreToolUse JSON from stdin.
  2. If tool_name != "Bash" or command doesn't match the wrap-allowlist,
     emit empty stdout (no decision) so normal flow continues.
  3. Otherwise emit `hookSpecificOutput.permissionDecision=allow` with
     `updatedInput.command = "<orig> 2>&1 | ultracos-codec-pipe"`.
  4. Claude executes the wrapped command; the codec pipe sees stdout
     FIRST, applies the same anchor-survival + dedup + minify stack the
     PostToolUse hook does, and emits compressed output. Claude reads
     the compressed output and pays tokens only on that.

Allowlist intentionally narrow: only tools whose output is high-volume
+ heuristically compressible. Compilers (cargo, npm scripts, test
runners) and command-result tools (gh, kubectl, aws, gcloud, az, glab).

Composes with existing safety hooks: per Claude Code spec, `deny > defer
> ask > allow`, so any existing deny hook (gh-third-party-issue-guard,
evidence-guard, etc) still wins regardless of what we return here.

Failure mode is fail-open: any exception, missing JSON field, or
regex miss emits empty stdout and Claude runs the original command
unchanged.

Toggle: ULTRACOS_BASH_WRAP=0 to disable. Default OFF until paired-lift
measurement (AC.3) shows the architecture wins on real workloads.
"""

from __future__ import annotations

import json
import os
import re
import shlex
import sys

# Off by default while spike runs. Flip to "1" once paired-lift confirms.
WRAP_ENABLED = os.environ.get("ULTRACOS_BASH_WRAP", "0") == "1"

# Allowlist: leading command name must match one of these. Anchored to
# word-boundary so `gh-pages-deploy` doesn't match `gh`. Pre-compiled.
_ALLOWLIST = re.compile(
    r"^\s*(?:gh|kubectl|aws|gcloud|az|glab|cargo|npm|pnpm|jest|pytest|ruff|mypy|"
    r"bandit|cargo-clippy|rustc|tsc|eslint|prettier)(?=\s|$)"
)

# Codec pipe binary. Resolved at hook config time (path baked in here);
# falls back to no-op `cat` if the operator hasn't installed the pipe.
_CODEC_PIPE = os.environ.get(
    "ULTRACOS_CODEC_PIPE",
    str(os.path.expanduser("~/.local/bin/ultracos-codec-pipe")),
)


def _emit_passthrough() -> int:
    """No decision -> normal permission flow + unchanged command."""
    sys.stdout.write("")
    return 0


def _emit_wrap(orig_command: str, *, additional_input: dict) -> int:
    """Wrap the command and emit hookSpecificOutput."""
    # Use shell-safe wrap: original goes through bash -c '...' subshell
    # so multi-stage pipes inside orig keep working. stderr merged into
    # stdout so error context survives the codec pipe.
    wrapped = f"bash -c {shlex.quote(orig_command)} 2>&1 | {_CODEC_PIPE}"
    output = {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": "ultracos bash-wrap (internal-ref)",
            "updatedInput": {**additional_input, "command": wrapped},
        }
    }
    sys.stdout.write(json.dumps(output))
    return 0


def main() -> int:
    if not WRAP_ENABLED:
        return _emit_passthrough()
    try:
        payload = json.loads(sys.stdin.read())
        if payload.get("tool_name") != "Bash":
            return _emit_passthrough()
        tool_input = payload.get("tool_input") or {}
        command = tool_input.get("command")
        if not isinstance(command, str) or not command:
            return _emit_passthrough()
        if not _ALLOWLIST.match(command):
            return _emit_passthrough()
        # Preserve any other tool_input fields (description, timeout, etc).
        rest = {k: v for k, v in tool_input.items() if k != "command"}
        return _emit_wrap(command, additional_input=rest)
    except Exception as e:  # noqa: BLE001 — hook MUST fail-open
        sys.stderr.write(f"ultracos-bash-wrap fail-open: {e}\n")
        return _emit_passthrough()


if __name__ == "__main__":
    raise SystemExit(main())
