use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db;
use crate::paths;
use crate::stdin;
use crate::time::unix_now;
use crate::transcript;

/// Matches the legacy JS self-timeout: statusLine commands have no
/// harness-side timeout, so a pipe that never closes must not hang us.
const STDIN_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Deserialize, Default)]
struct Payload {
    #[serde(default)]
    model: Option<ModelInfo>,
    #[serde(default)]
    workspace: Option<Workspace>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    transcript_path: Option<String>,
    #[serde(default)]
    rate_limits: Option<RateLimits>,
}

#[derive(Deserialize, Default)]
struct ModelInfo {
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Deserialize, Default)]
struct Workspace {
    #[serde(default)]
    current_dir: Option<String>,
}

#[derive(Deserialize, Default)]
struct RateLimits {
    #[serde(default)]
    five_hour: Option<Window>,
    #[serde(default)]
    seven_day: Option<Window>,
}

#[derive(Deserialize, Default, Clone, Copy)]
struct Window {
    #[serde(default)]
    resets_at: Option<i64>,
    #[serde(default)]
    used_percentage: Option<f64>,
}

#[derive(Default, Clone, Copy)]
struct Totals {
    input: i64,
    output: i64,
    cache_creation: i64,
    cache_read: i64,
}

pub fn run() -> Result<()> {
    let Some(input) = stdin::read_to_string_timeout(STDIN_TIMEOUT) else {
        return Ok(());
    };
    let payload: Payload = serde_json::from_str(&input).unwrap_or_default();

    let model = payload
        .model
        .as_ref()
        .and_then(|m| m.display_name.clone())
        .unwrap_or_else(|| "Claude".to_string());
    let dir = payload
        .workspace
        .as_ref()
        .and_then(|w| w.current_dir.clone())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    let session = payload.session_id.unwrap_or_default();

    let five_hour = payload.rate_limits.as_ref().and_then(|r| r.five_hour);
    let seven_day = payload.rate_limits.as_ref().and_then(|r| r.seven_day);

    // One writable connection for the whole render (it migrates once); both
    // window upserts and the token read share it. Absent DB → None, and we fall
    // back to the legacy transcript reader below.
    let conn = match paths::db_path() {
        Ok(p) if p.exists() => db::open().ok(),
        _ => None,
    };
    if let Some(conn) = conn.as_ref() {
        let _ = write_window(conn, five_hour, "five_hour_window", "start_at", 5 * 3600);
        let _ = write_window(
            conn,
            seven_day,
            "seven_day_window",
            "starts_at",
            7 * 24 * 3600,
        );
    }

    let tokens = conn
        .as_ref()
        .and_then(|conn| read_window_tokens(conn, five_hour).ok().flatten())
        .or_else(|| read_session_tokens(payload.transcript_path.as_deref(), &session));

    let token_str = render_token_segment(five_hour, tokens.as_ref());

    let dirname = Path::new(&dir)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.clone());

    let line = format!("\x1b[2m{model}\x1b[0m │ \x1b[2m{dirname}\x1b[0m{token_str}");
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = out.write_all(line.as_bytes());
    Ok(())
}

fn render_token_segment(five_hour: Option<Window>, tokens: Option<&Totals>) -> String {
    if let Some(fh) = five_hour {
        if let Some(pct_raw) = fh.used_percentage {
            let pct = pct_raw.min(100.0).round() as i64;
            let filled = (pct / 10).clamp(0, 10) as usize;
            let bar: String = "█".repeat(filled) + &"░".repeat(10 - filled);
            let color = match pct {
                p if p < 50 => "32",
                p if p < 65 => "33",
                p if p < 80 => "38;5;208",
                _ => "31",
            };
            let mut out_label = String::new();
            if let Some(t) = tokens {
                if t.output > 0 {
                    if pct_raw > 0.0 {
                        let real_limit = (t.output as f64) / (pct_raw / 100.0);
                        out_label = format!(
                            "out:{}/{} · ",
                            fmt_tokens(t.output),
                            fmt_tokens(real_limit.round() as i64),
                        );
                    } else {
                        out_label = format!("out:{} · ", fmt_tokens(t.output));
                    }
                }
            }
            let countdown = match fh.resets_at {
                Some(r) => format!(" · resets:{}", fmt_countdown(r)),
                None => String::new(),
            };
            return format!(
                " │ \x1b[{color}m{bar}\x1b[0m \x1b[2m{out_label}{pct}%{countdown}\x1b[0m"
            );
        }
    }
    if let Some(t) = tokens {
        if t.output > 0 {
            return format!(" │ \x1b[2mout:{}\x1b[0m", fmt_tokens(t.output));
        }
    }
    String::new()
}

