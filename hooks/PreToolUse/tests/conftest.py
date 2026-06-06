"""Test isolation for glyphdown PreToolUse hook tests.

Redirects GLYPHDOWN_DATA_DIR and HOME to a per-test tmp dir so the real
~/.ultracos/{audit.jsonl,history_ring.json} is NEVER touched. The hook
captures _RING_FILE / _AUDIT_FILE at import time via _data_dir(), so tests
that need a specific data dir must set env BEFORE (re)importing the module
(see _load_hook in test_dedup_serve.py).
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

# Make the hook module importable (flat layout, one dir up).
HOOK_DIR = Path(__file__).resolve().parent.parent
if str(HOOK_DIR) not in sys.path:
    sys.path.insert(0, str(HOOK_DIR))


@pytest.fixture(autouse=True)
def _isolated_data_dir(tmp_path, monkeypatch):
    """Point all glyphdown filesystem writes at a tmp dir for every test."""
    data_dir = tmp_path / "ultracos"
    data_dir.mkdir(parents=True, exist_ok=True)
    home_dir = tmp_path / "home"
    home_dir.mkdir(parents=True, exist_ok=True)
    monkeypatch.setenv("GLYPHDOWN_DATA_DIR", str(data_dir))
    monkeypatch.setenv("HOME", str(home_dir))
    monkeypatch.delenv("GLYPHDOWN_DEDUP_SERVE", raising=False)
    yield data_dir
