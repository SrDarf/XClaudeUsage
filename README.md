# XClaudeUsage

> High-performance Claude Code statusline that tracks token usage and the 5-hour quota in real time — stays accurate when you run multiple sessions in parallel, via a shared SQLite log. Optional Turso layer keeps the counter consistent across multiple machines.
>
> Ships as a single static binary (~3.5 MB, no runtime dependencies — SQLite and TLS are linked in). Drop-in compatible with the [SrDarf/XClaudeUsage](https://github.com/SrDarf/XClaudeUsage/tree/main) local DB and Turso schema, so existing setups upgrade in place without touching their data.

---

## What it shows

```
claude-sonnet-4-6 │ my-project │ ██████░░░░ out:427.0k/700.0k · 61% · resets:1h23m
```

| Segment | Description |
|---|---|
| Model | Active Claude model name |
| Directory | Current working directory |
| Progress bar | 5-hour session usage (`█` filled, `░` empty), color-coded |
| `out:X/Y` | Total output tokens used (summed across all parallel sessions) vs the 5-hour limit |
| `X%` | Percentage of the 5-hour window consumed |
| `resets:Xh##m` | Exact time remaining until the 5-hour window resets |

**Bar colors:**
- Green — under 50%
- Yellow — 50–64%
- Orange — 65–79%
- Red — 80%+

---

## Performance

Hooks fire on every Claude Code `Stop`, every `SubagentStop`, and at every statusline tick — so the per-invocation wall time and memory footprint are what you notice in practice. The same workload was run against this binary and the upstream Node implementation on a Linux x86_64 host, 100 iterations per row, fresh process per call (matches how Claude Code fires hooks):

### Wall time per invocation

| Operation | Node ([SrDarf/XClaudeUsage][srdarf]) | This binary |
|---|---:|---:|
| `statusline` (one tick) | ~104 ms | ~5 ms |
| `record` — Stop hook, local DB only | ~114 ms | ~5 ms |
| `record` — Stop hook, cloud sync warm | ~184 ms | ~8 ms |
| `record` — Stop hook, cloud cold-start (single shot)¹ | ~1247 ms | ~1015 ms |

### Peak resident set size (single invocation)

| Operation | Node ([SrDarf/XClaudeUsage][srdarf]) | This binary |
|---|---:|---:|
| `statusline` | ~55 MB | ~5 MB |
| `record` | ~70 MB | ~6 MB |

### Install footprint

| | Node ([SrDarf/XClaudeUsage][srdarf]) | This binary |
|---|---:|---:|
| Artifact on disk | 3 JS scripts (~44 KB) | one static binary (~3.5 MB) |
| Runtime requirement | Node.js 22.5+ (for `node:sqlite`) | none — SQLite + TLS linked in |

¹ Cold-start path measured through an HTTP shim that injects 1000 ms before forwarding to a local `libsql-server` container, approximating Turso's median cold-start latency. Both implementations include that same 1000 ms in their cold-start figure; the rest of the row is per-call overhead. Steady-state cloud calls land in the "warm" row above; the cold path is hit at most once per idle session.

[srdarf]: https://github.com/SrDarf/XClaudeUsage/tree/main

---

## Requirements

- Claude Code CLI
- Any 64-bit Linux, macOS, or Windows host. Pre-built binaries are published for `x86_64`/`aarch64` Linux + macOS and `x86_64` Windows.
- No Node.js, no Python, no system SQLite — the binary statically links SQLite and TLS.

---

## Quick install (automated)

macOS / Linux:
```bash
curl -fsSL https://raw.githubusercontent.com/SrDarf/XClaudeUsage/HighPerformanceXClaudeUsage/install.sh | sh
```

Windows (PowerShell):
```powershell
irm https://raw.githubusercontent.com/SrDarf/XClaudeUsage/HighPerformanceXClaudeUsage/install.ps1 | iex
```

The shell script:
- Detects your OS and architecture.
- Downloads the latest `xclaudeusage` binary from GitHub Releases (verifying its SHA-256 against the published `SHA256SUMS` manifest).
- Drops it at `~/.claude/bin/xclaudeusage`.
- Hands off to `xclaudeusage install`, which is interactive: it merges `statusLine` + the required `hooks` entries into your existing `~/.claude/settings.json` **without overwriting anything that isn't XClaude's**, and optionally writes the Turso cloud config. A timestamped backup of `settings.json` is taken whenever the file content actually changes.

Pin a specific release by setting `XCLAUDEUSAGE_VERSION=v0.1.0` (or any tag) before the curl pipe.

The installer is interactive even when invoked through `curl | sh` — it opens the controlling TTY (`/dev/tty` on Unix, `\\.\CON` on Windows) directly.

**Re-running the installer is safe:** existing XClaude entries in `settings.json` are detected (including the legacy `xclaude-*.js` paths from the Node version) and updated in place. After a successful migration, the installer offers to delete the orphaned `~/.claude/hooks/xclaude-*.js` files.

It aborts cleanly without writing anything if:
- A non-XClaude `statusLine` is already configured.
- `settings.json` is malformed.
- The release download fails or its checksum doesn't match.

Restart Claude Code in a fresh session to pick up the changes.

> **First-run tip:** install (or upgrade) right before starting a **fresh** Claude Code session. On its first invocation, `xclaudeusage record` stamps every existing transcript message it finds with `executed_at = now`. If you install mid-session, all past messages of that session get back-filled into the current 5-hour window and the count will be inflated. A virgin session starts clean.

---

## Install (manual)

If you'd rather inspect every step, or are an AI agent walking through programmatically:

**1. Download the binary**

Grab the right archive for your platform from [the latest release](https://github.com/SrDarf/XClaudeUsage/releases/latest):

| Target | Archive |
|---|---|
| Linux x86_64 | `xclaudeusage-x86_64-unknown-linux-gnu.tar.gz` |
| Linux aarch64 | `xclaudeusage-aarch64-unknown-linux-gnu.tar.gz` |
| macOS Apple Silicon | `xclaudeusage-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 | `xclaudeusage-x86_64-pc-windows-msvc.zip` |

> **Intel Mac users:** there's no pre-built binary because GitHub's `macos-13` runners are unreliable for releases. Use `cargo install` (see below) — it builds cleanly under Rosetta or natively on Intel.

Extract `xclaudeusage` (or `xclaudeusage.exe`) and place it at `~/.claude/bin/xclaudeusage` (Linux/macOS) or `%USERPROFILE%\.claude\bin\xclaudeusage.exe` (Windows). Make it executable: `chmod +x ~/.claude/bin/xclaudeusage`.

**2. Run the interactive configurator**

```bash
~/.claude/bin/xclaudeusage install
```

It does the same thing as step 2 of the automated installer above.

**3. Restart Claude Code**

Done.

### Alternative: `cargo install`

If you have a Rust toolchain (≥ 1.74), build from source:

```bash
cargo install --git https://github.com/SrDarf/XClaudeUsage --branch HighPerformanceXClaudeUsage --locked
```

The binary lands at `~/.cargo/bin/xclaudeusage`. Symlink it into `~/.claude/bin/` (or update `settings.json` to point at the cargo location) and run `xclaudeusage install`.

---

## Multi-session

Run several Claude Code terminals in parallel **on the same machine** and the statusline in every one of them shows the same total — the sum of all sessions' tokens within the 5-hour window. The shared SQLite file at `~/.claude/data/xclaude-usage.db` is the source of truth; sessions write independently and never block each other (WAL mode + `busy_timeout = 5000`). No extra setup beyond the base install above.

---

## Multi-device sync (opt-in)

> **You only need this if you run Claude Code on two or more machines at the same time.** If you stick to a single device, skip this entire section — the base install already gives you a fully accurate counter and there is no benefit to enabling the cloud layer.
>
> **If you do use multiple devices in parallel, this is strongly recommended.** Anthropic's `rate_limits.five_hour.used_percentage` is account-wide and stays correct without sync, but `out:X` is the local token sum on the current machine — and the displayed limit `Y` is computed as `X ÷ used_percentage`, so it shrinks proportionally with X. Without cloud sync, every machine sees an `out:X/Y` smaller than reality, and the gap grows with how much you use the other devices. Enabling sync makes `X` the cross-device total and brings `Y` (the derived 5-hour limit) back to its true value.

When enabled, every device pushes one aggregated row per `Stop` / `SubagentStop` to a managed Turso (libSQL) database, and pulls the others' rows on `Stop` / `PostToolUse`. There is no intermediate server to host — the binary talks to Turso's HTTP API directly. Local data remains the source of truth; if Turso is unreachable, syncing pauses without breaking the statusline.

What does and does not leave the device:
- **Sent:** `device_id`, `model`, `event_type`, the four token counts, `executed_at`, `event_id`.
- **Never sent:** transcript content, prompts, tool inputs/outputs, session ids.

### A. Create the shared database (once, on any device)

You only do this **once total**, on any machine — the database it creates is shared across all your devices.

```bash
# Install the Turso CLI (skip if you already have it)
curl -sSfL https://get.tur.so/install.sh | bash
source ~/.bashrc

# Authenticate and provision
turso auth login
turso db create xclaude-usage
turso db shell xclaude-usage < cloud/schema.sql

# Generate a token and grab the URL — keep both for step B
turso db tokens create xclaude-usage --expiration none
turso db show xclaude-usage --url
```

Save the URL and the token; you will paste them into the cloud config on each device that should participate (including the one you ran this on).

### B. Add a device to the shared database (run on each device)

Run `xclaudeusage install` and answer **yes** to the cloud sync prompt — it asks for the URL, token, and a device label, then writes them to `~/.claude/data/xclaude-cloud.json` and registers the extra `SubagentStart`/`PostToolUse` hooks for you.

If you'd rather edit the file by hand:

```bash
mkdir -p ~/.claude/data
cat > ~/.claude/data/xclaude-cloud.json <<'EOF'
{
  "libsql_url": "<paste URL from step A>",
  "auth_token": "<paste token from step A>",
  "device_id": "this-device-name"
}
EOF
```

`device_id` is a short label unique to this machine (`luka-laptop`, `luka-desktop`, …). The same URL and token are used everywhere; only the label changes. The URL accepts both the `libsql://` form (Turso CLI default) and the equivalent `https://` form.

Restart Claude Code in a fresh session. After the next turn, verify from any machine:

```bash
turso db shell xclaude-usage "SELECT device_id, output, datetime(executed_at,'unixepoch','localtime') AS ts FROM token_delta ORDER BY id DESC LIMIT 10"
```

You should see one row per turn, tagged with the `device_id` of whichever machine produced it.

### Disabling

Rename or delete `~/.claude/data/xclaude-cloud.json` on the device you want to take offline. The binary immediately stops syncing, the statusline reverts to local-only aggregation, no errors are logged. The shared cloud database is untouched. Re-create the file later to resume.

---

## How it works

One Rust binary, three subcommands wired into Claude Code as hooks:

- **`xclaudeusage record`** runs on `Stop`, `SubagentStop`, and (when cloud sync is enabled) also on `SubagentStart` and `PostToolUse`. On every fire it reads new entries from the session transcript JSONL (incrementally, by stored byte offset) and inserts one row per token type (`input`, `output`, `cache_creation`, `cache_read`) into `~/.claude/data/xclaude-usage.db`. On `Stop` and `SubagentStop`, if cloud sync is configured, it also aggregates every unpushed local row into one delta per model and ships it to Turso; on `Stop` and `PostToolUse` it pulls the latest deltas from other devices into a local `cloud_cache` table.
- **`xclaudeusage statusline`** runs on every statusline tick. It sums local `token_usage` (per-type rows) plus `cloud_cache` (per-event rows from other devices) for the current 5-hour window (`[resets_at - 5h, resets_at)`), then derives the real output limit from Anthropic's `rate_limits.five_hour.used_percentage` — no hardcoded values.
- **`xclaudeusage install`** is run once on each device. It writes the `statusLine` + `hooks` entries into `~/.claude/settings.json`, optionally writes the Turso config, and migrates any leftover hook entries from the legacy Node version.

Resilience:
- WAL mode + `busy_timeout = 5000` handle multiple sessions writing concurrently.
- Local idempotency: unique index on `(message_uuid, token_type)` — re-firing the same hook is a no-op.
- Cloud idempotency: unique constraint on `event_id` in Turso — retries don't duplicate.
- Network failures during cloud sync leave entries in `cloud_outbox`; the next push event drains them.
- The exact reset countdown comes from `rate_limits.five_hour.resets_at`.
- Hook errors never bubble up: any failure inside `xclaudeusage record` or `statusline` is swallowed and logged to `~/.claude/data/xclaude-usage.log` (rotated at 1MB) so it can't break your Claude Code session.

Retention: `token_usage`, `cloud_cache`, and the Turso `token_delta` table all keep 15 days of history. Cleanup is opportunistic — at most once every 24h on `Stop`/`SubagentStop`/`SubagentStart`/`PostToolUse`.

---

## Files

| File | Purpose |
|---|---|
| `Cargo.toml` | Rust crate manifest |
| `src/main.rs`, `src/cli.rs` | clap-based CLI dispatcher |
| `src/record.rs` | `xclaudeusage record` — Stop / SubagentStop / SubagentStart / PostToolUse hook |
| `src/statusline.rs` | `xclaudeusage statusline` — reads local + cloud-cached tokens for the 5h window |
| `src/install/` | `xclaudeusage install` — interactive settings.json merger, JS→Rust migration, Turso config prompts |
| `src/db/`, `src/transcript.rs` | SQLite schema + incremental JSONL parser |
| `src/cloud/` | Turso (libSQL) HTTP `/v2/pipeline` client + push/pull/cleanup |
| `cloud/schema.sql` | Turso schema for the shared `token_delta` table |
| `install.sh`, `install.ps1` | Bootstrap scripts that download a release binary and exec `xclaudeusage install` |

---

## License

MIT
