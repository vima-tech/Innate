//! `innate upgrade` — self-update from GitHub Releases.
//!
//! Downloads the pre-built binary for the current platform, verifies its
//! SHA-256 checksum, atomically replaces the running executable, then
//! optionally runs `innate migrate` to bring the database schema up to date.

use std::io::Read;
use std::path::Path;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

const REPO: &str = "vima-tech/Innate";
const GITHUB_API: &str = "https://api.github.com";

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn run_upgrade(version: Option<&str>, db_path: &Path, check_only: bool) -> Result<()> {
    let current_ver = env!("CARGO_PKG_VERSION");
    let target = current_target();

    if target == "unknown" {
        bail!(
            "Unsupported platform for auto-upgrade. \
             Build from source: cargo build --release --manifest-path core/Cargo.toml"
        );
    }

    // Resolve the version to install.
    let target_ver = match version {
        Some(v) => v.trim_start_matches('v').to_string(),
        None => latest_version()?,
    };

    if target_ver == current_ver {
        println!("innate {current_ver} is already up to date.");
        return Ok(());
    }

    println!("innate {current_ver}  →  {target_ver}");

    if check_only {
        println!("Run `innate upgrade` (without --check) to install.");
        return Ok(());
    }

    let ext = if cfg!(target_os = "windows") {
        ".exe"
    } else {
        ""
    };
    let asset = format!("innate-{target}{ext}");
    let base = format!("https://github.com/{REPO}/releases/download/v{target_ver}");

    // 1. Fetch checksum.
    let sha_text = http_get_text(&format!("{base}/{asset}.sha256"))
        .with_context(|| format!("Could not fetch checksum for {asset} v{target_ver}"))?;
    let expected_sha = sha_text
        .split_whitespace()
        .next()
        .context("SHA-256 file is empty")?
        .to_lowercase();

    // 2. Download binary.
    println!("  Downloading {asset}…");
    let bytes = http_get_bytes(&format!("{base}/{asset}"))
        .with_context(|| format!("Could not download {asset}"))?;

    // 3. Verify checksum.
    let actual_sha = format!("{:x}", Sha256::digest(&bytes));
    if actual_sha != expected_sha {
        bail!(
            "SHA-256 mismatch — download may be corrupted.\n  \
             expected: {expected_sha}\n  \
             got:      {actual_sha}"
        );
    }

    // 4. Write to a temp file next to the running exe.
    let exe = std::env::current_exe().context("Cannot resolve current executable path")?;
    let tmp = exe.with_extension("upgrade.tmp");
    std::fs::write(&tmp, &bytes).context("Failed to write temporary binary")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .context("Failed to mark binary as executable")?;
    }

    // 5. Atomically replace running exe.
    //    On Windows the running exe is locked; save the old one as .old.exe first.
    #[cfg(target_os = "windows")]
    {
        let old = exe.with_extension("old.exe");
        std::fs::rename(&exe, &old).context("Failed to move old binary")?;
        if let Err(e) = std::fs::rename(&tmp, &exe) {
            // Roll back: restore the original so the user isn't left without a binary.
            let _ = std::fs::rename(&old, &exe);
            return Err(e).context("Failed to install new binary (rolled back)");
        }
        println!("  Previous binary saved as {}", old.display());
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::fs::rename(&tmp, &exe)
            .context("Failed to replace binary — try: sudo innate upgrade")?;
    }

    println!("✓ innate {target_ver} installed.");

    // 6. Run schema migration if a db path is usable.
    if db_path.exists() {
        println!("  Migrating schema…");
        match crate::migrate::run_migrations(db_path) {
            Ok(applied) if applied.is_empty() => println!("  Schema already at {target_ver}."),
            Ok(applied) => {
                for step in &applied {
                    println!("    applied: {step}");
                }
                println!("  Migration complete.");
            }
            Err(e) => eprintln!("  Migration warning (run `innate migrate` manually): {e}"),
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// GitHub helpers
// ---------------------------------------------------------------------------

fn latest_version() -> Result<String> {
    let url = format!("{GITHUB_API}/repos/{REPO}/releases/latest");
    let text = http_get_text(&url).context("Failed to fetch latest release info from GitHub")?;
    let json: serde_json::Value = serde_json::from_str(&text)?;
    json["tag_name"]
        .as_str()
        .map(|t| t.trim_start_matches('v').to_string())
        .context("GitHub response missing tag_name — try specifying a version: innate upgrade --version 0.1.6")
}

fn http_get_text(url: &str) -> Result<String> {
    let resp = ureq::get(url)
        .set(
            "User-Agent",
            &format!("innate/{}", env!("CARGO_PKG_VERSION")),
        )
        .set("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| format!("HTTP GET {url}"))?;
    Ok(resp.into_string()?)
}

fn http_get_bytes(url: &str) -> Result<Vec<u8>> {
    let resp = ureq::get(url)
        .set(
            "User-Agent",
            &format!("innate/{}", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .with_context(|| format!("HTTP GET {url}"))?;
    let mut buf = Vec::new();
    resp.into_reader().read_to_end(&mut buf)?;
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

fn current_target() -> &'static str {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return "x86_64-unknown-linux-musl";
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return "aarch64-unknown-linux-musl";
    #[cfg(all(target_os = "linux", target_arch = "arm"))]
    return "armv7-unknown-linux-musleabihf";
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return "x86_64-apple-darwin";
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return "aarch64-apple-darwin";
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return "x86_64-pc-windows-msvc";
    #[allow(unreachable_code)]
    "unknown"
}
