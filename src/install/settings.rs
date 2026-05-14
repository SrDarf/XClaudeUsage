// Reader / merger / writer for ~/.claude/settings.json.
//
// Hard rules (must not regress; carried over from the JS installer):
//   1. Refuse to overwrite a non-XClaude `statusLine`.
//   2. Identify our own entries by substring on `command`. Recognize BOTH the
//      legacy JS path (`xclaude-usage.js`/`xclaude-record.js`) AND the new
//      Rust invocation (`xclaudeusage statusline`/`xclaudeusage record`), so
//      re-runs upgrade in place instead of duplicating.
//   3. Preserve every hook entry that isn't ours.
//   4. Write a timestamped backup only when content actually changes.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

const HOOK_TIMEOUT: u64 = 10;

pub fn read(path: &Path) -> Result<Value> {
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_str(&raw)
        .with_context(|| format!("{} is not valid JSON", path.display()))?;
    if !value.is_object() {
        anyhow::bail!(
            "{} does not contain a JSON object at the top level",
            path.display()
        );
    }
    Ok(value)
}

/// Returns `(changed, backup_path)`. Writes only if the serialized content
/// differs from what's on disk.
pub fn write_if_changed(path: &Path, value: &Value) -> Result<(bool, Option<PathBuf>)> {
    let body = format!("{}\n", serde_json::to_string_pretty(value)?);
    let existing = fs::read_to_string(path).ok();
    if existing.as_deref() == Some(body.as_str()) {
        return Ok((false, None));
    }
    let backup = if path.exists() {
        let stamp = iso8601_for_filename(unix_now());
        let bk = path.with_extension(format!("json.backup-{stamp}"));
        fs::copy(path, &bk).with_context(|| format!("backing up {}", path.display()))?;
        Some(bk)
    } else {
        None
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("json.write-{}", std::process::id()));
    let mut f = fs::File::create(&tmp)?;
    f.write_all(body.as_bytes())?;
    drop(f);
    fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok((true, backup))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusLineState {
    /// No statusLine configured at all.
    Absent,
    /// statusLine present and recognized as ours (legacy or current).
    Ours,
    /// statusLine present, points at someone else's command. Do not touch.
    Foreign,
}

pub fn classify_status_line(settings: &Value) -> StatusLineState {
    let Some(sl) = settings.get("statusLine") else {
        return StatusLineState::Absent;
    };
    if sl.is_null() {
        return StatusLineState::Absent;
    }
    let cmd = sl.get("command").and_then(Value::as_str).unwrap_or("");
    if is_ours_statusline(cmd) {
        StatusLineState::Ours
    } else {
        StatusLineState::Foreign
    }
}

pub fn is_ours_statusline(cmd: &str) -> bool {
    cmd.contains("xclaude-usage.js") || cmd.contains("xclaudeusage statusline")
}

pub fn is_ours_record(cmd: &str) -> bool {
    cmd.contains("xclaude-record.js") || cmd.contains("xclaudeusage record")
}

/// Set the `statusLine` entry, preserving any extra fields on the existing
/// object. Caller is responsible for having checked it isn't Foreign.
pub fn upsert_status_line(settings: &mut Value, command: &str) -> &'static str {
    let desired = json!({ "type": "command", "command": command });
    let obj = settings.as_object_mut().expect("settings is object");
    match obj.get_mut("statusLine") {
        None => {
            obj.insert("statusLine".to_string(), desired);
            "created"
        }
        Some(existing) if existing.is_null() => {
            *existing = desired;
            "created"
        }
        Some(existing) => {
            // Preserve other fields (e.g. `padding`), only overwrite type/command.
            let existing_obj = match existing.as_object_mut() {
                Some(o) => o,
                None => {
                    *existing = desired;
                    return "updated";
                }
            };
            existing_obj.insert("type".to_string(), json!("command"));
            existing_obj.insert("command".to_string(), json!(command));
            "updated"
        }
    }
}

/// Ensure each event in `events` has exactly one hook entry whose command is
/// ours, with the given `command`. Other tools' entries in the same arrays are
/// left untouched. Returns a per-event action ("added" | "updated").
pub fn upsert_hooks(
    settings: &mut Value,
    events: &[&str],
    command: &str,
) -> Vec<(String, &'static str)> {
    let desired_entry = json!({
        "type": "command",
        "command": command,
        "timeout": HOOK_TIMEOUT,
    });
    let obj = settings.as_object_mut().expect("settings is object");
    let hooks = obj
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks_obj = match hooks.as_object_mut() {
        Some(o) => o,
        None => {
            *hooks = Value::Object(Map::new());
            hooks.as_object_mut().unwrap()
        }
    };

    let mut summary = Vec::new();
    for event in events {
        let list = hooks_obj
            .entry((*event).to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if !list.is_array() {
            *list = Value::Array(Vec::new());
        }
        let arr = list.as_array_mut().unwrap();

        let mut touched = false;
        for group in arr.iter_mut() {
            let Some(group_hooks) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            for entry in group_hooks.iter_mut() {
                let cmd = entry
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if is_ours_record(cmd) {
                    *entry = desired_entry.clone();
                    touched = true;
                }
            }
        }
        if !touched {
            arr.push(json!({ "hooks": [desired_entry.clone()] }));
            summary.push(((*event).to_string(), "added"));
        } else {
            summary.push(((*event).to_string(), "updated"));
        }
    }
    summary
}

/// Remove every XClaude entry from the settings tree. Used by `uninstall`.
/// Returns the number of entries removed (statusLine + hook entries).
pub fn remove_all_xclaude(settings: &mut Value) -> usize {
    let mut removed = 0;
    if let Some(obj) = settings.as_object_mut() {
        if let Some(sl) = obj.get("statusLine") {
            if let Some(cmd) = sl.get("command").and_then(Value::as_str) {
                if is_ours_statusline(cmd) {
                    obj.remove("statusLine");
                    removed += 1;
                }
            }
        }
        if let Some(hooks) = obj.get_mut("hooks").and_then(Value::as_object_mut) {
            let event_names: Vec<String> = hooks.keys().cloned().collect();
            for event in event_names {
                let Some(arr) = hooks.get_mut(&event).and_then(Value::as_array_mut) else {
                    continue;
                };
                arr.retain_mut(|group| {
                    let Some(group_hooks) = group.get_mut("hooks").and_then(Value::as_array_mut)
                    else {
                        return true;
                    };
                    group_hooks.retain(|entry| {
                        let cmd = entry
                            .get("command")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if is_ours_record(cmd) {
                            removed += 1;
                            false
                        } else {
                            true
                        }
                    });
                    !group_hooks.is_empty()
                });
            }
        }
    }
    removed
}

fn iso8601_for_filename(secs: i64) -> String {
    // Same format as the JS installer: ISO-8601 with `:` and `.` swapped for `-`.
    // Reuses the civil-date math from log.rs to avoid pulling in chrono.
    let (y, mo, d, h, mi, s) = unix_to_civil(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}-{mi:02}-{s:02}-000Z")
}

// Duplicated locally to keep `log.rs` private API surface minimal.
fn unix_to_civil(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400) as u32;
    let hour = sod / 3600;
    let min = (sod % 3600) / 60;
    let sec = sod % 60;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i32 + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d, hour, min, sec)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
