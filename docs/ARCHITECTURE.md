# Architecture

XClaudeUsage is a single Rust binary with a few subcommands wired into Claude
Code hooks. This document records the design decisions that are not obvious from
the code; the per-file map lives in the [README](../README.md) under "Files".

## Data flow

Claude Code fires hooks (`Stop`, `SubagentStop`, `PostToolUse`, ...) and renders
the status line on every tick. Each invocation is a fresh, short-lived process:

- `xclaudeusage record` (hooks) reads a JSON payload on stdin, ingests new
  transcript tokens into local SQLite, and optionally syncs to Turso.
- `xclaudeusage statusline` reads a JSON payload on stdin and prints one line:
  model, directory, and the 5-hour usage bar.

Because every call is a separate process, the on-disk SQLite log is the only
shared state. There is no daemon.

## Incremental transcript parsing (`src/transcript.rs`)

Transcripts are append-only JSONL. Re-reading a multi-megabyte file on every
hook would be wasteful, so each transcript path stores a byte offset in
`transcript_progress`. `read_new(path, offset)`:

- Seeks to `offset` and reads to EOF.
- Returns only up to the last newline, so a half-written final line is never
  parsed; its bytes stay unconsumed until a later call completes the line.
- If the file is smaller than `offset` (transcript recreated or truncated), it
  resets to 0 and re-reads from the start.

`parse_assistant_events` skips any line that is not a well-formed `assistant`
message carrying a `usage` block, so malformed or partial lines are ignored
rather than fatal.

## Subagent and workflow transcripts (`src/record.rs`)

Task subagents, deep-research, and ultracode dynamic workflows write their own
`agent-*.jsonl` files under a sibling `<session>/subagents/**` tree (including
`subagents/workflows/wf_*/`). Claude Code never folds these into the main
transcript and never hands the hook their paths, so `record` discovers them by
walking that tree (with bounded recursion) and ingests each with its own offset.

Rows are deduped by a unique `(message_uuid, token_type)` index with
`INSERT OR IGNORE`, so re-ingesting an overlapping range can never double-count.

## The 5-hour window (`src/statusline.rs`)

Claude's quota resets on a rolling 5-hour window. The payload carries
`resets_at`; the window is the half-open interval `[resets_at - 5h, resets_at)`.
`query_window_tokens` sums `token_usage` (local) plus `cloud_cache` (synced from
other devices) over exactly that interval. The boundary is half-open on purpose:
a row stamped exactly at `resets_at` belongs to the next window, not this one.

When the payload reports `used_percentage`, the displayed limit is back-solved as
`output / (used_percentage / 100)`, guarded against divide-by-zero. When the DB
is unavailable, a legacy per-session transcript parser provides a fallback count.

## Cloud sync and idempotency (`src/cloud/`)

Multi-device sync is opt-in (Turso). Pushes go through an outbox:

- Local events are grouped per model into `cloud_outbox` rows, each with a UUID
  `event_id`.
- The remote `token_delta` table uses `event_id` as an idempotency key, so a
  retried or duplicated push is a no-op (`INSERT OR IGNORE`).
- A push cursor (`last_pushed_id`) advances past every scanned row, including
  rows outside the retention window, so out-of-window rows are not rescanned.
- Pulls are cached locally in `cloud_cache` and merged into the window sum.

## Retention

Both `token_usage` and `cloud_cache` are pruned to a 15-day window. Cleanup is
opportunistic and gated to once per 24h (`last_cleanup_at`), so it never adds
latency to the common hook path.

## Concurrency

Several Claude Code sessions can run in parallel on one machine, each writing on
its own hook. SQLite runs in WAL mode with `busy_timeout = 5000` (plus a
readonly handle for the status line), so writers do not block each other and the
status line never blocks a writer.
