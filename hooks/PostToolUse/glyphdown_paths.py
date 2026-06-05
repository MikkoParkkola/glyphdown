#!/usr/bin/env python3
"""Cross-platform path resolution for glyphdown data directories (internal-ref G16).

Handles platform-specific locations:
- Linux/macOS: ~/.ultracos (legacy name kept across the glyphdown brand
  rename so existing audit/cache state is not orphaned; migration-safe)
- Windows: %LOCALAPPDATA%/ultracos
- Env override: GLYPHDOWN_DATA_DIR always wins
"""

from __future__ import annotations

import os
import platform
from pathlib import Path


def glyphdown_data_dir() -> Path:
    r"""Resolve glyphdown data directory with cross-platform support.

    Priority:
    1. GLYPHDOWN_DATA_DIR env var (always wins)
    2. Windows: %LOCALAPPDATA%/ultracos (with fallback to ~\AppData\Local\ultracos)
    3. Linux/macOS: ~/.ultracos
    4. Ensures directory exists with mkdir(parents=True, exist_ok=True)

    Returns:
        Path: absolute path to glyphdown data directory
    """
    # Env override always wins
    if env_dir := os.environ.get("GLYPHDOWN_DATA_DIR"):
        path = Path(env_dir).expanduser().resolve()
        path.mkdir(parents=True, exist_ok=True)
        return path

    # Platform-specific defaults
    system = platform.system()
    if system == "Windows":
        # Windows: use LOCALAPPDATA with fallback
        localappdata = os.environ.get("LOCALAPPDATA")
        if localappdata:
            path = Path(localappdata) / "ultracos"
        else:
            # Fallback if LOCALAPPDATA not set (edge case)
            path = Path.home() / "AppData" / "Local" / "ultracos"
    else:
        # Linux, macOS, others: use ~/.ultracos
        path = Path.home() / ".ultracos"

    path.mkdir(parents=True, exist_ok=True)
    return path


def audit_file() -> Path:
    """Path to audit.jsonl."""
    return glyphdown_data_dir() / "audit.jsonl"


def tuned_thresholds_file() -> Path:
    """Path to tuned_thresholds.json."""
    return glyphdown_data_dir() / "tuned_thresholds.json"


def tool_policy_file() -> Path:
    """Path to tool_policy.json."""
    return glyphdown_data_dir() / "tool_policy.json"
