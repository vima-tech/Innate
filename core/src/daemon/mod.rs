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

CREATE TABLE IF NOT EXISTS daemon_errors (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    watch_path TEXT,
    operation  TEXT NOT NULL,
    message    TEXT NOT NULL,
    ts         TEXT NOT NULL
);
"#;

mod command;
mod events;
mod process;
mod state;
mod watch;

#[cfg(test)]
mod tests;

pub(crate) use command::run_command;
pub use command::DaemonCommands;
pub use process::{start, status, stop};
pub use state::{health, is_running};
pub use watch::run_watch_loop;
