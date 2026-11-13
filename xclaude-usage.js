const fs = require('fs');
const path = require('path');
const os = require('os');

const DB_PATH = path.join(os.homedir(), '.claude', 'data', 'xclaude-usage.db');
const EMPTY_ENTRY = () => ({ offset: 0, input: 0, output: 0, cache_creation: 0, cache_read: 0 });
const FIVE_HOUR_SECONDS = 5 * 3600;
const SEVEN_DAY_SECONDS = 7 * 24 * 3600;

// Start-column names differ between the two tables (start_at vs starts_at);
// keep them as-is for compatibility with databases created by older versions.
const RATE_LIMIT_WINDOWS = [
  { key: 'five_hour', table: 'five_hour_window', startCol: 'start_at', seconds: FIVE_HOUR_SECONDS },
  { key: 'seven_day', table: 'seven_day_window', startCol: 'starts_at', seconds: SEVEN_DAY_SECONDS },
];
const [FIVE_HOUR_WINDOW, SEVEN_DAY_WINDOW] = RATE_LIMIT_WINDOWS;

// A drop in used_percentage within the same resets_at larger than this is an
// early reset (a global reset or a redeemed one), not rounding noise.
const RESET_DROP_POINTS = 1;

function getDatabaseSync() {
  try { return require('node:sqlite').DatabaseSync; } catch { return null; }
}

function closeQuietly(db) {
  if (db) try { db.close(); } catch {}
}

function writeRateLimitWindows(rateLimits) {
  const jobs = [];
  for (const win of RATE_LIMIT_WINDOWS) {
    const data = rateLimits?.[win.key];
    if (data
        && typeof data.resets_at === 'number'
        && typeof data.used_percentage === 'number') jobs.push([win, data]);
  }
  if (jobs.length === 0) return;
  if (!fs.existsSync(DB_PATH)) return;

  const DatabaseSync = getDatabaseSync();
  if (!DatabaseSync) return;

  let db;
  try {
    db = new DatabaseSync(DB_PATH);
    db.exec('PRAGMA busy_timeout = 2000;');
    const now = Math.floor(Date.now() / 1000);

    for (const [{ table, startCol, seconds }, data] of jobs) {
      db.exec(`
        CREATE TABLE IF NOT EXISTS ${table} (
          id              INTEGER PRIMARY KEY CHECK (id = 1),
          resets_at       INTEGER NOT NULL,
          ${startCol}     INTEGER NOT NULL,
          used_percentage REAL    NOT NULL,
          updated_at      INTEGER NOT NULL,
          reset_at        INTEGER
        )
      `);
      // CREATE TABLE IF NOT EXISTS never alters an existing table: databases
      // created before reset_at existed get the (nullable) column here.
      const cols = db.prepare(`PRAGMA table_info(${table})`).all();
      if (!cols.some(c => c.name === 'reset_at')) {
        db.exec(`ALTER TABLE ${table} ADD COLUMN reset_at INTEGER`);
      }
      // Same resets_at but used_percentage clearly lower is an early reset:
      // stamp reset_at. A new resets_at is a new window and clears it. SQLite
      // evaluates every SET expression against the old row, so the detection is
      // atomic with the write and concurrent hooks cannot interleave.
      db.prepare(`
        INSERT INTO ${table} (id, resets_at, ${startCol}, used_percentage, updated_at, reset_at)
        VALUES (1, ?, ?, ?, ?, NULL)
        ON CONFLICT(id) DO UPDATE SET
          reset_at = CASE
            WHEN excluded.resets_at <> ${table}.resets_at THEN NULL
            WHEN excluded.used_percentage < ${table}.used_percentage - ? THEN excluded.updated_at
            ELSE ${table}.reset_at END,
          resets_at       = excluded.resets_at,
          ${startCol}     = excluded.${startCol},
          used_percentage = excluded.used_percentage,
          updated_at      = excluded.updated_at
      `).run(data.resets_at, data.resets_at - seconds, data.used_percentage, now, RESET_DROP_POINTS);
    }
  } catch {
  } finally {
    closeQuietly(db);
  }
}

