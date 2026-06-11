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
            eprintln!("[innate daemon] warning: no --watch directories specified; \
                       daemon will start but won't monitor any logs");
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
        if let Some(p) = pid_file.parent() { std::fs::create_dir_all(p)?; }
        if let Some(p) = state_db.parent()  { std::fs::create_dir_all(p)?; }
        if let Some(p) = log_file.parent()  { std::fs::create_dir_all(p)?; }

        // Init daemon_state.sqlite.
        init_state_db(state_db)?;

        // Fork: parent writes pid and returns; child runs the watch loop.
        let watch_strs: Vec<String> = watch_dirs.iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let db_str   = db_path.to_string_lossy().into_owned();
        let sdb_str  = state_db.to_string_lossy().into_owned();
        let log_str  = log_file.to_string_lossy().into_owned();
        let pid_str  = pid_file.to_string_lossy().into_owned();

        // Re-exec self with a hidden marker flag so the child enters watch_loop directly.
        let self_exe = std::env::current_exe()?;
        let mut cmd = std::process::Command::new(&self_exe);
        cmd.arg("--daemon-internal-watch")
           .arg("--db").arg(&db_str)
           .arg("--state-db").arg(&sdb_str)
           .arg("--log-file").arg(&log_str)
           .arg("--pid-file").arg(&pid_str);
        for w in &watch_strs { cmd.arg("--watch-dir").arg(w); }

        // Detach from terminal.
        unsafe { cmd.pre_exec(|| { libc::setsid(); Ok(()) }); }
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
            None => anyhow::bail!("no pid file at {}; daemon may not be running", pid_file.display()),
            Some(pid) => {
                if !process_alive(pid) {
                    let _ = std::fs::remove_file(pid_file);
                    println!("daemon was not running (stale pid {pid}); pid file removed");
                    return Ok(());
                }
                // SIGTERM
                let r = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
                if r != 0 {
                    anyhow::bail!("kill({pid}, SIGTERM) failed: {}", std::io::Error::last_os_error());
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
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL); }
                let _ = std::fs::remove_file(pid_file);
                println!("daemon killed (pid {pid})");
                Ok(())
            }
        }
    }
}

pub fn status(state_db: &Path) -> anyhow::Result<()> {
    if !state_db.exists() {
        println!("daemon_state.sqlite not found at {}; daemon has never run.", state_db.display());
        return Ok(());
    }
    let conn = rusqlite::Connection::open(state_db)?;
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM watch_state", [], |r| r.get(0),
    ).unwrap_or(0);
    let processed: i64 = conn.query_row(
        "SELECT count(*) FROM processed_events", [], |r| r.get(0),
    ).unwrap_or(0);
    println!("watch_state entries  : {count}");
    println!("processed events     : {processed}");
    // List watch paths.
    let mut stmt = conn.prepare("SELECT watch_path, last_processed_offset, updated_at FROM watch_state")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_,String>(0)?, r.get::<_,i64>(1)?, r.get::<_,String>(2)?))
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
        .create(true).append(true).open(log_path);

    let mut logger: Box<dyn std::io::Write + Send> = match log_file {
        Ok(f) => Box::new(f),
        Err(_) => Box::new(std::io::stderr()),
    };

    let _ = writeln!(logger, "[innate-daemon] started pid={}", std::process::id());

    let state_db = match rusqlite::Connection::open(state_db_path) {
        Ok(c) => c,
        Err(e) => { let _ = writeln!(logger, "[innate-daemon] cannot open state db: {e}"); return; }
    };
    if state_db.execute_batch(DAEMON_SCHEMA).is_err() {
        let _ = writeln!(logger, "[innate-daemon] failed to init schema");
        return;
    }

    // Main poll loop: 500 ms tick.
    loop {
        for dir in watch_dirs {
            let dir_path = std::path::Path::new(dir);
            if !dir_path.exists() { continue; }
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

    if start_offset >= file_size { return; }

    use std::io::{BufRead, Seek};
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return,
    };
    if f.seek(std::io::SeekFrom::Start(start_offset as u64)).is_err() { return; }

    let reader = std::io::BufReader::new(&mut f);
    let mut new_offset = start_offset;

    for line_res in reader.lines() {
        let line = match line_res { Ok(l) => l, Err(_) => break };
        new_offset += line.len() as i64 + 1; // +1 for newline

        // Event classification per §九 mapping table.
        let event_type = classify_log_line(&line);
        if event_type.is_none() { continue; }
        let event_type = event_type.unwrap();

        // Compute event_id for idempotency.
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(path_str.as_bytes());
        h.update(b":");
        h.update(new_offset.to_string().as_bytes());
        h.update(b":");
        h.update(line.as_bytes());
        let event_id = format!("{:x}", h.finalize());

        // Skip if already processed.
        let already: i64 = state_db.query_row(
            "SELECT count(*) FROM processed_events WHERE event_id=?",
            rusqlite::params![event_id],
            |r| r.get(0),
        ).unwrap_or(0);
        if already > 0 { continue; }

        // Look up trace for this watch path.
        let trace_id: Option<String> = state_db.query_row(
            "SELECT trace_id FROM trace_context WHERE watch_path=?",
            rusqlite::params![path_str.as_ref()],
            |r| r.get(0),
        ).ok();

        if let Some(tid) = &trace_id {
            let outcome = match event_type {
                "ok"   => "ok",
                "fail" => "fail",
                _      => continue,
            };
            let result = call_cli_record(db_path, tid, outcome);
            let ts = crate::utils::utc_now_iso();
            let _ = state_db.execute(
                "INSERT OR IGNORE INTO processed_events(event_id, watch_path, trace_id, event_type, ts)
                 VALUES (?,?,?,?,?)",
                rusqlite::params![event_id, path_str.as_ref(), tid, event_type, ts],
            );
            if let Err(e) = result {
                let _ = writeln!(log, "[innate-daemon] record failed for trace {tid}: {e}");
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

fn classify_log_line(line: &str) -> Option<&'static str> {
    // §九 event mapping: success patterns → "ok", failure patterns → "fail".
    let success_patterns = ["Build successful", "Tests passed", "✓ ", " passed"];
    let fail_patterns    = ["SyntaxError", "Error:", "FAILED", "test result: FAILED"];

    for p in &success_patterns {
        if line.contains(p) { return Some("ok"); }
    }
    for p in &fail_patterns {
        if line.contains(p) { return Some("fail"); }
    }
    None
}

fn call_cli_record(
    db_path: &str,
    trace_id: &str,
    outcome: &str,
) -> anyhow::Result<()> {
    let self_exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&self_exe);
    cmd.args(["--db", db_path, "record", trace_id,
              "--outcome", outcome, "--source", "daemon"]);

    let status = cmd.status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => {
            // One retry with 200 ms backoff.
            std::thread::sleep(std::time::Duration::from_millis(200));
            let status2 = std::process::Command::new(&self_exe)
                .args(["--db", db_path, "record", trace_id,
                       "--outcome", outcome, "--source", "daemon"])
                .status()?;
            if status2.success() { Ok(()) } else {
                anyhow::bail!("record exited {:?} after retry", s.code())
            }
        }
        Err(e) => anyhow::bail!("record exec failed: {e}"),
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
