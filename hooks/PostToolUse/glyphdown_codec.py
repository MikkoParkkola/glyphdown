#!/usr/bin/env python3
"""glyphdown PostToolUse tool-result codec.

Implements P0 regression-immunity bundle (internal-ref):
- A1: ANSI strip + JSON minify + blank-line collapse
- A3: Trailing single marker on truncation (never inline)
- A4: Language::Data short-circuit guard (skip code regex on JSON/YAML/TOML)
- A9: Schema-tag prefix on every compressed payload (kills rtk#582 class)
- A10: Break-even guard (skip compaction when estimated savings < threshold)

Fail-open: any exception emits {"continue": true} and exits 0.
"""

from __future__ import annotations

import json
import os
import re
import sys
import time
from dataclasses import dataclass
from pathlib import Path

# SIL-3 A/B effectiveness monitor (internal-ref). Import is local — fail-open if
# the module is missing so codec degrades gracefully to with-tag always.
try:
    import glyphdown_ab as _ab  # noqa: F401
    _AB_AVAILABLE = True
except Exception:  # noqa: BLE001
    _AB_AVAILABLE = False

# SIL-2 (internal-ref): per-tool learned policy. Best-effort import — fail-open.
try:
    import glyphdown_policy as _policy  # noqa: F401
    _POLICY_AVAILABLE = True
except Exception:  # noqa: BLE001
    _policy = None  # type: ignore
    _POLICY_AVAILABLE = False

# internal-ref: PostToolUse dedup + summarize for Read/Grep/Glob/Monitor. Best-effort
# import — fail-open so codec degrades to compaction-only when module missing.
try:
    import glyphdown_dedup as _dedup  # noqa: F401
    _DEDUP_AVAILABLE = True
except Exception:  # noqa: BLE001
    _dedup = None  # type: ignore
    _DEDUP_AVAILABLE = False

# internal-ref: tee-on-failure raw-payload preservation. Best-effort import — fail-open.
try:
    import glyphdown_tee as _tee  # noqa: F401
    _TEE_AVAILABLE = True
except Exception:  # noqa: BLE001
    _tee = None  # type: ignore
    _TEE_AVAILABLE = False

# internal-ref G16: cross-platform path resolution. Best-effort import — fail-open
# back to the historical Path.home() / ".ultracos" layout if the helper is
# unavailable, so the codec still works in test/dev contexts where
# glyphdown_paths cannot be imported.
try:
    import glyphdown_paths as _paths  # noqa: F401
    _PATHS_AVAILABLE = True
except Exception:  # noqa: BLE001
    _paths = None  # type: ignore
    _PATHS_AVAILABLE = False

# internal-ref SIL-5: cache-aware compression — back off on cache-hot prefixes so
# Anthropic's native prompt cache stays warm. Best-effort import — fail-open;
# absence reverts to the historical always-compress behaviour.
try:
    import glyphdown_cache as _cache  # noqa: F401
    _CACHE_AVAILABLE = True
except Exception:  # noqa: BLE001
    _cache = None  # type: ignore
    _CACHE_AVAILABLE = False


def _audit_dir() -> Path:
    """Resolve audit dir each call so HOME / env changes (tests) are honored."""
    if _PATHS_AVAILABLE:
        try:
            return _paths.glyphdown_data_dir()  # type: ignore
        except Exception:  # noqa: BLE001 — fail-open to legacy layout
            pass
    return Path.home() / ".ultracos"


def _audit_file() -> Path:
    if _PATHS_AVAILABLE:
        try:
            return _paths.audit_file()  # type: ignore
        except Exception:  # noqa: BLE001
            pass
    return _audit_dir() / "audit.jsonl"


def _int_env(name: str, default: int) -> int:
    raw = os.environ.get(name)
    if raw is None or raw == "":
        return default
    try:
        return int(raw)
    except ValueError:
        sys.stderr.write(f"glyphdown: invalid {name}={raw!r}; using default {default}\n")
        return default


def _bool_env(name: str, default: bool = False) -> bool:
    raw_orig = os.environ.get(name)
    if raw_orig is None or raw_orig == "":
        return default
    raw = raw_orig.strip().lower()
    if raw in ("1", "true", "yes", "on"):
        return True
    if raw in ("0", "false", "no", "off"):
        return False
    sys.stderr.write(
        f"glyphdown: invalid {name}={raw_orig!r}; using default {default}\n"
    )
    return default


def _float_env(name: str, default: float) -> float:
    """internal-ref: env-overridable float parser for percent-based guards.

    Returns ``default`` and emits a stderr warning when the value is unset
    or unparseable, keeping the codec fail-open under hostile config.
    """
    raw = os.environ.get(name)
    if raw is None or raw == "":
        return default
    try:
        return float(raw)
    except ValueError:
        sys.stderr.write(
            f"glyphdown: invalid {name}={raw!r}; using default {default}\n"
        )
        return default


# G13: env-var overrides
TAG_PREFIX = os.environ.get("GLYPHDOWN_TAG_PREFIX", "[glyphdown:compact-v1")
DEFAULT_BREAK_EVEN_TOKENS = _int_env("GLYPHDOWN_BREAK_EVEN_TOKENS", 25)
# SIL-1: env-pinned threshold beats audit-fitted threshold
BREAK_EVEN_ENV_PINNED = os.environ.get("GLYPHDOWN_BREAK_EVEN_TOKENS", "") != ""
# internal-ref: percent-based break-even guard at the filter layer. Complements
# the absolute-token guard so large payloads that compact by only a few
# tokens (e.g. 30-token ANSI strip on a 5K-token blob) pass through
# untransformed — preserving the original verbatim instead of paying the
# tag-prefix tax for sub-noise savings. Default 5%; override with
# ``GLYPHDOWN_MIN_SAVINGS_RATIO=0.0`` to disable.
DEFAULT_MIN_SAVINGS_RATIO = _float_env("GLYPHDOWN_MIN_SAVINGS_RATIO", 0.05)
DEFAULT_TRUNCATE_BYTES = _int_env("GLYPHDOWN_TRUNCATE_BYTES", 8192)
# G14 (internal-ref): oversize bail-out — default 5MB. Hook reads at most
# MAX_INPUT_BYTES + 1 bytes from stdin; if the result exceeds the cap, emit
# {"continue": true} unchanged and log an `oversize-bail` audit row. Protects
# against pathological tool outputs that would burn CPU + memory inside the
# compaction pipeline. Override with GLYPHDOWN_MAX_INPUT_BYTES.
MAX_INPUT_BYTES = _int_env("GLYPHDOWN_MAX_INPUT_BYTES", 5 * 1024 * 1024)
# FIX #51: oversize truncate-then-compact. When stdin exceeds MAX_INPUT_BYTES
# the historical behaviour was a raw passthrough ({"continue": true}) that
# captured zero tokens — the largest payloads were the ones dumped to context
# uncompressed. With this flag ON (default), the codec instead bounds the head
# to MAX_INPUT_BYTES via truncate_with_marker and runs the normal compaction
# path on the bounded text, recovering tokens that would otherwise be lost.
# The MAX_INPUT_BYTES cap is preserved verbatim (stdin is still read at most
# MAX_INPUT_BYTES + 1), so the CPU/mem guard is intact. Opt out with
# GLYPHDOWN_OVERSIZE_COMPACT=0 to restore byte-identical raw passthrough.
OVERSIZE_COMPACT = _bool_env("GLYPHDOWN_OVERSIZE_COMPACT", default=True)
# G13: emergency kill switch
DISABLED = _bool_env("GLYPHDOWN_DISABLE", default=False)
# SIL-1: opt out of audit-driven auto-tuning
NO_LEARN = _bool_env("GLYPHDOWN_NO_LEARN", default=False)
# internal-ref G12: min-payload threshold; skip codec for small payloads on certain tools
DEFAULT_MIN_PAYLOAD_BYTES = _int_env("GLYPHDOWN_MIN_PAYLOAD_BYTES", 512)
MIN_PAYLOAD_TOOLS = {"Read", "Glob"}

