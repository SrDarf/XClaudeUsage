// Locate and (optionally) remove the legacy ~/.claude/hooks/xclaude-*.js
// files left over by a Node-based install.

use std::path::PathBuf;

use anyhow::Result;

pub const LEGACY_FILES: &[&str] = &["xclaude-usage.js", "xclaude-record.js"];

/// Return any legacy JS hook files that currently exist on disk.
pub fn legacy_hook_files() -> Result<Vec<PathBuf>> {
    let dir = crate::paths::hooks_dir()?;
    let mut found = Vec::new();
    for name in LEGACY_FILES {
        let p = dir.join(name);
        if p.exists() {
            found.push(p);
        }
    }
    Ok(found)
}
