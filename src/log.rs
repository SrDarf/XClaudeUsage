use std::fs::{self, OpenOptions};
use std::io::Write;

const LOG_MAX_BYTES: u64 = 1_000_000;

pub fn warn(msg: &str) {
    let _ = append(msg);
}

fn append(msg: &str) -> std::io::Result<()> {
    let path = match crate::paths::log_path() {
        Ok(p) => p,
        Err(_) => return Ok(()),
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let ts = iso8601_now();
    let line = format!("[{ts}] {msg}\n");
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    f.write_all(line.as_bytes())?;

    // Rotate if oversized. Best-effort; ignore failures.
    if let Ok(meta) = fs::metadata(&path) {
        if meta.len() > LOG_MAX_BYTES {
            let rotated = path.with_extension("log.1");
            let _ = fs::rename(&path, &rotated);
        }
    }
    Ok(())
}

fn iso8601_now() -> String {
    // "YYYY-MM-DDTHH:MM:SSZ" without pulling in chrono.
    let (year, month, day, hour, min, sec) = crate::time::unix_to_civil(crate::time::unix_now());
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}