# internal-ref anchor-survival guard (absorbed from claudioemmanuel/squeez).
# When the codec drops >= ANCHOR_REDUCTION_THRESHOLD of the original token
# count, verify high-value anchors (file:line refs, error codes, test
# verdicts) survive at >= ANCHOR_PRESERVATION_FLOOR. Below floor → revert.
# Default ON; toggle off with GLYPHDOWN_ANCHOR_GUARD=0 if the guard
# misfires on a corpus (file an issue if so).
ANCHOR_GUARD_ENABLED = _bool_env("GLYPHDOWN_ANCHOR_GUARD", default=True)
ANCHOR_REDUCTION_THRESHOLD = _float_env("GLYPHDOWN_ANCHOR_REDUCTION_THRESHOLD", 0.90)
ANCHOR_PRESERVATION_FLOOR = _float_env("GLYPHDOWN_ANCHOR_PRESERVATION_FLOOR", 0.70)


# internal-ref sub-floor classifier: Anthropic prompt-cache floors (published
# 2026-05). Below these counts a payload silently does NOT cache — codec
# savings on it accrue at full 1.0× base rate, strictly more valuable per
# token than savings on a cache_read payload (0.10× base). See
# docs/architecture/2026-05-19-cache-tokenomics-implications.md.
CACHE_FLOOR_OPUS = 4096
CACHE_FLOOR_SONNET = 1024
CACHE_FLOOR_HAIKU = 2048  # estimated; not Anthropic-published


def cache_class(token_estimate: int, model_floor: int = CACHE_FLOOR_SONNET) -> str:
    """Classify a payload's expected cache-billing behaviour.

    Returns one of:
      - "sub-floor"          — guaranteed uncached, codec saves at 1.0× base
      - "cache-write-likely" — at-or-above floor; first occurrence pays 1.25× base
      - "cache-read-likely"  — at-or-above floor; subsequent reads pay 0.10× base
                                (this codec call cannot know which; downstream
                                policy + Anthropic usage block disambiguate)

    `model_floor` defaults to Sonnet's 1024 — the lowest published floor —
    so the classifier is conservative (overcounts sub-floor on Sonnet
    workloads, but never misclassifies an Opus sub-floor payload as
    above-floor).
    """
    if token_estimate < model_floor:
        return "sub-floor"
    # We cannot tell from a single payload whether it's first or repeat
    # occurrence. Default to "cache-write-likely" — the more conservative
    # branch for dollar accounting (1.25× base, larger absolute saving).
    return "cache-write-likely"


# internal-ref volatile-content detector: identify cache-poisoning content
# (UUIDs, ISO-8601 timestamps, JWTs, hex hashes, ULIDs) so the operator
# can audit how often these slip into cached prefixes. Headroom's
# CacheAligner is detector-only and refuses to mutate; ours detects AND
# surfaces the result in the audit log so cohort analysis runs cleanly
# downstream. All patterns are conservative — prefer false-negatives
# over false-positives so we don't tag legitimate content as volatile.
_VOLATILE_PATTERNS: "dict[str, re.Pattern[str]]" = {
    "uuid": re.compile(
        r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-"
        r"[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b"
    ),
    "iso8601": re.compile(
        r"\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?"
        r"(?:Z|[+-]\d{2}:?\d{2})?\b"
    ),
    "jwt": re.compile(
        r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b"
    ),
    # Long hex (32+ chars) — sha256/sha1/md5/git-sha and similar
    "hex_hash": re.compile(r"\b[0-9a-f]{32,}\b"),
    # ULID (Crockford base32, 26 chars, monotonic)
    "ulid": re.compile(r"\b[0-9A-HJKMNP-TV-Z]{26}\b"),
    # Unix epoch milliseconds (13-digit, range 2001-01-01 to ~2286)
    "epoch_ms": re.compile(r"\b(?:[12]\d{12}|1[3-9]\d{11})\b"),
}


def detect_volatile_content(text: str, max_scan_bytes: int = 64_000) -> "dict[str, int]":
    """Scan `text` for cache-poisoning volatile patterns.

    Returns a dict mapping pattern kind → match count. Patterns checked:
    uuid, iso8601, jwt, hex_hash, ulid, epoch_ms.

    `max_scan_bytes` bounds CPU on adversarial input. Default 64KB covers
    typical tool-result payloads; larger payloads are scanned only at the
    head + tail (32KB each) to maintain O(1) latency.
    """
    if not text:
        return {kind: 0 for kind in _VOLATILE_PATTERNS}
    if len(text) <= max_scan_bytes:
        sample = text
    else:
        half = max_scan_bytes // 2
        sample = text[:half] + text[-half:]
    return {
        kind: len(pattern.findall(sample))
        for kind, pattern in _VOLATILE_PATTERNS.items()
    }


def volatile_content_summary(text: str) -> dict:
    """Compact summary of detect_volatile_content for audit-row inclusion.

    Returns `total` (sum of matches), `by_kind` (nonzero kinds only),
    `risk` (one of 'none' / 'low' / 'medium' / 'high').

    Risk thresholds (calibrated for typical tool-result payloads):
      - 0 matches → none
      - 1-3 matches → low
      - 4-10 matches → medium
      - 11+ matches → high
    """
    counts = detect_volatile_content(text)
    total = sum(counts.values())
    nonzero = {k: v for k, v in counts.items() if v > 0}
    if total == 0:
        risk = "none"
    elif total <= 3:
        risk = "low"
    elif total <= 10:
        risk = "medium"
    else:
        risk = "high"
    return {"total": total, "by_kind": nonzero, "risk": risk}


