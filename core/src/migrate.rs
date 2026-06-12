//! Schema migration runner — 4.0 → current chain.
//!
//! Each step is atomic: BEGIN IMMEDIATE … COMMIT. Any failure rolls back the
//! entire step and returns an error — no half-migrated state.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension};

use crate::errors::{InnateError, Result};

// Embedded migration SQL ordered from lowest to highest target version.
const MIGRATIONS: &[(&str, &str, &str)] = &[
    ("4.0", "4.1", include_str!("migrations/4.0_to_4.1.sql")),
    ("4.1", "4.2", include_str!("migrations/4.1_to_4.2.sql")),
    ("4.2", "4.3", include_str!("migrations/4.2_to_4.3.sql")),
    ("4.3", "4.4", include_str!("migrations/4.3_to_4.4.sql")),
    ("4.4", "4.5", include_str!("migrations/4.4_to_4.5.sql")),
    ("4.5", "4.5.1", include_str!("migrations/4.5_to_4.5.1.sql")),
    (
        "4.5.1",
        "4.5.2",
        include_str!("migrations/4.5.1_to_4.5.2.sql"),
    ),
    ("4.5.2", "4.6", include_str!("migrations/4.5.2_to_4.6.sql")),
    ("4.6", "4.7", include_str!("migrations/4.6_to_4.7.sql")),
    ("4.7", "4.8", include_str!("migrations/4.7_to_4.8.sql")),
    ("4.8", "4.9", include_str!("migrations/4.8_to_4.9.sql")),
    ("4.9", "4.10", include_str!("migrations/4.9_to_4.10.sql")),
    ("4.10", "4.11", include_str!("migrations/4.10_to_4.11.sql")),
    ("4.11", "4.12", include_str!("migrations/4.11_to_4.12.sql")),
    ("4.12", "4.13", include_str!("migrations/4.12_to_4.13.sql")),
];

const TARGET: &str = "4.13";

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
        let copy_last_used =
            *to == "4.12" && column_exists(&conn, "chunks", "last_used_at")?;
        // Run the step atomically.
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let r = conn.execute_batch(sql);
        match r {
            Ok(()) => {
                if copy_last_used {
                    if let Err(error) =
                        conn.execute("UPDATE chunks SET last_used_base=last_used_at", [])
                    {
                        let _ = conn.execute_batch("ROLLBACK");
                        return Err(error.into());
                    }
                }
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

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let sql = format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name=?");
    Ok(conn.query_row(&sql, [column], |row| row.get::<_, i64>(0))? > 0)
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
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
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
