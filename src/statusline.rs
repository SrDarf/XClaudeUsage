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

/// One rate-limit window and the singleton table that stores it. The names are
/// fixed constants, never user input (no injection risk in the `format!`ed SQL).
struct WindowTable {
    table: &'static str,
    /// The window-start column: `start_at` / `starts_at` (historical naming).
    start_col: &'static str,
    seconds: i64,
}

const FIVE_HOUR: WindowTable = WindowTable {
    table: "five_hour_window",
    start_col: "start_at",
    seconds: 5 * 3600,
};
const SEVEN_DAY: WindowTable = WindowTable {
    table: "seven_day_window",
    start_col: "starts_at",
    seconds: 7 * 24 * 3600,
};

/// A drop in `used_percentage` within the same `resets_at` larger than this is
/// an early reset (a global reset or a redeemed one), not rounding noise.
const RESET_DROP_POINTS: f64 = 1.0;

/// 256-color lilac for the low band of the 7-day bar (same code as the JS).
const WEEKLY_LOW_COLOR: &str = "38;5;183";

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
        let now = unix_now();
        let _ = write_window(conn, five_hour, &FIVE_HOUR, now);
        let _ = write_window(conn, seven_day, &SEVEN_DAY, now);
    }

    // Plans without a 5-hour cap (e.g. Ultra) only send rate_limits.seven_day:
    // fall back to the weekly window, drawn in its own color.
    let active = active_window(five_hour, seven_day);

    let tokens = active
        .zip(conn.as_ref())
        .and_then(|(a, conn)| read_window_tokens(conn, a.window, a.table).ok().flatten())
        .or_else(|| read_session_tokens(payload.transcript_path.as_deref(), &session));

    let token_str = render_token_segment(active, tokens.as_ref());

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

/// The rate-limit window the bar is drawn from.
#[derive(Clone, Copy)]
struct ActiveWindow {
    window: Window,
    table: &'static WindowTable,
    weekly: bool,
}

/// The 5-hour window when it carries a percentage, else the 7-day one.
fn active_window(five_hour: Option<Window>, seven_day: Option<Window>) -> Option<ActiveWindow> {
    let has_pct = |w: &Window| w.used_percentage.is_some();
    if let Some(window) = five_hour.filter(has_pct) {
        return Some(ActiveWindow {
            window,
            table: &FIVE_HOUR,
            weekly: false,
        });
    }
    seven_day.filter(has_pct).map(|window| ActiveWindow {
        window,
        table: &SEVEN_DAY,
        weekly: true,
    })
}

fn bar_color(pct: i64, weekly: bool) -> &'static str {
    match pct {
        p if p < 50 => {
            if weekly {
                WEEKLY_LOW_COLOR
            } else {
                "32"
            }
        }
        p if p < 65 => "33",
        p if p < 80 => "38;5;208",
        _ => "31",
    }
}

fn render_token_segment(active: Option<ActiveWindow>, tokens: Option<&Totals>) -> String {
    if let Some(a) = active {
        if let Some(pct_raw) = a.window.used_percentage {
            return format!(" │ {}", render_window(a.window, pct_raw, tokens, a.weekly));
        }
    }
    if let Some(t) = tokens {
        if t.output > 0 {
            return format!(" │ \x1b[2mout:{}\x1b[0m", fmt_tokens(t.output));
        }
    }
    String::new()
}

/// Same block bar for both windows; the 7-day one (plans without a 5h cap)
/// differs only in color: lilac instead of green in the low band.
fn render_window(win: Window, pct_raw: f64, tokens: Option<&Totals>, weekly: bool) -> String {
    let pct = pct_raw.clamp(0.0, 100.0).round() as i64;
    let filled = (pct / 10) as usize;
    let bar: String = "█".repeat(filled) + &"░".repeat(10 - filled);
    let color = bar_color(pct, weekly);
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
    let countdown = match win.resets_at {
        Some(r) => format!(" · resets:{}", fmt_countdown(r)),
        None => String::new(),
    };
    format!("\x1b[{color}m{bar}\x1b[0m \x1b[2m{out_label}{pct}%{countdown}\x1b[0m")
}

