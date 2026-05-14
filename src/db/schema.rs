// Schema identical to xclaude-record.js:openDB() (see commit 5da419f).
// Do NOT rename columns or drop indexes — existing users' DBs will be opened in place.

use anyhow::Result;
use rusqlite::Connection;

const DDL: &str = r#"
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

CREATE TABLE IF NOT EXISTS five_hour_window (
  id              INTEGER PRIMARY KEY CHECK (id = 1),
  resets_at       INTEGER NOT NULL,
  start_at        INTEGER NOT NULL,
  used_percentage REAL    NOT NULL,
  updated_at      INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS seven_day_window (
  id              INTEGER PRIMARY KEY CHECK (id = 1),
  resets_at       INTEGER NOT NULL,
  starts_at       INTEGER NOT NULL,
  used_percentage REAL    NOT NULL,
  updated_at      INTEGER NOT NULL
);
"#;

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(DDL)?;
    Ok(())
}
