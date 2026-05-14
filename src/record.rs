use std::io::{self, Read};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use uuid::Uuid;

use crate::cloud;
use crate::db;
use crate::transcript;

/// Per-hook capabilities. Source of truth — matches xclaude-record.js:HOOK_BEHAVIOR.
#[derive(Debug, Clone, Copy)]
struct Behavior {
    local_write: bool,
    push: bool,
    pull: bool,
    event_type: Option<&'static str>,
}

fn behavior_for(event: &str) -> Behavior {
    match event {
        "Stop" => Behavior {
            local_write: true,
            push: true,
            pull: true,
            event_type: Some("stop"),
        },
        "SubagentStop" => Behavior {
            local_write: true,
            push: true,
            pull: false,
            event_type: Some("subagent_stop"),
        },
        "SubagentStart" => Behavior {
            local_write: true,
            push: false,
            pull: false,
            event_type: None,
        },
        "PostToolUse" => Behavior {
            local_write: true,
            push: false,
            pull: true,
            event_type: None,
        },
        _ => Behavior {
            local_write: true,
            push: false,
            pull: false,
            event_type: None,
        },
    }
}

#[derive(Deserialize)]
struct HookPayload {
    session_id: Option<String>,
    transcript_path: Option<String>,
    #[serde(default)]
    hook_event_name: Option<String>,
    #[serde(default)]
    model: Option<ModelInfo>,
}

#[derive(Deserialize, Default)]
struct ModelInfo {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
}

pub fn run() -> Result<()> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).ok();
    if input.trim().is_empty() {
        return Ok(());
    }
    let payload: HookPayload = serde_json::from_str(&input)
        .context("hook payload is not valid JSON")?;
    record(payload)
}

fn record(payload: HookPayload) -> Result<()> {
    let session_id = payload.session_id.unwrap_or_default();
    let transcript_path = payload.transcript_path.unwrap_or_default();
    if session_id.is_empty() || transcript_path.is_empty() {
        return Ok(());
    }
    // Defensive: refuse path traversal in session_id (matches JS guard).
    if session_id.contains('/') || session_id.contains('\\') || session_id.contains("..") {
        return Ok(());
    }

    let event_name = payload
        .hook_event_name
        .as_deref()
        .unwrap_or("Stop")
        .to_string();
    let behavior = behavior_for(&event_name);
    if !behavior.local_write && !behavior.push && !behavior.pull {
        return Ok(());
    }

    let fallback_model = payload
        .model
        .as_ref()
        .and_then(|m| m.id.clone().or_else(|| m.display_name.clone()));
    let local_device_label = hostname();
    let now = unix_now();

    let mut db = db::open()?;

    if behavior.local_write {
        write_local(
            &mut db,
            &session_id,
            Path::new(&transcript_path),
            fallback_model.as_deref(),
            &local_device_label,
            now,
        )?;
    }

    // Opportunistic retention cleanup, gated to once per 24h.
    let cleanup_due =
        (now - cloud::get_last_cleanup_at(&db)?) >= cloud::CLEANUP_INTERVAL_SECONDS;
    if cleanup_due {
        if let Err(e) = run_retention_cleanup(&mut db, now) {
            crate::log::warn(&format!("retention cleanup failed: {e:#}"));
        }
    }

    if !behavior.push && !behavior.pull {
        return Ok(());
    }

    let Some(config) = cloud::load_config()? else {
        return Ok(());
    };
    let cloud_device_id = cloud::resolve_device_id(&db, config.device_id.as_deref())?;

    if behavior.push {
        enqueue_outbox(&mut db, &cloud_device_id, behavior.event_type, now)?;
    }

    if let Err(e) = cloud::sync::sync(
        &config,
        &db,
        &cloud_device_id,
        behavior.push,
        behavior.pull,
        cleanup_due,
    ) {
        crate::log::warn(&format!("cloud sync failed: {e:#}"));
    }
    Ok(())
}

fn write_local(
    db: &mut Connection,
    session_id: &str,
    transcript_path: &Path,
    fallback_model: Option<&str>,
    local_device_label: &str,
    now: i64,
) -> Result<()> {
    let tx = db.transaction()?;

    let offset: i64 = tx
        .query_row(
            "SELECT byte_offset FROM transcript_progress WHERE transcript_path = ?1",
            params![transcript_path.to_string_lossy()],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);

    let Some(read) = transcript::read_new(transcript_path, offset as u64)? else {
        tx.commit()?;
        return Ok(());
    };

    if !read.text.is_empty() {
        let events = transcript::parse_assistant_events(&read.text, fallback_model);
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO token_usage \
                 (session_id, model, token_type, quantity, executed_at, device_id, message_uuid) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for ev in events {
                for (ty, qty) in [
                    ("input", ev.usage.input),
                    ("output", ev.usage.output),
                    ("cache_creation", ev.usage.cache_creation),
                    ("cache_read", ev.usage.cache_read),
                ] {
                    if qty > 0 {
                        stmt.execute(params![
                            session_id,
                            ev.model,
                            ty,
                            qty,
                            now,
                            local_device_label,
                            ev.uuid,
                        ])?;
                    }
                }
            }
        }
    }

    tx.execute(
        "INSERT INTO transcript_progress (transcript_path, byte_offset, updated_at) \
         VALUES (?1, ?2, ?3) \
         ON CONFLICT(transcript_path) DO UPDATE SET \
           byte_offset = excluded.byte_offset, updated_at = excluded.updated_at",
        params![
            transcript_path.to_string_lossy(),
            read.new_offset as i64,
            now,
        ],
    )?;
    tx.commit()?;
    Ok(())
}