/// Upsert the singleton rate-limit window row. The hook is the only place that
/// sees both the stored and the new value, so it also detects an early reset:
/// same `resets_at` but `used_percentage` clearly lower stamps `reset_at = now`.
/// A new `resets_at` is a new window and clears it. SQLite evaluates every SET
/// expression against the old row, so the detection is part of the same atomic
/// statement and two concurrent hooks cannot interleave a read and a write.
fn write_window(
    conn: &Connection,
    window: Option<Window>,
    wt: &WindowTable,
    now: i64,
) -> Result<()> {
    let Some(window) = window else { return Ok(()) };
    let (Some(resets_at), Some(used)) = (window.resets_at, window.used_percentage) else {
        return Ok(());
    };
    let start = resets_at - wt.seconds;
    let WindowTable {
        table, start_col, ..
    } = wt;
    let sql = format!(
        "INSERT INTO {table} (id, resets_at, {start_col}, used_percentage, updated_at, reset_at) \
         VALUES (1, ?1, ?2, ?3, ?4, NULL) \
         ON CONFLICT(id) DO UPDATE SET \
           reset_at = CASE \
             WHEN excluded.resets_at <> {table}.resets_at THEN NULL \
             WHEN excluded.used_percentage < {table}.used_percentage - ?5 THEN excluded.updated_at \
             ELSE {table}.reset_at END, \
           resets_at = excluded.resets_at, \
           {start_col} = excluded.{start_col}, \
           used_percentage = excluded.used_percentage, \
           updated_at = excluded.updated_at"
    );
    conn.execute(
        &sql,
        params![resets_at, start, used, now, RESET_DROP_POINTS],
    )?;
    Ok(())
}