// Tokens spent in the current budget: from the window start, or from the early
// reset when one was recorded for this same window.
function readWindowTokensFromDB(window, { table, seconds } = FIVE_HOUR_WINDOW) {
  if (!window?.resets_at) return null;
  if (!fs.existsSync(DB_PATH)) return null;

  const DatabaseSync = getDatabaseSync();
  if (!DatabaseSync) return null;

  const end = window.resets_at;

  let db;
  try {
    db = new DatabaseSync(DB_PATH, { readOnly: true });
    db.exec('PRAGMA busy_timeout = 2000;');

    let resetAt = null;
    try {
      const row = db.prepare(`SELECT reset_at FROM ${table} WHERE id = 1 AND resets_at = ?`).get(end);
      if (row && row.reset_at != null) resetAt = Number(row.reset_at);
    } catch {}
    const start = resetAt != null ? Math.max(end - seconds, resetAt) : end - seconds;

    const totals = { input: 0, output: 0, cache_creation: 0, cache_read: 0 };
    let anyRows = false;

    const localRows = db.prepare(`
      SELECT token_type, SUM(quantity) AS total
      FROM token_usage
      WHERE executed_at >= ? AND executed_at < ?
      GROUP BY token_type
    `).all(start, end);
    if (localRows.length > 0) {
      anyRows = true;
      for (const r of localRows) {
        if (totals[r.token_type] != null) totals[r.token_type] += Number(r.total) || 0;
      }
    }

    try {
      const cloudRow = db.prepare(`
        SELECT
          COALESCE(SUM(input), 0)          AS input,
          COALESCE(SUM(output), 0)         AS output,
          COALESCE(SUM(cache_creation), 0) AS cache_creation,
          COALESCE(SUM(cache_read), 0)     AS cache_read,
          COUNT(*)                         AS n
        FROM cloud_cache
        WHERE executed_at >= ? AND executed_at < ?
      `).get(start, end);
      if (cloudRow && Number(cloudRow.n) > 0) {
        anyRows = true;
        totals.input          += Number(cloudRow.input) || 0;
        totals.output         += Number(cloudRow.output) || 0;
        totals.cache_creation += Number(cloudRow.cache_creation) || 0;
        totals.cache_read     += Number(cloudRow.cache_read) || 0;
      }
    } catch {}

    // Right after a reset the budget is genuinely empty: report zero instead of
    // null, or the caller falls back to the session transcript and shows tokens
    // spent before the reset.
    return anyRows || resetAt != null ? totals : null;
  } catch {
    return null;
  } finally {
    closeQuietly(db);
  }
}

function consumeTranscript(filePath, entry) {
  let stat;
  try { stat = fs.statSync(filePath); } catch { return entry; }
  const size = stat.size;
  if (entry.offset > size) Object.assign(entry, EMPTY_ENTRY());
  if (entry.offset === size) return entry;

  let fd;
  try {
    fd = fs.openSync(filePath, 'r');
    const readLen = size - entry.offset;
    const buf = Buffer.alloc(readLen);
    fs.readSync(fd, buf, 0, readLen, entry.offset);
    const text = buf.toString('utf8');

    const lastNl = text.lastIndexOf('\n');
    if (lastNl < 0) return entry;
    const processText = text.slice(0, lastNl);
    const consumed = lastNl + 1;

    for (const line of processText.split('\n')) {
      if (!line.trim()) continue;
      try {
        const obj = JSON.parse(line);
        if (obj.type === 'assistant' && obj.message?.usage) {
          const u = obj.message.usage;
          entry.input += u.input_tokens || 0;
          entry.output += u.output_tokens || 0;
          entry.cache_creation += u.cache_creation_input_tokens || 0;
          entry.cache_read += u.cache_read_input_tokens || 0;
        }
      } catch {}
    }
    entry.offset += consumed;
  } catch {}
  finally { if (fd !== undefined) try { fs.closeSync(fd); } catch {} }
  return entry;
}

function readSessionTokens(transcriptPath, session) {
  if (!transcriptPath) return null;
  const sessionSafe = session && !/[/\\]|\.\./.test(session);
  if (!sessionSafe) return null;

  const paths = [];
  if (fs.existsSync(transcriptPath)) paths.push(transcriptPath);

  const subagentDir = transcriptPath.replace(/\.jsonl$/, '') + '/subagents';
  if (fs.existsSync(subagentDir)) {
    try {
      for (const f of fs.readdirSync(subagentDir)) {
        if (f.endsWith('.jsonl')) paths.push(path.join(subagentDir, f));
      }
    } catch {}
  }
  if (paths.length === 0) return null;

  const cachePath = path.join(os.tmpdir(), `claude-tokens-${session}.json`);
  let cache = { files: {} };

  if (fs.existsSync(cachePath)) {
    try {
      const loaded = JSON.parse(fs.readFileSync(cachePath, 'utf8'));
      if (loaded && loaded.files) cache = loaded;
    } catch {}
  }

  for (const p of paths) {
    const entry = cache.files[p] || EMPTY_ENTRY();
    consumeTranscript(p, entry);
    cache.files[p] = entry;
  }

  try { fs.writeFileSync(cachePath, JSON.stringify(cache)); } catch {}

  const total = { input: 0, output: 0, cache_creation: 0, cache_read: 0, subagent_count: 0 };

  for (const [p, e] of Object.entries(cache.files)) {
    total.input += e.input;
    total.output += e.output;
    total.cache_creation += e.cache_creation;
    total.cache_read += e.cache_read;
    if (p !== transcriptPath) total.subagent_count += 1;
  }

  return total;
}

