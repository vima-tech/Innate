use super::*;

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
