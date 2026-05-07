const fs = require('fs');
const path = require('path');
const os = require('os');

const DATA_DIR = path.join(os.homedir(), '.claude', 'data');
const DB_PATH = path.join(DATA_DIR, 'xclaude-usage.db');
const LOG_PATH = path.join(DATA_DIR, 'xclaude-usage.log');
const LOG_MAX_BYTES = 1_000_000;

const TOKEN_FIELDS = [
  ['input', 'input_tokens'],
  ['output', 'output_tokens'],
  ['cache_creation', 'cache_creation_input_tokens'],
  ['cache_read', 'cache_read_input_tokens'],
];

function logErr(e) {
  try {
    fs.mkdirSync(DATA_DIR, { recursive: true });
    const line = `[${new Date().toISOString()}] ${e?.stack || e}\n`;
    fs.appendFileSync(LOG_PATH, line);
    try {
      const stat = fs.statSync(LOG_PATH);
      if (stat.size > LOG_MAX_BYTES) fs.renameSync(LOG_PATH, LOG_PATH + '.1');
    } catch {}
  } catch {}
}

function openDB() {
  const { DatabaseSync } = require('node:sqlite');
  fs.mkdirSync(DATA_DIR, { recursive: true });
  const db = new DatabaseSync(DB_PATH);
  db.exec(`
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = NORMAL;
    PRAGMA busy_timeout = 5000;
    CREATE TABLE IF NOT EXISTS token_usage (
      id            INTEGER PRIMARY KEY AUTOINCREMENT,
      session_id    TEXT    NOT NULL,
      model         TEXT    NOT NULL,
      token_type    TEXT    NOT NULL CHECK (token_type IN ('input','output','cache_creation','cache_read')),
      quantity      INTEGER NOT NULL,
      executed_at   INTEGER NOT NULL,
      device_id     TEXT    NOT NULL DEFAULT '',
      message_uuid  TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_token_usage_executed_at ON token_usage(executed_at);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_token_usage_msg_type
      ON token_usage(message_uuid, token_type) WHERE message_uuid IS NOT NULL;
    CREATE TABLE IF NOT EXISTS transcript_progress (
      transcript_path TEXT PRIMARY KEY,
      byte_offset     INTEGER NOT NULL,
      updated_at      INTEGER NOT NULL
    );
  `);
  return db;
}

function readNewTranscript(transcriptPath, offset) {
  let stat;
  try { stat = fs.statSync(transcriptPath); } catch { return null; }
  const size = stat.size;
  if (offset > size) offset = 0;
  if (offset === size) return { text: '', newOffset: offset };

  const fd = fs.openSync(transcriptPath, 'r');
  try {
    const buf = Buffer.alloc(size - offset);
    fs.readSync(fd, buf, 0, size - offset, offset);
    const text = buf.toString('utf8');
    const lastNl = text.lastIndexOf('\n');
    if (lastNl < 0) return { text: '', newOffset: offset };
    return { text: text.slice(0, lastNl), newOffset: offset + lastNl + 1 };
  } finally {
    try { fs.closeSync(fd); } catch {}
  }
}

function parseAssistantEvents(text, fallbackModel) {
  const out = [];
  for (const line of text.split('\n')) {
    if (!line.trim()) continue;
    let obj;
    try { obj = JSON.parse(line); } catch { continue; }
    if (obj.type !== 'assistant' || !obj.message?.usage) continue;
    out.push({
      model: obj.message.model || fallbackModel || 'unknown',
      uuid: obj.uuid || obj.message?.id || null,
      usage: obj.message.usage,
    });
  }
  return out;
}

function record(payload) {
  const sessionId = payload.session_id;
  const transcriptPath = payload.transcript_path;
  if (!sessionId || !transcriptPath) return;
  if (/[/\\]|\.\./.test(sessionId)) return;

  const fallbackModel = payload.model?.id || payload.model?.display_name || null;
  const deviceId = os.hostname() || '';
  const now = Math.floor(Date.now() / 1000);

  const db = openDB();
  try {
    const getOffset = db.prepare('SELECT byte_offset FROM transcript_progress WHERE transcript_path = ?');
    const setOffset = db.prepare(`
      INSERT INTO transcript_progress (transcript_path, byte_offset, updated_at)
      VALUES (?, ?, ?)
      ON CONFLICT(transcript_path) DO UPDATE SET byte_offset = excluded.byte_offset, updated_at = excluded.updated_at
    `);
    const insertToken = db.prepare(`
      INSERT OR IGNORE INTO token_usage
        (session_id, model, token_type, quantity, executed_at, device_id, message_uuid)
      VALUES (?, ?, ?, ?, ?, ?, ?)
    `);

    db.exec('BEGIN IMMEDIATE');
    try {
      const row = getOffset.get(transcriptPath);
      const offset = row?.byte_offset ?? 0;
      const result = readNewTranscript(transcriptPath, offset);
      if (!result) { db.exec('COMMIT'); return; }
      if (result.text) {
        const events = parseAssistantEvents(result.text, fallbackModel);
        for (const ev of events) {
          for (const [type, field] of TOKEN_FIELDS) {
            const qty = ev.usage[field] || 0;
            if (qty > 0) insertToken.run(sessionId, ev.model, type, qty, now, deviceId, ev.uuid);
          }
        }
      }
      setOffset.run(transcriptPath, result.newOffset, now);
      db.exec('COMMIT');
    } catch (e) {
      try { db.exec('ROLLBACK'); } catch {}
      throw e;
    }
  } finally {
    try { db.close(); } catch {}
  }
}

function run() {
  let input = '';
  const timeout = setTimeout(() => process.exit(0), 8000);
  process.stdin.setEncoding('utf8');
  process.stdin.on('data', chunk => input += chunk);
  process.stdin.on('end', () => {
    clearTimeout(timeout);
    try {
      record(JSON.parse(input));
    } catch (e) {
      logErr(e);
    }
  });
}

if (require.main === module) run();
