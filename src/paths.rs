use std::path::PathBuf;

use anyhow::{Context, Result};

pub fn home() -> Result<PathBuf> {
    dirs::home_dir().context("could not determine home directory")
}

pub fn claude_dir() -> Result<PathBuf> {
    Ok(home()?.join(".claude"))
}

pub fn data_dir() -> Result<PathBuf> {
    Ok(claude_dir()?.join("data"))
}

// hooks_dir is referenced by the installer (task #6) when offering to remove
// legacy ~/.claude/hooks/xclaude-*.js files after a migration.
#[allow(dead_code)]
pub fn hooks_dir() -> Result<PathBuf> {
    Ok(claude_dir()?.join("hooks"))
}

pub fn db_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("xclaude-usage.db"))
}

pub fn log_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("xclaude-usage.log"))
}

pub fn cloud_config_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("xclaude-cloud.json"))
}

pub fn settings_path() -> Result<PathBuf> {
    Ok(claude_dir()?.join("settings.json"))
}

pub fn ensure_data_dir() -> Result<()> {
    let dir = data_dir()?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    Ok(())
}