/// Upsert the singleton rate-limit window row for `table`. `start_col` is the
/// table's window-start column (`start_at` / `starts_at`) and `window_secs` the
/// window length — both fixed constants, never user input (no injection risk in
/// the `format!`ed SQL).
fn write_window(
    conn: &Connection,
    window: Option<Window>,
    table: &str,
    start_col: &str,
    window_secs: i64,
) -> Result<()> {
    let Some(window) = window else { return Ok(()) };
    let (Some(resets_at), Some(used)) = (window.resets_at, window.used_percentage) else {
        return Ok(());
    };
    let start = resets_at - window_secs;
    let now = unix_now();
    let sql = format!(
        "INSERT INTO {table} (id, resets_at, {start_col}, used_percentage, updated_at) \
         VALUES (1, ?1, ?2, ?3, ?4) \
         ON CONFLICT(id) DO UPDATE SET \
           resets_at = excluded.resets_at, \
           {start_col} = excluded.{start_col}, \
           used_percentage = excluded.used_percentage, \
           updated_at = excluded.updated_at"
    );
    conn.execute(&sql, params![resets_at, start, used, now])?;
    Ok(())
}

fn read_window_tokens(conn: &Connection, fh: Option<Window>) -> Result<Option<Totals>> {
    let Some(fh) = fh else { return Ok(None) };
    let Some(resets_at) = fh.resets_at else {
        return Ok(None);
    };
    let end = resets_at;
    let start = end - 5 * 3600;
    query_window_tokens(conn, start, end)
}

/// Sum `token_usage` + `cloud_cache` rows whose `executed_at` falls in the
/// half-open window `[start, end)`. Split out from `read_window_tokens` so the
/// boundary logic is unit-testable against an in-memory DB without touching the
/// real db path.
fn query_window_tokens(conn: &Connection, start: i64, end: i64) -> Result<Option<Totals>> {
    let mut totals = Totals::default();
    let mut any_rows = false;

    let mut stmt = conn.prepare(
        "SELECT token_type, SUM(quantity) FROM token_usage \
         WHERE executed_at >= ?1 AND executed_at < ?2 GROUP BY token_type",
    )?;
    let rows = stmt
        .query_map(params![start, end], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1).unwrap_or(0)))
        })?
        .filter_map(|r| r.ok());
    for (ty, total) in rows {
        any_rows = true;
        match ty.as_str() {
            "input" => totals.input += total,
            "output" => totals.output += total,
            "cache_creation" => totals.cache_creation += total,
            "cache_read" => totals.cache_read += total,
            _ => {}
        }
    }
    drop(stmt);

    // cloud_cache may not exist yet on legacy DBs — but our schema migration
    // creates it. Still wrap defensively to mirror the JS catch-all.
    let cloud_row: Option<(i64, i64, i64, i64, i64)> = conn
        .query_row(
            "SELECT \
               COALESCE(SUM(input), 0), \
               COALESCE(SUM(output), 0), \
               COALESCE(SUM(cache_creation), 0), \
               COALESCE(SUM(cache_read), 0), \
               COUNT(*) \
             FROM cloud_cache WHERE executed_at >= ?1 AND executed_at < ?2",
            params![start, end],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    if let Some((i, o, cc, cr, n)) = cloud_row {
        if n > 0 {
            any_rows = true;
            totals.input += i;
            totals.output += o;
            totals.cache_creation += cc;
            totals.cache_read += cr;
        }
    }
    Ok(if any_rows { Some(totals) } else { None })
}

// ---------------------------------------------------------------------------
// Legacy single-session fallback: parse the transcript JSONL directly. Used
// when the DB is unavailable (Node < 22.5 install pattern, or DB missing).
// Kept in Rust for symmetry with the JS implementation.

