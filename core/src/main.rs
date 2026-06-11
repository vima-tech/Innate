fn main() {
    if let Err(e) = innate_core::cli::run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