function fmtCountdown(resetsAt) {
  const secsLeft = Math.max(0, resetsAt - Math.floor(Date.now() / 1000));
  const h = Math.floor(secsLeft / 3600);
  const m = Math.floor((secsLeft % 3600) / 60);
  const s = secsLeft % 60;
  if (h >= 24) return `${Math.floor(h / 24)}d${String(h % 24).padStart(2, '0')}h`;
  if (h > 0) return `${h}h${String(m).padStart(2, '0')}m`;
  if (m > 0) return `${m}m${String(s).padStart(2, '0')}s`;
  return `${s}s`;
}

function fmtTokens(n) {
  if (!n) return '0';
  if (n < 1000) return String(n);
  if (n < 1_000_000) return (n / 1000).toFixed(1) + 'k';
  return (n / 1_000_000).toFixed(2) + 'M';
}

function barColor(pct, weekly) {
  if (pct < 50) return weekly ? '38;5;183' : '32';
  if (pct < 65) return '33';
  if (pct < 80) return '38;5;208';
  return '31';
}

// Same block bar for both windows; the 7-day one (plans without a 5h cap)
// differs only in color: lilac instead of green in the low band.
function renderWindow(win, tokens, weekly) {
  const pct = Math.min(100, Math.max(0, Math.round(win.used_percentage)));
  const filled = Math.floor(pct / 10);
  const color = barColor(pct, weekly);

  const bar = `\x1b[${color}m${'█'.repeat(filled)}${'░'.repeat(10 - filled)}\x1b[0m`;

  let outLabel = '';
  if (tokens && tokens.output > 0) {
    outLabel = win.used_percentage > 0
      ? `out:${fmtTokens(tokens.output)}/${fmtTokens(Math.round(tokens.output / (win.used_percentage / 100)))} · `
      : `out:${fmtTokens(tokens.output)} · `;
  }

  const countdownLabel = win.resets_at ? ` · resets:${fmtCountdown(win.resets_at)}` : '';
  return `${bar} \x1b[2m${outLabel}${pct}%${countdownLabel}\x1b[0m`;
}

function runStatusline() {
  let input = '';
  const stdinTimeout = setTimeout(() => process.exit(0), 3000);

  process.stdin.setEncoding('utf8');
  process.stdin.on('data', chunk => input += chunk);
  process.stdin.on('end', () => {
    clearTimeout(stdinTimeout);

    try {
      const data = JSON.parse(input);

      const model = data.model?.display_name || 'Claude';
      const dir = data.workspace?.current_dir || process.cwd();
      const session = data.session_id || '';

      // Plans without a 5-hour cap (e.g. Ultra) only send rate_limits.seven_day:
      // fall back to the weekly window, drawn in its own color.
      writeRateLimitWindows(data.rate_limits);
      const fiveHour = data.rate_limits?.five_hour;
      const sevenDay = data.rate_limits?.seven_day;
      const hasPct = w => w != null && typeof w.used_percentage === 'number';
      const active =
        hasPct(fiveHour) ? { win: fiveHour, table: FIVE_HOUR_WINDOW, weekly: false } :
        hasPct(sevenDay) ? { win: sevenDay, table: SEVEN_DAY_WINDOW, weekly: true } :
        null;

      const tokens =
        (active && readWindowTokensFromDB(active.win, active.table)) ||
        readSessionTokens(data.transcript_path, session);

      let tokenStr = '';
      if (active) {
        tokenStr = ' │ ' + renderWindow(active.win, tokens, active.weekly);
      } else if (tokens && tokens.output > 0) {
        tokenStr = ` │ \x1b[2mout:${fmtTokens(tokens.output)}\x1b[0m`;
      }

      const dirname = path.basename(dir);
      process.stdout.write(`\x1b[2m${model}\x1b[0m │ \x1b[2m${dirname}\x1b[0m${tokenStr}`);

    } catch {}
  });
}

if (require.main === module) runStatusline();
