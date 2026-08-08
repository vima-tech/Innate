use super::{events::*, *};

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
    let mut last_evolve_poll = std::time::Instant::now();
    const EVOLVE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
    let mut last_backup_poll = std::time::Instant::now();
    const BACKUP_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30 * 60);
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
        // Periodically consume pending evolve_requests so knowledge grows even without session_end.
        if last_evolve_poll.elapsed() >= EVOLVE_POLL_INTERVAL {
            if let Err(error) = call_cli_evolve(db_path, "scheduled") {
                let _ = writeln!(logger, "[innate-daemon] scheduled evolve failed: {error}");
                record_daemon_error(
                    &state_db,
                    "<scheduler>",
                    "scheduled_evolve",
                    &error.to_string(),
                );
            }
            last_evolve_poll = std::time::Instant::now();
        }
        if last_backup_poll.elapsed() >= BACKUP_POLL_INTERVAL {
            // Only attempt a backup when R2 backup is actually configured + enabled.
            // Otherwise `innate backup run` exits non-zero every cycle, spamming the log
            // and inflating the daemon error counter with a non-error (settings.json is
            // read here, never the db — preserves the daemon's no-db-open contract).
            let backup_enabled = crate::settings::load()
                .map(|s| s.backup.is_some_and(|b| b.enable && b.r2.is_some()))
                .unwrap_or(false);
            if backup_enabled {
                if let Err(error) = call_cli_backup(db_path) {
                    let _ = writeln!(logger, "[innate-daemon] auto-backup failed: {error}");
                    record_daemon_error(
                        &state_db,
                        "<scheduler>",
                        "auto_backup",
                        &error.to_string(),
                    );
                }
            }
            last_backup_poll = std::time::Instant::now();
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

pub(in crate::daemon) fn process_log_file(
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

    let mut reader = std::io::BufReader::new(&mut f);
    let mut new_offset = start_offset;
    let mut line_buf = String::new();

    loop {
        let line_start_offset = new_offset;
        line_buf.clear();
        let bytes_read = match reader.read_line(&mut line_buf) {
            Ok(n) => n,
            Err(_) => break,
        };
        if bytes_read == 0 {
            break; // EOF
        }
        // Partial line at EOF (no trailing newline): leave offset before this line
        // so it is re-read once the writer completes it.
        if !line_buf.ends_with('\n') {
            new_offset = line_start_offset;
            break;
        }
        new_offset += bytes_read as i64;
        let line = line_buf.trim_end_matches('\n').trim_end_matches('\r');

        let Some(event) = parse_log_event(line) else {
            continue;
        };
        let event_type = event.kind;

        // Compute event_id for idempotency.
        // Include inode so that a rotated file at the same path with the same
        // offset + content is not mistakenly treated as a duplicate event.
        let event_id = event
            .event_id
            .clone()
            .unwrap_or_else(|| event_id_for_line(path_str.as_ref(), &inode, new_offset, line));

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

        // The session-tracking chain (start → recall, ok/fail/feedback → record)
        // is structurally incapable of producing knowledge, so it is not run:
        //   - the hook emits outcome="unknown", which record() filters out, so
        //     every such trace retires as abandoned → discarded;
        //   - the daemon recalls with --session, which suppresses `selected`
        //     events, so attribution validation rejects any `used` id anyway.
        // It still cost one remote embedding call plus a full vector scan per
        // session start, and the recalled knowledge was discarded on the spot.
        // Reinstating it requires distillation eligibility to stop depending on
        // a confidence-bearing outcome — until then this chain only burns quota.
        // `end` is still handled below: its evolve trigger is the useful part.
        if matches!(event_type, "start" | "ok" | "fail" | "feedback") {
            continue;
        }

        // Only logs that carry their own trace_id can be retired; the daemon no
        // longer opens traces of its own.
        let trace_id = event.trace_id.clone();

        if event_type == "end" {
            if let Some(tid) = &trace_id {
                if let Err(e) = call_cli_record(db_path, tid, &event) {
                    let ts = crate::utils::utc_now_iso();
                    let _ = writeln!(log, "{ts} [daemon] end trace retirement failed: {e}");
                    record_daemon_error(
                        state_db,
                        path_str.as_ref(),
                        "record_session_end",
                        &e.to_string(),
                    );
                }
            }
            let result = call_cli_evolve(db_path, "manual");
            let ts = crate::utils::utc_now_iso();
            // The session has ended regardless of whether evolve succeeded, so the
            // end-of-session bookkeeping must run on BOTH branches: otherwise a
            // failed manual evolve loses the end event (never recorded in
            // processed_events, so once the read offset advances it is neither
            // retried nor logged). A failed manual evolve is independently retried
            // by the periodic scheduled tick, so it must not block this cleanup.
            // Errors here are surfaced to the log instead of being silently swallowed.
            if let Err(e) = state_db.execute(
                "INSERT OR IGNORE INTO processed_events
                 (event_id, watch_path, trace_id, event_type, ts)
                 VALUES (?,?,?,?,?)",
                rusqlite::params![event_id, path_str.as_ref(), trace_id, event_type, ts],
            ) {
                let _ = writeln!(log, "{ts} [daemon] failed to record processed end event: {e}");
            }
            match result {
                Ok(()) => {
                    let _ = call_cli_evolve(db_path, "scheduled");
                    let _ = writeln!(log, "{ts} [daemon] end evolve ok");
                }
                Err(e) => {
                    let _ = writeln!(log, "{ts} [daemon] end evolve failed (scheduled tick will retry): {e}");
                    record_daemon_error(state_db, path_str.as_ref(), "evolve", &e.to_string());
                }
            }
            continue;
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
