// Single-roundtrip cloud sync: drain the local outbox into Turso, pull
// other devices' deltas into cloud_cache, and (when due) prune rows older
// than RETENTION_SECONDS. Mirrors xclaude-record.js:syncCloud.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::pipeline::{self, Statement};
use super::{
    get_pull_cursor, set_pull_cursor, CloudConfig, PULL_LIMIT, RETENTION_SECONDS,
};

pub fn sync(
    config: &CloudConfig,
    db: &Connection,
    device_id: &str,
    do_push: bool,
    do_pull: bool,
    cleanup_due: bool,
) -> Result<()> {
    let outbox_rows: Vec<(String, String)> = if do_push {
        let mut stmt = db.prepare(
            "SELECT event_id, payload FROM cloud_outbox ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.filter_map(|r| r.ok()).collect()
    } else {
        Vec::new()
    };

    let last_remote_id = if do_pull { get_pull_cursor(db)? } else { 0 };
    let now = unix_now();

    let mut statements: Vec<Statement> = Vec::new();

    // 1. INSERT every outbox row as a token_delta.
    for (event_id, payload) in &outbox_rows {
        let Ok(p) = serde_json::from_str::<Value>(payload) else { continue };
        statements.push(Statement {
            sql: "INSERT OR IGNORE INTO token_delta \
                  (device_id, model, event_type, input, output, cache_creation, cache_read, executed_at, event_id) \
                  VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
                .to_string(),
            args: vec![
                p.get("device_id").cloned().unwrap_or(Value::Null),
                p.get("model").cloned().unwrap_or(Value::Null),
                p.get("event_type").cloned().unwrap_or(Value::Null),
                p.get("input").cloned().unwrap_or(json!(0)),
                p.get("output").cloned().unwrap_or(json!(0)),
                p.get("cache_creation").cloned().unwrap_or(json!(0)),
                p.get("cache_read").cloned().unwrap_or(json!(0)),
                p.get("executed_at").cloned().unwrap_or(json!(now)),
                Value::String(event_id.clone()),
            ],
        });
    }

    // 2. SELECT new deltas from other devices.
    let pull_index: Option<usize> = if do_pull {
        let idx = statements.len();
        statements.push(Statement {
            sql: "SELECT id, device_id, model, input, output, cache_creation, cache_read, executed_at \
                  FROM token_delta \
                  WHERE id > ? AND device_id != ? AND executed_at >= ? \
                  ORDER BY id ASC LIMIT ?"
                .to_string(),
            args: vec![
                json!(last_remote_id),
                Value::String(device_id.to_string()),
                json!(now - RETENTION_SECONDS),
                json!(PULL_LIMIT),
            ],
        });
        Some(idx)
    } else {
        None
    };

    // 3. Opportunistic remote cleanup.
    if cleanup_due {
        statements.push(Statement {
            sql: "DELETE FROM token_delta WHERE executed_at < ?".to_string(),
            args: vec![json!(now - RETENTION_SECONDS)],
        });
    }

    if statements.is_empty() {
        return Ok(());
    }

    let results = pipeline::execute(&config.libsql_url, &config.auth_token, &statements)?;

    let pull_rows = pull_index
        .and_then(|idx| results.get(idx).and_then(|opt| opt.as_ref()))
        .map(|r| r.rows.clone())
        .unwrap_or_default();

    // 4. Commit local-side bookkeeping in a single transaction.
    db.execute_batch("BEGIN IMMEDIATE")?;
    let result: Result<()> = (|| {
        if !outbox_rows.is_empty() {
            let mut del = db.prepare("DELETE FROM cloud_outbox WHERE event_id = ?")?;
            for (event_id, _) in &outbox_rows {
                del.execute(params![event_id])?;
            }
        }
        let mut new_max = last_remote_id;
        if !pull_rows.is_empty() {
            let mut insert = db.prepare(
                "INSERT OR REPLACE INTO cloud_cache \
                 (remote_id, device_id, model, input, output, cache_creation, cache_read, executed_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for row in &pull_rows {
                let id = pipeline::row_i64(row, 0);
                insert.execute(params![
                    id,
                    pipeline::row_str(row, 1),
                    pipeline::row_str(row, 2),
                    pipeline::row_i64(row, 3),
                    pipeline::row_i64(row, 4),
                    pipeline::row_i64(row, 5),
                    pipeline::row_i64(row, 6),
                    pipeline::row_i64(row, 7),
                ])?;
                if id > new_max {
                    new_max = id;
                }
            }
        }
        if new_max > last_remote_id {
            set_pull_cursor(db, new_max)?;
        }
        // Local-side cloud_cache retention. JS does this every sync round; we
        // match that — it's a single indexed DELETE, essentially free.
        db.execute(
            "DELETE FROM cloud_cache WHERE executed_at < ?1",
            params![now - RETENTION_SECONDS],
        )?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            db.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            let _ = db.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
