use std::fs::{self, OpenOptions};
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

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
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_iso8601_utc(secs)
}

fn format_iso8601_utc(secs: i64) -> String {
    // Convert a unix timestamp to "YYYY-MM-DDTHH:MM:SSZ" without pulling in chrono.
    let (year, month, day, hour, min, sec) = unix_to_civil(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

// Howard Hinnant's date algorithm: days since 1970-01-01 → (year, month, day).
// Reference: http://howardhinnant.github.io/date_algorithms.html
fn unix_to_civil(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days_total = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400) as u32;
    let hour = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    let sec = secs_of_day % 60;

    let z = days_total + 719_468;
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
