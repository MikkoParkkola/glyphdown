# Changelog

All notable changes to Glyphdown (formerly UltraCoS). Format: [Keep a Changelog]. Versioning: [SemVer].

> **Note:** the product was renamed from **UltraCoS** to **Glyphdown**. Historical
> release entries below intentionally retain the original `UltraCoS` / `ULTRACOS_*`
> names as they were shipped — they are a factual record and are not rewritten.

## [0.5.0]

### Added
- **Read section-extraction + Rewind retrieval** (F1) — large file-reads are
  compacted to a structural outline + every load-bearing anchor line + a head
  preview, with each dropped body run replaced by a marker carrying its exact
  retrieval range. The full original is stashed in a hash-addressed `rewind` store
  (`<data_dir>/rewind/`), recoverable byte-for-byte by id+range via the new
  `retrieve` MCP/CLI tool. Reversible-lossy, never lossy. Opt-in:
  `ULTRACOS_READ_EXTRACT=1` (default OFF).
- **State-aware Gate** (F2) — compression adapts to the agent's state: ULTRA
  collapses an exact repeat to one line; FULL backs OFF when the agent is stuck
  (same target failed >=2x and a fix was attempted) to preserve full signal;
  STANDARD is the normal path. Opt-in: `ULTRACOS_GATE=1` (default OFF).
- New subcommands: `extract-read <sid>`, `retrieve <sid> <id> [A-B]`.

### Notes
- Both features default OFF — the default codec path is byte-for-byte unchanged.
- Validated before build (fail-fast on real session data): read-extraction GO
  (12/13 reads >1K tok), Rewind essential (6/12 aggressive reads drop anchors),
  Gate GO (97 stuck states / 387 events). See internal-ref / internal-ref.

## [0.4.0]

### Added
- **Runtime-loadable dialect** — the ULTRACOS-L1 dialect is now a data file the
  binary loads at startup (`ULTRACOS_DIALECT`), with the compiled-in table as the
  bundled default and a lossless self-check on load. Customize compression with no
  rebuild. New `ultracos-core dialect-export` dumps the default as JSON to edit.
- **`compress-config`** — compress your static config (`CLAUDE.md`, skills, agent
  descriptions) with the active dialect: the system prompt ships on every request,
  so this is the only always-on saving. Dry-run by default, lossless-gated, backs
  up to `<file>.ultracos.bak`, refuses already-dense files.

### Changed
- **README** — hero metrics, three architecture diagrams, a general-vs-model-
  specific tokenizer section, a researched competitor landscape, a quickstart,
  and a FAQ.

## [0.3.0]

### Added
- **Audit-row observability** on the default Rust codec — savings-per-tool are
  recorded to a local append-only log so the codec's effect is measurable.

## [0.2.0]

### Added
- **Rust hot-path codec** — prebuilt binaries (macOS + Linux, arm64 + x86_64)
  run the PostToolUse codec natively (~5 ms vs ~170 ms Python), with Python as
  the fail-open fallback. Set `ULTRACOS_RUST=0` to force the Python path.
- **Session dedup (A8)** — repeated `Read`/`Grep`/`Glob` output is replaced with
  a short reference to its earlier occurrence.
- Binaries ship with a reproducible build script and SHA-256 checksums.

### Changed
- The PostToolUse codec defaults to the Rust binary.

## [0.1.0]

### Added
- Initial release: lossless tool-result codec (ANSI strip, JSON minify,
  blank-collapse, shape-aware compaction, path-list prefix folding, oversize
  truncation with anchor-survival guard, break-even gating, schema-tag prefix).
- Plugin hooks (PostToolUse codec, PreCompact, UserPromptSubmit, PreToolUse,
  SessionStart) — all fail-open.
- `ultracos-stats` command and a bundled benchmark corpus.

[Keep a Changelog]: https://keepachangelog.com/
[SemVer]: https://semver.org/