/// Tokens spent in the current budget: from the window start, or from the
/// early reset when one was recorded for this same window.
fn read_window_tokens(
    conn: &Connection,
    window: Window,
    wt: &WindowTable,
) -> Result<Option<Totals>> {
    let Some(end) = window.resets_at else {
        return Ok(None);
    };
    let reset_at: Option<i64> = conn
        .query_row(
            &format!(
                "SELECT reset_at FROM {} WHERE id = 1 AND resets_at = ?1",
                wt.table
            ),
            params![end],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let start = (end - wt.seconds).max(reset_at.unwrap_or(i64::MIN));
    let totals = query_window_tokens(conn, start, end)?;
    // Right after a reset the budget is genuinely empty: report zero instead of
    // None, or the caller falls back to the session transcript and shows tokens
    // spent before the reset.
    Ok(totals.or(reset_at.map(|_| Totals::default())))
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
    if h >= 24 {
        format!("{}d{:02}h", h / 24, h % 24)
    } else if h > 0 {
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

    fn five(w: Window) -> Option<ActiveWindow> {
        active_window(Some(w), None)
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
        assert_eq!(fmt_countdown_secs(86_399), "23h59m");
        assert_eq!(fmt_countdown_secs(86_400), "1d00h");
        assert_eq!(fmt_countdown_secs(5 * 86_400 + 10 * 3600 + 59), "5d10h");
    }

    #[test]
    fn active_window_prefers_five_hour_and_falls_back_to_seven_day() {
        let a = active_window(Some(win(None, Some(10.0))), Some(win(None, Some(40.0)))).unwrap();
        assert!(!a.weekly);
        assert_eq!(a.table.table, FIVE_HOUR.table);

        // Ultra: only seven_day is sent.
        let a = active_window(None, Some(win(None, Some(39.0)))).unwrap();
        assert!(a.weekly);
        assert_eq!(a.table.table, SEVEN_DAY.table);

        // A five_hour object without a percentage does not win.
        let a = active_window(Some(win(Some(1), None)), Some(win(None, Some(39.0)))).unwrap();
        assert!(a.weekly);

        assert!(active_window(None, Some(win(Some(1), None))).is_none());
        assert!(active_window(None, None).is_none());
    }

    #[test]
    fn render_weekly_low_band_is_lilac_other_bands_unchanged() {
        let weekly = |pct| active_window(None, Some(win(None, Some(pct))));
        let seg = render_token_segment(weekly(39.0), Some(&out_totals(3_280_000)));
        assert!(seg.contains("\x1b[38;5;183m███░░░░░░░"), "got: {seg}");
        assert!(seg.contains("out:3.28M/8.41M"), "got: {seg}");
        assert!(render_token_segment(weekly(55.0), None).contains("\x1b[33m"));
        assert!(render_token_segment(weekly(90.0), None).contains("\x1b[31m"));
    }

    #[test]
    fn render_extrapolates_real_limit_from_percentage() {
        // 300k output reported at 50% -> implied 5h limit of 600k.
        let seg = render_token_segment(five(win(None, Some(50.0))), Some(&out_totals(300_000)));
        assert!(seg.contains("out:300.0k/600.0k"), "got: {seg}");
        assert!(seg.contains("50%"), "got: {seg}");
        // 50 lands in the 50..65 band -> yellow "33".
        assert!(seg.contains("\x1b[33m"), "expected yellow band, got: {seg}");
        // bar: 5 filled, 5 empty.
        assert!(seg.contains("█████░░░░░"), "got: {seg}");
    }

    #[test]
    fn render_color_bands_track_percentage() {
        assert!(render_token_segment(five(win(None, Some(30.0))), None).contains("\x1b[32m"));
        assert!(render_token_segment(five(win(None, Some(70.0))), None).contains("\x1b[38;5;208m"));
        assert!(render_token_segment(five(win(None, Some(90.0))), None).contains("\x1b[31m"));
    }

    #[test]
    fn render_drops_limit_when_percentage_is_zero() {
        // pct_raw == 0 -> no divide, show the bare out:X label.
        let seg = render_token_segment(five(win(None, Some(0.0))), Some(&out_totals(427_000)));
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

    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::migrate(&conn).unwrap();
        conn
    }

    fn stored_reset_at(conn: &Connection) -> Option<i64> {
        conn.query_row("SELECT reset_at FROM seven_day_window", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn write_window_records_early_reset_and_clears_it_on_new_window() {
        let conn = mem_db();
        let resets = 10_000_000;
        let w = |pct| Some(win(Some(resets), Some(pct)));

        write_window(&conn, w(97.0), &SEVEN_DAY, 100).unwrap();
        assert_eq!(stored_reset_at(&conn), None, "first write is not a reset");
        write_window(&conn, w(98.0), &SEVEN_DAY, 200).unwrap();
        assert_eq!(stored_reset_at(&conn), None, "rising usage is not a reset");
        write_window(&conn, w(97.5), &SEVEN_DAY, 250).unwrap();
        assert_eq!(
            stored_reset_at(&conn),
            None,
            "a drop within 1 point is noise"
        );

        // The real case from issue #8: 98 -> 2 with the same resets_at.
        write_window(&conn, w(2.0), &SEVEN_DAY, 300).unwrap();
        assert_eq!(stored_reset_at(&conn), Some(300));
        write_window(&conn, w(5.0), &SEVEN_DAY, 400).unwrap();
        assert_eq!(
            stored_reset_at(&conn),
            Some(300),
            "kept while usage grows again"
        );

        write_window(
            &conn,
            Some(win(Some(resets + 7 * 86_400), Some(1.0))),
            &SEVEN_DAY,
            500,
        )
        .unwrap();
        assert_eq!(stored_reset_at(&conn), None, "a new window clears it");
    }

    #[test]
    fn read_window_tokens_starts_at_the_early_reset() {
        let conn = mem_db();
        let end = 10_000_000i64;
        let start = end - SEVEN_DAY.seconds;
        for (at, qty) in [(start + 10, 1_000), (start + 500, 20)] {
            conn.execute(
                "INSERT INTO token_usage (session_id, model, token_type, quantity, executed_at) \
                 VALUES ('s','m','output',?1,?2)",
                params![qty, at],
            )
            .unwrap();
        }
        let w = |pct| Some(win(Some(end), Some(pct)));
        write_window(&conn, w(98.0), &SEVEN_DAY, start + 100).unwrap();
        let before = read_window_tokens(&conn, w(98.0).unwrap(), &SEVEN_DAY).unwrap();
        assert_eq!(before.unwrap().output, 1_020);

        write_window(&conn, w(2.0), &SEVEN_DAY, start + 200).unwrap();
        let after = read_window_tokens(&conn, w(2.0).unwrap(), &SEVEN_DAY).unwrap();
        assert_eq!(
            after.unwrap().output,
            20,
            "tokens before the reset no longer count"
        );

        // A reset with nothing spent since is zero, not None (which would fall
        // back to the session transcript).
        write_window(&conn, w(0.0), &SEVEN_DAY, start + 600).unwrap();
        let empty = read_window_tokens(&conn, w(0.0).unwrap(), &SEVEN_DAY).unwrap();
        assert_eq!(empty.unwrap().output, 0);
    }

    #[test]
    fn migrate_adds_reset_at_to_an_existing_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE seven_day_window (id INTEGER PRIMARY KEY CHECK (id = 1), \
               resets_at INTEGER NOT NULL, starts_at INTEGER NOT NULL, \
               used_percentage REAL NOT NULL, updated_at INTEGER NOT NULL); \
             INSERT INTO seven_day_window VALUES (1, 10, 0, 50.0, 5);",
        )
        .unwrap();
        crate::db::schema::migrate(&conn).unwrap();
        crate::db::schema::migrate(&conn).unwrap(); // idempotent
        assert_eq!(stored_reset_at(&conn), None, "existing row keeps working");
        write_window(&conn, Some(win(Some(10), Some(3.0))), &SEVEN_DAY, 9).unwrap();
        assert_eq!(stored_reset_at(&conn), Some(9));
    }
}
