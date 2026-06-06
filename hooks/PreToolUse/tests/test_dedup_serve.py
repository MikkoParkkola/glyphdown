"""Tests for glyphdown_history_dedup dedup-serve upgrade (task #53).

Behavior contract:
  - Default (gate OFF): byte-identical warn-only — never denies.
  - Gate ON: a duplicate Read with identical args inside the window is denied
    with a pointer (event=history-dedup-serve).
  - Gate ON but a write-capable tool ran after the prior read: NOT served
    (Read-after-Edit stays correct).
  - Gate ON: Bash duplicates are never served (time-varying / side effects).
  - Fail-open: malformed input → {"continue": true}.
"""

from __future__ import annotations

import importlib
import io
import json
import sys
import time
from contextlib import redirect_stdout


def _load_hook(monkeypatch, **env):
    """Set env, then (re)import the hook so import-time paths bind to tmp dir."""
    for k, v in env.items():
        monkeypatch.setenv(k, v)
    sys.modules.pop("glyphdown_history_dedup", None)
    return importlib.import_module("glyphdown_history_dedup")


def _run(hook, monkeypatch, payload):
    monkeypatch.setattr("sys.stdin", io.StringIO(json.dumps(payload)))
    buf = io.StringIO()
    with redirect_stdout(buf):
        rc = hook.main()
    return rc, json.loads(buf.getvalue())


def _read_payload(session="s1", path="/tmp/foo.txt"):
    return {
        "tool_name": "Read",
        "session_id": session,
        "tool_input": {"file_path": path},
    }


def _seed_prior(hook, monkeypatch, payload):
    """First call records the entry in the ring (no dup yet)."""
    rc, resp = _run(hook, monkeypatch, payload)
    assert rc == 0
    return resp


def test_default_gate_off_is_warn_only_never_denies(monkeypatch):
    hook = _load_hook(monkeypatch)  # gate unset
    p = _read_payload()
    _seed_prior(hook, monkeypatch, p)
    rc, resp = _run(hook, monkeypatch, p)  # duplicate
    assert rc == 0
    assert resp.get("continue") is True
    assert "hookSpecificOutput" not in resp
    # warn advisory still fires
    assert "additionalContext" in resp


def test_gate_on_duplicate_read_is_served(monkeypatch):
    hook = _load_hook(monkeypatch, GLYPHDOWN_DEDUP_SERVE="1")
    p = _read_payload()
    _seed_prior(hook, monkeypatch, p)
    rc, resp = _run(hook, monkeypatch, p)
    assert rc == 0
    hso = resp.get("hookSpecificOutput")
    assert hso is not None
    assert hso["permissionDecision"] == "deny"
    assert "Reuse" in hso["permissionDecisionReason"]


def test_gate_on_first_call_not_served(monkeypatch):
    hook = _load_hook(monkeypatch, GLYPHDOWN_DEDUP_SERVE="1")
    rc, resp = _run(hook, monkeypatch, _read_payload())
    assert rc == 0
    assert resp.get("continue") is True
    assert "hookSpecificOutput" not in resp


def test_gate_on_write_after_prior_read_invalidates_serve(monkeypatch):
    hook = _load_hook(monkeypatch, GLYPHDOWN_DEDUP_SERVE="1")
    p = _read_payload()
    _seed_prior(hook, monkeypatch, p)
    # An Edit to anything lands in the ring after the prior read.
    _run(hook, monkeypatch, {
        "tool_name": "Edit",
        "session_id": "s1",
        "tool_input": {"file_path": "/tmp/foo.txt"},
    })
    rc, resp = _run(hook, monkeypatch, p)  # re-read after the write
    assert rc == 0
    assert resp.get("continue") is True  # NOT served — correctness preserved
    assert "hookSpecificOutput" not in resp


def test_gate_on_bash_after_prior_read_invalidates_serve(monkeypatch):
    hook = _load_hook(monkeypatch, GLYPHDOWN_DEDUP_SERVE="1")
    p = _read_payload()
    _seed_prior(hook, monkeypatch, p)
    _run(hook, monkeypatch, {
        "tool_name": "Bash",
        "session_id": "s1",
        "tool_input": {"command": "echo hi > /tmp/foo.txt"},
    })
    rc, resp = _run(hook, monkeypatch, p)
    assert rc == 0
    assert resp.get("continue") is True
    assert "hookSpecificOutput" not in resp


