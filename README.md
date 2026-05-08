# XClaudeUsage

> Claude Code statusline that tracks token usage and the 5-hour quota in real time — stays accurate when you run multiple sessions in parallel, via a shared SQLite log. Optional Turso layer keeps the counter consistent across multiple machines.

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

## Requirements

- Node.js 22.5+ (for built-in `node:sqlite`; older versions still work in single-session legacy mode)
- Claude Code CLI

---

## Install

**1. Download both hooks**

macOS/Linux:
```bash
mkdir -p ~/.claude/hooks
curl -o ~/.claude/hooks/xclaude-usage.js \
  https://raw.githubusercontent.com/SrDarf/XClaudeUsage/main/xclaude-usage.js
curl -o ~/.claude/hooks/xclaude-record.js \
  https://raw.githubusercontent.com/SrDarf/XClaudeUsage/main/xclaude-record.js
```

Windows (PowerShell):
```powershell
New-Item -ItemType Directory -Force "$env:USERPROFILE\.claude\hooks" | Out-Null
curl -o "$env:USERPROFILE\.claude\hooks\xclaude-usage.js" `
  https://raw.githubusercontent.com/SrDarf/XClaudeUsage/main/xclaude-usage.js
curl -o "$env:USERPROFILE\.claude\hooks\xclaude-record.js" `
  https://raw.githubusercontent.com/SrDarf/XClaudeUsage/main/xclaude-record.js
```

---

**2. Register the base hooks in `settings.json`**

Open `~/.claude/settings.json` (Windows: `%USERPROFILE%\.claude\settings.json`) and add the `statusLine` plus the `Stop` and `SubagentStop` hooks. This is everything you need for single-device use:

```json
{
  "statusLine": {
    "type": "command",
    "command": "node \"/Users/yourname/.claude/hooks/xclaude-usage.js\""
  },
  "hooks": {
    "Stop": [
      { "hooks": [{ "type": "command", "command": "node \"/Users/yourname/.claude/hooks/xclaude-record.js\"", "timeout": 10 }] }
    ],
    "SubagentStop": [
      { "hooks": [{ "type": "command", "command": "node \"/Users/yourname/.claude/hooks/xclaude-record.js\"", "timeout": 10 }] }
    ]
  }
}
```

Windows example for the command field:
```json
"command": "node \"C:/Users/yourname/.claude/hooks/xclaude-record.js\""
```

> If `settings.json` already exists with other settings, just merge these keys alongside them — do not replace the whole file.

---

**3. Restart Claude Code**

The statusline appears automatically on the next session.

> **First-run tip:** install (or upgrade) right before starting a **fresh** Claude Code session — ideally run the very first turn of a brand-new session after installing. On its first invocation, `xclaude-record.js` stamps every existing transcript message it finds with `executed_at = now`. If you install mid-session, all past messages of that session get back-filled into the current 5-hour window and the count will be inflated. A virgin session has no history to back-fill, so the counter starts clean.

---

## Multi-session

Run several Claude Code terminals in parallel **on the same machine** and the statusline in every one of them shows the same total — the sum of all sessions' tokens within the 5-hour window. The shared SQLite file at `~/.claude/data/xclaude-usage.db` is the source of truth; sessions write independently and never block each other. No extra setup beyond the base install above.

---

## Multi-device sync (opt-in)

> **You only need this if you run Claude Code on two or more machines at the same time.** If you stick to a single device, skip this entire section — the base install already gives you a fully accurate counter and there is no benefit to enabling the cloud layer.
>
> **If you do use multiple devices in parallel, this is strongly recommended.** Anthropic's `rate_limits.five_hour.used_percentage` is account-wide and stays correct without sync, but `out:X` is the local token sum on the current machine — and the displayed limit `Y` is computed as `X ÷ used_percentage`, so it shrinks proportionally with X. Without cloud sync, every machine sees an `out:X/Y` smaller than reality, and the gap grows with how much you use the other devices. Enabling sync makes `X` the cross-device total and brings `Y` (the derived 5-hour limit) back to its true value.

