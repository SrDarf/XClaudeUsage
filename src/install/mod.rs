// Top-level orchestration of `xclaudeusage install` / `uninstall` / `doctor`.

mod migrate;
mod prompt;
mod settings;

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::paths;

const BASE_EVENTS: &[&str] = &["Stop", "SubagentStop"];
const CLOUD_EVENTS: &[&str] = &["Stop", "SubagentStop", "SubagentStart", "PostToolUse"];

#[derive(Debug)]
struct CloudAnswer {
    enabled: bool,
    libsql_url: String,
    auth_token: String,
    device_id: String,
}

pub fn run() -> Result<()> {
    let bin = std::env::current_exe().context("locating xclaudeusage binary")?;
    let bin_str = bin.to_string_lossy().into_owned();
    let statusline_cmd = format!("\"{bin_str}\" statusline");
    let record_cmd = format!("\"{bin_str}\" record");

    let settings_path = paths::settings_path()?;
    let mut settings = settings::read(&settings_path)
        .context("reading existing ~/.claude/settings.json")?;

    match settings::classify_status_line(&settings) {
        settings::StatusLineState::Foreign => {
            anyhow::bail!(
                "a non-XClaude `statusLine` is already configured in {}. \
                 Refusing to overwrite it. Remove or rename it manually, then re-run.",
                settings_path.display()
            );
        }
        _ => {}
    }

    let mut tty = prompt::Tty::open()?;
    tty.writeln("XClaudeUsage installer")?;
    tty.writeln(&format!("  binary: {bin_str}"))?;
    tty.writeln(&format!("  settings: {}", settings_path.display()))?;

    let cloud = prompt_cloud(&mut tty)?;

    let events: &[&str] = if cloud.enabled { CLOUD_EVENTS } else { BASE_EVENTS };

    let status_action = settings::upsert_status_line(&mut settings, &statusline_cmd);
    let hook_actions = settings::upsert_hooks(&mut settings, events, &record_cmd);

    let cloud_result = if cloud.enabled {
        Some(write_cloud_config(&cloud)?)
    } else {
        None
    };

    paths::ensure_data_dir()?;
    let (changed, backup) = settings::write_if_changed(&settings_path, &settings)
        .with_context(|| format!("writing {}", settings_path.display()))?;

    tty.writeln("")?;
    tty.writeln("[install] done.")?;
    if changed {
        if let Some(b) = backup {
            tty.writeln(&format!("  backed up settings.json -> {}", b.display()))?;
        }
        tty.writeln(&format!("  statusLine: {status_action}"))?;
        for (ev, action) in &hook_actions {
            tty.writeln(&format!("  hook {ev}: {action}"))?;
        }
    } else {
        tty.writeln("  settings.json: up to date (no changes)")?;
    }
    tty.writeln(&format!(
        "  cloud sync: {}",
        if cloud.enabled { "enabled" } else { "disabled" }
    ))?;
    if let Some(action) = cloud_result {
        tty.writeln(&format!(
            "  cloud config: {}",
            if action { "written" } else { "up to date" }
        ))?;
    }

    let legacy = migrate::legacy_hook_files()?;
    if !legacy.is_empty() {
        tty.writeln("")?;
        tty.writeln("Detected legacy JS hook files from a previous Node-based install:")?;
        for p in &legacy {
            tty.writeln(&format!("  - {}", p.display()))?;
        }
        let ans = tty.ask("Delete them? [y/N]: ")?.trim().to_lowercase();
        if ans == "y" || ans == "yes" {
            for p in &legacy {
                if let Err(e) = fs::remove_file(p) {
                    tty.writeln(&format!("  could not remove {}: {e}", p.display()))?;
                } else {
                    tty.writeln(&format!("  removed {}", p.display()))?;
                }
            }
        } else {
            tty.writeln("Keeping legacy files in place. Re-run install later to clean up.")?;
        }
    }

    tty.writeln("")?;
    tty.writeln("Restart Claude Code in a fresh session to pick up the changes.")?;
    tty.writeln(
        "Tip: installing mid-session back-fills the current 5-hour window with the \
         transcript's history. A virgin session starts the counter clean.",
    )?;
    Ok(())
}

