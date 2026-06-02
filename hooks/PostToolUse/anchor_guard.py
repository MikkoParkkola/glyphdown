"""anchor_guard — preservation-floor guard for aggressive compressions.

internal-ref. Absorbs claudioemmanuel/squeez's `RISK_PRESERVATION_FLOOR` primitive
(see squeez teardown verdict on Linear internal-ref): when codec output drops
>= REDUCTION_THRESHOLD of original tokens, verify that high-value "anchor"
strings (file paths, error markers, test verdicts) survive intact at
>= PRESERVATION_FLOOR. If not, the aggressive transform is reverted.

Rationale:
  Our codec's reduction pipeline is high-recall (LZW/dedup/factor) and the
  break-even policy + AB monitor catch *token-level* misses, but neither
  catches the case where a 95% compression silently dropped the one
  file:line reference that made the original output useful. squeez ships
  a hardcoded anchor-survival check that has been proving its weight on
  126-star real-world Claude Code workloads.

Why a separate module (vs inlining in ultracos_codec.py):
  - 48 KB codec.py is already dense; adding regex tables hurts readability.
  - Anchor patterns are domain-specific (compilers, test runners, panics)
    and benefit from isolated tests.
  - Future absorption of squeez's other preservation primitives lands here.

Public API:
  - extract_anchors(text) -> set[str]      anchors found in text
  - survival_ratio(orig, compact) -> float fraction of orig anchors retained
  - should_revert(orig, compact, *, reduction_threshold, preservation_floor)
        -> bool                             True iff caller must drop compression

Defaults match squeez: REDUCTION_THRESHOLD=0.90, PRESERVATION_FLOOR=0.70.

V (verified):
  - file:line regex matches `path/to/file.py:NN`, `src/main.rs:42:7`
  - error[E\\d+] matches `error[E0308]`, `error[E0432]: unresolved import`
  - TS\\d+ matches `TS2345: argument of type 'string' is not assignable`
  - panic/FAIL token matches `thread 'main' panicked`, `FAIL src/lib.rs`
  - "N passed/failed" matches `12 passed, 3 failed`
I (inferred):
  - 0.70 floor will hold on our PostToolUse corpus once internal-ref lands.
    Below floor empirically rare on existing eval rows (0/12 in spot check).
A (assumption):
  - Anchor set is monotone w.r.t. compression — i.e. compressed output
    cannot introduce *new* anchors that weren't in the original. True for
    all current codec transforms (minify/dedup/factor/truncate).
"""

from __future__ import annotations

import re
from typing import Iterable

# Tuned to match claudioemmanuel/squeez/src/economy/preservation.rs defaults.
# Bump REDUCTION_THRESHOLD higher to widen the guard's blast radius;
# lower PRESERVATION_FLOOR to accept more aggressive compression.
DEFAULT_REDUCTION_THRESHOLD = 0.90
DEFAULT_PRESERVATION_FLOOR = 0.70

# Anchor regexes. Each captures the *whole* anchor as group 0 so the
# extracted strings can be compared verbatim. Ordered by frequency
# (file:line is the single most common anchor in agent output).
_ANCHOR_PATTERNS: tuple[re.Pattern[str], ...] = (
    # file:line  e.g. `src/main.rs:42`, `lib/foo.py:128:8`, `./tests/a.ts:7`
    re.compile(r"(?:[A-Za-z0-9_./\-]+/)?[A-Za-z0-9_\-]+\.[A-Za-z0-9]{1,5}:\d+(?::\d+)?"),
    # Rust compiler errors:  error[E0308]
    re.compile(r"error\[E\d{3,5}\]"),
    # TypeScript / JavaScript compiler errors:  TS2345
    re.compile(r"\bTS\d{3,5}\b"),
    # Test verdicts:  `12 passed`, `3 failed`, `1 error`, `2 skipped`
    re.compile(r"\b\d+\s+(?:passed|failed|errored?|skipped)\b", re.IGNORECASE),
    # Bare UPPERCASE per-test verdicts:  pytest `test_x FAILED`, cargo
    # `test foo ... FAILED`. Uppercase only — `passed`/`ok` in prose are too
    # noisy, but ALL-CAPS verdicts are near-exclusive to test output, so this
    # catches the mid-line per-test verdict the counted pattern misses (it
    # requires a leading number) without backfiring on prose.
    re.compile(r"\b(?:PASSED|FAILED|ERRORED?|SKIPPED)\b"),
    # panic / FAIL / FATAL markers (line-anchored; FAIL inside random prose
    # is too noisy to count as an anchor).
    re.compile(r"^(?:thread\s+'\w+'\s+panicked|FAIL(?:URE)?\b|FATAL\b|PANIC\b)",
               re.MULTILINE),
    # exit/abort codes:  `exit status 1`, `signal 11`, `Killed (SIGKILL)`
    re.compile(r"\b(?:exit\s+status|signal|SIG(?:KILL|TERM|SEGV|ABRT))\s+\d+\b",
               re.IGNORECASE),
)


def extract_anchors(text: str) -> set[str]:
    """Extract distinct anchor strings from `text`.

    Returns a set so duplicate anchors in the original don't inflate the
    denominator of survival_ratio — a single surviving file:line still
    counts as 1/1, not 1/N where N is duplicate count.
    """
    anchors: set[str] = set()
    for pat in _ANCHOR_PATTERNS:
        for match in pat.finditer(text):
            anchors.add(match.group(0))
    return anchors


def survival_ratio(original: str, compact: str) -> float:
    """Fraction of `original` anchors that still appear verbatim in `compact`.

    Returns 1.0 when `original` has zero anchors (vacuous truth — no
    preservation duty when there was nothing to preserve). This is the
    correct behavior for prose payloads (no file refs, no errors): they
    should never trigger a revert.
    """
    orig_anchors = extract_anchors(original)
    if not orig_anchors:
        return 1.0
    compact_anchors = extract_anchors(compact)
    surviving = orig_anchors & compact_anchors
    return len(surviving) / len(orig_anchors)


def should_revert(
    original: str,
    compact: str,
    *,
    reduction_threshold: float = DEFAULT_REDUCTION_THRESHOLD,
    preservation_floor: float = DEFAULT_PRESERVATION_FLOOR,
) -> tuple[bool, float, float]:
    """Decide whether codec output should be reverted to original.

    Returns (revert, reduction_ratio, survival).

    Guard fires iff BOTH:
      - reduction_ratio >= reduction_threshold (compression was aggressive)
      - survival < preservation_floor          (anchors dropped below floor)

    The AND-gate is critical: small compressions never revert (the bytes
    they saved aren't worth a preservation check), and high-survival
    compressions never revert (the anchors are still there).

    Lengths use raw len() not estimate_tokens — preservation is a
    character-level structural check, not a billing-level one, and we
    want the cheap O(n) call here (this runs on every codec hit).
    """
    orig_len = len(original)
    if orig_len == 0:
        return False, 0.0, 1.0
    reduction = 1.0 - (len(compact) / orig_len)
    if reduction < reduction_threshold:
        return False, reduction, 1.0  # not aggressive enough to check
    survival = survival_ratio(original, compact)
    return survival < preservation_floor, reduction, survival


def _iter_anchor_patterns() -> Iterable[re.Pattern[str]]:
    """Test/debug hook: expose the compiled patterns for inspection."""
    return _ANCHOR_PATTERNS