# internal-ref compaction break-even — closed-form math from Skidmore 2026-05-17
# (docs/architecture/2026-05-19-cache-tokenomics-implications.md).
#
# Derivation (model-independent):
#   read_cost   = N * R                  (per future turn after compaction)
#   compact_cost = N*R + S*5B + S*W      (read original + write summary + cache write)
#   per-turn saving = (N - S) * R        (cached portion shrinks from N to S)
#   break_even_turns = compact_cost / per_turn_saving
#                    = (N*R + 5*S*B + S*W) / ((N - S)*R)
#                    = (N + 62.5*S) / (N - S)               [substituting W=1.25*B, R=0.10*B]
#                    = (1 + 62.5*r) / (1 - r)               [where r = S/N]
#
# Both the model and prefix size cancel. Only the compression ratio matters.

def compaction_break_even_turns(compression_ratio: float) -> float:
    """Closed-form break-even count for whether compaction pays back.

    Args:
      compression_ratio: r = S/N where S is the compressed (summary) token
        count and N is the original token count. Must lie in (0, 1) —
        r=0 is a degenerate noop, r=1 means no compression at all.

    Returns:
      Number of future turns required for the per-turn cache-read savings
      to recover the one-time compaction cost (read+summarize+rewrite).
      Returns math.inf when r >= 1 (no compression → never pays back).

    Examples:
      r = 0.10 (10:1 compression)  →  8.06 turns
      r = 0.20 (5:1)               →  17.06 turns
      r = 0.50 (2:1)               →  65.0 turns
      r = 0.62 (Glyphdown measured) →  104.9 turns
      r = 0.05 (20:1 aggressive)   →  4.34 turns
    """
    if compression_ratio <= 0:
        return 1.0  # degenerate: vanishingly small summary
    if compression_ratio >= 1.0:
        import math as _math
        return _math.inf
    return (1.0 + 62.5 * compression_ratio) / (1.0 - compression_ratio)


def codec_pays_back_at(
    compression_ratio: float, expected_remaining_turns: int
) -> bool:
    """Quick gate: does compaction pay back given an expected turn budget?

    Use in policy code (e.g. the proxy-controller UES2 gate) to reject
    compactions that won't recover their cost before the session ends.
    """
    return expected_remaining_turns >= compaction_break_even_turns(compression_ratio)




# internal-ref: per-tool codec routing allowlist. Only these tools run through the
# codec by default — others (Edit, Write, Task, MCP wrappers, etc.) bypass
# entirely so we don't waste CPU on inputs the codec cannot meaningfully
# compress. Override with GLYPHDOWN_TOOL_ALLOWLIST="ToolA,ToolB"; empty string
# disables the allowlist and reverts to "all tools eligible".
def _parse_tool_allowlist() -> set[str] | None:
    raw = os.environ.get("GLYPHDOWN_TOOL_ALLOWLIST")
    if raw is None:
        return {"Read", "Grep", "Glob", "Monitor", "Bash"}
    raw = raw.strip()
    if raw == "":
        return None  # explicit opt-out → all tools eligible
    return {t.strip() for t in raw.split(",") if t.strip()}


TOOL_ALLOWLIST = _parse_tool_allowlist()

# G2: audit trail. Resolved lazily inside _write_audit so per-test HOME /
# GLYPHDOWN_DATA_DIR overrides are honored. Module-level names retained so
# existing tests that monkeypatch _AUDIT_FILE continue to work — they take
# precedence over the lazy resolver.
_AUDIT_DIR = _audit_dir()
_AUDIT_FILE = _AUDIT_DIR / "audit.jsonl"

# internal-ref G2: per-session audit JSONL — append-only per-session file located
# under HOME/.claude/data/glyphdown/audit-<session_id>.jsonl. Coexists with
# the global audit.jsonl so the SIL-1 auto-tuner (glyphdown_tuned.load_audit)
# keeps working. Paths resolved each call so HOME changes in tests are
# honored.
_SESSION_AUDIT_SUBDIR = Path(".claude") / "data" / "glyphdown"
_FALLBACK_SESSION_ID = "session-unspecified"
# Restrict session_id chars to a conservative safe set for filenames.
_SAFE_SESSION_ID_RE = re.compile(r"[^A-Za-z0-9_-]")


def _session_audit_dir() -> Path:
    return Path.home() / _SESSION_AUDIT_SUBDIR


def _sanitize_session_id(session_id: str) -> str:
    """Strip path-traversal / shell-hostile chars from session_id.

    External input (payload field or env var) is hostile — sanitize before
    using as a filename component. Collapses anything outside [A-Za-z0-9_-]
    to '_' (note: dot is NOT preserved — runs of dots could be confused for
    parent-dir refs by downstream consumers), forbids leading dots / dashes,
    caps length, and falls back to a neutral placeholder on empty result.
    """
    if not isinstance(session_id, str) or not session_id:
        return _FALLBACK_SESSION_ID
    cleaned = _SAFE_SESSION_ID_RE.sub("_", session_id)
    cleaned = cleaned.lstrip(".-")[:128]
    return cleaned or _FALLBACK_SESSION_ID


def _session_audit_file(session_id: str) -> Path:
    safe = _sanitize_session_id(session_id)
    return _session_audit_dir() / f"audit-{safe}.jsonl"


ANSI_RE = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07")
TRAILING_WS_RE = re.compile(r"[ \t]+$", re.MULTILINE)
MULTI_BLANK_RE = re.compile(r"\n{3,}")

# internal-ref: Language::Data short-circuit guard. Detects payloads where the
# codec cannot meaningfully compress (image/binary base64 blobs, data URIs,
# HTML/XML markup) and bypasses transforms entirely. Without this guard the
# Read/Grep/Glob hook can burn cycles + tag-prefix tokens on payloads that
# are inherently opaque or whose structure must be preserved verbatim.
DATA_URI_RE = re.compile(r"data:(image|application|audio|video)/[\w.+-]+;base64,")
# A long unbroken base64-ish run (>=256 chars of [A-Za-z0-9+/=]) is almost
# certainly an embedded binary, not prose.
BASE64_BLOB_RE = re.compile(r"[A-Za-z0-9+/=]{256,}")
# HTML/XML probe: opens with a tag and has multiple closing tags in head.
HTML_DOCTYPE_RE = re.compile(r"^\s*<!DOCTYPE\s+html", re.IGNORECASE)
HTML_TAG_RE = re.compile(r"</[a-zA-Z][\w:-]*\s*>")


