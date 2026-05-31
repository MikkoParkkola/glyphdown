# Changelog

All notable changes to UltraCoS. Format: [Keep a Changelog]. Versioning: [SemVer].

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
