const fs = require('fs');
const path = require('path');
const os = require('os');

const DB_PATH = path.join(os.homedir(), '.claude', 'data', 'xclaude-usage.db');
const EMPTY_ENTRY = () => ({ offset: 0, input: 0, output: 0, cache_creation: 0, cache_read: 0 });

function readWindowTokensFromDB(fiveHour) {
  if (!fiveHour?.resets_at) return null;
  if (!fs.existsSync(DB_PATH)) return null;

  let DatabaseSync;
  try { ({ DatabaseSync } = require('node:sqlite')); } catch { return null; }

  const end = fiveHour.resets_at;
  const start = end - 5 * 3600;

  let db;
  try {
    db = new DatabaseSync(DB_PATH, { readOnly: true });
    db.exec('PRAGMA busy_timeout = 2000;');

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

    return anyRows ? totals : null;
  } catch {
    return null;
  } finally {
    if (db) try { db.close(); } catch {}
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

      let tokenStr = '';
      const fiveHour = data.rate_limits?.five_hour;
      const tokens = readWindowTokensFromDB(fiveHour) || readSessionTokens(data.transcript_path, session);

      if (fiveHour != null && fiveHour.used_percentage != null) {
        const tokenPct = Math.min(100, Math.round(fiveHour.used_percentage));
        const filled = Math.floor(tokenPct / 10);
        const bar = '█'.repeat(filled) + '░'.repeat(10 - filled);

        const barColor =
          tokenPct < 50 ? '32' :
          tokenPct < 65 ? '33' :
          tokenPct < 80 ? '38;5;208' : '31';

        let outLabel = '';
        if (tokens && tokens.output > 0) {
          const pct = fiveHour.used_percentage;
          if (pct > 0) {
            const realLimit = tokens.output / (pct / 100);
            outLabel = `out:${fmtTokens(tokens.output)}/${fmtTokens(Math.round(realLimit))} · `;
          } else {
            outLabel = `out:${fmtTokens(tokens.output)} · `;
          }
        }

        const countdownLabel = fiveHour.resets_at
          ? ` · resets:${fmtCountdown(fiveHour.resets_at)}`
          : '';

        tokenStr =
          ` │ \x1b[${barColor}m${bar}\x1b[0m ` +
          `\x1b[2m${outLabel}${tokenPct}%${countdownLabel}\x1b[0m`;
      } else if (tokens && tokens.output > 0) {
        tokenStr = ` │ \x1b[2mout:${fmtTokens(tokens.output)}\x1b[0m`;
      }

      const dirname = path.basename(dir);
      process.stdout.write(`\x1b[2m${model}\x1b[0m │ \x1b[2m${dirname}\x1b[0m${tokenStr}`);

    } catch {}
  });
}

if (require.main === module) runStatusline();