def looks_like_binary_base64(text: str) -> bool:
    """Detect inline binary blobs: data URIs or long unbroken base64 runs."""
    head = text[:4096]
    if DATA_URI_RE.search(head):
        return True
    if BASE64_BLOB_RE.search(head):
        return True
    return False


def looks_like_html(text: str) -> bool:
    """Detect HTML/XML markup: <!DOCTYPE html> or 3+ closing tags in head."""
    head = text.lstrip()[:4096]
    if HTML_DOCTYPE_RE.match(head):
        return True
    stripped = text.lstrip()
    if stripped.startswith("<") and len(HTML_TAG_RE.findall(head)) >= 3:
        return True
    return False


def is_language_data(text: str) -> bool:
    """internal-ref: True if payload is opaque Language::Data the codec should
    leave alone (image/binary base64, data URIs, HTML/XML markup).
    """
    if not text:
        return False
    return looks_like_binary_base64(text) or looks_like_html(text)


def _write_audit(row: dict) -> None:
    """G2: append-only audit JSONL. Fail-open on any I/O error.

    Writes the row to two destinations (each failure is independent):
      1. Global HOME/.ultracos/audit.jsonl — feeds SIL-1 auto-tuner (internal-ref).
      2. Per-session HOME/.claude/data/glyphdown/audit-<session_id>.jsonl
         (internal-ref G2) — when row carries a non-empty session_id.

    O_APPEND on POSIX is atomic for writes <PIPE_BUF (4096 on Linux); single
    JSON rows stay well under that, so concurrent codec invocations
    interleave safely.
    """
    line = json.dumps(row, separators=(",", ":")) + "\n"
    # 1) Global audit (back-compat, feeds SIL-1).
    try:
        _AUDIT_DIR.mkdir(parents=True, exist_ok=True)
        with open(_AUDIT_FILE, "a", encoding="utf-8") as f:
            f.write(line)
    except OSError:
        pass  # audit failure must never block hook

    # 2) Per-session audit (internal-ref G2).
    try:
        session_id = row.get("session_id")
        if isinstance(session_id, str) and session_id:
            session_dir = _session_audit_dir()
            session_dir.mkdir(parents=True, exist_ok=True)
            with open(_session_audit_file(session_id), "a", encoding="utf-8") as f:
                f.write(line)
    except OSError:
        pass  # per-session audit failure must never block hook



def looks_like_json(text: str) -> bool:
    """Cheap JSON shape probe — first non-space is { or [."""
    stripped = text.lstrip()
    return bool(stripped) and stripped[0] in "{["


def looks_like_yaml(text: str) -> bool:
    """YAML probe: opens with ---, has key: value lines, no bare prose."""
    if text.lstrip().startswith("---"):
        return True
    head = "\n".join(text.splitlines()[:10])
    keyish = re.findall(r"^[A-Za-z_][\w-]*\s*:", head, re.MULTILINE)
    return len(keyish) >= 3


def looks_like_toml(text: str) -> bool:
    head = "\n".join(text.splitlines()[:10])
    return bool(re.search(r"^\[[\w.-]+\]\s*$", head, re.MULTILINE))


def looks_like_code(text: str) -> bool:
    """G8: detect source code by counting language keywords in first 50 lines.

    Returns True if 3+ occurrences of def/fn/function/class/import/package/use/
    #include/public found in first 50 lines AND shape is not JSON/YAML/TOML.
    """
    head_lines = "\n".join(text.splitlines()[:50])
    keywords = [
        "def ", "function ", "fn ", "class ", "import ", "package ",
        "use ", "#include ", "public class "
    ]
    count = sum(head_lines.count(kw) for kw in keywords)
    return count >= 3


# internal-ref /!:improve P3: Glob slice was 0% engaged on codec-corpus-2026-05-19
# (13 fixtures, all passing through). Path-list payloads (Glob / find output)
# share massive common prefixes — lossless prefix-factoring engages where the
# generic A1 transforms cannot.
_PATH_LIST_LINE = re.compile(r"^(/|~/|[A-Z]:\\)")


def looks_like_path_list(text: str) -> bool:
    """Detect filesystem-path-list payloads (Glob, find, ls -R style output).

    Heuristic: ≥3 non-blank lines, ≥80% of non-blank lines start with `/`,
    `~/`, or `<drive>:\\` AND have no whitespace in the first 30 chars (i.e.,
    they look like paths, not prose).
    """
    lines = [ln for ln in text.splitlines() if ln.strip()]
    if len(lines) < 3:
        return False
    path_like = sum(
        1 for ln in lines
        if _PATH_LIST_LINE.match(ln) and " " not in ln[:30]
    )
    return path_like >= max(3, int(len(lines) * 0.8))


def _longest_common_path_prefix(lines: list[str]) -> str:
    """Return the longest path-prefix common to all lines, snapped to the
    last `/` so we don't split mid-component."""
    if not lines:
        return ""
    prefix = lines[0]
    for ln in lines[1:]:
        # Reduce prefix length while prefix is not a prefix of ln
        while not ln.startswith(prefix):
            prefix = prefix[:-1]
            if not prefix:
                return ""
    # Snap to the last path-separator so we never split mid-component
    sep_idx = max(prefix.rfind("/"), prefix.rfind("\\"))
    if sep_idx <= 0:
        return ""
    return prefix[: sep_idx + 1]


def compress_path_list(text: str) -> str | None:
    """Lossless prefix-factoring transform for path-list payloads.

    Returns a transformed string when factoring yields meaningful savings,
    or None when the savings are below 64 bytes (let the break-even guard
    decide downstream). Reconstruction is trivial — consumer prepends the
    prefix from the schema-tag header to each subsequent line.
    """
    lines = [ln for ln in text.splitlines() if ln.strip()]
    if len(lines) < 3:
        return None
    prefix = _longest_common_path_prefix(lines)
    if len(prefix) < 16:
        # Too little to factor — not worth the schema-tag overhead
        return None
    relative_lines = [ln[len(prefix):] for ln in lines]
    out = (
        f"[glyphdown:cpc-v1 prefix={prefix} n={len(lines)}]\n"
        + "\n".join(relative_lines)
    )
    if len(out) >= len(text) - 64:
        return None
    return out


def classify_payload(text: str) -> str:
    """A4: classify shape so JSON/YAML/TOML bypass code-style regex.

    internal-ref: Language::Data shapes (binary/base64, html) classified first so
    callers / audit rows see the real shape even when we short-circuit.
    internal-ref /!:improve: path-list classification added for Glob-style output.
    """
    if is_language_data(text):
        if looks_like_html(text):
            return "html"
        return "binary"
    if looks_like_json(text):
        return "json"
    if looks_like_toml(text):
        return "toml"
    if looks_like_yaml(text):
        return "yaml"
    # G8: code detection before fallback to text
    if looks_like_code(text):
        return "code"
    # internal-ref: path-list before generic text — Glob/find output
    if looks_like_path_list(text):
        return "path-list"
    return "text"


