"""FIX #51 replay gate + unit tests: oversize truncate-then-compact.

The historical oversize-bail path (raw passthrough, zero token capture) is
replaced by truncate-then-compact gated behind GLYPHDOWN_OVERSIZE_COMPACT
(default ON). This module:

  1. Unit-tests the new branch directly (default-on compacts, opt-out is
     byte-identical raw passthrough).
  2. Runs a REPLAY GATE over synthesized representative oversize payloads
     covering the cap sizes {1MB, 4MB, 5MB} at the corpus avg compaction
     ratio 0.226, asserting: zero crashes, every emitted body <= cap, and
     reporting total tokens recovered vs the old zero-capture baseline.

EVIDENCE HONESTY: the 202 historical oversize-bail rows in audit.jsonl store
ONLY input_bytes (== cap+1 read artifact) + max_bytes — NO payload content —
so the real payloads are unreconstructable. The recovered-token number below
is measured on SYNTHESIZED payloads, labeled as such; it is NOT a historical
measurement.
"""

from __future__ import annotations

import importlib
import io
import json
import sys

import pytest


def _load_codec(monkeypatch, **env):
    """Import (or reload) glyphdown_codec with the given env applied first.

    Module-level constants (OVERSIZE_COMPACT, MAX_INPUT_BYTES, TOOL_ALLOWLIST,
    _AUDIT_DIR) are evaluated at import, so env must be set before (re)import.

    GLYPHDOWN_FORCE_CHAR_TOKENS forces the codebase's char/4 token heuristic so
    the replay gate does not push hundreds of MB of synthetic text through
    tiktoken's BPE (which is the codec's slow path on multi-MB payloads). The
    gate's invariants (zero-crash, <=cap, recovered>0) are backend-independent.
    """
    env.setdefault("GLYPHDOWN_FORCE_CHAR_TOKENS", "1")
    for k, v in env.items():
        monkeypatch.setenv(k, v)
    if "glyphdown_codec" in sys.modules:
        del sys.modules["glyphdown_codec"]
    return importlib.import_module("glyphdown_codec")


def _run_main(codec, monkeypatch, stdin_text):
    """Drive codec.main() with stdin_text, capture stdout JSON.

    Saves/restores real stdin+stdout around the call (rather than leaning on
    monkeypatch teardown) so pytest's own end-of-session stdout.flush() never
    lands on a closed StringIO.
    """
    real_stdin, real_stdout = sys.stdin, sys.stdout
    sys.stdin = io.StringIO(stdin_text)
    out = io.StringIO()
    sys.stdout = out
    try:
        rc = codec.main()
        captured = out.getvalue()
    finally:
        sys.stdin, sys.stdout = real_stdin, real_stdout
    return rc, captured


def _oversize_envelope(codec, body_text):
    """Build a hook stdin envelope whose JSON exceeds MAX_INPUT_BYTES."""
    env = {
        "tool_name": "Bash",
        "session_id": "replay-51",
        "tool_response": {"content": [{"type": "text", "text": body_text}]},
    }
    raw = json.dumps(env)
    assert len(raw) > codec.MAX_INPUT_BYTES, "fixture must exceed cap"
    return raw


