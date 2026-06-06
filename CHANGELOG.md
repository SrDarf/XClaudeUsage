# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

This changelog covers the Rust rewrite on the `HighPerformanceXClaudeUsage`
branch. The original Node.js implementation lives on `main`.

## [Unreleased]

### Added
- `mise` install path: `mise use -g ubi:SrDarf/XClaudeUsage` (pre-built binary)
  or `mise use -g cargo:xclaudeusage` (crates.io build).
- AUR packaging under `packaging/aur/` for an `xclaudeusage-bin` package.
- `docs/ARCHITECTURE.md` covering the incremental parser, the 5-hour window
  model, subagent/workflow ingestion, and cloud idempotency.
- This changelog. Release notes are now generated from it instead of GitHub's
  auto-generated notes.

## [0.1.4] - 2026-06-06

### Added
- First release on [crates.io](https://crates.io/crates/xclaudeusage):
  `cargo install xclaudeusage`.

### Fixed
- `cargo publish` for 0.1.3 failed because `Cargo.lock` was not bumped alongside
  `Cargo.toml`; the lockfile is now kept in sync with the package version.

## [0.1.3] - 2026-06-06

> GitHub release binaries only. The crates.io publish for this version failed on
> a stale `Cargo.lock` and was superseded by 0.1.4.

### Added
- CI gate (`.github/workflows/ci.yml`): `cargo fmt --check`,
  `clippy -D warnings`, `cargo test`, and `cargo audit` on every push and PR.
- Unit tests for the core: the JSONL transcript parser, the 5-hour window
  summation, and the record ingestion path (9 to 35 tests).
- A guarded crates.io publish job in the release workflow.

## [0.1.2] - 2026-06-06

### Fixed
- Capture token usage from subagent and workflow transcripts (Task subagents,
  deep-research, and ultracode dynamic workflows). Claude Code writes these to a
  sibling `<session>/subagents/**` tree and never folds them into the main
  transcript, so they were previously uncounted.

## [0.1.1] - 2026-05-14

### Fixed
- Detect quoted binary paths in `settings.json` when classifying an existing
  status line.
- Dedupe duplicate XClaude hook entries on re-install.

## [0.1.0] - 2026-05-14

### Added
- Initial Rust rewrite: a single static binary (~3.5 MB, no runtime
  dependencies) replacing the three Node.js scripts.
- `statusline` subcommand: a token-usage and 5-hour quota bar that stays
  accurate across parallel sessions via a shared SQLite log.
- `record` subcommand: per-turn token recording into local SQLite (WAL,
  multi-session safe).
- Optional Turso cloud layer for multi-device sync (idempotent push/pull).
- Interactive installer (`xclaudeusage install`): non-destructive `settings.json`
  merge with a backup, SHA-256 verified downloads, and migration of the legacy
  Node hook entries.

[Unreleased]: https://github.com/SrDarf/XClaudeUsage/compare/v0.1.4...HighPerformanceXClaudeUsage
[0.1.4]: https://github.com/SrDarf/XClaudeUsage/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/SrDarf/XClaudeUsage/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/SrDarf/XClaudeUsage/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/SrDarf/XClaudeUsage/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/SrDarf/XClaudeUsage/releases/tag/v0.1.0
