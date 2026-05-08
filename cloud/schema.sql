CREATE TABLE IF NOT EXISTS token_delta (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  device_id       TEXT NOT NULL,
  model           TEXT NOT NULL,
  event_type      TEXT NOT NULL,
  input           INTEGER NOT NULL DEFAULT 0,
  output          INTEGER NOT NULL DEFAULT 0,
  cache_creation  INTEGER NOT NULL DEFAULT 0,
  cache_read      INTEGER NOT NULL DEFAULT 0,
  executed_at     INTEGER NOT NULL,
  event_id        TEXT NOT NULL UNIQUE
);

CREATE INDEX IF NOT EXISTS idx_token_delta_executed ON token_delta(executed_at);
CREATE INDEX IF NOT EXISTS idx_token_delta_id ON token_delta(id);
