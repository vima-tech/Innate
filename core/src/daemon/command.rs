#[derive(Subcommand)]
pub enum DaemonCommands {
    /// Start the background log-watcher daemon
    Start {
        #[arg(long = "watch", value_name = "LOG_DIR")]
        watch: Vec<std::path::PathBuf>,
        #[arg(long, value_name = "PATH")]
        pid_file: Option<std::path::PathBuf>,
        #[arg(long, value_name = "PATH")]
        state_db: Option<std::path::PathBuf>,
        #[arg(long, value_name = "PATH")]
        log_file: Option<std::path::PathBuf>,
    },
    /// Stop a running daemon
    Stop {
        #[arg(long, value_name = "PATH")]
        pid_file: Option<std::path::PathBuf>,
    },
    /// Show daemon status
    Status {
        #[arg(long, value_name = "PATH")]
        state_db: Option<std::path::PathBuf>,
        #[arg(long, value_name = "PATH")]
        pid_file: Option<std::path::PathBuf>,
    },
}
fn default_pid_file() -> std::path::PathBuf {
    crate::paths::daemon_pid_path()
}

fn default_state_db() -> std::path::PathBuf {
    crate::paths::daemon_state_path()
}

fn default_log_file() -> std::path::PathBuf {
    crate::paths::daemon_log_path()
}

pub(crate) fn run_command(action: &DaemonCommands, db_path: &Path) -> anyhow::Result<()> {
    match action {
        DaemonCommands::Start {
            watch,
            pid_file,
            state_db,
            log_file,
        } => {
            // Merge CLI --watch dirs with settings watch_dirs when CLI provides none.
            let effective_watch: Vec<std::path::PathBuf> = if !watch.is_empty() {
                watch.clone()
            } else {
                let s = crate::settings::load()?;
                crate::settings::resolved_watch_dirs(&s)
                    .into_iter()
                    .map(std::path::PathBuf::from)
                    .collect()
            };
            super::start(
                &effective_watch,
                db_path,
                pid_file.as_deref().unwrap_or(&default_pid_file()),
                state_db.as_deref().unwrap_or(&default_state_db()),
                log_file.as_deref().unwrap_or(&default_log_file()),
            )
        }
        DaemonCommands::Stop { pid_file } => {
            super::stop(pid_file.as_deref().unwrap_or(&default_pid_file()))
        }
        DaemonCommands::Status { state_db, pid_file } => super::status(
            state_db.as_deref().unwrap_or(&default_state_db()),
            pid_file.as_deref().unwrap_or(&default_pid_file()),
        ),
    }
}
use std::path::Path;

use clap::Subcommand;
