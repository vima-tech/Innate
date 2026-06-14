use super::{agents::*, path::*, settings::*, skills::*, ui::*, *};

// ── Main entry point ──────────────────────────────────────────────────────────

pub fn run_install() -> anyhow::Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    box_open(&format!("Innate v{version}"));

    let current_exe = std::env::current_exe()?;

    // ── 1. Scope: global vs project ────────────────────────────────────────
    let scope_options = ["All projects (global)", "Just this project"];
    let scope_idx = prompt_select(
        "Apply agent configs to all your projects, or just this one?",
        &scope_options,
    );
    let global = scope_idx == 0;

    // ── 2. Agent selection ─────────────────────────────────────────────────
    let agents = detect_agents(global);
    let options: Vec<(&str, bool)> = agents
        .iter()
        .map(|a| (a.label.as_str(), a.detected))
        .collect();
    let selected = prompt_multi_select("Which agents should Innate configure?", &options);
    let chosen_agents: Vec<&Agent> = agents
        .iter()
        .zip(selected.iter())
        .filter(|(_, &s)| s)
        .map(|(a, _)| a)
        .collect();

    // ── 3. PATH installation ───────────────────────────────────────────────
    // When the binary is on PATH, write just "innate" into agent configs so
    // the config is portable across machines and users.  Only fall back to an
    // absolute path when the user declines PATH installation.
    let on_path = check_on_path();
    let binary_path: PathBuf = if let Some(p) = &on_path {
        question("Install innate CLI on your PATH?");
        info(&format!(
            "Already on PATH {}",
            gray(&format!("({})", p.display()))
        ));
        sep();
        PathBuf::from(binary_name())
    } else {
        let do_install = prompt_confirm(
            "Install innate CLI on your PATH? (Required for agents to launch the MCP server)",
            true,
        );
        if do_install {
            match install_to_path(&current_exe) {
                Ok(dest) => {
                    result_line(&format!(
                        "Installed innate to {}",
                        bold(&dest.display().to_string())
                    ));
                    if !path_has_local_bin() {
                        let written = write_path_to_profiles();
                        if written.is_empty() {
                            #[cfg(windows)]
                            warn_line(&yellow(
                                "Add .local\\bin to PATH:\
                                \n│    [Environment]::SetEnvironmentVariable('PATH', $env:USERPROFILE + '\\.local\\bin;' + $env:PATH, 'User')",
                            ));
                            #[cfg(not(windows))]
                            warn_line(&yellow(
                                "Add ~/.local/bin to PATH in your shell profile:\
                                \n│    export PATH=\"$HOME/.local/bin:$PATH\"",
                            ));
                        } else {
                            for p in &written {
                                result_line(&format!(
                                    "Added PATH export to {}",
                                    bold(&tilde_path(p))
                                ));
                            }
                            #[cfg(windows)]
                            info(&dim(
                                "Open a new terminal for the PATH change to take effect",
                            ));
                            #[cfg(not(windows))]
                            info(&dim("Run: source ~/.bashrc  (or open a new terminal)"));
                        }
                    }
                    sep();
                    PathBuf::from(binary_name())
                }
                Err(e) => {
                    warn_line(&format!("Could not install to PATH: {e}"));
                    info("Falling back to current binary location");
                    sep();
                    current_exe.clone()
                }
            }
        } else {
            current_exe.clone()
        }
    };

    // ── 4. Auto-allow ──────────────────────────────────────────────────────
    let auto_allow = prompt_confirm(
        "Auto-allow Innate MCP tools? (Skips permission prompts in agents)",
        true,
    );

    // ── 5. LLM configuration (optional) ───────────────────────────────────
    configure_llm_interactive();

    // ── 5b. Daemon watch dirs (optional) ──────────────────────────────────
    configure_daemon_interactive();

    // ── 6. Apply configs ───────────────────────────────────────────────────
    for agent in &chosen_agents {
        let status = match agent.id {
            "claude" => configure_claude(agent, &binary_path, auto_allow),
            "codex" => configure_codex(agent, &binary_path, auto_allow),
            "opencode" => configure_opencode(agent, &binary_path, auto_allow),
            _ => ConfigStatus::Skipped("unknown agent".into()),
        };

        match &status {
            ConfigStatus::Updated(p) => {
                result_line(&format!(
                    "{}: Updated {}",
                    bold(agent.id),
                    gray(&tilde_path(p))
                ));
            }
            ConfigStatus::Unchanged(p) => {
                result_line(&format!(
                    "{}: {}",
                    bold(agent.id),
                    gray(&format!("Unchanged {}", tilde_path(p)))
                ));
            }
            ConfigStatus::Skipped(reason) => {
                warn_line(&format!(
                    "{}: {}",
                    bold(agent.id),
                    yellow(&format!("Skipped — {reason}"))
                ));
            }
            ConfigStatus::Error(e) => {
                warn_line(&format!("{}: \x1b[31mError — {e}\x1b[0m", bold(agent.id)));
            }
        }

        if agent.id == "claude" {
            // Hooks must live in a settings.json file — Claude Code does NOT read hooks from
            // ~/.claude.json (that file is MCP/OAuth/state only). In global scope agent.config
            // points at ~/.claude.json (correct for MCP), so derive the hooks target separately.
            let hook_config = if global {
                home_dir().join(".claude").join("settings.json")
            } else {
                agent.config.clone()
            };
            match install_skill() {
                ConfigStatus::Updated(p) => {
                    result_line(&format!(
                        "{}: Installed skill {}",
                        bold("claude"),
                        gray(&tilde_path(&p))
                    ));
                }
                ConfigStatus::Unchanged(p) => {
                    result_line(&format!(
                        "{}: {}",
                        bold("claude"),
                        gray(&format!("Skill unchanged {}", tilde_path(&p)))
                    ));
                }
                ConfigStatus::Skipped(reason) => {
                    warn_line(&format!(
                        "{}: {}",
                        bold("claude"),
                        yellow(&format!("Skill skipped — {reason}"))
                    ));
                }
                ConfigStatus::Error(e) => {
                    warn_line(&format!(
                        "{}: \x1b[31mSkill error — {e}\x1b[0m",
                        bold("claude")
                    ));
                }
            }

            // Install slash commands alongside the skill
            for (name, status) in install_commands() {
                match status {
                    ConfigStatus::Updated(p) => {
                        result_line(&format!(
                            "{}: /{name} {}",
                            bold("claude"),
                            gray(&tilde_path(&p))
                        ));
                    }
                    ConfigStatus::Unchanged(_) => {}
                    ConfigStatus::Skipped(_) => {}
                    ConfigStatus::Error(e) => {
                        warn_line(&format!(
                            "{}: \x1b[31mCommand /{name} error — {e}\x1b[0m",
                            bold("claude")
                        ));
                    }
                }
            }

            // Install Stop hook so daemon gets session events automatically.
            match configure_claude_stop_hook(&hook_config, &binary_path) {
                ConfigStatus::Updated(p) => {
                    result_line(&format!(
                        "{}: Stop hook → {}",
                        bold("claude"),
                        gray(&tilde_path(&p))
                    ));
                }
                ConfigStatus::Unchanged(_) => {}
                ConfigStatus::Skipped(_) => {}
                ConfigStatus::Error(e) => {
                    warn_line(&format!(
                        "{}: \x1b[31mStop hook error — {e}\x1b[0m",
                        bold("claude")
                    ));
                }
            }

            // Install UserPromptSubmit hook: relevance-gated auto-recall on every prompt.
            match configure_claude_prompt_hook(&hook_config, &binary_path) {
                ConfigStatus::Updated(p) => {
                    result_line(&format!(
                        "{}: UserPromptSubmit hook → {}",
                        bold("claude"),
                        gray(&tilde_path(&p))
                    ));
                }
                ConfigStatus::Unchanged(_) => {}
                ConfigStatus::Skipped(_) => {}
                ConfigStatus::Error(e) => {
                    warn_line(&format!(
                        "{}: \x1b[31mUserPromptSubmit hook error — {e}\x1b[0m",
                        bold("claude")
                    ));
                }
            }

            // Install SessionStart hook: warm up project knowledge at session start.
            match configure_claude_session_start_hook(&hook_config, &binary_path) {
                ConfigStatus::Updated(p) => {
                    result_line(&format!(
                        "{}: SessionStart hook → {}",
                        bold("claude"),
                        gray(&tilde_path(&p))
                    ));
                }
                ConfigStatus::Unchanged(_) => {}
                ConfigStatus::Skipped(_) => {}
                ConfigStatus::Error(e) => {
                    warn_line(&format!(
                        "{}: \x1b[31mSessionStart hook error — {e}\x1b[0m",
                        bold("claude")
                    ));
                }
            }

            // Install SubagentStop hook so Task-tool subagents also feed session events.
            match configure_claude_subagent_stop_hook(&hook_config, &binary_path) {
                ConfigStatus::Updated(p) => {
                    result_line(&format!(
                        "{}: SubagentStop hook → {}",
                        bold("claude"),
                        gray(&tilde_path(&p))
                    ));
                }
                ConfigStatus::Unchanged(_) => {}
                ConfigStatus::Skipped(_) => {}
                ConfigStatus::Error(e) => {
                    warn_line(&format!(
                        "{}: \x1b[31mSubagentStop hook error — {e}\x1b[0m",
                        bold("claude")
                    ));
                }
            }
        }
    }
    sep();

    // ── 7. Quick start ─────────────────────────────────────────────────────
    // Box: inner display width = 28.  Total row width = │  {28}│ = 32 chars.
    // Header dashes = 28 - 12 ("Quick start ") = 16.
    // Bottom dashes = 28 + 2 = 30.
    const INNER: usize = 28;
    let bar = gray("│");
    let qs_top = format!(
        "{}  Quick start {}{}",
        cyan("◇"),
        gray(&"─".repeat(INNER - 12)),
        gray("╮")
    );
    let qs_row = |s: &str| -> String {
        let pad = INNER.saturating_sub(s.chars().count());
        format!("{bar}  {s}{}{bar}", " ".repeat(pad))
    };
    let qs_empty = qs_row("");
    let qs_sep = format!("{}{}╯", gray("├"), gray(&"─".repeat(INNER + 2)));

    println!("{qs_top}");
    println!("{qs_empty}");
    println!("{}", qs_row("innate recall \"query\""));
    println!("{}", qs_row("innate record <trace_id>"));
    println!("{}", qs_row("innate evolve"));
    println!("{qs_empty}");
    println!("{qs_sep}");

    box_close("Done! Restart your agents to use Innate.");
    Ok(())
}
