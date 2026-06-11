fn main() {
    // Internal flag: re-execed child process enters the daemon watch loop directly.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("--daemon-internal-watch") {
        let get_flag = |flag: &str| -> Option<String> {
            args.windows(2)
                .find(|w| w[0] == flag)
                .map(|w| w[1].clone())
        };
        let get_multi = |flag: &str| -> Vec<String> {
            args.windows(2)
                .filter(|w| w[0] == flag)
                .map(|w| w[1].clone())
                .collect()
        };
        let db_path    = get_flag("--db").unwrap_or_default();
        let state_db   = get_flag("--state-db").unwrap_or_default();
        let log_file   = get_flag("--log-file").unwrap_or_default();
        let pid_file   = get_flag("--pid-file").unwrap_or_default();
        let watch_dirs = get_multi("--watch-dir");
        innate_core::daemon::run_watch_loop(&watch_dirs, &db_path, &state_db, &log_file, &pid_file);
        return;
    }

    if let Err(e) = innate_core::cli::run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
