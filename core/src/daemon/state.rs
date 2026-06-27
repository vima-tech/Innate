use super::*;
use rusqlite::OptionalExtension;

pub(in crate::daemon) fn init_state_db(path: &Path) -> Result<()> {
    let conn = rusqlite::Connection::open(path)?;
    conn.execute_batch(DAEMON_SCHEMA)?;
    Ok(())
}

pub(in crate::daemon) fn read_pid(pid_file: &Path) -> Option<u32> {
    std::fs::read_to_string(pid_file).ok()?.trim().parse().ok()
}

/// Returns `true` if a daemon process recorded in `pid_file` is currently alive.
pub fn is_running(pid_file: &Path) -> bool {
    read_pid(pid_file).is_some_and(process_alive)
}

#[cfg(target_os = "linux")]
pub(in crate::daemon) fn process_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(not(target_os = "linux"))]
pub(in crate::daemon) fn process_alive(_pid: u32) -> bool {
    false
}

/// Read-only daemon health for `inspect().operational.daemon` (design doc §5.1).
///
/// Opens `daemon_state.sqlite` with an **independent read-only connection** (never
/// ATTACH into the main db) so a missing/corrupt/locked daemon db degrades gracefully
/// and can never poison the caller's transaction. Returns:
/// - `{"state":"never_run"}` when the state db does not exist,
/// - `{"state":"unknown","error":…}` when it cannot be opened/read,
/// - otherwise a summary with `errors_by_operation` + `errors_24h/7d` (rates, not the
///   misleading cumulative count), `last_error`, `processed_events`, and per-watch
///   `lag_bytes` (file size − last processed offset).
pub fn health(state_db: &Path, pid_file: &Path, now_iso: &str) -> serde_json::Value {
    use serde_json::json;
    let running = read_pid(pid_file).is_some_and(process_alive);
    let pid = read_pid(pid_file);

    if !state_db.exists() {
        return json!({"state": "never_run", "running": running, "pid": pid});
    }

    let conn = match rusqlite::Connection::open_with_flags(
        state_db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(e) => {
            return json!({"state": "unknown", "running": running, "pid": pid, "error": e.to_string()})
        }
    };

    let read = || -> rusqlite::Result<serde_json::Value> {
        let cutoff_24h = days_ago_iso(now_iso, 1);
        let cutoff_7d = days_ago_iso(now_iso, 7);

        let mut by_op = serde_json::Map::new();
        {
            let mut stmt = conn.prepare(
                "SELECT operation, COUNT(*) FROM daemon_errors
                 WHERE ts >= ?1 GROUP BY operation ORDER BY COUNT(*) DESC",
            )?;
            let rows =
                stmt.query_map([&cutoff_7d], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            for row in rows.flatten() {
                by_op.insert(row.0, json!(row.1));
            }
        }
        let errors_24h: i64 =
            conn.query_row("SELECT COUNT(*) FROM daemon_errors WHERE ts >= ?1", [&cutoff_24h], |r| {
                r.get(0)
            })?;
        let errors_7d: i64 =
            conn.query_row("SELECT COUNT(*) FROM daemon_errors WHERE ts >= ?1", [&cutoff_7d], |r| {
                r.get(0)
            })?;
        let last_error = conn
            .query_row(
                "SELECT operation, message, ts FROM daemon_errors ORDER BY ts DESC LIMIT 1",
                [],
                |r| {
                    Ok(json!({
                        "operation": r.get::<_, String>(0)?,
                        "message": r.get::<_, String>(1)?.chars().take(200).collect::<String>(),
                        "ts": r.get::<_, String>(2)?,
                    }))
                },
            )
            .optional()?
            .unwrap_or(serde_json::Value::Null);
        let processed: i64 = conn
            .query_row("SELECT COUNT(*) FROM processed_events", [], |r| r.get(0))
            .unwrap_or(0);

        // Per-watch lag: file size − last processed offset.
        let mut watches = Vec::new();
        {
            let mut stmt =
                conn.prepare("SELECT watch_path, last_processed_offset, updated_at FROM watch_state")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?))
            })?;
            for row in rows.flatten() {
                let size = std::fs::metadata(&row.0).map(|m| m.len() as i64).unwrap_or(-1);
                let lag = if size >= 0 { (size - row.1).max(0) } else { -1 };
                watches.push(json!({
                    "path": row.0, "offset": row.1, "lag_bytes": lag, "updated_at": row.2,
                }));
            }
        }

        Ok(json!({
            "state": if running { "running" } else { "stopped" },
            "running": running,
            "pid": pid,
            "errors_by_operation": by_op,
            "errors_24h": errors_24h,
            "errors_7d": errors_7d,
            "last_error": last_error,
            "processed_events": processed,
            "watches": watches,
        }))
    };

    match read() {
        Ok(v) => v,
        Err(e) => json!({"state": "unknown", "running": running, "pid": pid, "error": e.to_string()}),
    }
}

/// `now_iso` minus `days`, in the same `YYYY-MM-DDTHH:MM:SS.mmmZ` lexicographic format.
fn days_ago_iso(now_iso: &str, days: i64) -> String {
    use chrono::{DateTime, Duration, Utc};
    match now_iso.parse::<DateTime<Utc>>() {
        Ok(dt) => (dt - Duration::days(days))
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string(),
        Err(_) => now_iso.to_string(),
    }
}
