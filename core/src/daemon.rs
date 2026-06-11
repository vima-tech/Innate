//! Daemon: background log-watcher that bridges closed systems to the knowledge layer.
//!
//! Design: §九 — daemon does NOT open the knowledge database directly.
//! All knowledge-layer actions go through the CLI binary (subprocess).
//! Daemon state (offsets, inode, processed events) lives in daemon_state.sqlite only.
//!
//! Platform: Linux (fork + /proc). Non-Linux: return an informative error.

use std::path::Path;

use crate::errors::Result;

// ── Schema for daemon_state.sqlite ──────────────────────────────────────────

const DAEMON_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS watch_state (
    watch_path            TEXT PRIMARY KEY,
    last_processed_offset INTEGER NOT NULL DEFAULT 0,
    last_processed_inode  TEXT,
    updated_at            TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS processed_events (
    event_id   TEXT PRIMARY KEY,
    watch_path TEXT,
    trace_id   TEXT,
    event_type TEXT,
    ts         TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS trace_context (
    watch_path TEXT PRIMARY KEY,
    trace_id   TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS daemon_errors (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    watch_path TEXT,
    operation  TEXT NOT NULL,
    message    TEXT NOT NULL,
    ts         TEXT NOT NULL
);
"#;

// ── Public entry points ──────────────────────────────────────────────────────

pub fn start(
    watch_dirs: &[std::path::PathBuf],
    db_path: &Path,
    pid_file: &Path,
    state_db: &Path,
    log_file: &Path,
) -> anyhow::Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        anyhow::bail!(
            "innate daemon is only supported on Linux. \
             On other platforms use the SDK or CLI directly."
        );
    }

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;

        // Validate: warn if no watch dirs.
        if watch_dirs.is_empty() {
            eprintln!(
                "[innate daemon] warning: no --watch directories specified; \
                       daemon will start but won't monitor any logs"
            );
        }

        // Already running?
        if let Some(running_pid) = read_pid(pid_file) {
            if process_alive(running_pid) {
                anyhow::bail!(
                    "daemon already running (pid {}). \
                     Use `innate daemon stop` first.",
                    running_pid
                );
            }
        }

        // Create parent dirs.
        if let Some(p) = pid_file.parent() {
            std::fs::create_dir_all(p)?;
        }
        if let Some(p) = state_db.parent() {
            std::fs::create_dir_all(p)?;
        }
        if let Some(p) = log_file.parent() {
            std::fs::create_dir_all(p)?;
        }

        // Init daemon_state.sqlite.
        init_state_db(state_db)?;

        // Fork: parent writes pid and returns; child runs the watch loop.
        let watch_strs: Vec<String> = watch_dirs
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let db_str = db_path.to_string_lossy().into_owned();
        let sdb_str = state_db.to_string_lossy().into_owned();
        let log_str = log_file.to_string_lossy().into_owned();
        let pid_str = pid_file.to_string_lossy().into_owned();

        // Re-exec self with a hidden marker flag so the child enters watch_loop directly.
        let self_exe = std::env::current_exe()?;
        let mut cmd = std::process::Command::new(&self_exe);
        cmd.arg("--daemon-internal-watch")
            .arg("--db")
            .arg(&db_str)
            .arg("--state-db")
            .arg(&sdb_str)
            .arg("--log-file")
            .arg(&log_str)
            .arg("--pid-file")
            .arg(&pid_str);
        for w in &watch_strs {
            cmd.arg("--watch-dir").arg(w);
        }

        // Detach from terminal.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;

        std::fs::write(pid_file, child.id().to_string())?;
        println!("daemon started (pid {})", child.id());
        Ok(())
    }
}

pub fn stop(pid_file: &Path) -> anyhow::Result<()> {
    #[cfg(not(target_os = "linux"))]
    anyhow::bail!("innate daemon is only supported on Linux.");

    #[cfg(target_os = "linux")]
    {
        match read_pid(pid_file) {
            None => anyhow::bail!(
                "no pid file at {}; daemon may not be running",
                pid_file.display()
            ),
            Some(pid) => {
                if !process_alive(pid) {
                    let _ = std::fs::remove_file(pid_file);
                    println!("daemon was not running (stale pid {pid}); pid file removed");
                    return Ok(());
                }
                // SIGTERM
                let r = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
                if r != 0 {
                    anyhow::bail!(
                        "kill({pid}, SIGTERM) failed: {}",
                        std::io::Error::last_os_error()
                    );
                }
                // Wait up to 3 s then SIGKILL.
                for _ in 0..30 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if !process_alive(pid) {
                        let _ = std::fs::remove_file(pid_file);
                        println!("daemon stopped (pid {pid})");
                        return Ok(());
                    }
                }
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                }
                let _ = std::fs::remove_file(pid_file);
                println!("daemon killed (pid {pid})");
                Ok(())
            }
        }
    }
}