When enabled, every device pushes one aggregated row per `Stop` / `SubagentStop` to a managed Turso (libSQL) database, and pulls the others' rows on `Stop` / `PostToolUse`. There is no intermediate server to host — the hook talks to Turso's HTTP API directly. Local data remains the source of truth; if Turso is unreachable, syncing pauses without breaking the statusline.

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

Save the URL and the token; you will paste them into a config file on each device that should participate (including the one you ran this on).

### B. Add a device to the shared database (run on each device)

Run on every machine that should report into the shared counter — including the one where you ran step A.

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

Then add two more hook entries to `~/.claude/settings.json` so the statusline pulls fresh data during long turns and so subagent boundaries are also captured. Merge them alongside the existing `Stop` and `SubagentStop`:

```json
"hooks": {
  "Stop":          [ /* unchanged */ ],
  "SubagentStop":  [ /* unchanged */ ],
  "SubagentStart": [
    { "hooks": [{ "type": "command", "command": "node \"/Users/yourname/.claude/hooks/xclaude-record.js\"", "timeout": 10 }] }
  ],
  "PostToolUse": [
    { "hooks": [{ "type": "command", "command": "node \"/Users/yourname/.claude/hooks/xclaude-record.js\"", "timeout": 10 }] }
  ]
}
```

Restart Claude Code in a fresh session. After the next turn, verify from any machine:

```bash
turso db shell xclaude-usage "SELECT device_id, output, datetime(executed_at,'unixepoch','localtime') AS ts FROM token_delta ORDER BY id DESC LIMIT 10"
```

You should see one row per turn, tagged with the `device_id` of whichever machine produced it.

### Disabling

Rename or delete `~/.claude/data/xclaude-cloud.json` on the device you want to take offline. Hooks immediately stop syncing, the statusline reverts to local-only aggregation, no errors are logged. The shared cloud database is untouched. Re-create the file later to resume.

---

## How it works

Two cooperating hooks share state via a local SQLite database, with an optional cloud layer for multi-device aggregation:

- **`xclaude-record.js`** runs on `Stop`, `SubagentStop`, and (when cloud sync is enabled) also on `SubagentStart` and `PostToolUse`. On every fire it reads new entries from the session transcript JSONL (incrementally, by stored byte offset) and inserts one row per token type (`input`, `output`, `cache_creation`, `cache_read`) into `~/.claude/data/xclaude-usage.db`. On `Stop` and `SubagentStop`, if cloud sync is configured, it also aggregates every unpushed local row into one delta per model and ships it to Turso; on `Stop` and `PostToolUse` it pulls the latest deltas from other devices into a local `cloud_cache` table.
- **`xclaude-usage.js`** runs on every statusline tick. It sums local `token_usage` (per-type rows) plus `cloud_cache` (per-event rows from other devices) for the current 5-hour window (`[resets_at - 5h, resets_at)`), then derives the real output limit from Anthropic's `rate_limits.five_hour.used_percentage` — no hardcoded values.

Resilience:
- WAL mode + `busy_timeout = 5000` handle multiple sessions writing concurrently.
- Local idempotency: unique index on `(message_uuid, token_type)` — re-firing the same hook is a no-op.
- Cloud idempotency: unique constraint on `event_id` in Turso — retries don't duplicate.
- Network failures during cloud sync leave entries in `cloud_outbox`; the next push event drains them.
- If the local DB is missing, empty for the current window, or `node:sqlite` is unavailable (Node < 22.5), the statusline falls back to the legacy single-session JSONL parser.
- The exact reset countdown comes from `rate_limits.five_hour.resets_at`.

---

## Files

| File | Purpose |
|---|---|
| `xclaude-usage.js` | Statusline hook — reads local + cloud-cached tokens for the 5h window |
| `xclaude-record.js` | Stop / SubagentStop / SubagentStart / PostToolUse hook — writes locally, optionally syncs with Turso |
| `cloud/schema.sql` | Turso (libSQL) schema for the shared `token_delta` table |

---

## License

MIT
