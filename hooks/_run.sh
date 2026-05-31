#!/bin/sh
# UltraCoS hook runtime dispatcher.
#
# Two invocation forms:
#   1. python-only hook (7 of 8 hooks):
#        sh _run.sh <script.py> [args…]
#   2. rust-capable hook (PostToolUse codec):
#        sh _run.sh --rust <subcmd> <script.py> [args…]
#      where <subcmd> is the ultracos-core subcommand (e.g. posttooluse).
#
# Runtime selection order:
#   A. RUST FAST PATH (DEFAULT, opt out with ULTRACOS_RUST=0): a prebuilt
#      `ultracos-core` (~5ms vs ~170ms python).
#        - $ULTRACOS_BIN explicit override wins, else resolve
#          bin/<target-triple>/ultracos-core by `uname -s`/`uname -m`.
#        - exec the binary with <subcmd>; stdin/stdout pass through.
#        - The rust codec is proven equivalent to python on the bench corpus
#          (equiv_rust_vs_python.py 100%), the SIL-5 cache-bypass + internal-ref
#          anchor guards (equiv_guards_* all green), AND session-dedup (A8,
#          dedup-parity 104/104 + state-file interop). Dedup — the one ACTIVE
#          token-saver among the python features — is now on the rust path, so
#          the default flip no longer trades tokens for latency.
#        - Still NOT on the rust path (2b.2 remainder, all non-savers): SIL-2
#          learned skip-policy + min-payload/allowlist gates (these SKIP
#          compaction -> rust just compacts more), A/B no-tag experiment (~10%
#          cohort; rust ships the with-tag control), and the codec audit.jsonl
#          rows (telemetry; ultracos-stats reads the separate arc-event ledger).
#          Set ULTRACOS_RUST=0 for the full-featured python path.
#   B. $ULTRACOS_PYTHON — explicit fast-interpreter override (bypass slow pyenv
#      SHIMS; ~6x faster than a re-resolving shim).
#   C. python3 on PATH.
#
# Fail-open at every step: if the binary is missing / not executable / the
# platform is unsupported, fall through to python silently; the hook's own
# fail-open contract ({"continue":true} on any error) is preserved. exec
# replaces this shell so there is no added latency.

rust_subcmd=""
if [ "$1" = "--rust" ]; then
  rust_subcmd="$2"
  shift 2
fi

script="$1"
[ -n "$script" ] || { echo '{"continue":true}'; exit 0; }
shift

# ── A. Rust fast path (default ON; opt out with ULTRACOS_RUST=0) ─────────────
if [ -n "$rust_subcmd" ] && [ "$ULTRACOS_RUST" != "0" ]; then
  bin="$ULTRACOS_BIN"
  if [ -z "$bin" ]; then
    # Resolve the prebuilt binary by platform triple.
    root="${CLAUDE_PLUGIN_ROOT:-$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)}"
    os=$(uname -s 2>/dev/null)
    arch=$(uname -m 2>/dev/null)
    triple=""
    case "$os" in
      Darwin)
        case "$arch" in
          arm64|aarch64) triple="aarch64-apple-darwin" ;;
          x86_64)        triple="x86_64-apple-darwin" ;;
        esac ;;
      Linux)
        case "$arch" in
          aarch64|arm64) triple="aarch64-unknown-linux-gnu" ;;
          x86_64)        triple="x86_64-unknown-linux-gnu" ;;
        esac ;;
    esac
    if [ -n "$triple" ]; then
      cand="$root/bin/$triple/ultracos-core"
      [ -x "$cand" ] && bin="$cand"
    fi
  fi
  if [ -n "$bin" ] && [ -x "$bin" ]; then
    exec "$bin" "$rust_subcmd"
  fi
  # else: fall through to python (fail-open)
fi

# ── B/C. Python path ────────────────────────────────────────────────────────
if [ -n "$ULTRACOS_PYTHON" ] && [ -x "$ULTRACOS_PYTHON" ]; then
  exec "$ULTRACOS_PYTHON" "$script" "$@"
fi

exec python3 "$script" "$@"
