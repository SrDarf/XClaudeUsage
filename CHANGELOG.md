# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

This changelog covers the Rust rewrite on the `HighPerformanceXClaudeUsage`
branch. The original Node.js implementation lives on `main`.

## [Unreleased]

## [0.1.6] - 2026-06-11

Hardening release: a full-branch code review surfaced 18 issues; all are fixed
here.

### Fixed
- **Token loss:** a JSON `null` in any usage field dropped the whole assistant
  event (all its valid tokens with it); usage fields are now null-tolerant.
- **Token loss:** write transactions are `BEGIN IMMEDIATE` again (as the JS
  was), so concurrent Stop/SubagentStop hooks no longer fail with
  `SQLITE_BUSY_SNAPSHOT` and silently skip recording.
- **Double-count:** the cloud push cursor is read inside the enqueue
  transaction, closing a race where two concurrent hooks enqueued the same
  rows under fresh event UUIDs.
- **Data loss:** a short libsql pipeline response is now an error instead of
  being padded with empty results; outbox rows are only deleted after their
  INSERTs actually executed remotely. Unparseable outbox payloads are dropped
  deliberately with a log warning.
- **Double-count:** pulls exclude every device id this machine has ever synced
  under, so renaming `device_id` in `xclaude-cloud.json` no longer re-imports
  your own rows.
- Windows: re-install and uninstall now recognize our own
  `"...\xclaudeusage.exe" statusline` entries (the `.exe` broke the old
  substring match, wedging re-installs as "Foreign").
- Installer: a foreign command that merely embeds the words
  `xclaudeusage statusline` (e.g. a wrapper) is no longer claimed as ours and
  overwritten; matching is by program token now.
- Installer: answering "no" to cloud sync on re-install actually disables it —
  cloud-only hooks (SubagentStart/PostToolUse) are removed and
  `xclaude-cloud.json` is parked as `.json.disabled` (credentials kept and
  offered back on re-enable).
- `install.sh`: the binary is staged next to its destination and swapped with
  an atomic same-filesystem rename (a cross-filesystem `mv` could race a live
  session); the temp dir is cleaned up before the final `exec`.
- `install.ps1`: Windows on ARM fails with a clear "not pre-built" message
  instead of a raw 404.
- Statusline fallback: shares the recorder's subagent discovery (it previously
  counted stray `.jsonl` files and missed workflow transcripts) and the
  byte-level incremental reader (invalid UTF-8 could skew the saved offset and
  drop lines for the rest of the session); an empty `session_id` no longer
  collapses sessions onto one shared temp cache.
- AUR: `.SRCINFO` was stale at 0.1.4 while `PKGBUILD` said 0.1.5-2.

### Changed
- Cloud pulls are gated to once per 60 s — with cloud sync enabled,
  `PostToolUse` no longer pays a blocking network roundtrip (up to 5 s) on
  every tool call.
- The stdin self-timeout from the JS implementation is back (3 s statusline /
  8 s record), so a pipe that never closes can't hang hook processes.
- Hostname resolution reads `/proc/sys/kernel/hostname` first instead of
  trusting the `HOSTNAME` env var or spawning `hostname(1)` on every hook.

## [0.1.5] - 2026-06-11

### Added
- `mise` install path: `mise use -g ubi:SrDarf/XClaudeUsage` (pre-built binary)
  or `mise use -g cargo:xclaudeusage` (crates.io build).
- AUR packaging under `packaging/aur/` for an `xclaudeusage-bin` package
  ([xclaudeusage-bin](https://aur.archlinux.org/packages/xclaudeusage-bin)).
- `docs/ARCHITECTURE.md` covering the incremental parser, the 5-hour window
  model, subagent/workflow ingestion, and cloud idempotency.
- This changelog. Release notes are now generated from it instead of GitHub's
  auto-generated notes.

### Changed
- Internal quality pass (no behavior change): shared time helpers in a single
  module, parameterized `cloud_state` accessors, one DB connection per
  statusline render (was 3 opens + 2 migrations), a substring pre-filter
  before full JSON parsing of transcript lines, and dead-code removal.

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

[Unreleased]: https://github.com/SrDarf/XClaudeUsage/compare/v0.1.6...HighPerformanceXClaudeUsage
[0.1.6]: https://github.com/SrDarf/XClaudeUsage/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/SrDarf/XClaudeUsage/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/SrDarf/XClaudeUsage/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/SrDarf/XClaudeUsage/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/SrDarf/XClaudeUsage/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/SrDarf/XClaudeUsage/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/SrDarf/XClaudeUsage/releases/tag/v0.1.0