def strip_ansi(text: str) -> str:
    return ANSI_RE.sub("", text)


def toonify_uniform_array(text: str) -> str | None:
    """internal-ref: encode a uniform array-of-objects payload as TOON-like tabular
    form. Detects:
      - JSON root is a list of length >= 10
      - All entries are dicts with identical key set
      - All leaf values are primitives (str/int/float/bool/None)

    Returns the encoded string when the shape matches, else None.
    Lossless roundtrip via from_toon() not yet implemented; this is the
    forward-only encoder for the opt-in --toon flag.

    Format follows toon-format/toon spec:
      users[3]{id,name,role}:
        1,alice,admin
        2,bob,user
        3,carol,user
    """
    try:
        parsed = json.loads(text)
    except (json.JSONDecodeError, ValueError):
        return None
    if not isinstance(parsed, list) or len(parsed) < 10:
        return None
    if not all(isinstance(x, dict) for x in parsed):
        return None
    keys = list(parsed[0].keys())
    if not keys:
        return None
    for entry in parsed:
        if list(entry.keys()) != keys:
            return None
        for v in entry.values():
            if not isinstance(v, (str, int, float, bool, type(None))):
                return None

    def _fmt(v):
        if v is None:
            return ""
        if isinstance(v, bool):
            return "true" if v else "false"
        if isinstance(v, str):
            # CSV-escape commas / newlines / quotes
            if "," in v or "\n" in v or '"' in v:
                return '"' + v.replace('"', '""') + '"'
            return v
        return str(v)

    header = f"items[{len(parsed)}]" + "{" + ",".join(keys) + "}:"
    body = "\n".join("  " + ",".join(_fmt(e[k]) for k in keys) for e in parsed)
    return header + "\n" + body


def minify_json(text: str) -> str:
    """Lossless JSON minify; passthrough on any parse error."""
    try:
        parsed = json.loads(text)
    except (json.JSONDecodeError, ValueError):
        return text
    return json.dumps(parsed, separators=(",", ":"), ensure_ascii=False)


def collapse_blanks(text: str) -> str:
    text = TRAILING_WS_RE.sub("", text)
    text = MULTI_BLANK_RE.sub("\n\n", text)
    return text


# internal-ref G1: accurate token counting via tiktoken o200k_base, with
# fail-open fallback to len(s)//4. Backend selection cached after first call.
try:
    from glyphdown_tokenizer import count_tokens as _count_tokens  # type: ignore
    _TOKENIZER_AVAILABLE = True
except Exception:  # noqa: BLE001
    _TOKENIZER_AVAILABLE = False

# Last-used backend label, exposed for telemetry/tests.
LAST_TOKEN_BACKEND: str = "fallback-len4"


