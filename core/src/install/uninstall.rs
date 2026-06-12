use super::{agents::*, skills::*, ui::*, *};

fn remove_binary_from_path() -> ConfigStatus {
    let dest = home_dir().join(".local").join("bin").join(binary_name());
    if !dest.exists() && !dest.is_symlink() {
        return ConfigStatus::Skipped(format!("~/.local/bin/{} not found", binary_name()));
    }
    match std::fs::remove_file(&dest) {
        Ok(()) => ConfigStatus::Updated(dest),
        Err(e) => ConfigStatus::Error(e.to_string()),
    }
}

fn remove_data_dir() -> ConfigStatus {
    let data = home_dir().join(".innate");
    if !data.exists() {
        return ConfigStatus::Skipped("~/.innate/ not found".into());
    }
    match std::fs::remove_dir_all(&data) {
        Ok(()) => ConfigStatus::Updated(data),
        Err(e) => ConfigStatus::Error(e.to_string()),
    }
}

pub fn run_uninstall(yes: bool, purge_data: bool) -> anyhow::Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    box_open(&format!("Innate v{version} — Uninstall"));

    // ── 1. Confirm ─────────────────────────────────────────────────────────
    if !yes && !prompt_confirm("Remove Innate from your system?", false) {
        println!("{}", gray("│"));
        box_close("Cancelled — nothing changed.");
        return Ok(());
    }

    // ── 2. Data prompt ─────────────────────────────────────────────────────
    let remove_data = purge_data
        || (!yes
            && prompt_confirm(
                "Also delete knowledge data (~/.innate/)? This cannot be undone.",
                false,
            ));

    // ── 3. Remove agent configs ────────────────────────────────────────────
    let global_claude = home_dir().join(".claude.json");
    // Also clean up the old incorrect path for users who installed with older versions.
    let global_claude_legacy = home_dir().join(".claude").join("settings.json");
    let project_claude = PathBuf::from(".claude").join("settings.json");

    for (label, path) in &[
        ("claude (global)", global_claude.as_path()),
        ("claude (global, legacy)", global_claude_legacy.as_path()),
        ("claude (project)", project_claude.as_path()),
    ] {
        match remove_claude_config(path) {
            ConfigStatus::Updated(p) => {
                result_line(&format!(
                    "{}: Removed MCP config {}",
                    bold(label),
                    gray(&tilde_path(&p))
                ));
            }
            ConfigStatus::Error(e) => {
                warn_line(&format!("{}: \x1b[31mError — {e}\x1b[0m", bold(label)));
            }
            _ => {}
        }
    }

    match remove_codex_config() {
        ConfigStatus::Updated(p) => {
            result_line(&format!(
                "{}: Removed MCP config {}",
                bold("codex"),
                gray(&tilde_path(&p))
            ));
        }
        ConfigStatus::Error(e) => {
            warn_line(&format!("{}: \x1b[31mError — {e}\x1b[0m", bold("codex")));
        }
        _ => {}
    }

    match remove_opencode_config() {
        ConfigStatus::Updated(p) => {
            result_line(&format!(
                "{}: Removed MCP config {}",
                bold("opencode"),
                gray(&tilde_path(&p))
            ));
        }
        ConfigStatus::Error(e) => {
            warn_line(&format!("{}: \x1b[31mError — {e}\x1b[0m", bold("opencode")));
        }
        _ => {}
    }

    // ── 4. Remove skill + slash commands ──────────────────────────────────
    match remove_skill() {
        ConfigStatus::Updated(p) => {
            result_line(&format!(
                "{}: Removed skill {}",
                bold("claude"),
                gray(&tilde_path(&p))
            ));
        }
        ConfigStatus::Error(e) => {
            warn_line(&format!(
                "{}: \x1b[31mSkill error — {e}\x1b[0m",
                bold("claude")
            ));
        }
        _ => {}
    }

    for (name, status) in remove_commands() {
        match status {
            ConfigStatus::Updated(p) => {
                result_line(&format!(
                    "{}: Removed /{name} {}",
                    bold("claude"),
                    gray(&tilde_path(&p))
                ));
            }
            ConfigStatus::Error(e) => {
                warn_line(&format!(
                    "{}: \x1b[31mCommand /{name} error — {e}\x1b[0m",
                    bold("claude")
                ));
            }
            _ => {}
        }
    }

    // ── 5. Remove binary ───────────────────────────────────────────────────
    match remove_binary_from_path() {
        ConfigStatus::Updated(p) => {
            result_line(&format!("Removed binary {}", gray(&tilde_path(&p))));
        }
        ConfigStatus::Error(e) => {
            warn_line(&format!("\x1b[31mCould not remove binary: {e}\x1b[0m"));
        }
        _ => {}
    }

    // ── 6. Remove data ─────────────────────────────────────────────────────
    if remove_data {
        match remove_data_dir() {
            ConfigStatus::Updated(p) => {
                result_line(&format!("Removed data {}", gray(&tilde_path(&p))));
            }
            ConfigStatus::Error(e) => {
                warn_line(&format!("\x1b[31mCould not remove data: {e}\x1b[0m"));
            }
            _ => {}
        }
    } else {
        info(&gray("Knowledge data (~/.innate/) kept."));
    }

    sep();
    box_close("Innate uninstalled. Restart your agents.");
    Ok(())
}
