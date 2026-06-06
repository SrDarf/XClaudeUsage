use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
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
    let payload: HookPayload =
        serde_json::from_str(&input).context("hook payload is not valid JSON")?;
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
    let cleanup_due = (now - cloud::get_last_cleanup_at(&db)?) >= cloud::CLEANUP_INTERVAL_SECONDS;
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

    // Main session transcript.
    ingest_transcript(
        &tx,
        session_id,
        transcript_path,
        fallback_model,
        local_device_label,
        now,
    )?;

    // Subagent and workflow transcripts. Claude Code writes the assistant
    // events of Task subagents, deep-research, and ultracode dynamic workflows
    // into a sibling `<session>/subagents/**/agent-*.jsonl` tree marked
    // `isSidechain: true` — these are NEVER folded into the main transcript and
    // the hook never hands us their paths, so we discover and ingest them here.
    // Each file is tracked by its own `transcript_progress` offset and deduped
    // by `message_uuid`, exactly like the main transcript.
    for sub in subagent_transcripts(transcript_path) {
        ingest_transcript(
            &tx,
            session_id,
            &sub,
            fallback_model,
            local_device_label,
            now,
        )?;
    }

    tx.commit()?;
    Ok(())
}

/// Incrementally read a single JSONL transcript from its saved byte offset,
/// record any new assistant token-usage rows, and advance the offset. Safe to
/// call for the main transcript and every subagent/workflow transcript within
/// the same transaction.
fn ingest_transcript(
    tx: &Transaction<'_>,
    session_id: &str,
    transcript_path: &Path,
    fallback_model: Option<&str>,
    device_label: &str,
    now: i64,
) -> Result<()> {
    let offset: i64 = tx
        .query_row(
            "SELECT byte_offset FROM transcript_progress WHERE transcript_path = ?1",
            params![transcript_path.to_string_lossy()],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);

    // `None` means the file couldn't be stat'd (e.g. removed between discovery
    // and read) — skip it without disturbing other transcripts in this tx.
    let Some(read) = transcript::read_new(transcript_path, offset as u64)? else {
        return Ok(());
    };

    if !read.text.is_empty() {
        let events = transcript::parse_assistant_events(&read.text, fallback_model);
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
                        device_label,
                        ev.uuid,
                    ])?;
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
    Ok(())
}

/// Discover the subagent/workflow transcripts that belong to a main session
/// transcript. For `.../projects/<proj>/<session>.jsonl` they live under
/// `.../projects/<proj>/<session>/subagents/` — plain Task subagents directly
/// inside it, and deep-research / ultracode dynamic workflows nested under
/// `subagents/workflows/wf_*/` — all named `agent-*.jsonl`. Returns an empty
/// vec when the directory is absent (the common case for a session that never
/// spawned a subagent).
fn subagent_transcripts(main: &Path) -> Vec<PathBuf> {
    // `<...>/<session>.jsonl` -> `<...>/<session>` -> `<...>/<session>/subagents`
    let root = main.with_extension("").join("subagents");
    let mut out = Vec::new();
    collect_agent_files(&root, &mut out, 0);
    out
}

fn collect_agent_files(dir: &Path, out: &mut Vec<PathBuf>, depth: u32) {
    // The real tree is at most `subagents/workflows/wf_*/agent-*.jsonl`; bound
    // recursion so a pathological symlink loop can't wedge the hook.
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            collect_agent_files(&path, out, depth + 1);
        } else if ft.is_file() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with("agent-") && name.ends_with(".jsonl") {
                    out.push(path);
                }
            }
        }
    }
}

