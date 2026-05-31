# `bin/` — prebuilt `ultracos-core` hot-path binaries

These four binaries let the ultracos PostToolUse codec run natively (~5 ms)
instead of via the python interpreter (~170 ms) on a fresh marketplace install,
with **no build step required** of the user.

| Triple | Platform | Built by |
|---|---|---|
| `aarch64-apple-darwin` | macOS Apple Silicon | native `cargo build` |
| `x86_64-apple-darwin` | macOS Intel | native `cargo build` |
| `aarch64-unknown-linux-gnu` | Linux arm64 | `cargo zigbuild` (zig linker) |
| `x86_64-unknown-linux-gnu` | Linux x86_64 | `cargo zigbuild` (zig linker) |

## Provenance (these are NOT opaque blobs)

The full source is in [`../ultracos-core/src/`](../ultracos-core/src). The
binaries are reproducible from it and their behavior is verified against the
python reference codec on every push.

- **Rebuild from source:** `sh build.sh` (installs the rustup targets, builds
  all four, refreshes `SHA256SUMS`).
- **Verify the shipped blobs:** `cd bin && shasum -a 256 -c SHA256SUMS`.
- **Verify behavior == python reference** (the real guarantee — byte-for-byte
  reproducibility is best-effort because rust release builds may embed toolchain
  paths even with `strip=true`):
  ```sh
  (cd ultracos-core && cargo test --release)
  python3 bench/equiv_rust_vs_python.py          # 2a transform parity: 52/52 = 100%
  python3 bench/equiv_guards_rust_vs_python.py   # 2b guard parity: signature 40/40, anchor 55/55, cache interop PASS
  ```
  CI (`.github/workflows/ultracos-tests.yml` → `rust-parity` job) runs exactly
  these on every push, so the binaries can never drift from the source behavior.

Toolchain for the committed binaries is recorded in the `SHA256SUMS` header
(rustc 1.95.0, cargo-zigbuild + zig 0.16.0, `lto=thin`, `strip=true`).

## Safety

The dispatcher ([`../hooks/_run.sh`](../hooks/_run.sh)) only execs a binary that
is present and executable, and the hook's own contract emits `{"continue":true}`
on any error — a missing, corrupt, or mismatched binary cannot break a session,
it simply falls back to the python codec. Set `ULTRACOS_RUST=0` to force python
unconditionally.