#[derive(Serialize, Deserialize, Default)]
struct LegacyEntry {
    offset: u64,
    input: i64,
    output: i64,
    cache_creation: i64,
    cache_read: i64,
}

#[derive(Serialize, Deserialize, Default)]
struct LegacyCache {
    files: HashMap<String, LegacyEntry>,
}

fn read_session_tokens(transcript_path: Option<&str>, session: &str) -> Option<Totals> {
    let transcript_path = transcript_path?;
    // Non-empty + no traversal: an empty session would collapse every session
    // onto one shared claude-tokens-.json cache, mixing their totals.
    if !paths::is_safe_session_id(session) {
        return None;
    }
    let mut paths_vec: Vec<PathBuf> = Vec::new();
    let main = Path::new(transcript_path);
    if main.exists() {
        paths_vec.push(main.to_path_buf());
    }
    // Same discovery walk as the recorder, so the fallback and the DB path can
    // never disagree about which subagent/workflow files count.
    paths_vec.extend(transcript::subagent_transcripts(main));
    if paths_vec.is_empty() {
        return None;
    }

    let cache_path = std::env::temp_dir().join(format!("claude-tokens-{session}.json"));
    let mut cache: LegacyCache = fs::read_to_string(&cache_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    for p in &paths_vec {
        let key = p.to_string_lossy().into_owned();
        let entry = cache.files.entry(key).or_default();
        consume_transcript(p, entry);
    }

    if let Ok(serialized) = serde_json::to_string(&cache) {
        let _ = fs::write(&cache_path, serialized);
    }

    let mut total = Totals::default();
    for e in cache.files.values() {
        total.input += e.input;
        total.output += e.output;
        total.cache_creation += e.cache_creation;
        total.cache_read += e.cache_read;
    }
    Some(total)
}

fn consume_transcript(path: &Path, entry: &mut LegacyEntry) {
    // A shrunken file means the transcript was recreated: read_new would
    // silently restart from 0, so the accumulated counters must reset too or
    // the re-read would double-count.
    if let Ok(meta) = fs::metadata(path) {
        if entry.offset > meta.len() {
            *entry = LegacyEntry::default();
        }
    }
    // Delegate the incremental read to the shared reader — it finds the last
    // newline at the byte level, so invalid UTF-8 can't skew the saved offset
    // (each lossy U+FFFD replacement is 3 bytes, not 1).
    let Ok(Some(read)) = transcript::read_new(path, entry.offset) else {
        return;
    };
    for ev in transcript::parse_assistant_events(&read.text, None) {
        entry.input += ev.usage.input;
        entry.output += ev.usage.output;
        entry.cache_creation += ev.usage.cache_creation;
        entry.cache_read += ev.usage.cache_read;
    }
    entry.offset = read.new_offset;
}

// ---------------------------------------------------------------------------
// Formatting helpers — bit-identical to xclaude-usage.js.

fn fmt_countdown(resets_at: i64) -> String {
    fmt_countdown_secs(resets_at - unix_now())
}

fn fmt_countdown_secs(secs_left: i64) -> String {
    let secs_left = secs_left.max(0);
    let h = secs_left / 3600;
    let m = (secs_left % 3600) / 60;
    let s = secs_left % 60;
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

fn fmt_tokens(n: i64) -> String {
    if n == 0 {
        return "0".to_string();
    }
    if n < 1_000 {
        return n.to_string();
    }
    if n < 1_000_000 {
        return format!("{:.1}k", (n as f64) / 1_000.0);
    }
    format!("{:.2}M", (n as f64) / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(resets_at: Option<i64>, pct: Option<f64>) -> Window {
        Window {
            resets_at,
            used_percentage: pct,
        }
    }

    fn out_totals(output: i64) -> Totals {
        Totals {
            input: 0,
            output,
            cache_creation: 0,
            cache_read: 0,
        }
    }

    #[test]
    fn fmt_tokens_scales_by_magnitude() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1_000), "1.0k");
        assert_eq!(fmt_tokens(427_000), "427.0k");
        assert_eq!(fmt_tokens(1_000_000), "1.00M");
        assert_eq!(fmt_tokens(2_500_000), "2.50M");
    }

    #[test]
    fn fmt_countdown_secs_formats_units_and_clamps_negative() {
        assert_eq!(fmt_countdown_secs(-10), "0s");
        assert_eq!(fmt_countdown_secs(0), "0s");
        assert_eq!(fmt_countdown_secs(5), "5s");
        assert_eq!(fmt_countdown_secs(65), "1m05s");
        assert_eq!(fmt_countdown_secs(3600), "1h00m");
        assert_eq!(fmt_countdown_secs(3661), "1h01m");
        assert_eq!(fmt_countdown_secs(7322), "2h02m");
    }

    #[test]
    fn render_extrapolates_real_limit_from_percentage() {
        // 300k output reported at 50% -> implied 5h limit of 600k.
        let seg = render_token_segment(Some(win(None, Some(50.0))), Some(&out_totals(300_000)));
        assert!(seg.contains("out:300.0k/600.0k"), "got: {seg}");
        assert!(seg.contains("50%"), "got: {seg}");
        // 50 lands in the 50..65 band -> yellow "33".
        assert!(seg.contains("\x1b[33m"), "expected yellow band, got: {seg}");
        // bar: 5 filled, 5 empty.
        assert!(seg.contains("█████░░░░░"), "got: {seg}");
    }

    #[test]
    fn render_color_bands_track_percentage() {
        assert!(render_token_segment(Some(win(None, Some(30.0))), None).contains("\x1b[32m"));
        assert!(render_token_segment(Some(win(None, Some(70.0))), None).contains("\x1b[38;5;208m"));
        assert!(render_token_segment(Some(win(None, Some(90.0))), None).contains("\x1b[31m"));
    }

    #[test]
    fn render_drops_limit_when_percentage_is_zero() {
        // pct_raw == 0 -> no divide, show the bare out:X label.
        let seg = render_token_segment(Some(win(None, Some(0.0))), Some(&out_totals(427_000)));
        assert!(seg.contains("out:427.0k ·"), "got: {seg}");
        assert!(
            !seg.contains('/'),
            "no extrapolated limit when pct is 0: {seg}"
        );
    }

    #[test]
    fn render_legacy_segment_without_window() {
        // No five-hour window, but session tokens exist -> bare out: label.
        let seg = render_token_segment(None, Some(&out_totals(12_345)));
        assert_eq!(seg, " │ \x1b[2mout:12.3k\x1b[0m");
    }

    #[test]
    fn render_empty_when_no_window_and_no_output() {
        assert_eq!(render_token_segment(None, None), "");
        assert_eq!(render_token_segment(None, Some(&out_totals(0))), "");
    }

    #[test]
    fn query_window_tokens_respects_half_open_boundary() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::migrate(&conn).unwrap();
        let start = 1_000_000i64;
        let end = start + 5 * 3600;
        let insert = |at: i64, qty: i64| {
            conn.execute(
                "INSERT INTO token_usage (session_id, model, token_type, quantity, executed_at) \
                 VALUES ('s','m','output',?1,?2)",
                params![qty, at],
            )
            .unwrap();
        };
        insert(start - 1, 1); // before window -> excluded
        insert(start, 10); // >= start -> included
        insert(end - 1, 100); // < end -> included
        insert(end, 1000); // == end -> excluded (half-open)

        let totals = query_window_tokens(&conn, start, end).unwrap().unwrap();
        assert_eq!(totals.output, 110, "only [start, end) rows are summed");
    }

    #[test]
    fn query_window_tokens_includes_cloud_cache() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::migrate(&conn).unwrap();
        let start = 2_000_000i64;
        let end = start + 5 * 3600;
        conn.execute(
            "INSERT INTO token_usage (session_id, model, token_type, quantity, executed_at) \
             VALUES ('s','m','output',5,?1)",
            params![start + 1],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cloud_cache (remote_id, device_id, model, output, executed_at) \
             VALUES (1,'d','m',7,?1)",
            params![start + 2],
        )
        .unwrap();
        let totals = query_window_tokens(&conn, start, end).unwrap().unwrap();
        assert_eq!(
            totals.output, 12,
            "local token_usage + cloud_cache combined"
        );
    }

    #[test]
    fn query_window_tokens_is_none_when_empty() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::migrate(&conn).unwrap();
        assert!(query_window_tokens(&conn, 0, 100).unwrap().is_none());
    }
}
