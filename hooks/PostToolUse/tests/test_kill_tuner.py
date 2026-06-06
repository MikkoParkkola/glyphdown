"""FIX #52: kill the dead p10 break-even tuner.

The per-tool / per-shape p10 break-even tuner (`glyphdown_tuned`) governed
only ±1.5% and was NEGATIVE on void samples; the static
``DEFAULT_BREAK_EVEN_TOKENS`` already captures 100%. #52 deletes the dead
machinery so break-even resolution collapses to ``env > DEFAULT`` — which,
because the env override is baked into ``DEFAULT_BREAK_EVEN_TOKENS`` at import
(``_int_env("GLYPHDOWN_BREAK_EVEN_TOKENS", 25)``), is just the constant.

These tests are the subtraction proof:

  1. Break-even resolves to ``DEFAULT_BREAK_EVEN_TOKENS`` with NO tuned file
     present (the operator's real state — the file never existed).
  2. Break-even resolves to ``DEFAULT_BREAK_EVEN_TOKENS`` even when a stray
     ``tuned_thresholds.json`` exists in the data dir — it MUST be inert.
  3. Regression guard: the normal compaction success path is byte-identical
     to a captured golden (same input → same compacted output) after the
     tuner removal.
  4. The deleted machinery is actually gone (no ``glyphdown_tuned`` import,
     no ``_resolve_threshold``, no ``BREAK_EVEN_ENV_PINNED``).

The conftest fixture redirects GLYPHDOWN_DATA_DIR + HOME to a tmp dir, so the
real ~/.ultracos is never touched and no stray file leaks to the operator.
"""

from __future__ import annotations

import importlib
import io
import json
import sys

import pytest


def _load_codec(monkeypatch, **env):
    """Import (or reload) glyphdown_codec with the given env applied first."""
    env.setdefault("GLYPHDOWN_FORCE_CHAR_TOKENS", "1")
    for k, v in env.items():
        monkeypatch.setenv(k, v)
    if "glyphdown_codec" in sys.modules:
        del sys.modules["glyphdown_codec"]
    return importlib.import_module("glyphdown_codec")


def _run_main(codec, stdin_text):
    """Drive codec.main() with stdin_text, capture stdout JSON."""
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


def _normal_envelope(body_text):
    """A normal (sub-cap) hook envelope with a compressible text payload."""
    return json.dumps({
        "tool_name": "Bash",
        "session_id": "fix52",
        "tool_response": {"content": [{"type": "text", "text": body_text}]},
    })


# A representative member of the "370-event class": ANSI-laden Bash output
# with collapsible trailing whitespace + blank runs — the lossless A1
# pipeline (ansi-strip + blank-collapse) yields a real, deterministic win.
_NORMAL_BODY = (
    "\x1b[32mPASS\x1b[0m test_one   \n\n\n\n"
    "\x1b[32mPASS\x1b[0m test_two   \n\n\n\n"
    "\x1b[31mFAIL\x1b[0m test_three \n\n\n\n"
) * 40


def _compacted_text(codec, body):
    rc, out = _run_main(codec, _normal_envelope(body))
    assert rc == 0
    resp = json.loads(out)
    assert "updatedToolOutput" in resp, "normal path must compact this body"
    return resp["updatedToolOutput"]["content"][0]["text"]


def test_break_even_resolves_to_default_no_file(monkeypatch):
    """With NO tuned file present, the absolute-token guard uses DEFAULT."""
    codec = _load_codec(monkeypatch)
    # The compaction success path runs the A10 guard at DEFAULT_BREAK_EVEN_TOKENS.
    # A body whose savings exceed DEFAULT compacts; this is the resolution proof.
    text = _compacted_text(codec, _NORMAL_BODY)
    assert text.startswith(codec.TAG_PREFIX)
    assert codec.DEFAULT_BREAK_EVEN_TOKENS == 25  # the 100%-capture constant


def test_stray_tuned_thresholds_file_is_inert(monkeypatch, tmp_path):
    """A stray tuned_thresholds.json must NOT change break-even resolution.

    The file's presence must be fully inert after #52. We create the stray
    inside the monkeypatched GLYPHDOWN_DATA_DIR (NOT ~/.ultracos), with
    aggressive per-tool + per-shape thresholds that, if honored, would
    suppress the compaction the default threshold allows. Output must be
    byte-identical to the no-file case.
    """
    # Baseline: output with no stray file.
    codec = _load_codec(monkeypatch)
    baseline = _compacted_text(codec, _NORMAL_BODY)

    # Plant a stray tuned file in the (tmp) data dir with thresholds high
    # enough to suppress compaction IF the dead tuner were still wired.
    import glyphdown_paths as paths
    stray = paths.glyphdown_data_dir() / "tuned_thresholds.json"
    stray.write_text(json.dumps({
        "per_tool": {"Bash": 100_000},
        "per_shape": {"text": 100_000, "code": 100_000},
    }))
    assert stray.exists()

    try:
        codec2 = _load_codec(monkeypatch)
        with_stray = _compacted_text(codec2, _NORMAL_BODY)
        # Inert: the stray must not change the compacted output at all.
        assert with_stray == baseline
        assert with_stray.startswith(codec2.TAG_PREFIX)
    finally:
        # Never leave the stray on disk (it is under tmp_path anyway).
        stray.unlink(missing_ok=True)
    assert not stray.exists()


def test_normal_path_byte_identical_golden(monkeypatch):
    """Regression guard: the normal success path matches a frozen golden.

    The golden was captured from the pre-#52 codec on this exact input.
    Byte-equality proves the tuner removal is behavior-preserving for the
    370-event normal compaction class.
    """
    codec = _load_codec(monkeypatch)
    text = _compacted_text(codec, _NORMAL_BODY)
    expected = (
        "[glyphdown:compact-v1 shape=text ratio=0.54 "
        "applied=ansi-strip,blank-collapse]\n"
        + ("PASS test_one\n\nPASS test_two\n\nFAIL test_three\n\n" * 40)
    )
    assert text == expected


def test_tuner_machinery_is_gone(monkeypatch):
    """The dead machinery must be fully removed (pure subtraction)."""
    codec = _load_codec(monkeypatch)
    src_path = codec.__file__
    with open(src_path, encoding="utf-8") as f:
        src = f.read()
    assert "glyphdown_tuned" not in src
    assert "_resolve_threshold" not in src
    assert "BREAK_EVEN_ENV_PINNED" not in src
    assert "load_shape_thresholds" not in src
    assert "load_thresholds" not in src
    # The tuner module itself must be deleted.
    with pytest.raises(ModuleNotFoundError):
        importlib.import_module("glyphdown_tuned")


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-s", "-v"]))
