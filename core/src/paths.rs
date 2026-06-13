//! Single source of truth for the `~/.innate` directory layout.
//!
//! ```text
//! ~/.innate/
//!   settings.json            ← user config (root)
//!   settings.schema.jsonc    ← schema for settings.json (root)
//!   data/                    ← databases + runtime state
//!     personal.db (+ -shm, -wal)
//!     daemon_state.sqlite
//!     daemon.pid
//!   logs/                    ← operational logs
//!     daemon.log
//!     mcp.log
//!   sessions/                ← agent session traces (watched by the daemon)
//!     session.log
//! ```
//!
//! All callers must go through these helpers rather than re-deriving paths, so
//! the layout stays consistent. `ensure_layout()` creates the subdirectories and
//! relocates files from the older flat layout on first run.

use std::path::PathBuf;

/// Root `~/.innate` directory (falls back to `./.innate` if home is unknown).
pub fn innate_home() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".innate")
}

/// `~/.innate/data` — databases and runtime state.
pub fn data_dir() -> PathBuf {
    innate_home().join("data")
}

/// `~/.innate/logs` — operational logs.
pub fn logs_dir() -> PathBuf {
    innate_home().join("logs")
}

/// `~/.innate/sessions` — agent session traces watched by the daemon.
pub fn sessions_dir() -> PathBuf {
    innate_home().join("sessions")
}

pub fn settings_path() -> PathBuf {
    innate_home().join("settings.json")
}

pub fn default_db_path() -> PathBuf {
    data_dir().join("personal.db")
}

pub fn daemon_pid_path() -> PathBuf {
    data_dir().join("daemon.pid")
}

pub fn daemon_state_path() -> PathBuf {
    data_dir().join("daemon_state.sqlite")
}

pub fn daemon_log_path() -> PathBuf {
    logs_dir().join("daemon.log")
}

pub fn mcp_log_path() -> PathBuf {
    logs_dir().join("mcp.log")
}

pub fn session_log_path() -> PathBuf {
    sessions_dir().join("session.log")
}

/// Local cache of the last backup time.
pub fn backup_state_path() -> PathBuf {
    data_dir().join("backup_state.json")
}

/// Ephemeral working directory (e.g. backup VACUUM temp copies).
pub fn tmp_dir() -> PathBuf {
    data_dir().join("tmp")
}

/// Create the standard subdirectories and migrate any files left over from the
/// older flat layout (`~/.innate/<file>`) into their new homes. Idempotent and
/// best-effort: a single failed move never aborts startup.
pub fn ensure_layout() {
    ensure_layout_at(&innate_home());
}

/// Layout logic against an explicit base directory (testable without touching
/// the real `$HOME`). Database sidecars (`-shm`/`-wal`) ride along with the main
/// db so SQLite stays consistent.
pub fn ensure_layout_at(home: &std::path::Path) {
    let data = home.join("data");
    let logs = home.join("logs");
    for dir in [&data, &logs, &home.join("sessions")] {
        let _ = std::fs::create_dir_all(dir);
    }

    let moves: [(PathBuf, PathBuf); 8] = [
        (home.join("personal.db"), data.join("personal.db")),
        (home.join("personal.db-shm"), data.join("personal.db-shm")),
        (home.join("personal.db-wal"), data.join("personal.db-wal")),
        (home.join("daemon_state.sqlite"), data.join("daemon_state.sqlite")),
        (home.join("daemon.pid"), data.join("daemon.pid")),
        (home.join("backup_state.json"), data.join("backup_state.json")),
        (home.join("daemon.log"), logs.join("daemon.log")),
        (home.join("mcp.log"), logs.join("mcp.log")),
    ];
    for (legacy, target) in moves {
        if legacy.exists() && !target.exists() {
            let _ = std::fs::rename(&legacy, &target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_layout_migrates_legacy_flat_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // Seed the old flat layout.
        for f in [
            "personal.db",
            "personal.db-wal",
            "daemon_state.sqlite",
            "daemon.pid",
            "daemon.log",
            "mcp.log",
        ] {
            std::fs::write(home.join(f), b"x").unwrap();
        }

        ensure_layout_at(home);

        // Subdirectories created.
        assert!(home.join("data").is_dir());
        assert!(home.join("logs").is_dir());
        assert!(home.join("sessions").is_dir());
        // Files relocated, originals gone.
        assert!(home.join("data/personal.db").exists());
        assert!(home.join("data/personal.db-wal").exists());
        assert!(home.join("data/daemon_state.sqlite").exists());
        assert!(home.join("data/daemon.pid").exists());
        assert!(home.join("logs/daemon.log").exists());
        assert!(home.join("logs/mcp.log").exists());
        assert!(!home.join("personal.db").exists());
        assert!(!home.join("mcp.log").exists());

        // Idempotent: a second run is a no-op and does not error.
        ensure_layout_at(home);
        assert!(home.join("data/personal.db").exists());
    }
}
