// ── PATH installation ─────────────────────────────────────────────────────────

use super::{agents::*, *};

pub(super) fn check_on_path() -> Option<PathBuf> {
    which_binary("innate")
}

pub(super) fn install_to_path(current_exe: &Path) -> anyhow::Result<PathBuf> {
    let local_bin = home_dir().join(".local").join("bin");
    std::fs::create_dir_all(&local_bin)?;
    let dest = local_bin.join(binary_name());

    if dest.exists() || dest.is_symlink() {
        std::fs::remove_file(&dest)?;
    }

    #[cfg(unix)]
    std::os::unix::fs::symlink(current_exe, &dest)?;
    #[cfg(not(unix))]
    std::fs::copy(current_exe, &dest)?;

    Ok(dest)
}

pub(super) fn path_has_local_bin() -> bool {
    let local_bin = home_dir().join(".local").join("bin");
    std::env::var("PATH")
        .map(|p| {
            p.split(path_sep())
                .any(|d| local_bin.as_path() == Path::new(d))
        })
        .unwrap_or(false)
}

pub(super) fn write_path_to_profiles() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        write_path_windows()
    }
    #[cfg(not(windows))]
    {
        write_path_unix()
    }
}

/// Linux / macOS: append export line to every shell profile that exists and
/// doesn't already mention `.local/bin`.  Returns the list of files written.
#[cfg(not(windows))]
fn write_path_unix() -> Vec<PathBuf> {
    let home = home_dir();
    let export_line = r#"export PATH="$HOME/.local/bin:$PATH""#;
    let block = format!("\n# innate\n{export_line}\n");
    // .zprofile covers macOS login shells (Catalina+); .zshrc covers interactive
    let profiles = [
        ".bashrc",
        ".zshrc",
        ".zprofile",
        ".bash_profile",
        ".profile",
    ];
    let mut updated = Vec::new();

    for name in &profiles {
        let path = home.join(name);
        if !path.exists() {
            continue;
        }
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        if content.contains(".local/bin") {
            continue;
        }
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&path) {
            if f.write_all(block.as_bytes()).is_ok() {
                updated.push(path);
            }
        }
    }

    // Fish shell: ~/.config/fish/config.fish
    let fish_config = home.join(".config").join("fish").join("config.fish");
    if fish_config.exists() {
        let content = std::fs::read_to_string(&fish_config).unwrap_or_default();
        if !content.contains(".local/bin") {
            let fish_block = "\n# innate\nfish_add_path \"$HOME/.local/bin\"\n";
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&fish_config) {
                if f.write_all(fish_block.as_bytes()).is_ok() {
                    updated.push(fish_config);
                }
            }
        }
    }

    updated
}

/// Windows: modify the User-level PATH in the registry via PowerShell.
/// No new crate dependency — PowerShell ships with every modern Windows.
#[cfg(windows)]
fn write_path_windows() -> Vec<PathBuf> {
    let local_bin = home_dir().join(".local").join("bin");
    // Escape single-quotes for PowerShell string literal
    let dir = local_bin.to_string_lossy().replace('\'', "''");
    let ps = format!(
        "$dir='{dir}';\
        $old=[Environment]::GetEnvironmentVariable('PATH','User');\
        if($old -notlike \"*$dir*\"){{\
        [Environment]::SetEnvironmentVariable('PATH',$old+';'+$dir,'User')\
        }}"
    );
    let ok = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        vec![PathBuf::from("User PATH (Windows registry)")]
    } else {
        vec![]
    }
}