def test_gate_on_bash_duplicate_never_served(monkeypatch):
    hook = _load_hook(monkeypatch, GLYPHDOWN_DEDUP_SERVE="1")
    bash = {
        "tool_name": "Bash",
        "session_id": "s1",
        "tool_input": {"command": "git status"},
    }
    _seed_prior(hook, monkeypatch, bash)
    rc, resp = _run(hook, monkeypatch, bash)  # identical Bash dup
    assert rc == 0
    assert resp.get("continue") is True  # Bash excluded from serve scope
    assert "hookSpecificOutput" not in resp


def test_served_event_written_to_audit(monkeypatch):
    hook = _load_hook(monkeypatch, GLYPHDOWN_DEDUP_SERVE="1")
    p = _read_payload()
    _seed_prior(hook, monkeypatch, p)
    _run(hook, monkeypatch, p)
    audit = hook._AUDIT_FILE.read_text(encoding="utf-8")
    events = [json.loads(ln)["event"] for ln in audit.splitlines() if ln.strip()]
    assert "history-dedup-serve" in events


def test_malformed_input_fails_open(monkeypatch):
    hook = _load_hook(monkeypatch, GLYPHDOWN_DEDUP_SERVE="1")
    monkeypatch.setattr("sys.stdin", io.StringIO("{not json"))
    buf = io.StringIO()
    with redirect_stdout(buf):
        rc = hook.main()
    assert rc == 0
    assert json.loads(buf.getvalue()) == {"continue": True}


def test_stale_duplicate_outside_window_not_served(monkeypatch):
    hook = _load_hook(monkeypatch, GLYPHDOWN_DEDUP_SERVE="1")
    p = _read_payload()
    # Hand-seed a ring entry older than the dedup window.
    old = time.time() - (hook._DEDUP_WINDOW_SECS + 60)
    hook._save_ring({"s1": [{
        "tool": "Read",
        "hash": hook._fnv1a_hash(hook._normalize_args(p["tool_input"])),
        "ts": old,
    }]})
    rc, resp = _run(hook, monkeypatch, p)
    assert rc == 0
    assert resp.get("continue") is True
    assert "hookSpecificOutput" not in resp


def _audit_events(hook, event):
    audit = hook._AUDIT_FILE.read_text(encoding="utf-8")
    return [
        json.loads(ln)
        for ln in audit.splitlines()
        if ln.strip() and json.loads(ln).get("event") == event
    ]


def test_warn_event_carries_saved_tokens_for_real_read(monkeypatch, tmp_path):
    """Default warn mode quantifies would-have-saved volume via os.stat proxy."""
    hook = _load_hook(monkeypatch)  # gate OFF — warn only
    real = tmp_path / "data.txt"
    real.write_bytes(b"x" * 4000)  # 4000 bytes -> ~1000 tokens est
    p = _read_payload(path=str(real))
    _seed_prior(hook, monkeypatch, p)
    _run(hook, monkeypatch, p)  # duplicate
    warns = _audit_events(hook, "history-dedup-warn")
    assert warns, "expected a warn audit event"
    assert warns[-1]["saved_bytes"] == 4000
    assert warns[-1]["saved_tokens_est"] == 1000


def test_serve_event_carries_saved_tokens(monkeypatch, tmp_path):
    hook = _load_hook(monkeypatch, GLYPHDOWN_DEDUP_SERVE="1")
    real = tmp_path / "data.txt"
    real.write_bytes(b"y" * 800)
    p = _read_payload(path=str(real))
    _seed_prior(hook, monkeypatch, p)
    _run(hook, monkeypatch, p)
    serves = _audit_events(hook, "history-dedup-serve")
    assert serves and serves[-1]["saved_tokens_est"] == 200


def test_missing_file_omits_size_fields_no_crash(monkeypatch):
    """os.stat failure is fail-open: warn still fires, no size keys."""
    hook = _load_hook(monkeypatch)
    p = _read_payload(path="/nonexistent/path/never.txt")
    _seed_prior(hook, monkeypatch, p)
    rc, resp = _run(hook, monkeypatch, p)
    assert rc == 0
    warns = _audit_events(hook, "history-dedup-warn")
    assert warns and "saved_bytes" not in warns[-1]


def test_bash_dup_has_no_size_estimate(monkeypatch):
    """Non-Read tools never carry a size estimate (no file_path proxy)."""
    hook = _load_hook(monkeypatch)
    bash = {
        "tool_name": "Bash",
        "session_id": "s1",
        "tool_input": {"command": "git status"},
    }
    _seed_prior(hook, monkeypatch, bash)
    _run(hook, monkeypatch, bash)
    warns = _audit_events(hook, "history-dedup-warn")
    assert warns and "saved_bytes" not in warns[-1]

