use super::{state::*, *};

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