pub fn status(state_db: &Path, pid_file: &Path) -> anyhow::Result<()> {
    let pid = read_pid(pid_file);
    let running = pid.is_some_and(process_alive);
    println!(
        "status               : {}",
        if running { "running" } else { "stopped" }
    );
    println!(
        "pid                  : {}",
        pid.map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string())
    );

    if !state_db.exists() {
        println!(
            "daemon_state.sqlite not found at {}; daemon has never run.",
            state_db.display()
        );
        return Ok(());
    }
    let conn = rusqlite::Connection::open(state_db)?;
    let count: i64 = conn
        .query_row("SELECT count(*) FROM watch_state", [], |r| r.get(0))
        .unwrap_or(0);
    let processed: i64 = conn
        .query_row("SELECT count(*) FROM processed_events", [], |r| r.get(0))
        .unwrap_or(0);
    let errors: i64 = conn
        .query_row("SELECT count(*) FROM daemon_errors", [], |r| r.get(0))
        .unwrap_or(0);
    println!("watch_state entries  : {count}");
    println!("processed events     : {processed}");
    println!("errors               : {errors}");
    // List watch paths.
    let mut stmt =
        conn.prepare("SELECT watch_path, last_processed_offset, updated_at FROM watch_state")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    for row in rows.flatten() {
        println!("  {} offset={} updated={}", row.0, row.1, row.2);
    }
    Ok(())
}

// ── Internal: watch loop (called in the forked child) ───────────────────────

