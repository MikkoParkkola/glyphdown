#!/usr/bin/env python3
"""internal-ref G1: Accurate token estimator using tiktoken o200k_base.

Replaces naive len(s) // 4 with tiktoken-based counting.
Fail-open: any exception in tiktoken path falls back to len(s)//4.
"""

import sys
from typing import Tuple

# Module-level encoder cache
_ENCODER = None
_ENCODER_LOADED = False


def _load_encoder():
    """Try to load tiktoken o200k_base encoder. Return encoder or None."""
    global _ENCODER, _ENCODER_LOADED
    if _ENCODER_LOADED:
        return _ENCODER

    try:
        import tiktoken  # noqa: F401
        _ENCODER = tiktoken.get_encoding("o200k_base")
        _ENCODER_LOADED = True
        return _ENCODER
    except Exception:  # noqa: BLE001
        # tiktoken not available or broken; cache None so we don't retry
        _ENCODER = None
        _ENCODER_LOADED = True
        return None


def count_tokens(s: str) -> Tuple[int, str]:
    """Count tokens in string using o200k_base if available, else fallback.

    Returns (token_count, backend_name) where backend_name is either
    "tiktoken-o200k" or "fallback-len4".

    Fail-open: any exception returns fallback with len(s)//4.
    """
    try:
        encoder = _load_encoder()
        if encoder is not None:
            # tiktoken succeeded; use it
            count = len(encoder.encode(s))
            return (max(1, count), "tiktoken-o200k")
    except Exception:  # noqa: BLE001
        pass

    # Fallback: len(s) // 4 (always succeeds)
    return (max(1, len(s) // 4), "fallback-len4")
