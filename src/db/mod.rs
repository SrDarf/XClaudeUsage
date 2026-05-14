pub mod schema;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

use crate::paths;

pub fn open() -> Result<Connection> {
    paths::ensure_data_dir()?;
    let path = paths::db_path()?;
    let conn = Connection::open(&path)
        .with_context(|| format!("opening sqlite db at {}", path.display()))?;
    apply_pragmas(&conn)?;
    schema::migrate(&conn)?;
    Ok(conn)
}

pub fn open_readonly() -> Result<Connection> {
    let path = paths::db_path()?;
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening sqlite db (readonly) at {}", path.display()))?;
    // busy_timeout still valid against a readonly handle when WAL writers exist.
    conn.busy_timeout(std::time::Duration::from_millis(2000))?;
    Ok(conn)
}

fn apply_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;\n\
         PRAGMA synchronous = NORMAL;",
    )?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    Ok(())
}