/// Entry point for the detached child process.
/// Called when the binary is re-executed with `--daemon-internal-watch`.
pub fn run_watch_loop(
    watch_dirs: &[String],
    db_path: &str,
    state_db_path: &str,
    log_path: &str,
    pid_file: &str,
) {
    // Write our own pid.
    let _ = std::fs::write(pid_file, std::process::id().to_string());

    // Open log file (append).
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path);

    let mut logger: Box<dyn std::io::Write + Send> = match log_file {
        Ok(f) => Box::new(f),
        Err(_) => Box::new(std::io::stderr()),
    };

    let _ = writeln!(logger, "[innate-daemon] started pid={}", std::process::id());

    let state_db = match rusqlite::Connection::open(state_db_path) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(logger, "[innate-daemon] cannot open state db: {e}");
            return;
        }
    };
    if state_db.execute_batch(DAEMON_SCHEMA).is_err() {
        let _ = writeln!(logger, "[innate-daemon] failed to init schema");
        return;
    }

    // Main poll loop: 500 ms tick.
    loop {
        for dir in watch_dirs {
            let dir_path = std::path::Path::new(dir);
            if !dir_path.exists() {
                continue;
            }
            // Find .log files in directory.
            if let Ok(entries) = std::fs::read_dir(dir_path) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().and_then(|e| e.to_str()) == Some("log") {
                        process_log_file(&p, &state_db, db_path, &mut *logger);
                    }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

fn process_log_file(
    path: &Path,
    state_db: &rusqlite::Connection,
    db_path: &str,
    log: &mut dyn std::io::Write,
) {
    let path_str = path.to_string_lossy();
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return,
    };

    // inode detection for rotation.
    #[cfg(target_os = "linux")]
    let inode = {
        use std::os::linux::fs::MetadataExt;
        meta.st_ino().to_string()
    };
    #[cfg(not(target_os = "linux"))]
    let inode = String::new();

    let (saved_offset, saved_inode): (i64, Option<String>) = state_db.query_row(
        "SELECT last_processed_offset, last_processed_inode FROM watch_state WHERE watch_path=?",
        rusqlite::params![path_str.as_ref()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap_or((0, None));

    // Reset on file rotation (inode change or file got shorter).
    let file_size = meta.len() as i64;
    let start_offset = if saved_inode.as_deref() != Some(&inode) || file_size < saved_offset {
        0
    } else {
        saved_offset
    };

    if start_offset >= file_size {
        return;
    }

    use std::io::{BufRead, Seek};
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return,
    };
    if f.seek(std::io::SeekFrom::Start(start_offset as u64))
        .is_err()
    {
        return;
    }

    let reader = std::io::BufReader::new(&mut f);
    let mut new_offset = start_offset;

    for line_res in reader.lines() {
        let line = match line_res {
            Ok(l) => l,
            Err(_) => break,
        };
        new_offset += line.len() as i64 + 1; // +1 for newline

        let Some(event) = parse_log_event(&line) else {
            continue;
        };
        let event_type = event.kind;

        // Compute event_id for idempotency.
        // Include inode so that a rotated file at the same path with the same
        // offset + content is not mistakenly treated as a duplicate event.
        let event_id = event
            .event_id
            .clone()
            .unwrap_or_else(|| event_id_for_line(path_str.as_ref(), &inode, new_offset, &line));

        // Skip if already processed.
        let already: i64 = state_db
            .query_row(
                "SELECT count(*) FROM processed_events WHERE event_id=?",
                rusqlite::params![event_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if already > 0 {
            continue;
        }

        // Handle "start": recall to open a trace and store it.
        if event_type == "start" {
            let query = event.query.as_deref().unwrap_or(&line);
            match call_cli_recall(db_path, query) {
                Ok(tid) => {
                    let ts = crate::utils::utc_now_iso();
                    let _ = state_db.execute(
                        "INSERT OR REPLACE INTO trace_context(watch_path, trace_id, updated_at) VALUES (?,?,?)",
                        rusqlite::params![path_str.as_ref(), &tid, &ts],
                    );
                    let _ = state_db.execute(
                        "INSERT OR IGNORE INTO processed_events(event_id, watch_path, trace_id, event_type, ts) VALUES (?,?,?,?,?)",
                        rusqlite::params![event_id, path_str.as_ref(), &tid, event_type, &ts],
                    );
                }
                Err(e) => {
                    let _ = writeln!(log, "[innate-daemon] recall for start event failed: {e}");
                    record_daemon_error(state_db, path_str.as_ref(), "recall", &e.to_string());
                }
            }
            continue;
        }

        // Look up trace for this watch path (ok/fail/end events).
        let context_trace_id: Option<String> = state_db
            .query_row(
                "SELECT trace_id FROM trace_context WHERE watch_path=?",
                rusqlite::params![path_str.as_ref()],
                |r| r.get(0),
            )
            .ok();
        let trace_id = event.trace_id.clone().or(context_trace_id);

        if event_type == "end" {
            let result = call_cli_evolve(db_path);
            let ts = crate::utils::utc_now_iso();
            match result {
                Ok(()) => {
                    let _ = state_db.execute(
                        "DELETE FROM trace_context WHERE watch_path=?",
                        rusqlite::params![path_str.as_ref()],
                    );
                    let _ = state_db.execute(
                        "INSERT OR IGNORE INTO processed_events
                         (event_id, watch_path, trace_id, event_type, ts)
                         VALUES (?,?,?,?,?)",
                        rusqlite::params![event_id, path_str.as_ref(), trace_id, event_type, ts],
                    );
                }
                Err(e) => {
                    let _ = writeln!(log, "[innate-daemon] evolve at session end failed: {e}");
                    record_daemon_error(state_db, path_str.as_ref(), "evolve", &e.to_string());
                }
            }
            continue;
        }

        if matches!(event_type, "ok" | "fail" | "feedback") {
            let Some(tid) = &trace_id else {
                record_daemon_error(
                    state_db,
                    path_str.as_ref(),
                    "record",
                    "event has no trace_id and no active trace context",
                );
                continue;
            };
            let result = call_cli_record(db_path, tid, &event);
            let ts = crate::utils::utc_now_iso();
            match result {
                Ok(()) => {
                    let _ = state_db.execute(
                        "INSERT OR IGNORE INTO processed_events
                         (event_id, watch_path, trace_id, event_type, ts)
                         VALUES (?,?,?,?,?)",
                        rusqlite::params![event_id, path_str.as_ref(), tid, event_type, ts],
                    );
                }
                Err(e) => {
                    let _ = writeln!(log, "[innate-daemon] record failed for trace {tid}: {e}");
                    record_daemon_error(state_db, path_str.as_ref(), "record", &e.to_string());
                }
            }
        }
    }

    // Update watch_state.
    let ts = crate::utils::utc_now_iso();
    let _ = state_db.execute(
        "INSERT OR REPLACE INTO watch_state(watch_path, last_processed_offset, last_processed_inode, updated_at)
         VALUES (?,?,?,?)",
        rusqlite::params![path_str.as_ref(), new_offset, inode, ts],
    );
}

#[derive(Debug, Default)]
struct DaemonEvent {
    kind: &'static str,
    event_id: Option<String>,
    trace_id: Option<String>,
    query: Option<String>,
    output_summary: Option<String>,
    outcome: Option<String>,
    used: Vec<String>,
    feedback: Option<String>,
    nomination: Option<String>,
    priority: i64,
}

fn parse_log_event(line: &str) -> Option<DaemonEvent> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
        let event_type = value.get("event_type").and_then(ValueExt::string)?;
        let (kind, default_outcome) = match event_type {
            "session_start" => ("start", None),
            "tool_success" => ("ok", Some("ok")),
            "tool_error" => ("fail", Some("fail")),
            "session_end" => ("end", None),
            "user_feedback" => ("feedback", None),
            _ => return None,
        };
        return Some(DaemonEvent {
            kind,
            event_id: value.get("event_id").and_then(ValueExt::owned_string),
            trace_id: value.get("trace_id").and_then(ValueExt::owned_string),
            query: value.get("query").and_then(ValueExt::owned_string),
            output_summary: value.get("output_summary").and_then(ValueExt::owned_string),
            outcome: value
                .get("outcome")
                .and_then(ValueExt::owned_string)
                .or_else(|| default_outcome.map(str::to_string)),
            used: value
                .get("used")
                .and_then(|used| used.as_array())
                .map(|used| {
                    used.iter()
                        .filter_map(ValueExt::owned_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            feedback: value.get("feedback").and_then(ValueExt::owned_string),
            nomination: value.get("nomination").and_then(ValueExt::owned_string),
            priority: value.get("priority").and_then(|v| v.as_i64()).unwrap_or(0),
        });
    }

    let kind = classify_text_line(line)?;
    Some(DaemonEvent {
        kind,
        query: (kind == "start").then(|| line.to_string()),
        outcome: match kind {
            "ok" => Some("ok".to_string()),
            "fail" => Some("fail".to_string()),
            _ => None,
        },
        ..DaemonEvent::default()
    })
}

fn classify_text_line(line: &str) -> Option<&'static str> {
    let start_patterns = [
        "Starting ",
        "Running ",
        "Executing ",
        "BEGIN ",
        "Task started",
    ];
    let success_patterns = ["Build successful", "Tests passed", "✓ ", " passed"];
    let fail_patterns = ["SyntaxError", "Error:", "FAILED", "test result: FAILED"];
    let end_patterns = [
        "Session ended",
        "Session End",
        "Conversation closed",
        "IDE exited",
    ];

    for p in &end_patterns {
        if line.contains(p) {
            return Some("end");
        }
    }
    for p in &start_patterns {
        if line.contains(p) {
            return Some("start");
        }
    }
    for p in &success_patterns {
        if line.contains(p) {
            return Some("ok");
        }
    }
    for p in &fail_patterns {
        if line.contains(p) {
            return Some("fail");
        }
    }
    None
}

trait ValueExt {
    fn string(&self) -> Option<&str>;
    fn owned_string(&self) -> Option<String>;
}

impl ValueExt for serde_json::Value {
    fn string(&self) -> Option<&str> {
        self.as_str().filter(|value| !value.is_empty())
    }

    fn owned_string(&self) -> Option<String> {
        self.string().map(str::to_string)
    }
}

fn event_id_for_line(watch_path: &str, inode: &str, offset: i64, line: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hash = Sha256::new();
    hash.update(watch_path.as_bytes());
    hash.update(b":");
    hash.update(inode.as_bytes());
    hash.update(b":");
    hash.update(offset.to_string().as_bytes());
    hash.update(b":");
    hash.update(line.as_bytes());
    format!("{:x}", hash.finalize())
}

fn record_daemon_error(
    state_db: &rusqlite::Connection,
    watch_path: &str,
    operation: &str,
    message: &str,
) {
    let _ = state_db.execute(
        "INSERT INTO daemon_errors(watch_path, operation, message, ts)
         VALUES (?,?,?,?)",
        rusqlite::params![watch_path, operation, message, crate::utils::utc_now_iso()],
    );
}

fn call_cli_record(db_path: &str, trace_id: &str, event: &DaemonEvent) -> anyhow::Result<()> {
    let self_exe = std::env::current_exe()?;
    let run = || {
        let mut command = std::process::Command::new(&self_exe);
        command.args(["--db", db_path, "record", trace_id]);
        if let Some(query) = &event.query {
            command.args(["--query", query]);
        }
        if let Some(outcome) = &event.outcome {
            command.args(["--outcome", outcome]);
        }
        if !event.used.is_empty() {
            command.args(["--used", &event.used.join(",")]);
        }
        if let Some(summary) = &event.output_summary {
            command.args(["--output-summary", summary]);
        }
        if let Some(feedback) = &event.feedback {
            command.args(["--feedback", feedback]);
        }
        if let Some(nomination) = &event.nomination {
            command.args(["--nomination", nomination]);
        }
        command.args(["--priority", &event.priority.to_string()]);
        command.args(["--source", "daemon"]);
        command.status()
    };

    let first = run()?;
    if first.success() {
        return Ok(());
    }

    std::thread::sleep(std::time::Duration::from_millis(200));
    let second = run()?;
    if second.success() {
        Ok(())
    } else {
        anyhow::bail!("record exited {:?} after retry", second.code())
    }
}

fn call_cli_recall(db_path: &str, query: &str) -> anyhow::Result<String> {
    let self_exe = std::env::current_exe()?;
    let output = std::process::Command::new(&self_exe)
        .args([
            "--db", db_path, "recall", query, "--format", "json", "--source", "daemon",
        ])
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "recall exited non-zero: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| anyhow::anyhow!("recall json parse error: {e}"))?;
    parsed
        .get("trace_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("no trace_id in recall output"))
}

fn call_cli_evolve(db_path: &str) -> anyhow::Result<()> {
    let self_exe = std::env::current_exe()?;
    let run = || {
        std::process::Command::new(&self_exe)
            .args(["--db", db_path, "evolve", "--trigger", "manual"])
            .status()
    };
    let first = run()?;
    if first.success() {
        return Ok(());
    }

    std::thread::sleep(std::time::Duration::from_millis(200));
    let second = run()?;
    if second.success() {
        Ok(())
    } else {
        anyhow::bail!("evolve exited {:?} after retry", second.code())
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn init_state_db(path: &Path) -> Result<()> {
    let conn = rusqlite::Connection::open(path)?;
    conn.execute_batch(DAEMON_SCHEMA)?;
    Ok(())
}

fn read_pid(pid_file: &Path) -> Option<u32> {
    std::fs::read_to_string(pid_file).ok()?.trim().parse().ok()
}

#[cfg(target_os = "linux")]
fn process_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(not(target_os = "linux"))]
fn process_alive(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::{event_id_for_line, init_state_db, parse_log_event};
    use tempfile::NamedTempFile;

    #[test]
    fn classifies_session_end_events() {
        assert_eq!(
            parse_log_event("Session ended").map(|event| event.kind),
            Some("end")
        );
        assert_eq!(
            parse_log_event(r#"{"event_type":"session_end"}"#).map(|event| event.kind),
            Some("end")
        );
    }

    #[test]
    fn daemon_state_schema_tracks_errors() {
        let file = NamedTempFile::new().unwrap();
        init_state_db(file.path()).unwrap();
        let conn = rusqlite::Connection::open(file.path()).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type='table' AND name='daemon_errors'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn parses_structured_hook_payload() {
        let event = parse_log_event(
            r#"{
                "event_id":"evt-1",
                "event_type":"user_feedback",
                "trace_id":"trace-1",
                "query":"retry task",
                "output_summary":"bounded retry worked",
                "used":["chunk-1","chunk-2"],
                "feedback":"up",
                "nomination":"keep this approach",
                "priority":7
            }"#,
        )
        .unwrap();

        assert_eq!(event.kind, "feedback");
        assert_eq!(event.event_id.as_deref(), Some("evt-1"));
        assert_eq!(event.trace_id.as_deref(), Some("trace-1"));
        assert_eq!(
            event.output_summary.as_deref(),
            Some("bounded retry worked")
        );
        assert_eq!(event.used, vec!["chunk-1", "chunk-2"]);
        assert_eq!(event.feedback.as_deref(), Some("up"));
        assert_eq!(event.nomination.as_deref(), Some("keep this approach"));
        assert_eq!(event.priority, 7);
    }

    #[test]
    fn generated_event_id_changes_after_log_rotation() {
        let before = event_id_for_line("/tmp/agent.log", "inode-1", 42, "Tests passed");
        let after = event_id_for_line("/tmp/agent.log", "inode-2", 42, "Tests passed");
        assert_ne!(before, after);
    }
}
