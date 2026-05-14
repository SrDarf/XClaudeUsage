// Cloud sync (Turso/libSQL) — see plan §"cloud sync".
// Some helpers below (`get_pull_cursor`, `set_*_cursor`, etc.) are called only
// from `sync.rs`, which is a no-op stub during phases 1-4. Allow dead code so
// the partial build is clean — every helper is exercised once task #5 lands.
#![allow(dead_code)]

pub mod pipeline;
pub mod sync;

use std::fs;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use uuid::Uuid;

use crate::paths;

pub const NETWORK_TIMEOUT: Duration = Duration::from_secs(5);
pub const PULL_LIMIT: i64 = 500;
pub const RETENTION_SECONDS: i64 = 15 * 24 * 3600;
pub const CLEANUP_INTERVAL_SECONDS: i64 = 24 * 3600;

#[derive(Debug, Clone)]
pub struct CloudConfig {
    pub libsql_url: String,
    pub auth_token: String,
    pub device_id: Option<String>,
}

#[derive(Deserialize)]
struct RawCloudConfig {
    libsql_url: Option<String>,
    auth_token: Option<String>,
    #[serde(default)]
    device_id: Option<String>,
}

/// Load `~/.claude/data/xclaude-cloud.json`. Returns `Ok(None)` if the file is
/// absent (cloud sync disabled). Returns `Err` if the file exists but is malformed.
pub fn load_config() -> Result<Option<CloudConfig>> {
    let path = paths::cloud_config_path()?;
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let cfg: RawCloudConfig = serde_json::from_str(&raw)
        .with_context(|| format!("{} is not valid JSON", path.display()))?;
    let libsql_url = cfg
        .libsql_url
        .ok_or_else(|| anyhow::anyhow!("xclaude-cloud.json: libsql_url missing"))?;
    // Match the JS regex /^(libsql|https?):\/\//. The http:// path is the
    // common one for self-hosted libsql containers (no TLS termination).
    if !libsql_url.starts_with("libsql://")
        && !libsql_url.starts_with("https://")
        && !libsql_url.starts_with("http://")
    {
        anyhow::bail!(
            "xclaude-cloud.json: libsql_url must start with libsql://, https:// or http://"
        );
    }
    let auth_token = cfg
        .auth_token
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("xclaude-cloud.json: auth_token missing or empty"))?;
    // libSQL clients translate libsql:// to https:// for HTTP transport.
    let http_url = libsql_url
        .replacen("libsql://", "https://", 1)
        .trim_end_matches('/')
        .to_string();
    Ok(Some(CloudConfig {
        libsql_url: http_url,
        auth_token,
        device_id: cfg.device_id.filter(|s| !s.is_empty()),
    }))
}

/// Return the cloud-side device id. Prefer the config value; otherwise reuse the
/// random UUID we persisted on first run.
pub fn resolve_device_id(db: &Connection, config_device_id: Option<&str>) -> Result<String> {
    if let Some(id) = config_device_id {
        return Ok(id.to_string());
    }
    let existing: Option<String> = db
        .query_row(
            "SELECT value FROM cloud_state WHERE key = 'device_id'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = Uuid::new_v4().to_string();
    db.execute(
        "INSERT OR IGNORE INTO cloud_state (key, value) VALUES ('device_id', ?1)",
        params![id],
    )?;
    let stored: String = db.query_row(
        "SELECT value FROM cloud_state WHERE key = 'device_id'",
        [],
        |row| row.get(0),
    )?;
    Ok(stored)
}

pub fn get_pull_cursor(db: &Connection) -> Result<i64> {
    let v: Option<String> = db
        .query_row(
            "SELECT value FROM cloud_state WHERE key = 'last_remote_id'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(v.and_then(|s| s.parse().ok()).unwrap_or(0))
}

pub fn set_pull_cursor(db: &Connection, id: i64) -> Result<()> {
    db.execute(
        "INSERT INTO cloud_state (key, value) VALUES ('last_remote_id', ?1)\n         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![id.to_string()],
    )?;
    Ok(())
}

/// `last_pushed_id` initialization: on first read for a fresh install, seed it
/// to `MAX(token_usage.id)` so we don't retroactively flood the cloud with
/// every row that already existed locally. From there it advances as rows are
/// pushed (or skipped because they fell out of the window).
pub fn get_or_init_push_cursor(db: &Connection) -> Result<i64> {
    let existing: Option<String> = db
        .query_row(
            "SELECT value FROM cloud_state WHERE key = 'last_pushed_id'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(v) = existing {
        return Ok(v.parse().unwrap_or(0));
    }
    let max: i64 = db
        .query_row(
            "SELECT COALESCE(MAX(id), 0) FROM token_usage",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    db.execute(
        "INSERT INTO cloud_state (key, value) VALUES ('last_pushed_id', ?1)",
        params![max.to_string()],
    )?;
    Ok(max)
}

pub fn set_push_cursor(db: &Connection, id: i64) -> Result<()> {
    db.execute(
        "INSERT INTO cloud_state (key, value) VALUES ('last_pushed_id', ?1)\n         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![id.to_string()],
    )?;
    Ok(())
}

pub fn get_last_cleanup_at(db: &Connection) -> Result<i64> {
    let v: Option<String> = db
        .query_row(
            "SELECT value FROM cloud_state WHERE key = 'last_cleanup_at'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(v.and_then(|s| s.parse().ok()).unwrap_or(0))
}

pub fn set_last_cleanup_at(db: &Connection, ts: i64) -> Result<()> {
    db.execute(
        "INSERT INTO cloud_state (key, value) VALUES ('last_cleanup_at', ?1)\n         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![ts.to_string()],
    )?;
    Ok(())
}
