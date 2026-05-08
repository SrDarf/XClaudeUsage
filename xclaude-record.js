const fs = require('fs');
const path = require('path');
const os = require('os');
const crypto = require('crypto');

const DATA_DIR = path.join(os.homedir(), '.claude', 'data');
const DB_PATH = path.join(DATA_DIR, 'xclaude-usage.db');
const LOG_PATH = path.join(DATA_DIR, 'xclaude-usage.log');
const CLOUD_CONFIG_PATH = path.join(DATA_DIR, 'xclaude-cloud.json');
const LOG_MAX_BYTES = 1_000_000;
const PULL_LIMIT = 500;
const NETWORK_TIMEOUT_MS = 5000;
const WINDOW_SECONDS = 5 * 3600;

const TOKEN_FIELDS = [
  ['input', 'input_tokens'],
  ['output', 'output_tokens'],
  ['cache_creation', 'cache_creation_input_tokens'],
  ['cache_read', 'cache_read_input_tokens'],
];

const HOOK_BEHAVIOR = {
  Stop:          { localWrite: true, push: true,  pull: true,  eventType: 'stop' },
  SubagentStop:  { localWrite: true, push: true,  pull: false, eventType: 'subagent_stop' },
  SubagentStart: { localWrite: true, push: false, pull: false, eventType: null },
  PostToolUse:   { localWrite: true, push: false, pull: true,  eventType: null },
};
const DEFAULT_BEHAVIOR = { localWrite: true, push: false, pull: false, eventType: null };

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
    CREATE TABLE IF NOT EXISTS cloud_outbox (
      event_id    TEXT PRIMARY KEY,
      payload     TEXT NOT NULL,
      created_at  INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS cloud_cache (
      remote_id       INTEGER PRIMARY KEY,
      device_id       TEXT NOT NULL,
      model           TEXT NOT NULL,
      input           INTEGER NOT NULL DEFAULT 0,
      output          INTEGER NOT NULL DEFAULT 0,
      cache_creation  INTEGER NOT NULL DEFAULT 0,
      cache_read      INTEGER NOT NULL DEFAULT 0,
      executed_at     INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_cloud_cache_executed ON cloud_cache(executed_at);
    CREATE TABLE IF NOT EXISTS cloud_state (
      key   TEXT PRIMARY KEY,
      value TEXT NOT NULL
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

function loadCloudConfig() {
  let raw;
  try { raw = fs.readFileSync(CLOUD_CONFIG_PATH, 'utf8'); } catch { return null; }
  let cfg;
  try { cfg = JSON.parse(raw); } catch (e) {
    logErr(`xclaude-cloud.json invalid JSON: ${e.message}`); return null;
  }
  if (!cfg || typeof cfg !== 'object') {
    logErr('xclaude-cloud.json: top-level value must be an object'); return null;
  }
  if (typeof cfg.libsql_url !== 'string' || !/^(libsql|https?):\/\//.test(cfg.libsql_url)) {
    logErr(`xclaude-cloud.json: libsql_url must start with libsql:// or https:// (got: ${String(cfg.libsql_url).slice(0,12)}...)`);
    return null;
  }
  if (typeof cfg.auth_token !== 'string' || !cfg.auth_token) {
    logErr('xclaude-cloud.json: auth_token missing or empty'); return null;
  }
  // libSQL clients translate libsql:// to https:// for HTTP transport.
  const httpUrl = cfg.libsql_url.replace(/^libsql:\/\//, 'https://').replace(/\/+$/, '');
  return {
    libsqlUrl: httpUrl,
    authToken: cfg.auth_token,
    deviceId: typeof cfg.device_id === 'string' && cfg.device_id ? cfg.device_id : null,
  };
}

function getCloudDeviceId(db, configDeviceId) {
  if (configDeviceId) return configDeviceId;
  const id = crypto.randomUUID();
  db.prepare("INSERT OR IGNORE INTO cloud_state (key, value) VALUES ('device_id', ?)").run(id);
  return db.prepare("SELECT value FROM cloud_state WHERE key = 'device_id'").get().value;
}

function getCloudCursor(db) {
  const row = db.prepare("SELECT value FROM cloud_state WHERE key = 'last_remote_id'").get();
  return row ? Number(row.value) || 0 : 0;
}

function setCloudCursor(db, id) {
  db.prepare(`
    INSERT INTO cloud_state (key, value) VALUES ('last_remote_id', ?)
    ON CONFLICT(key) DO UPDATE SET value = excluded.value
  `).run(String(id));
}

function getOrInitPushCursor(db) {
  const row = db.prepare("SELECT value FROM cloud_state WHERE key = 'last_pushed_id'").get();
  if (row) return Number(row.value) || 0;
  // First run: skip whatever already exists locally so we don't retroactively
  // flood the cloud with everything since install. New rows from now on get pushed.
  const max = Number(db.prepare('SELECT COALESCE(MAX(id), 0) AS m FROM token_usage').get().m) || 0;
  db.prepare("INSERT INTO cloud_state (key, value) VALUES ('last_pushed_id', ?)").run(String(max));
  return max;
}

function setPushCursor(db, id) {
  db.prepare(`
    INSERT INTO cloud_state (key, value) VALUES ('last_pushed_id', ?)
    ON CONFLICT(key) DO UPDATE SET value = excluded.value
  `).run(String(id));
}

function toLibsqlValue(v) {
  if (v === null || v === undefined) return { type: 'null' };
  if (typeof v === 'number') {
    return Number.isInteger(v)
      ? { type: 'integer', value: String(v) }
      : { type: 'float', value: v };
  }
  if (typeof v === 'bigint') return { type: 'integer', value: v.toString() };
  return { type: 'text', value: String(v) };
}

function decodeCell(cell) {
  if (!cell || cell.type === 'null') return null;
  if (cell.type === 'integer') return Number(cell.value);
  if (cell.type === 'float') return Number(cell.value);
  return cell.value;
}

function decodeRows(execResult) {
  if (!execResult?.cols || !execResult?.rows) return [];
  const cols = execResult.cols.map(c => c.name);
  return execResult.rows.map(row => {
    const obj = {};
    for (let i = 0; i < cols.length; i++) obj[cols[i]] = decodeCell(row[i]);
    return obj;
  });
}

async function libsqlPipeline(url, token, statements) {
  const requests = statements.map(s => ({
    type: 'execute',
    stmt: { sql: s.sql, args: (s.args || []).map(toLibsqlValue) },
  }));
  requests.push({ type: 'close' });
  const res = await fetch(`${url}/v2/pipeline`, {
    method: 'POST',
    headers: {
      'Authorization': `Bearer ${token}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ requests }),
    signal: AbortSignal.timeout(NETWORK_TIMEOUT_MS),
  });
  if (!res.ok) {
    const body = await res.text().catch(() => '');
    throw new Error(`libsql ${res.status}: ${body.slice(0, 200)}`);
  }
  return res.json();
}

async function syncCloud(config, db, deviceId, doPush, doPull) {
  const outboxRows = doPush
    ? db.prepare('SELECT event_id, payload FROM cloud_outbox ORDER BY created_at ASC').all()
    : [];
  const lastRemoteId = doPull ? getCloudCursor(db) : 0;
  const now = Math.floor(Date.now() / 1000);

  const statements = [];

  for (const row of outboxRows) {
    let p;
    try { p = JSON.parse(row.payload); } catch { continue; }
    statements.push({
      sql: `INSERT OR IGNORE INTO token_delta
        (device_id, model, event_type, input, output, cache_creation, cache_read, executed_at, event_id)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      args: [
        p.device_id, p.model, p.event_type,
        p.input || 0, p.output || 0, p.cache_creation || 0, p.cache_read || 0,
        p.executed_at, row.event_id,
      ],
    });
  }

  let pullStatementIndex = -1;
  if (doPull) {
    pullStatementIndex = statements.length;
    statements.push({
      sql: `SELECT id, device_id, model, input, output, cache_creation, cache_read, executed_at
            FROM token_delta
            WHERE id > ? AND device_id != ? AND executed_at >= ?
            ORDER BY id ASC LIMIT ?`,
      args: [lastRemoteId, deviceId, now - WINDOW_SECONDS, PULL_LIMIT],
    });
  }

  // Opportunistic TTL cleanup runs on every sync.
  statements.push({
    sql: 'DELETE FROM token_delta WHERE executed_at < ?',
    args: [now - WINDOW_SECONDS],
  });

  if (statements.length === 0) return;

  const response = await libsqlPipeline(config.libsqlUrl, config.authToken, statements);
  const results = response?.results || [];

  for (let i = 0; i < results.length; i++) {
    const r = results[i];
    if (r?.type !== 'ok') {
      throw new Error(`libsql stmt ${i} failed: ${JSON.stringify(r).slice(0, 200)}`);
    }
  }

  let pullRows = [];
  if (pullStatementIndex >= 0) {
    pullRows = decodeRows(results[pullStatementIndex]?.response?.result);
  }

  db.exec('BEGIN IMMEDIATE');
  try {
    if (outboxRows.length) {
      const del = db.prepare('DELETE FROM cloud_outbox WHERE event_id = ?');
      for (const row of outboxRows) del.run(row.event_id);
    }
    if (pullRows.length) {
      const insertCache = db.prepare(`
        INSERT OR REPLACE INTO cloud_cache
          (remote_id, device_id, model, input, output, cache_creation, cache_read, executed_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
      `);
      let maxId = lastRemoteId;
      for (const r of pullRows) {
        insertCache.run(
          r.id, r.device_id, r.model,
          r.input || 0, r.output || 0, r.cache_creation || 0, r.cache_read || 0,
          r.executed_at,
        );
        if (r.id > maxId) maxId = r.id;
      }
      if (maxId > lastRemoteId) setCloudCursor(db, maxId);
    }
    // Trim local cache to the 5h window so it can't grow unbounded.
    db.prepare('DELETE FROM cloud_cache WHERE executed_at < ?').run(now - WINDOW_SECONDS);
    db.exec('COMMIT');
  } catch (e) {
    try { db.exec('ROLLBACK'); } catch {}
    throw e;
  }
}

async function record(payload) {
  const sessionId = payload.session_id;
  const transcriptPath = payload.transcript_path;
  if (!sessionId || !transcriptPath) return;
  if (/[/\\]|\.\./.test(sessionId)) return;

  const eventName = payload.hook_event_name || 'Stop';
  const behavior = HOOK_BEHAVIOR[eventName] || DEFAULT_BEHAVIOR;
  if (!behavior.localWrite && !behavior.push && !behavior.pull) return;

  const fallbackModel = payload.model?.id || payload.model?.display_name || null;
  const localDeviceLabel = os.hostname() || '';
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
      if (behavior.localWrite) {
        const row = getOffset.get(transcriptPath);
        const offset = row?.byte_offset ?? 0;
        const result = readNewTranscript(transcriptPath, offset);
        if (result) {
          if (result.text) {
            const events = parseAssistantEvents(result.text, fallbackModel);
            for (const ev of events) {
              for (const [type, field] of TOKEN_FIELDS) {
                const qty = ev.usage[field] || 0;
                if (qty > 0) insertToken.run(sessionId, ev.model, type, qty, now, localDeviceLabel, ev.uuid);
              }
            }
          }
          setOffset.run(transcriptPath, result.newOffset, now);
        }
      }
      db.exec('COMMIT');
    } catch (e) {
      try { db.exec('ROLLBACK'); } catch {}
      throw e;
    }

    if (!behavior.push && !behavior.pull) return;
    const config = loadCloudConfig();
    if (!config) return;

    const cloudDeviceId = getCloudDeviceId(db, config.deviceId);

    // On push events, package every token_usage row added since the last
    // successful push, regardless of which hook fire originally captured it.
    // PostToolUse and SubagentStart write locally but don't push, so the
    // next Stop / SubagentStop closes the books on accumulated rows.
    if (behavior.push) {
      db.exec('BEGIN IMMEDIATE');
      try {
        const lastPushed = getOrInitPushCursor(db);
        const windowStart = now - WINDOW_SECONDS;
        const groups = db.prepare(`
          SELECT
            model,
            SUM(CASE WHEN token_type='input'          THEN quantity ELSE 0 END) AS input,
            SUM(CASE WHEN token_type='output'         THEN quantity ELSE 0 END) AS output,
            SUM(CASE WHEN token_type='cache_creation' THEN quantity ELSE 0 END) AS cache_creation,
            SUM(CASE WHEN token_type='cache_read'     THEN quantity ELSE 0 END) AS cache_read,
            MAX(executed_at) AS executed_at
          FROM token_usage
          WHERE id > ? AND executed_at >= ?
          GROUP BY model
        `).all(lastPushed, windowStart);

        const enqueueOutbox = db.prepare(`
          INSERT OR IGNORE INTO cloud_outbox (event_id, payload, created_at)
          VALUES (?, ?, ?)
        `);
        for (const g of groups) {
          const total = (g.input || 0) + (g.output || 0) + (g.cache_creation || 0) + (g.cache_read || 0);
          if (total === 0) continue;
          const payloadJson = JSON.stringify({
            device_id: cloudDeviceId,
            model: g.model,
            event_type: behavior.eventType,
            input: g.input || 0,
            output: g.output || 0,
            cache_creation: g.cache_creation || 0,
            cache_read: g.cache_read || 0,
            executed_at: g.executed_at || now,
          });
          enqueueOutbox.run(crypto.randomUUID(), payloadJson, now);
        }

        // Advance the cursor past every newer row (in-window or not), so we
        // don't keep re-scanning rows that fell out of the window without
        // being pushed. Their tokens are already lost to the prior window.
        const newCursor = Number(
          db.prepare('SELECT COALESCE(MAX(id), ?) AS m FROM token_usage WHERE id > ?')
            .get(lastPushed, lastPushed).m
        );
        if (newCursor > lastPushed) setPushCursor(db, newCursor);
        db.exec('COMMIT');
      } catch (e) {
        try { db.exec('ROLLBACK'); } catch {}
        throw e;
      }
    }

    try {
      await syncCloud(config, db, cloudDeviceId, behavior.push, behavior.pull);
    } catch (e) {
      logErr(e);
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
  process.stdin.on('end', async () => {
    clearTimeout(timeout);
    try {
      await record(JSON.parse(input));
    } catch (e) {
      logErr(e);
    }
  });
}

if (require.main === module) run();
