# XClaudeUsage

> Claude Code statusline that tracks token usage and the 5-hour quota in real time — stays accurate when you run multiple sessions in parallel, via a shared SQLite log.

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

**2. Register in settings.json**

Open `~/.claude/settings.json` (Windows: `%USERPROFILE%\.claude\settings.json`) and add the `statusLine` key plus the `Stop`/`SubagentStop` hooks:

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
>
> The `Stop` / `SubagentStop` hooks are optional. Without them the statusline still works but degrades to the legacy single-session mode (only the current terminal's tokens are counted).

---

**3. Restart Claude Code**

The statusline appears automatically on the next session.

> **First-run tip:** install (or upgrade) right before starting a **fresh** Claude Code session — ideally run the very first turn of a brand-new session after installing. On its first invocation, `xclaude-record.js` stamps every existing transcript message it finds with `executed_at = now`. If you install mid-session, all past messages of that session get back-filled into the current 5-hour window and the count will be inflated. A virgin session has no history to back-fill, so the counter starts clean.

---

## How it works

Two cooperating hooks share state via a local SQLite database:

- **`xclaude-record.js`** runs on every `Stop` / `SubagentStop` event. It reads new entries from the session transcript JSONL (incrementally, by stored byte offset) and inserts one row per token type (`input`, `output`, `cache_creation`, `cache_read`) into `~/.claude/data/xclaude-usage.db`. Each row carries `session_id`, `model`, `quantity`, `executed_at` (epoch seconds), `device_id` (defaults to hostname), and `message_uuid` (used for idempotency).
- **`xclaude-usage.js`** runs on every statusline tick. It queries the DB for `SUM(quantity)` of all rows whose `executed_at` falls inside the current 5-hour window (`[resets_at - 5h, resets_at)`), giving an aggregate across **all parallel sessions on this machine**. It then derives the real output limit from Anthropic's `rate_limits.five_hour.used_percentage` — no hardcoded values.

Resilience:
- WAL mode + `busy_timeout = 5000` handle multiple sessions writing concurrently.
- Idempotency via a unique index on `(message_uuid, token_type)` — duplicate hook invocations are silently ignored.
- If the DB is missing, empty for the current window, or `node:sqlite` is unavailable (Node < 22.5), the statusline falls back to the legacy single-session JSONL parser.
- The exact reset countdown comes from `rate_limits.five_hour.resets_at`.

---

## Multi-session

Run several Claude Code terminals in parallel and the statusline in every one of them shows the same total — the sum of all sessions' tokens within the 5-hour window. The shared SQLite file at `~/.claude/data/xclaude-usage.db` is the source of truth; sessions write independently and never block each other.

A `device_id` column is already present in the schema as a hook for future multi-device sync (currently not implemented; each device keeps its own DB).

---

## Files

| File | Purpose |
|---|---|
| `xclaude-usage.js` | Statusline hook — reads aggregated tokens from the DB |
| `xclaude-record.js` | Stop / SubagentStop hook — writes new tokens to the DB |

---

## License

MIT
