//! Schema migration runner — 4.0 → 4.5.1 chain.
//!
//! Each step is atomic: BEGIN IMMEDIATE … COMMIT. Any failure rolls back the
//! entire step and returns an error — no half-migrated state.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension};

use crate::errors::{InnateError, Result};

// Embedded migration SQL ordered from lowest to highest target version.
const MIGRATIONS: &[(&str, &str, &str)] = &[
    ("4.0", "4.1",   include_str!("migrations/4.0_to_4.1.sql")),
    ("4.1", "4.2",   include_str!("migrations/4.1_to_4.2.sql")),
    ("4.2", "4.3",   include_str!("migrations/4.2_to_4.3.sql")),
    ("4.3", "4.4",   include_str!("migrations/4.3_to_4.4.sql")),
    ("4.4", "4.5",   include_str!("migrations/4.4_to_4.5.sql")),
    ("4.5", "4.5.1", include_str!("migrations/4.5_to_4.5.1.sql")),
];

const TARGET: &str = "4.5.1";

/// Run all pending migrations on `db_path`. Idempotent if already at target.
/// Returns the list of migration steps executed.
pub fn run_migrations(db_path: impl AsRef<Path>) -> Result<Vec<String>> {
    let conn = Connection::open(db_path.as_ref())?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA foreign_keys=ON;
         PRAGMA synchronous=NORMAL;",
    )?;

    let current = schema_version(&conn)?;
    if current == TARGET {
        return Ok(vec![]);
    }

    let mut applied = vec![];
    let mut ver = current;

    for (from, to, sql) in MIGRATIONS {
        if ver_tuple(&ver) >= ver_tuple(to) {
            continue; // already at or beyond this step
        }
        if ver_tuple(&ver) < ver_tuple(from) {
            return Err(InnateError::Other(format!(
                "Migration gap: database at {ver}, expected {from}→{to}. \
                 Is the database from an unsupported version?"
            )));
        }
        // Run the step atomically.
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let r = conn.execute_batch(sql);
        match r {
            Ok(()) => {
                conn.execute_batch("COMMIT")?;
                applied.push(format!("{from}→{to}"));
                ver = to.to_string();
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(InnateError::Other(format!(
                    "Migration {from}→{to} failed: {e}"
                )));
            }
        }
    }

    if ver != TARGET {
        return Err(InnateError::Other(format!(
            "After all migrations, schema version is {ver}, expected {TARGET}."
        )));
    }

    Ok(applied)
}

fn schema_version(conn: &Connection) -> Result<String> {
    let has_meta: bool = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='meta'",
        [],
        |r| r.get::<_, i64>(0),
    )? > 0;

    if !has_meta {
        return Err(InnateError::Other(
            "Database has no meta table — cannot migrate. \
             Use `innate` to create a fresh database."
                .into(),
        ));
    }

    let ver: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| r.get(0))
        .optional()?;

    ver.ok_or_else(|| InnateError::Other("meta table missing schema_version".into()))
}

fn ver_tuple(v: &str) -> (u32, u32, u32) {
    let parts: Vec<u32> = v.split('.').filter_map(|s| s.parse().ok()).collect();
    (
        parts.first().copied().unwrap_or(0),
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    )
}
