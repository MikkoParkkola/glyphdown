#!/bin/sh
# UltraCoS MCP server launcher.
#
# `mcpServers` config in plugin.json is a single static command, but the
# prebuilt `ultracos-core` binary is shipped per platform triple under
# bin/<triple>/. This thin launcher resolves the right binary by uname (the
# SAME logic as hooks/_run.sh) and execs it in `mcp` mode, turning the codec
# into an MCP server any client can drive over stdio.
#
# Resolution order:
#   1. $ULTRACOS_BIN explicit override.
#   2. bin/<target-triple>/ultracos-core by `uname -s`/`uname -m`.
#   3. `ultracos-core` on PATH (dev / cargo install).
#
# Fail-open: if no binary resolves, print a JSON-RPC-shaped note to STDERR
# (never stdout — stdout is the protocol channel) and exit non-zero so the
# client reports the server as unavailable rather than hanging.

bin="$ULTRACOS_BIN"

if [ -z "$bin" ]; then
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

# PATH fallback (dev builds / `cargo install`).
if [ -z "$bin" ] && command -v ultracos-core >/dev/null 2>&1; then
  bin="ultracos-core"
fi

if [ -z "$bin" ]; then
  echo "ultracos-mcp: no ultracos-core binary for $(uname -s)/$(uname -m); set ULTRACOS_BIN" >&2
  exit 127
fi

exec "$bin" mcp