def estimate_tokens(s: str) -> int:
    """Token estimate.

    Uses tiktoken o200k_base when available (accurate for GPT-4o/Claude-class
    BPE token counts); falls back to the historical 1 token ~= 4 chars
    heuristic if tiktoken is missing or raises. Backend name is recorded on
    module-level LAST_TOKEN_BACKEND for callers that want to flag fallback.
    """
    global LAST_TOKEN_BACKEND
    # PHASE 2a A/B: force the 4-char fallback so python and the tokenizer-free
    # Rust port make identical keep/passthrough decisions and identical tag
    # ratios. Set by bench/equiv_rust_vs_python.py; no effect in production.
    if os.environ.get("GLYPHDOWN_FORCE_CHAR_TOKENS", "") not in ("", "0"):
        LAST_TOKEN_BACKEND = "forced-len4"
        return max(1, len(s) // 4)
    if _TOKENIZER_AVAILABLE:
        try:
            count, backend = _count_tokens(s)
            LAST_TOKEN_BACKEND = backend
            return max(1, int(count))
        except Exception:  # noqa: BLE001
            LAST_TOKEN_BACKEND = "fallback-len4"
            # fall through to explicit fallback
    else:
        LAST_TOKEN_BACKEND = "fallback-len4"
    return max(1, len(s) // 4)


@dataclass
class CompactResult:
    output: str
    original_tokens: int
    compact_tokens: int
    shape: str
    applied: list[str]

    @property
    def saved_tokens(self) -> int:
        return self.original_tokens - self.compact_tokens

    @property
    def ratio(self) -> float:
        if self.original_tokens == 0:
            return 1.0
        return self.compact_tokens / self.original_tokens


def truncate_with_marker(text: str, max_bytes: int) -> tuple[str, int]:
    """A3: tail-truncate with single trailing marker. Never inline.

    Returns (truncated_text, hidden_bytes). Marker placed AFTER content.
    """
    raw = text.encode("utf-8")
    if len(raw) <= max_bytes:
        return text, 0
    head_bytes = max_bytes - 96  # reserve room for marker line
    head = raw[:head_bytes].decode("utf-8", errors="ignore").rstrip()
    hidden_lines = text.count("\n") - head.count("\n")
    hidden_bytes = len(raw) - len(head.encode("utf-8"))
    marker = f"\n\n[truncated: {hidden_lines} lines / {hidden_bytes} bytes hidden]"
    return head + marker, hidden_bytes


def compact_payload(
    text: str,
    *,
    break_even_tokens: int = DEFAULT_BREAK_EVEN_TOKENS,
    truncate_bytes: int = DEFAULT_TRUNCATE_BYTES,
    min_savings_ratio: float = DEFAULT_MIN_SAVINGS_RATIO,
) -> CompactResult:
    """Apply A1+A3+A4+A9+A10 to a tool-result payload.

    A10 break-even guard: if savings would be below ``break_even_tokens``
    OR the percent saved is below ``min_savings_ratio`` (internal-ref),
    pass through unmodified (no tag prefix either).
    """
    if not text:
        return CompactResult(text, 0, 0, "empty", [])

    # internal-ref: Language::Data short-circuit guard. Image/binary base64 blobs,
    # data URIs, and HTML/XML markup are opaque to the codec — no lossless
    # transform we apply will yield real savings, and tag-prefixing the output
    # would burn tokens for zero benefit. Detect first, return passthrough.
    if is_language_data(text):
        original_tokens = estimate_tokens(text)
        shape = "html" if looks_like_html(text) else "binary"
        return CompactResult(text, original_tokens, original_tokens, shape, [])

    original = text
    original_tokens = estimate_tokens(original)
    shape = classify_payload(text)
    applied: list[str] = []

    # A4: data-shape short-circuit. JSON gets minify-only; YAML/TOML pass through.
    if shape == "json":
        # internal-ref G-TOON: opt-in tabular encoding for uniform-array payloads
        toon_enabled = _bool_env("GLYPHDOWN_TOON", default=False)
        toon_out = toonify_uniform_array(text) if toon_enabled else None
        if toon_out is not None and len(toon_out) < len(text) - 64:
            candidate = toon_out
            applied.append("toon-encode")
            # Strip ANSI on the toon output too
            stripped = strip_ansi(candidate)
            if stripped != candidate:
                applied.append("ansi-strip")
            candidate = stripped
        else:
            candidate = minify_json(text)
            if candidate != text:
                applied.append("json-minify")
            # Strip ANSI even on data (rare but valid lossless transform)
            stripped = strip_ansi(candidate)
            if stripped != candidate:
                applied.append("ansi-strip")
            candidate = stripped
    elif shape in ("yaml", "toml"):
        # Lossless-only: ANSI strip + trailing-WS / blank collapse on data
        candidate = strip_ansi(text)
        if candidate != text:
            applied.append("ansi-strip")
        collapsed = collapse_blanks(candidate)
        if collapsed != candidate:
            applied.append("blank-collapse")
        candidate = collapsed
    elif shape == "code":
        # G8: code shape gets ANSI strip + blank-collapse only (preserve syntax)
        candidate = strip_ansi(text)
        if candidate != text:
            applied.append("ansi-strip")
        collapsed = collapse_blanks(candidate)
        if collapsed != candidate:
            applied.append("blank-collapse")
        candidate = collapsed
    elif shape == "path-list":
        # internal-ref /!:improve: lossless common-prefix factoring on Glob/find
        # output. Falls through to the generic A1 pipeline on the factored
        # output for trailing-WS + blank-collapse cleanup.
        factored = compress_path_list(text)
        if factored is not None:
            applied.append("path-prefix-factor")
            candidate = factored
        else:
            candidate = text
        stripped = strip_ansi(candidate)
        if stripped != candidate:
            applied.append("ansi-strip")
        collapsed = collapse_blanks(stripped)
        if collapsed != stripped:
            applied.append("blank-collapse")
        candidate = collapsed
    else:
        # Plain text / bash output: full A1 pipeline
        candidate = strip_ansi(text)
        if candidate != text:
            applied.append("ansi-strip")
        collapsed = collapse_blanks(candidate)
        if collapsed != candidate:
            applied.append("blank-collapse")
        candidate = collapsed

    # A3: truncate if still too large — but never truncate JSON (would invalidate).
    if shape != "json":
        truncated, hidden = truncate_with_marker(candidate, truncate_bytes)
        if hidden > 0:
            applied.append("truncate")
            candidate = truncated

    compact_tokens = estimate_tokens(candidate)
    saved = original_tokens - compact_tokens

    # A10: break-even guard — pass through if savings below threshold
    # internal-ref: also pass through when the percent saved is below
    # ``min_savings_ratio`` so we don't pay tag-prefix overhead for
    # sub-noise wins on large payloads (e.g. 30 tokens shaved off 5K).
    ratio_saved = (saved / original_tokens) if original_tokens > 0 else 0.0
    if (
        saved < break_even_tokens
        or not applied
        or (min_savings_ratio > 0.0 and ratio_saved < min_savings_ratio)
    ):
        return CompactResult(original, original_tokens, original_tokens, shape, [])

    # A9: schema-tag prefix on compressed payload (kills rtk#582 class)
    ratio = compact_tokens / original_tokens if original_tokens else 1.0
    tag = (
        f"{TAG_PREFIX} shape={shape} "
        f"ratio={ratio:.2f} applied={','.join(applied)}]\n"
    )
    return CompactResult(
        tag + candidate, original_tokens, compact_tokens + estimate_tokens(tag),
        shape, applied,
    )


# ── Hook entry point ───────────────────────────────────────────────────────

def main() -> int:
    # internal-ref: capture raw stdin in the outer scope so the exception handler
    # can tee it to ~/.ultracos/failed-payloads/<ts>.json for replay debugging.
    raw: str = ""
    tool_name_for_failure: str = ""
    try:
        # G13: emergency kill switch
        if DISABLED:
            sys.stdin.read()  # drain stdin so caller doesn't block on pipe
            print(json.dumps({"continue": True}))
            return 0

        # G14: oversize handling. Read at most MAX_INPUT_BYTES + 1 to detect
        # overflow — this preserves the CPU/mem cap regardless of mode below.
        raw = sys.stdin.read(MAX_INPUT_BYTES + 1)
        if len(raw) > MAX_INPUT_BYTES:
            # FIX #51: truncate-then-compact. Instead of a zero-capture raw
            # passthrough, bound the head to MAX_INPUT_BYTES and run the normal
            # compaction path on it. `raw` here is the full hook JSON envelope
            # truncated mid-string by read(MAX+1), so json.loads(raw) would
            # fail — we deliberately treat the bounded head as opaque text and
            # wrap the compacted result in a synthetic tool_response. The
            # recovery is the *bounding* (unbounded → ≤ MAX), so we emit the
            # bounded head even when compact_payload returns a passthrough
            # (incompressible / language-data). Fail-open on any error reverts
            # to the historical raw passthrough.
            if OVERSIZE_COMPACT:
                try:
                    head, _hidden = truncate_with_marker(raw, MAX_INPUT_BYTES)
                    result = compact_payload(head)
                    new_resp = {
                        "content": [{"type": "text", "text": result.output}],
                    }
                    _write_audit({
                        "ts": time.time(),
                        "event": "oversize-compact",
                        "input_bytes": len(raw),
                        "max_bytes": MAX_INPUT_BYTES,
                        "shape": result.shape,
                        "applied": result.applied,
                        "bounded_tokens": result.original_tokens,
                        "compact_tokens": result.compact_tokens,
                        "saved_tokens": result.saved_tokens,
                    })
                    print(json.dumps({
                        "continue": True,
                        "updatedToolOutput": new_resp,
                    }))
                    return 0
                except Exception:  # noqa: BLE001 — fail-open to raw passthrough
                    pass
            _write_audit({
                "ts": time.time(),
                "event": "oversize-bail",
                "input_bytes": len(raw),
                "max_bytes": MAX_INPUT_BYTES,
            })
            print(json.dumps({"continue": True}))
            return 0

        payload = json.loads(raw)
        tool_name = payload.get("tool_name", "")
        tool_name_for_failure = tool_name
        session_id = payload.get("session_id") or os.environ.get(
            "CLAUDE_SESSION_ID", f"pid-{os.getpid()}"
        )

        # internal-ref: per-tool allowlist gate. Bypass codec entirely for tools
        # outside the allowlist (e.g. Edit/Write) — short-circuit before we
        # spend CPU on classification, dedup lookups, structuredContent
        # serialization, etc.
        if TOOL_ALLOWLIST is not None and tool_name not in TOOL_ALLOWLIST:
            print(json.dumps({"continue": True}))
            return 0

        tool_response = payload.get("tool_response", {}) or {}
        content_items = tool_response.get("content")
        if not isinstance(content_items, list):
            # No content[] — but structuredContent may still be present
            content_items = []

        # SIL-1: resolve per-tool break-even threshold.
        # Priority: GLYPHDOWN_BREAK_EVEN_TOKENS env > tuned[shape] > tuned[tool] > DEFAULT.
        # Shape is the tighter signal — the same tool (Bash) produces both
        # JSON and ANSI-laden text payloads, and the absolute-token guard
        # should track the payload shape, not just the producing tool.
        # Resolved per text item below so shape can be classified once on
        # the actual payload.
        if BREAK_EVEN_ENV_PINNED or NO_LEARN:
            tuned_tool: dict[str, int] = {}
            tuned_shape: dict[str, int] = {}
        else:
            try:
                from glyphdown_tuned import (  # local import, fail-open
                    load_shape_thresholds,
                    load_thresholds,
                )
                tuned_tool = load_thresholds()
                tuned_shape = load_shape_thresholds()
            except Exception:  # noqa: BLE001
                tuned_tool = {}
                tuned_shape = {}

        def _resolve_threshold(payload_text: str) -> int:
            """Pick break-even threshold for this payload.

            Env-pinned and NO_LEARN already collapse tuned_* to {} above,
            so this falls through to DEFAULT_BREAK_EVEN_TOKENS in those
            modes. Otherwise shape > tool > DEFAULT.
            """
            if BREAK_EVEN_ENV_PINNED or NO_LEARN:
                return DEFAULT_BREAK_EVEN_TOKENS
            if tuned_shape:
                shape = classify_payload(payload_text)
                if shape in tuned_shape:
                    return int(tuned_shape[shape])
            if tool_name in tuned_tool:
                return int(tuned_tool[tool_name])
            return DEFAULT_BREAK_EVEN_TOKENS

        any_changed = False
        for item in content_items:
            if not isinstance(item, dict):
                continue
            item_type = item.get("type")

            # internal-ref: skip image/binary shapes entirely (cannot compress)
            if item_type in ("image", "binary"):
                continue

            if item_type != "text":
                continue
            text = item.get("text")
            if not isinstance(text, str):
                continue

            # internal-ref: tee raw payload BEFORE any transform
            tee_path = None
            if _TEE_AVAILABLE:
                try:
                    tee_path = _tee.tee_payload(tool_name, text, time.time())
                except Exception:  # noqa: BLE001 — fail-open
                    pass

            # internal-ref (A8.1..A8.4): dedup + summarize for Read/Grep/Glob/Monitor.
            # Runs BEFORE compaction AND before the min-payload gate so that
            # session-level repetition is captured even on tiny payloads —
            # repeated Read("foo.py") in a session is the dedup hot path and
            # has nothing to do with per-payload compression economics.
            # On dedup hit, full-text replacement short-circuits compaction.
            # On summarize, the summarized output still flows through
            # compaction for further savings.
            if _DEDUP_AVAILABLE and tool_name in _dedup.DEDUP_TOOLS:
                try:
                    res = _dedup.maybe_dedup_or_summarize(
                        tool_name, text, session_id
                    )
                except Exception:  # noqa: BLE001 — A8.4 fail-open
                    res = None
                if res is not None:
                    new_text, mode = res
                    item["text"] = new_text
                    any_changed = True
                    _write_audit({
                        "ts": time.time(),
                        "event": f"dedup-{mode}",
                        "tool": tool_name,
                        "session_id": session_id,
                        "original_bytes": len(text),
                        "new_bytes": len(new_text),
                    })
                    if mode == "dedup":
                        # Skip compaction for full-replacement placeholders.
                        continue
                    # mode == "summarize" → fall through and let compaction
                    # apply lossless cleanups on the summarized text.
                    text = new_text

            # internal-ref G12: min-payload threshold gate for COMPACTION ONLY.
            # Must run AFTER dedup recording (above) — see MIK-NEW 2026-05-19:
            # small payloads were silently skipping dedup entirely because
            # this gate `continue`d before _dedup.maybe_dedup_or_summarize
            # could record the hash, so 2nd-call hits never fired.
            if tool_name in MIN_PAYLOAD_TOOLS:
                text_bytes = len(text.encode("utf-8"))
                if text_bytes < DEFAULT_MIN_PAYLOAD_BYTES:
                    continue

            # SIL-2 (internal-ref): per-tool learned policy — skip compaction on
            # buckets that have proven low-entropy. Cooldown re-samples every
            # COOLDOWN_SKIPS hits so drift is caught.
            policy_active = _POLICY_AVAILABLE and not NO_LEARN
            bucket = None
            if policy_active:
                try:
                    if tool_name == "Bash":
                        cmd_preview = (
                            payload.get("tool_input", {}) or {}
                        ).get("command", "")
                        if not isinstance(cmd_preview, str):
                            cmd_preview = ""
                        bucket = _policy.bucket_key(tool_name, cmd_preview[:100])
                    else:
                        bucket = _policy.bucket_key(tool_name, text[:100])
                    if _policy.should_skip(bucket):
                        _write_audit({
                            "ts": time.time(),
                            "event": "skip-learned",
                            "tool": tool_name,
                            "session_id": session_id,
                            "bucket": bucket,
                        })
                        continue
                except Exception:  # noqa: BLE001 — fail-open
                    bucket = None

            # internal-ref SIL-5: cache-aware compression. If this payload's
            # leading prefix has already been observed enough times to be
            # treated as cache-hot, bypass compaction entirely so the
            # Anthropic native prompt cache key stays stable. The probe
            # itself records the sighting, so the first occurrence still
            # flows through compaction and subsequent identical prefixes
            # short-circuit. Fail-open: any exception reverts to the
            # historical always-compress behaviour.
            if _CACHE_AVAILABLE:
                try:
                    if _cache.should_bypass_for_cache(text):
                        _write_audit({
                            "ts": time.time(),
                            "event": "skip-cache-hot",
                            "tool": tool_name,
                            "session_id": session_id,
                            "prefix_sig": _cache.prefix_signature(text),
                        })
                        continue
                except Exception:  # noqa: BLE001 — fail-open
                    pass

            result = compact_payload(text, break_even_tokens=_resolve_threshold(text))

            # SIL-2: record the actual saved_tokens for this bucket so the
            # rolling mean converges. Update even when applied is empty —
            # that's the signal a bucket is consistently low-entropy.
            if policy_active and bucket is not None:
                try:
                    _policy.update_policy(bucket, int(result.saved_tokens))
                except Exception:  # noqa: BLE001
                    pass

            if result.applied:
                # internal-ref anchor-survival guard (absorbed from
                # claudioemmanuel/squeez). Aggressive compressions can silently
                # drop the one file:line ref or error code that made the
                # original output useful — break-even policy + AB monitor catch
                # token-level misses, not structural-anchor ones. Check here
                # BEFORE the AB variant decision so a reverted compression
                # doesn't pollute the AB cohort.
                anchor_revert = False
                anchor_reduction = 0.0
                anchor_survival = 1.0
                if ANCHOR_GUARD_ENABLED:
                    try:
                        from anchor_guard import should_revert  # local import, fail-open
                        anchor_revert, anchor_reduction, anchor_survival = should_revert(
                            text,
                            result.output,
                            reduction_threshold=ANCHOR_REDUCTION_THRESHOLD,
                            preservation_floor=ANCHOR_PRESERVATION_FLOOR,
                        )
                    except Exception:  # noqa: BLE001 — guard MUST fail-open
                        anchor_revert = False

                if anchor_revert:
                    # Revert: leave item["text"] untouched, log the guard hit
                    # so we can tune the floor from real-world misfires.
                    _write_audit({
                        "ts": time.time(),
                        "event": "anchor-revert",
                        "tool": tool_name,
                        "session_id": session_id,
                        "shape": result.shape,
                        "applied": result.applied,
                        "reduction": round(anchor_reduction, 4),
                        "survival": round(anchor_survival, 4),
                        "original_tokens": result.original_tokens,
                        "compact_tokens": result.compact_tokens,
                    })
                    # Skip the rest of the apply branch — no AB decision,
                    # no item mutation, no compact audit row.
                    continue

                # SIL-3 A/B (internal-ref): decide whether to keep or strip tag
                variant = "with-tag"
                output_text = result.output
                if _AB_AVAILABLE:
                    try:
                        variant = _ab.decide_variant(session_id)
                        if variant == _ab.VARIANT_NO_TAG:
                            output_text = _ab.strip_tag_prefix(
                                output_text, TAG_PREFIX
                            )
                    except Exception:  # noqa: BLE001 — fail-open to with-tag
                        variant = "with-tag"
                        output_text = result.output
                item["text"] = output_text
                any_changed = True
                audit_row = {
                    "ts": time.time(),
                    "event": "compact",
                    "tool": tool_name,
                    "session_id": session_id,
                    "shape": result.shape,
                    "applied": result.applied,
                    "original_tokens": result.original_tokens,
                    "compact_tokens": result.compact_tokens,
                    "saved_tokens": result.saved_tokens,
                    "ratio": round(result.ratio, 4),
                    # internal-ref sub-floor classifier: enables cohort analysis of
                    # codec dollar-savings by cache-billing class. See
                    # docs/architecture/2026-05-19-cache-tokenomics-implications.md
                    "cache_class": cache_class(result.original_tokens),
                    # internal-ref volatile-content detector: counts cache-poisoning
                    # patterns (UUIDs, timestamps, JWTs, hex hashes, ULIDs,
                    # epoch_ms) in the payload. Risk levels: none/low/medium/high.
                    # Audit-only — does not mutate. Headroom's CacheAligner is
                    # similarly detector-only; we additionally surface the
                    # counts so the operator can correlate volatile-content
                    # density with cache-miss rate downstream.
                    "volatile": volatile_content_summary(text),
                }
                if tee_path is not None:
                    audit_row["tee_path"] = str(tee_path)
                if _AB_AVAILABLE:
                    try:
                        _ab.record_variant(audit_row, variant)
                    except Exception:  # noqa: BLE001
                        audit_row["variant"] = "with-tag"
                else:
                    audit_row["variant"] = "with-tag"
                _write_audit(audit_row)

        # internal-ref G6: handle structuredContent (JSON) if present
        struct_content = tool_response.get("structuredContent")
        if struct_content is not None:
            try:
                # json.dumps to text, run compact_payload, parse back
                struct_text = json.dumps(struct_content, separators=(",", ":"), ensure_ascii=False)
                struct_bytes = len(struct_text.encode("utf-8"))

                # internal-ref G12: min-payload threshold check for structuredContent too
                if struct_bytes >= DEFAULT_MIN_PAYLOAD_BYTES:
                    result = compact_payload(struct_text, break_even_tokens=_resolve_threshold(struct_text))

                    if result.applied:
                        # Parse the compacted text back to JSON
                        try:
                            compacted_obj = json.loads(result.output.split("\n", 1)[1])
                            tool_response["structuredContent"] = compacted_obj
                            any_changed = True
                            audit_row = {
                                "ts": time.time(),
                                "event": "compact",
                                "tool": tool_name,
                                "session_id": session_id,
                                "shape": result.shape,
                                "applied": result.applied,
                                "original_tokens": result.original_tokens,
                                "compact_tokens": result.compact_tokens,
                                "saved_tokens": result.saved_tokens,
                                "ratio": round(result.ratio, 4),
                                "variant": "structuredContent",
                            }
                            _write_audit(audit_row)
                        except (json.JSONDecodeError, ValueError, IndexError):
                            # Fail-open: if we can't parse back, leave it as-is
                            pass
            except Exception:  # noqa: BLE001 — fail-open on any structuredContent error
                pass

        if any_changed:
            print(json.dumps({
                "continue": True,
                "updatedToolOutput": tool_response,
            }))
        else:
            print(json.dumps({"continue": True}))
        return 0
    except Exception as exc:  # noqa: BLE001 — fail-open is the contract
        # internal-ref: tee raw payload to ~/.ultracos/failed-payloads/<ts>.json
        # so the failing input can be replayed against the codec. Best-effort —
        # tee itself is fail-open, the codec contract must not break.
        if _TEE_AVAILABLE and raw:
            try:
                fail_path = _tee.tee_on_failure(
                    raw, tool_name=tool_name_for_failure, error=exc
                )
                if fail_path is not None:
                    _write_audit({
                        "ts": time.time(),
                        "event": "codec-failure-tee",
                        "tool": tool_name_for_failure,
                        "error": str(exc)[:200],
                        "failed_payload_path": str(fail_path),
                    })
            except Exception:  # noqa: BLE001
                pass
        print(json.dumps({"continue": True}))
        return 0


if __name__ == "__main__":
    sys.exit(main())
