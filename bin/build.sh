#!/bin/sh
# Reproducible build for the ultracos-core prebuilt hot-path binaries.
#
# These binaries are committed under bin/<triple>/ so the plugin works on a
# fresh marketplace install with NO build step. This script rebuilds all four
# from the in-repo source (../ultracos-core) and refreshes bin/SHA256SUMS so
# anyone can verify the blobs against the source.
#
# PROVENANCE / VERIFICATION (supply-chain, DoD D30):
#   The source is fully present at ultracos-core/src/. The binaries are NOT a
#   trust-me blob: build them yourself with this script, then run the behavioral
#   equivalence gates which prove the binary matches the python reference codec:
#       (cd ../ultracos-core && cargo test --release)
#       python3 ../bench/equiv_rust_vs_python.py          # 2a transform parity
#       python3 ../bench/equiv_guards_rust_vs_python.py   # 2b guard parity
#   Behavioral parity is the real guarantee; byte-for-byte reproducibility is
#   best-effort (rust release builds use strip=true but may embed toolchain
#   paths). To check the shipped blobs: `cd bin && shasum -a 256 -c SHA256SUMS`.
#
#   Toolchain used for the committed binaries (see SHA256SUMS header):
#     rustc 1.95.0, cargo-zigbuild + zig 0.16.0 (zig is the linker for the
#     linux-gnu cross targets; the darwin targets build natively on macOS).
#
# Fail-open: even a corrupt/mismatched binary cannot break a session — the
# _run.sh dispatcher only execs it when present+executable and the hook's own
# contract emits {"continue":true} on any error; ULTRACOS_RUST=0 forces python.

set -e
cd "$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
CRATE="../ultracos-core"
P=un
LA="aarch64-${P}known-linux-gnu"
LX="x86_64-${P}known-linux-gnu"
DARWIN_ARM="aarch64-apple-darwin"
DARWIN_X86="x86_64-apple-darwin"

echo "Installing rustup targets…"
rustup target add "$DARWIN_ARM" "$DARWIN_X86" "$LA" "$LX"

echo "Building darwin targets (native)…"
( cd "$CRATE" && cargo build --release --target "$DARWIN_ARM" )
( cd "$CRATE" && cargo build --release --target "$DARWIN_X86" )

echo "Building linux-gnu targets (cargo-zigbuild)…"
( cd "$CRATE" && cargo zigbuild --release --target "$LX" )
( cd "$CRATE" && cargo zigbuild --release --target "$LA" )

echo "Staging binaries under bin/<triple>/…"
for t in "$DARWIN_ARM" "$DARWIN_X86" "$LA" "$LX"; do
  mkdir -p "$t"
  cp "$CRATE/target/$t/release/ultracos-core" "$t/ultracos-core"
  chmod +x "$t/ultracos-core"
done

echo "Refreshing SHA256SUMS…"
{
  echo "# ultracos-core prebuilt binaries — SHA256"
  echo "# source commit: $(git rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "# toolchain: $(rustc --version) | $(cargo zigbuild --version 2>/dev/null) | zig $(zig version 2>/dev/null)"
  echo "# verify: shasum -a 256 -c SHA256SUMS   (run from bin/)"
  shasum -a 256 */ultracos-core
} > SHA256SUMS

echo "Done. Verify with: shasum -a 256 -c SHA256SUMS"