pub fn doctor() -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "xclaudeusage doctor")?;
    writeln!(out, "  binary:        {}", std::env::current_exe()?.display())?;
    writeln!(out, "  claude_dir:    {}", paths::claude_dir()?.display())?;
    writeln!(out, "  db_path:       {}", paths::db_path()?.display())?;
    writeln!(out, "  settings_path: {}", paths::settings_path()?.display())?;
    writeln!(out, "  log_path:      {}", paths::log_path()?.display())?;
    writeln!(out, "  cloud_config:  {}", paths::cloud_config_path()?.display())?;
    let db_path = paths::db_path()?;
    if db_path.exists() {
        let conn = crate::db::open_readonly()?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM token_usage", [], |r| r.get(0))?;
        let cloud_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM cloud_cache", [], |r| r.get(0))
            .unwrap_or(0);
        let outbox: i64 = conn
            .query_row("SELECT COUNT(*) FROM cloud_outbox", [], |r| r.get(0))
            .unwrap_or(0);
        writeln!(out, "  token_usage rows:  {count}")?;
        writeln!(out, "  cloud_cache rows:  {cloud_count}")?;
        writeln!(out, "  cloud_outbox rows: {outbox}")?;
    } else {
        writeln!(out, "  (db not yet created — run a Claude Code turn first)")?;
    }
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let settings_path = paths::settings_path()?;
    let mut settings = settings::read(&settings_path)?;
    let removed = settings::remove_all_xclaude(&mut settings);
    let (changed, backup) = settings::write_if_changed(&settings_path, &settings)?;
    if !changed {
        println!("[uninstall] no XClaude entries found in {}", settings_path.display());
        return Ok(());
    }
    println!("[uninstall] removed {removed} entries from settings.json");
    if let Some(b) = backup {
        println!("[uninstall] backed up settings.json -> {}", b.display());
    }
    println!(
        "[uninstall] kept ~/.claude/data/xclaude-usage.db and xclaude-cloud.json. Delete manually if desired."
    );
    Ok(())
}

fn prompt_cloud(tty: &mut prompt::Tty) -> Result<CloudAnswer> {
    let existing = paths::cloud_config_path()
        .ok()
        .and_then(|p| fs::read_to_string(&p).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());

    let prompt_msg = "Enable Turso cloud sync for multi-device aggregation? [y/N]: ";
    let answer = tty.ask(prompt_msg)?.trim().to_lowercase();
    if answer != "y" && answer != "yes" {
        return Ok(CloudAnswer {
            enabled: false,
            libsql_url: String::new(),
            auth_token: String::new(),
            device_id: String::new(),
        });
    }

    if let Some(cfg) = existing.as_ref().filter(|v| v.is_object()) {
        let url = cfg.get("libsql_url").and_then(|v| v.as_str()).unwrap_or("");
        let token = cfg.get("auth_token").and_then(|v| v.as_str()).unwrap_or("");
        let device = cfg.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
        if !url.is_empty() && !token.is_empty() && !device.is_empty() {
            tty.writeln("")?;
            tty.writeln("Found an existing cloud config:")?;
            tty.writeln(&format!("  libsql_url: {url}"))?;
            tty.writeln(&format!("  auth_token: {}", prompt::mask_token(token)))?;
            tty.writeln(&format!("  device_id:  {device}"))?;
            let reuse = tty.ask("Use this existing config? [Y/n]: ")?.trim().to_lowercase();
            if reuse.is_empty() || reuse == "y" || reuse == "yes" {
                return Ok(CloudAnswer {
                    enabled: true,
                    libsql_url: url.to_string(),
                    auth_token: token.to_string(),
                    device_id: device.to_string(),
                });
            }
        }
    }

    tty.writeln("")?;
    tty.writeln(
        "You must have already created the Turso database (see README \"Multi-device sync\"). \
         The installer does NOT create the database — it only stores credentials.",
    )?;
    let libsql_url = ask_required(tty, "libsql_url (libsql:// or https://): ", |s| {
        s.starts_with("libsql://") || s.starts_with("https://") || s.starts_with("http://")
    })?;
    let auth_token = ask_required(tty, "auth_token: ", |s| !s.is_empty())?;
    let device_id = ask_required(tty, "device_id (e.g. luka-laptop): ", |s| !s.is_empty())?;
    Ok(CloudAnswer {
        enabled: true,
        libsql_url,
        auth_token,
        device_id,
    })
}

fn ask_required(
    tty: &mut prompt::Tty,
    prompt: &str,
    validate: impl Fn(&str) -> bool,
) -> Result<String> {
    loop {
        let v = tty.ask(prompt)?.trim().to_string();
        if !v.is_empty() && validate(&v) {
            return Ok(v);
        }
        tty.writeln(&format!("  invalid: {prompt}"))?;
    }
}

/// Returns `true` if the file was rewritten, `false` if it was already current.
fn write_cloud_config(cloud: &CloudAnswer) -> Result<bool> {
    let path = paths::cloud_config_path()?;
    paths::ensure_data_dir()?;
    let body = format!(
        "{}\n",
        serde_json::to_string_pretty(&serde_json::json!({
            "libsql_url": cloud.libsql_url,
            "auth_token": cloud.auth_token,
            "device_id": cloud.device_id,
        }))?
    );
    if let Ok(existing) = fs::read_to_string(&path) {
        if existing == body {
            return Ok(false);
        }
    }
    let tmp: PathBuf = path.with_extension(format!("json.write-{}", std::process::id()));
    let mut f = fs::File::create(&tmp)?;
    f.write_all(body.as_bytes())?;
    drop(f);
    fs::rename(&tmp, &path)?;
    Ok(true)
}