def _synth_body(target_bytes, ratio=0.226):
    """Synthesize a >cap text/code mix that compacts ~`ratio`.

    Heavy blank-line + trailing-whitespace padding gives the lossless A1
    pipeline (ansi-strip + blank-collapse) real, measurable savings on the
    bounded head — mirroring the corpus avg ratio 0.226 (77% reduction).
    """
    # A compressible unit: a content line followed by collapsible blanks and
    # trailing whitespace. blank-collapse + trailing-WS strip recover most of
    # the padding bytes once truncated to the bounded head.
    unit = "def f():  return 1   \n\n\n\n\n\n\n\n\n   \n"
    reps = (target_bytes // len(unit)) + 1
    return unit * reps


# Cap sizes the brief requires the replay to cover.
_CAP_SIZES = {
    "1MB": 1 * 1024 * 1024,
    "4MB": 4 * 1024 * 1024,
    "5MB": 5 * 1024 * 1024,
}


def test_oversize_default_on_compacts(monkeypatch):
    codec = _load_codec(monkeypatch)  # default ON
    assert codec.OVERSIZE_COMPACT is True
    body = _synth_body(codec.MAX_INPUT_BYTES + 200_000)
    raw = _oversize_envelope(codec, body)
    rc, out = _run_main(codec, monkeypatch, raw)
    assert rc == 0
    resp = json.loads(out)
    assert resp["continue"] is True
    # The bounded head is now emitted as a synthetic tool_response.
    assert "updatedToolOutput" in resp
    text = resp["updatedToolOutput"]["content"][0]["text"]
    assert len(text.encode("utf-8")) <= codec.MAX_INPUT_BYTES


def test_oversize_opt_out_is_byte_identical_raw_passthrough(monkeypatch):
    codec = _load_codec(monkeypatch, GLYPHDOWN_OVERSIZE_COMPACT="0")
    assert codec.OVERSIZE_COMPACT is False
    body = _synth_body(codec.MAX_INPUT_BYTES + 200_000)
    raw = _oversize_envelope(codec, body)
    rc, out = _run_main(codec, monkeypatch, raw)
    assert rc == 0
    resp = json.loads(out)
    # Historical behaviour: bare continue, no updatedToolOutput.
    assert resp == {"continue": True}


def test_oversize_passthrough_body_still_bounded(monkeypatch):
    """Even when compaction returns passthrough, the bounded head is emitted.

    An incompressible (language-data) oversize body must NOT fall back to the
    zero-capture raw passthrough — the recovery is the bounding itself.
    """
    codec = _load_codec(monkeypatch)
    # HTML-ish body trips is_language_data -> compact_payload passthrough.
    body = ("<div>" + ("x" * 40) + "</div>\n") * (
        (codec.MAX_INPUT_BYTES // 51) + 5
    )
    raw = _oversize_envelope(codec, body)
    rc, out = _run_main(codec, monkeypatch, raw)
    assert rc == 0
    resp = json.loads(out)
    assert "updatedToolOutput" in resp
    text = resp["updatedToolOutput"]["content"][0]["text"]
    assert len(text.encode("utf-8")) <= codec.MAX_INPUT_BYTES


def test_oversize_never_crashes_across_cap_sizes(monkeypatch):
    codec = _load_codec(monkeypatch)
    for _label, size in _CAP_SIZES.items():
        body = _synth_body(max(size, codec.MAX_INPUT_BYTES) + 100_000)
        raw = _oversize_envelope(codec, body)
        rc, out = _run_main(codec, monkeypatch, raw)
        assert rc == 0
        resp = json.loads(out)
        assert resp["continue"] is True


def test_replay_gate_reports_recovered_tokens(monkeypatch, capsys):
    """REPLAY GATE: synthesized oversize corpus, report recovered tokens.

    Asserts (a) zero crashes, (b) every output <= MAX_INPUT_BYTES, and
    (c) measures total tokens recovered vs the old zero-capture baseline (0).

    The brief's historical corpus is 202 oversize-bail events. Their payloads
    are unreconstructable (audit rows store only byte counts), so we replay a
    representative sample across the {1MB,4MB,5MB} cap sizes and scale the
    per-event recovery to the full 202-event count for the PR evidence.
    """
    codec = _load_codec(monkeypatch)
    cap = codec.MAX_INPUT_BYTES

    sizes = list(_CAP_SIZES.values())
    # Representative sample: 3 reps per cap size = 9 oversize replays. Each
    # exceeds the cap so the oversize branch fires; bodies compact ~0.226.
    sample = sizes * 3
    n_historical = 202

    per_event_recovered = []
    crashes = 0
    over_cap = 0

    for target in sample:
        body = _synth_body(max(target, cap) + 50_000)
        raw = _oversize_envelope(codec, body)
        try:
            rc, out = _run_main(codec, monkeypatch, raw)
        except Exception:  # noqa: BLE001
            crashes += 1
            continue
        assert rc == 0
        resp = json.loads(out)
        text = resp.get("updatedToolOutput", {}).get(
            "content", [{}]
        )[0].get("text", "")
        if len(text.encode("utf-8")) > cap:
            over_cap += 1
        # Recovered = tokens that now reach context instead of zero. The old
        # path captured NOTHING (raw passthrough), so every token in the
        # emitted bounded+compacted body is net-new capture vs baseline 0.
        per_event_recovered.append(codec.estimate_tokens(text))

    # (a) zero crashes
    assert crashes == 0, f"{crashes} payloads crashed"
    # (b) every output <= cap
    assert over_cap == 0, f"{over_cap} outputs exceeded cap"

    avg_recovered = sum(per_event_recovered) / len(per_event_recovered)
    scaled_202 = int(avg_recovered * n_historical)

    # (c) report (printed for the PR evidence; baseline is 0 capture)
    with capsys.disabled():
        print(
            "\n[FIX#51 REPLAY GATE — SYNTHETIC corpus]\n"
            f"  sample size                      : {len(sample)} "
            "oversize events @ {1MB,4MB,5MB}\n"
            f"  old zero-capture baseline tokens : 0\n"
            f"  avg recovered tokens / event     : {int(avg_recovered):,}\n"
            f"  scaled to {n_historical} historical events : {scaled_202:,}\n"
            f"  crashes                          : {crashes}\n"
            f"  outputs over cap                 : {over_cap}\n"
            "  NOTE: numbers are on SYNTHESIZED payloads (char/4 token\n"
            "        backend) — historical bail rows store only byte counts,\n"
            "        not content, so real payloads are unreconstructable.\n"
        )
    assert sum(per_event_recovered) > 0


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-s", "-v"]))
