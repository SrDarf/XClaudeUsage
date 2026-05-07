# XClaudeUsage

> Claude Code statusline hook that shows real-time token usage and 5-hour session limit directly in your terminal.

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
| `out:X/Y` | Output tokens used vs derived session limit |
| `X%` | Percentage of 5-hour window consumed |
| `resets:Xh##m` | Exact time remaining until the 5-hour window resets |

**Bar colors:**
- Green — under 50%
- Yellow — 50–64%
- Orange — 65–79%
- Red — 80%+

---

## Requirements

- Node.js (any modern version)
- Claude Code CLI

---

## Install

**1. Download the hook**

macOS/Linux:
```bash
mkdir -p ~/.claude/hooks
curl -o ~/.claude/hooks/xclaude-usage.js \
  https://raw.githubusercontent.com/SrDarf/XClaudeUsage/main/xclaude-usage.js
```

Windows (PowerShell):
```powershell
New-Item -ItemType Directory -Force "$env:USERPROFILE\.claude\hooks" | Out-Null
curl -o "$env:USERPROFILE\.claude\hooks\xclaude-usage.js" `
  https://raw.githubusercontent.com/SrDarf/XClaudeUsage/main/xclaude-usage.js
```

---

**2. Register in settings.json**

Open `~/.claude/settings.json` (Windows: `%USERPROFILE%\.claude\settings.json`) and add the `statusLine` key:

```json
{
  "statusLine": {
    "type": "command",
    "command": "node \"/Users/yourname/.claude/hooks/xclaude-usage.js\""
  }
}
```

Windows example:
```json
"command": "node \"C:/Users/yourname/.claude/hooks/xclaude-usage.js\""
```

> If `settings.json` already exists with other settings, just add the `"statusLine"` key alongside them — do not replace the whole file.

---

**3. Restart Claude Code**

The statusline appears automatically on the next session.

---

## How it works

- Reads session data injected by Claude Code via stdin (model, directory, session ID, transcript path, rate limits)
- Parses transcript JSONL files incrementally to count input/output/cache tokens
- Caches progress in `%TEMP%/claude-tokens-{session}.json` to avoid re-reading on every tick
- Derives the real output token limit from Anthropic's own `rate_limits.five_hour.used_percentage` — no hardcoded values
- Calculates exact reset countdown from `rate_limits.five_hour.resets_at` Unix timestamp
- Falls back to raw output token count when rate limit data isn't yet available

---

## Files

| File | Purpose |
|---|---|
| `xclaude-usage.js` | The hook — single file, zero dependencies |

---

## License

MIT
