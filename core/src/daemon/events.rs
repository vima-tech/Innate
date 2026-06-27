/// Resolve the `innate` binary to shell out to. `std::env::current_exe()` becomes a
/// dangling/`(deleted)` path the moment the binary is upgraded in place (`rm` + `cp`,
/// as the installer/`upgrade` does), which silently breaks every daemon shell-out with
/// `No such file or directory (os error 2)` until the daemon is restarted. When the
/// resolved exe no longer exists, fall back to looking `innate` up on PATH (which now
/// points at the freshly-installed binary), so a long-lived daemon survives upgrades.
pub(in crate::daemon) fn resolve_self_exe() -> std::path::PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if exe.exists() {
            return exe;
        }
    }
    if let Some(found) = find_on_path("innate", std::env::var("PATH").ok().as_deref()) {
        return found;
    }
    // Last resort: hand back current_exe() (or the bare name) and let the spawn surface
    // the error, preserving the previous behaviour rather than panicking.
    std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("innate"))
}

/// Search a `PATH`-style string for an existing `name` entry. Split out from
/// `resolve_self_exe` so the fallback is unit-testable without deleting our own binary.
pub(in crate::daemon) fn find_on_path(name: &str, path_var: Option<&str>) -> Option<std::path::PathBuf> {
    let path = path_var?;
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = std::path::Path::new(dir).join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

#[derive(Debug, Default)]
pub(in crate::daemon) struct DaemonEvent {
    pub(in crate::daemon) kind: &'static str,
    pub(in crate::daemon) event_id: Option<String>,
    pub(in crate::daemon) trace_id: Option<String>,
    pub(in crate::daemon) query: Option<String>,
    pub(in crate::daemon) output_summary: Option<String>,
    pub(in crate::daemon) outcome: Option<String>,
    pub(in crate::daemon) used: Option<Vec<String>>,
    pub(in crate::daemon) feedback: Option<String>,
    pub(in crate::daemon) nomination: Option<String>,
    pub(in crate::daemon) priority: i64,
}

pub(in crate::daemon) fn parse_log_event(line: &str) -> Option<DaemonEvent> {
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
                }),
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

pub(in crate::daemon) fn event_id_for_line(
    watch_path: &str,
    inode: &str,
    offset: i64,
    line: &str,
) -> String {
    use sha2::{Digest, Sha256};

    let mut hash = Sha256::new();
    hash.update(watch_path.as_bytes());
    hash.update(b":");
    hash.update(inode.as_bytes());
    hash.update(b":");
    hash.update(offset.to_string().as_bytes());
    hash.update(b":");
    hash.update(line.as_bytes());
    crate::utils::hex(&hash.finalize())
}

pub(in crate::daemon) fn record_daemon_error(
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

pub(in crate::daemon) fn call_cli_record(
    db_path: &str,
    trace_id: &str,
    event: &DaemonEvent,
) -> anyhow::Result<()> {
    let self_exe = resolve_self_exe();
    let run = || {
        let mut command = std::process::Command::new(&self_exe);
        command.args(["--db", db_path, "record", trace_id]);
        if let Some(query) = &event.query {
            command.args(["--query", query]);
        }
        if let Some(outcome) = &event.outcome {
            command.args(["--outcome", outcome]);
        }
        if let Some(used) = &event.used {
            command.args(["--used", &used.join(",")]);
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

pub(in crate::daemon) fn call_cli_recall(db_path: &str, query: &str) -> anyhow::Result<String> {
    let self_exe = resolve_self_exe();
    let output = std::process::Command::new(&self_exe)
        .args([
            "--db",
            db_path,
            "recall",
            query,
            "--format",
            "json",
            "--source",
            "daemon",
            // Session trace only: the daemon discards the recalled knowledge and
            // keeps just the trace_id, so it must not claim any `selected` chunks.
            "--session",
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

pub(in crate::daemon) fn call_cli_backup(db_path: &str) -> anyhow::Result<()> {
    let self_exe = resolve_self_exe();
    let status = std::process::Command::new(&self_exe)
        .args(["--db", db_path, "backup", "run"])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("innate backup run exited {:?}", status.code())
    }
}

pub(in crate::daemon) fn call_cli_evolve(db_path: &str, trigger: &str) -> anyhow::Result<()> {
    let self_exe = resolve_self_exe();
    let run = || {
        std::process::Command::new(&self_exe)
            .args(["--db", db_path, "evolve", "--trigger", trigger])
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
