# UltraCoS — token-savings codec for Claude Code

UltraCoS is a Claude Code plugin that compresses tool-result output before it
enters the model's context, cutting token cost with no change to what the agent
can see. It is **lossless by default**, **fail-open** (any error passes the
original output through untouched), and runs as a prebuilt native binary on the
hot path with a Python fallback.

## What it does

On every tool result, UltraCoS applies safe, reversible-in-meaning transforms:

- **ANSI strip · JSON minify · blank-collapse · trailing-whitespace trim** —
  mechanical, lossless cleanups.
- **Shape-aware compaction** — JSON, YAML/TOML, code, and filesystem path-lists
  each get the transform that suits them; path-lists are folded by common prefix.
- **Oversize truncation with an anchor-survival guard** — large outputs are
  tail-trimmed, but a compression that would drop the one `file:line` or error
  code that made the output useful is automatically reverted.
- **Session dedup** — a repeated `Read`/`Grep`/`Glob` result is replaced with a
  short reference to its earlier occurrence.
- **Break-even gating** — compaction only applies when it actually saves enough
  tokens to be worth a schema tag.

Everything is gated so the codec can reduce tokens but never corrupt an agent's
context.

## Install

```sh
claude plugin marketplace add MikkoParkkola/ultracos
claude plugin install ultracos
```

Hooks fire automatically on the next session. Inspect savings with the
`ultracos-stats` command.

## The Rust hot path

The codec ships as prebuilt binaries under [`bin/<triple>/`](bin/) (macOS and
Linux, arm64 and x86_64) and runs by default — roughly `5 ms` versus `~170 ms`
for the Python interpreter, with identical compression. The Python codec is the
portable fallback (unsupported platform, missing binary, `ULTRACOS_RUST=0`).
Every path is fail-open. Binaries are reproducible from the in-repo source via
[`bin/build.sh`](bin/build.sh) and verified by `bin/SHA256SUMS`.

See [`ultracos-core/`](ultracos-core/) for the codec source — it is fully open;
read every line. Configuration lives in the env-var table inside the codec docs.

## Calibration: open results, kept-fresh by a private loop

The codec's keep-vs-compress boundary depends on a token estimate. UltraCoS ships
a small, signed **calibration snapshot** that tunes that estimate to the model's
real tokenizer. The snapshot is open data — inspect it, use it, fork it — but it
is **kept fresh by a private learning loop** that measures the live tokenizer
from real traffic. When a model's tokenizer changes, our loop re-fits and ships
an updated snapshot; a frozen copy keeps working but stops tracking the change.
You enjoy the result without needing to reproduce how it is made.

## License

**PolyForm Noncommercial License 1.0.0** — free for any noncommercial use.
**Commercial use requires a separate paid license**; contact
**mikko.parkkola@iki.fi**. See [LICENSE](LICENSE).

UltraCoS is free today. If it proves its worth, the project may move to a paid
subscription for commercial users; noncommercial use stays free.