fn run_retention_cleanup(db: &mut Connection, now: i64) -> Result<()> {
    let cutoff = now - cloud::RETENTION_SECONDS;
    let tx = db.transaction()?;
    tx.execute(
        "DELETE FROM token_usage WHERE executed_at < ?1",
        params![cutoff],
    )?;
    tx.execute(
        "DELETE FROM cloud_cache WHERE executed_at < ?1",
        params![cutoff],
    )?;
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
    let Some(event_type) = event_type else {
        return Ok(());
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    const EV1: &str = r#"{"type":"assistant","uuid":"uuid-1","message":{"model":"claude-x","usage":{"input_tokens":10,"output_tokens":20,"cache_creation_input_tokens":5,"cache_read_input_tokens":3}}}"#;
    const EV2: &str = r#"{"type":"assistant","uuid":"uuid-2","message":{"model":"claude-x","usage":{"output_tokens":40}}}"#;

    fn unique_tmp(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("xcu-test-{}-{}-{}", tag, std::process::id(), n));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::schema::migrate(&conn).unwrap();
        conn
    }

    fn write_transcript(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, contents).unwrap();
        p
    }

    fn sum_quantity(db: &Connection, ty: &str) -> i64 {
        db.query_row(
            "SELECT COALESCE(SUM(quantity), 0) FROM token_usage WHERE token_type = ?1",
            params![ty],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn row_count(db: &Connection) -> i64 {
        db.query_row("SELECT COUNT(*) FROM token_usage", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn write_local_ingests_main_transcript_tokens() {
        let dir = unique_tmp("ingest");
        let main = write_transcript(&dir, "session.jsonl", &format!("{EV1}\n"));
        let mut db = mem_db();
        write_local(&mut db, "sess", &main, None, "dev", 1234).unwrap();

        assert_eq!(sum_quantity(&db, "input"), 10);
        assert_eq!(sum_quantity(&db, "output"), 20);
        assert_eq!(sum_quantity(&db, "cache_creation"), 5);
        assert_eq!(sum_quantity(&db, "cache_read"), 3);
        assert_eq!(row_count(&db), 4, "one row per non-zero token type");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_local_skips_zero_quantity_token_types() {
        let dir = unique_tmp("zero");
        let main = write_transcript(&dir, "session.jsonl", &format!("{EV2}\n"));
        let mut db = mem_db();
        write_local(&mut db, "sess", &main, None, "dev", 1).unwrap();
        assert_eq!(row_count(&db), 1, "only the non-zero output row is written");
        assert_eq!(sum_quantity(&db, "output"), 40);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_local_is_incremental_and_idempotent() {
        let dir = unique_tmp("incr");
        let main = write_transcript(&dir, "session.jsonl", &format!("{EV1}\n"));
        let mut db = mem_db();

        // First pass ingests EV1.
        write_local(&mut db, "sess", &main, None, "dev", 1).unwrap();
        // Re-running with no new bytes must add nothing (offset == size).
        write_local(&mut db, "sess", &main, None, "dev", 2).unwrap();
        assert_eq!(row_count(&db), 4, "no duplicate rows on a no-op re-run");

        // Appending a new event ingests only the delta.
        std::fs::write(&main, format!("{EV1}\n{EV2}\n")).unwrap();
        write_local(&mut db, "sess", &main, None, "dev", 3).unwrap();
        assert_eq!(sum_quantity(&db, "output"), 60, "20 from EV1 + 40 from EV2");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_local_advances_byte_offset() {
        let dir = unique_tmp("offset");
        let body = format!("{EV1}\n");
        let main = write_transcript(&dir, "session.jsonl", &body);
        let mut db = mem_db();
        write_local(&mut db, "sess", &main, None, "dev", 1).unwrap();
        let offset: i64 = db
            .query_row(
                "SELECT byte_offset FROM transcript_progress WHERE transcript_path = ?1",
                params![main.to_string_lossy()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(offset, body.len() as i64);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_local_ingests_subagent_transcripts() {
        let dir = unique_tmp("subingest");
        let main = write_transcript(&dir, "session.jsonl", &format!("{EV1}\n"));
        // Sibling subagent tree: <dir>/session/subagents/agent-a.jsonl
        let sub = dir.join("session").join("subagents");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("agent-a.jsonl"), format!("{EV2}\n")).unwrap();

        let mut db = mem_db();
        write_local(&mut db, "sess", &main, None, "dev", 1).unwrap();
        // EV1 output 20 (main) + EV2 output 40 (subagent) = 60.
        assert_eq!(sum_quantity(&db, "output"), 60);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ingest_dedupes_duplicate_message_uuid() {
        let dir = unique_tmp("dedup");
        let main = write_transcript(&dir, "session.jsonl", &format!("{EV1}\n"));
        // A second transcript carrying the SAME message uuid as EV1.
        let other = write_transcript(&dir, "other.jsonl", &format!("{EV1}\n"));
        let mut db = mem_db();
        {
            let tx = db.transaction().unwrap();
            ingest_transcript(&tx, "sess", &main, None, "dev", 1).unwrap();
            ingest_transcript(&tx, "sess", &other, None, "dev", 1).unwrap();
            tx.commit().unwrap();
        }
        // The unique (message_uuid, token_type) index dedupes the second file's rows.
        assert_eq!(
            sum_quantity(&db, "output"),
            20,
            "duplicate uuid not double-counted"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn discovers_plain_and_workflow_subagent_transcripts() {
        // Mirror the on-disk layout:
        //   <proj>/<session>.jsonl                                   (main)
        //   <proj>/<session>/subagents/agent-aaa.jsonl              (Task subagent)
        //   <proj>/<session>/subagents/workflows/wf_x/agent-bbb.jsonl (workflow)
        let proj = unique_tmp("layout");
        let session = "11111111-2222-3333-4444-555555555555";
        let main = proj.join(format!("{session}.jsonl"));
        std::fs::write(&main, "{}\n").unwrap();

        let sub = proj.join(session).join("subagents");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("agent-aaa.jsonl"), "{}\n").unwrap();

        let wf = sub.join("workflows").join("wf_deadbeef");
        std::fs::create_dir_all(&wf).unwrap();
        std::fs::write(wf.join("agent-bbb.jsonl"), "{}\n").unwrap();

        // Decoys that must NOT be picked up.
        std::fs::write(sub.join("scratch.jsonl"), "{}\n").unwrap();
        std::fs::write(sub.join("agent-notes.txt"), "x").unwrap();

        let mut found: Vec<String> = subagent_transcripts(&main)
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        found.sort();

        assert_eq!(found, vec!["agent-aaa.jsonl", "agent-bbb.jsonl"]);
        std::fs::remove_dir_all(&proj).ok();
    }

    #[test]
    fn subagents_dir_absent_yields_empty() {
        let proj = unique_tmp("empty");
        let main = proj.join("66666666-7777-8888-9999-000000000000.jsonl");
        std::fs::write(&main, "{}\n").unwrap();
        assert!(subagent_transcripts(&main).is_empty());
        std::fs::remove_dir_all(&proj).ok();
    }
}
