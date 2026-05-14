# Cloud schema reference

The shared Turso database holds a single table:

```sql
CREATE TABLE token_delta (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  device_id       TEXT NOT NULL,
  model           TEXT NOT NULL,
  event_type      TEXT NOT NULL,           -- 'stop' | 'subagent_stop'
  input           INTEGER NOT NULL DEFAULT 0,
  output          INTEGER NOT NULL DEFAULT 0,
  cache_creation  INTEGER NOT NULL DEFAULT 0,
  cache_read      INTEGER NOT NULL DEFAULT 0,
  executed_at     INTEGER NOT NULL,
  event_id        TEXT NOT NULL UNIQUE     -- UUID per hook invocation, idempotency key
);
```

See [`schema.sql`](schema.sql) for the canonical version that includes indexes, and apply it once with:

```bash
turso db shell xclaude-usage < cloud/schema.sql
```

Setup walkthrough (creating the database, generating a token, configuring each device) lives in the main [README](../README.md#multi-device-sync-opt-in).

## Retention

Each sync round (every `Stop` from a device that participates) appends `DELETE FROM token_delta WHERE executed_at < unixepoch() - 15*86400` to its pipeline, so rows older than **15 days** are pruned opportunistically. There is no separate cron required.

## Inspection

A few queries that come up while debugging:

```sql
-- Most recent rows across devices
SELECT id, device_id, model, output, datetime(executed_at,'unixepoch','localtime') AS ts
FROM token_delta ORDER BY id DESC LIMIT 20;

-- Per-device totals in the current 5-hour window
SELECT device_id, COUNT(*) AS rows, SUM(output) AS out_total
FROM token_delta
WHERE executed_at >= unixepoch() - 5*3600
GROUP BY device_id;

-- Sanity check: rows per device in the last hour
SELECT device_id, COUNT(*) FROM token_delta
WHERE executed_at >= unixepoch() - 3600
GROUP BY device_id;
```

Run them via `turso db shell xclaude-usage "<query>"`.
