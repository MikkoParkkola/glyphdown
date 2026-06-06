"""Test isolation for glyphdown hook tests.

Redirects the glyphdown data dir (audit.jsonl, tuned_thresholds.json, etc.)
and HOME to a per-test tmp directory so the real ~/.ultracos/audit.jsonl is
NEVER touched and no tuned_thresholds.json is ever created in the operator's
data dir.

GLYPHDOWN_DATA_DIR wins in glyphdown_paths.glyphdown_data_dir(); HOME is
redirected too because the per-session audit path uses Path.home() directly.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

# Make the hook modules importable (they live one dir up, flat layout).
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
    # Determinism: ensure no env-pin leaks into break-even resolution tests.
    monkeypatch.delenv("GLYPHDOWN_BREAK_EVEN_TOKENS", raising=False)
    yield data_dir