fn run_retention_cleanup(db: &mut Connection, now: i64) -> Result<()> {
    let cutoff = now - cloud::RETENTION_SECONDS;
    let tx = db.transaction()?;
    tx.execute("DELETE FROM token_usage WHERE executed_at < ?1", params![cutoff])?;
    tx.execute("DELETE FROM cloud_cache WHERE executed_at < ?1", params![cutoff])?;
    tx.execute(
        "INSERT INTO cloud_state (key, value) VALUES ('last_cleanup_at', ?1) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![now.to_string()],
    )?;
    tx.commit()?;
    Ok(())
}

/// Group new token_usage rows since `last_pushed_id` by model and enqueue one
/// outbox row per model+event. Advance the cursor past every newer row,
/// including ones outside the retention window — see plan §"Cursor avança além
/// da janela".
fn enqueue_outbox(
    db: &mut Connection,
    cloud_device_id: &str,
    event_type: Option<&str>,
    now: i64,
) -> Result<()> {
    let Some(event_type) = event_type else { return Ok(()) };
    let last_pushed = cloud::get_or_init_push_cursor(db)?;
    let window_start = now - cloud::RETENTION_SECONDS;

    let tx = db.transaction()?;
    let groups: Vec<(String, i64, i64, i64, i64, i64)> = {
        let mut stmt = tx.prepare(
            "SELECT \
               model, \
               SUM(CASE WHEN token_type='input'          THEN quantity ELSE 0 END) AS input, \
               SUM(CASE WHEN token_type='output'         THEN quantity ELSE 0 END) AS output, \
               SUM(CASE WHEN token_type='cache_creation' THEN quantity ELSE 0 END) AS cache_creation, \
               SUM(CASE WHEN token_type='cache_read'     THEN quantity ELSE 0 END) AS cache_read, \
               MAX(executed_at) AS executed_at \
             FROM token_usage \
             WHERE id > ?1 AND executed_at >= ?2 \
             GROUP BY model",
        )?;
        let rows = stmt.query_map(params![last_pushed, window_start], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1).unwrap_or(0),
                row.get::<_, i64>(2).unwrap_or(0),
                row.get::<_, i64>(3).unwrap_or(0),
                row.get::<_, i64>(4).unwrap_or(0),
                row.get::<_, i64>(5).unwrap_or(now),
            ))
        })?;
        rows.filter_map(|r| r.ok()).collect()
    };

    {
        let mut insert = tx.prepare(
            "INSERT OR IGNORE INTO cloud_outbox (event_id, payload, created_at) \
             VALUES (?1, ?2, ?3)",
        )?;
        for (model, input, output, cache_creation, cache_read, executed_at) in groups {
            let total = input + output + cache_creation + cache_read;
            if total == 0 {
                continue;
            }
            let payload = serde_json::json!({
                "device_id": cloud_device_id,
                "model": model,
                "event_type": event_type,
                "input": input,
                "output": output,
                "cache_creation": cache_creation,
                "cache_read": cache_read,
                "executed_at": executed_at,
            })
            .to_string();
            insert.execute(params![Uuid::new_v4().to_string(), payload, now])?;
        }
    }

    // Advance cursor past every newer row regardless of window membership,
    // so we don't keep re-scanning out-of-window rows that will never be pushed.
    let new_cursor: i64 = tx.query_row(
        "SELECT COALESCE(MAX(id), ?1) FROM token_usage WHERE id > ?1",
        params![last_pushed],
        |row| row.get(0),
    )?;
    if new_cursor > last_pushed {
        tx.execute(
            "INSERT INTO cloud_state (key, value) VALUES ('last_pushed_id', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![new_cursor.to_string()],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn hostname() -> String {
    // Avoid an extra crate for this — just call the system. Best-effort; empty
    // string on failure matches the JS `os.hostname() || ''`.
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| {
            #[cfg(unix)]
            {
                use std::process::Command;
                Command::new("hostname")
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            }
            #[cfg(not(unix))]
            {
                std::env::var("COMPUTERNAME").ok()
            }
        })
        .unwrap_or_default()
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
