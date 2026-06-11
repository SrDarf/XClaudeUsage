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

// Used by the migrator to remove legacy ~/.claude/hooks/xclaude-*.js files.
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
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(())
}

/// A session id is interpolated into file names and DB rows: it must be
/// non-empty (an empty id would collapse every session onto one shared cache
/// file) and free of path separators / parent-dir traversal. Single guard
/// shared by the recorder and the statusline so the rule can't drift.
pub fn is_safe_session_id(s: &str) -> bool {
    !s.is_empty() && !s.contains('/') && !s.contains('\\') && !s.contains("..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_guard_rejects_empty_and_traversal() {
        assert!(is_safe_session_id("11111111-2222-3333-4444-555555555555"));
        assert!(!is_safe_session_id(""));
        assert!(!is_safe_session_id("../../etc"));
        assert!(!is_safe_session_id("a/b"));
        assert!(!is_safe_session_id("a\\b"));
    }
}
