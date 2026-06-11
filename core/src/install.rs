//! `innate install` — interactive setup wizard (clack-style TUI).
//!
//! No extra dependencies — uses only what Innate already pulls in.
//! Configures Claude Code, Codex CLI, and opencode to use innate's MCP server.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

const SKILL_MD: &str = include_str!("../../skills/innate-memory/SKILL.md");

const INNATE_TOOLS: &[&str] = &[
    "innate_recall",
    "innate_record",
    "innate_add",
    "innate_spark",
    "innate_evolve",
    "innate_inspect",
    "innate_approve",
    "innate_archive",
    "innate_invalidate",
    "innate_restore",
    "innate_mature_spark",
    "innate_promote_spark",
    "innate_drop_spark",
];

// ── Clack-style output ────────────────────────────────────────────────────────

fn tty() -> bool {
    #[cfg(unix)]
    unsafe {
        use std::os::unix::io::AsRawFd;
        libc::isatty(io::stdout().as_raw_fd()) == 1 && std::env::var("NO_COLOR").is_err()
    }
    #[cfg(not(unix))]
    false
}

fn c(s: &str, code: u8) -> String {
    if tty() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

fn green(s: &str) -> String {
    c(s, 32)
}
fn gray(s: &str) -> String {
    c(s, 90)
}
fn bold(s: &str) -> String {
    c(s, 1)
}
fn cyan(s: &str) -> String {
    c(s, 36)
}
fn yellow(s: &str) -> String {
    c(s, 33)
}
fn dim(s: &str) -> String {
    c(s, 2)
}

/// `┌  title`
fn box_open(title: &str) {
    println!("{}", green(&format!("┌  {}", bold(title))));
    println!("{}", gray("│"));
}

/// `└  message`
fn box_close(msg: &str) {
    println!("{}", gray("│"));
    println!("{}", green(&format!("└  {msg}")));
}

/// Gray vertical bar separator
fn sep() {
    println!("{}", gray("│"));
}

/// `◇  question`
fn question(q: &str) {
    println!("{}", cyan(&format!("◇  {q}")));
}

/// `│  text`
fn info(text: &str) {
    println!("{}  {text}", gray("│"));
}

/// `◆  text`  (result / completed step)
fn result_line(text: &str) {
    println!("{}  {text}", green("◆"));
}

/// `◆  text` in yellow (warning / unchanged)
fn warn_line(text: &str) {
    println!("{}  {}", yellow("◆"), text);
}

// ── Interactive prompts ───────────────────────────────────────────────────────

/// Multi-select prompt. Returns a `Vec<bool>` parallel to `options`.
/// `selected[i] = true` means the option was confirmed.
///
/// Display:
/// ```text
/// ◇  prompt
/// │  [1] ✓ option A
/// │  [2] ✓ option B
/// │  [3] ✗ option C
/// │
/// │  ENTER to confirm, type a number to toggle:
/// │  >
/// ```
fn prompt_multi_select(prompt: &str, options: &[(&str, bool)]) -> Vec<bool> {
    question(prompt);
    let mut selected: Vec<bool> = options.iter().map(|(_, s)| *s).collect();

    loop {
        for (i, (name, _)) in options.iter().enumerate() {
            let mark = if selected[i] {
                green("✓")
            } else {
                gray("✗")
            };
            info(&format!("[{}] {mark} {name}", i + 1));
        }
        sep();
        info("ENTER to confirm, or type a number to toggle:");
        print!("{}  {} ", gray("│"), dim("▶"));
        io::stdout().flush().ok();

        let line = read_line().trim().to_string();
        if line.is_empty() {
            break;
        }

        if let Ok(n) = line.parse::<usize>() {
            if n >= 1 && n <= options.len() {
                selected[n - 1] = !selected[n - 1];
                // Redraw — clear the lines we just printed (options + sep + info + prompt)
                let lines_back = options.len() + 3;
                if tty() {
                    print!("\x1b[{}A\x1b[J", lines_back);
                    io::stdout().flush().ok();
                }
                continue;
            }
        }
        break;
    }

    // Print the confirmed answer line.
    let chosen: Vec<&str> = options
        .iter()
        .zip(selected.iter())
        .filter(|(_, &s)| s)
        .map(|((name, _), _)| *name)
        .collect();
    let answer = if chosen.is_empty() {
        "none".to_string()
    } else {
        chosen.join(", ")
    };
    info(&green(&answer));
    sep();

    selected
}

/// Yes/No confirm. Returns true if the user says yes (default can be true or false).
fn prompt_confirm(prompt: &str, default_yes: bool) -> bool {
    let hint = if default_yes { "Y/n" } else { "y/N" };
    question(&format!("{prompt} ({hint})"));
    print!("{}  {} ", gray("│"), dim("▶"));
    io::stdout().flush().ok();

    let line = read_line().trim().to_lowercase();
    let result = if line.is_empty() {
        default_yes
    } else {
        line.starts_with('y')
    };
    info(&green(if result { "Yes" } else { "No" }));
    sep();
    result
}

/// Single-select from a list of options. Returns the chosen index.
fn prompt_select(prompt: &str, options: &[&str]) -> usize {
    question(prompt);
    for (i, opt) in options.iter().enumerate() {
        info(&format!("[{}] {opt}", i + 1));
    }
    sep();
    print!("{}  {} ", gray("│"), dim("▶"));
    io::stdout().flush().ok();

    let line = read_line().trim().to_string();
    let idx = line
        .parse::<usize>()
        .unwrap_or(1)
        .saturating_sub(1)
        .min(options.len() - 1);
    info(&green(options[idx]));
    sep();
    idx
}

fn read_line() -> String {
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line).ok();
    // In pipe/non-TTY mode stdin has no echo, so we need an explicit newline
    // to move past the ▶ prompt before printing the answer.
    #[cfg(unix)]
    if unsafe { libc::isatty(0) } == 0 {
        println!();
    }
    line
}

// ── Agent detection ───────────────────────────────────────────────────────────

#[derive(Debug)]
struct Agent {
    id: &'static str,
    label: String, // display name with "(detected)" suffix if found
    detected: bool,
    config: PathBuf,
}

fn detect_agents(global: bool) -> Vec<Agent> {
    let home = home_dir();

    // Claude Code: global = ~/.claude/settings.json, project = .claude/settings.json
    let claude_global = home.join(".claude").join("settings.json");
    let claude_project = PathBuf::from(".claude").join("settings.json");
    let claude_config = if global {
        claude_global.clone()
    } else {
        claude_project
    };
    let claude_detected =
        claude_global.exists() || home.join(".claude").exists() || which_binary("claude").is_some();

    // Codex CLI: always global
    let codex_config = home.join(".codex").join("config.toml");
    let codex_detected = codex_config.exists() || which_binary("codex").is_some();

    // opencode: always global
    let opencode_config = home.join(".config").join("opencode").join("opencode.jsonc");
    let opencode_detected = opencode_config.exists() || which_binary("opencode").is_some();

    vec![
        Agent {
            id: "claude",
            label: if claude_detected {
                format!("Claude Code {}", gray("(detected)"))
            } else {
                "Claude Code".to_string()
            },
            detected: claude_detected,
            config: claude_config,
        },
        Agent {
            id: "codex",
            label: if codex_detected {
                format!("Codex CLI {}", gray("(detected)"))
            } else {
                "Codex CLI".to_string()
            },
            detected: codex_detected,
            config: codex_config,
        },
        Agent {
            id: "opencode",
            label: if opencode_detected {
                format!("opencode {}", gray("(detected)"))
            } else {
                "opencode".to_string()
            },
            detected: opencode_detected,
            config: opencode_config,
        },
    ]
}

fn which_binary(name: &str) -> Option<PathBuf> {
    std::env::var("PATH").ok().and_then(|path| {
        path.split(':').find_map(|dir| {
            let p = PathBuf::from(dir).join(name);
            if p.exists() {
                Some(p)
            } else {
                None
            }
        })
    })
}

// ── Config writers ────────────────────────────────────────────────────────────

#[derive(Debug)]
enum ConfigStatus {
    Updated(PathBuf),
    Unchanged(PathBuf),
    Skipped(String),
    Error(String),
}

fn configure_claude(agent: &Agent, binary: &Path, auto_allow: bool) -> ConfigStatus {
    let path = &agent.config;
    let mut settings: Value = read_json(path).unwrap_or(json!({}));

    let binary_str = binary.to_string_lossy().to_string();

    // Check current state
    let existing_cmd = settings
        .pointer("/mcpServers/innate/command")
        .and_then(Value::as_str)
        .unwrap_or("");
    let already_allowed = !auto_allow
        || settings
            .pointer("/permissions/allow")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().any(|v| v.as_str() == Some("mcp__innate__*")))
            .unwrap_or(false);

    if existing_cmd == binary_str && already_allowed {
        return ConfigStatus::Unchanged(path.clone());
    }

    // Set mcpServers.innate
    settings
        .as_object_mut()
        .unwrap()
        .entry("mcpServers")
        .or_insert(json!({}))
        .as_object_mut()
        .unwrap()
        .insert(
            "innate".to_string(),
            json!({
                "command": binary_str,
                "args": ["mcp"]
            }),
        );

    // Set permissions.allow
    if auto_allow {
        let allow = settings
            .as_object_mut()
            .unwrap()
            .entry("permissions")
            .or_insert(json!({}))
            .as_object_mut()
            .unwrap()
            .entry("allow")
            .or_insert(json!([]));
        let arr = allow.as_array_mut().unwrap();
        let pat = "mcp__innate__*";
        if !arr.iter().any(|v| v.as_str() == Some(pat)) {
            arr.push(json!(pat));
        }
    }

    match write_json(path, &settings) {
        Ok(()) => ConfigStatus::Updated(path.clone()),
        Err(e) => ConfigStatus::Error(e.to_string()),
    }
}

fn configure_codex(agent: &Agent, binary: &Path, auto_allow: bool) -> ConfigStatus {
    let path = &agent.config;
    if !path.parent().map(|p| p.exists()).unwrap_or(false) {
        return ConfigStatus::Skipped("~/.codex/ not found — install Codex CLI first".to_string());
    }

    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let binary_str = binary.to_string_lossy();

    // Check if already configured
    let already = existing.contains("[mcp_servers.innate]");
    if already {
        // Check if command matches
        if existing.contains(&format!("command = \"{binary_str}\"")) {
            return ConfigStatus::Unchanged(path.clone());
        }
    }

    let mut addition =
        format!("\n[mcp_servers.innate]\ncommand = \"{binary_str}\"\nargs = [\"mcp\"]\n");

    if auto_allow {
        for tool in INNATE_TOOLS {
            addition.push_str(&format!(
                "\n[mcp_servers.innate.tools.{tool}]\napproval_mode = \"auto\"\n"
            ));
        }
    }

    let new_content = if already {
        // Replace existing innate section (simplified: just append updated block at end)
        // Strip old innate block and append fresh one
        let stripped = strip_toml_section(&existing, "mcp_servers.innate");
        stripped + &addition
    } else {
        existing + &addition
    };

    match std::fs::write(path, new_content) {
        Ok(()) => ConfigStatus::Updated(path.clone()),
        Err(e) => ConfigStatus::Error(e.to_string()),
    }
}

fn configure_opencode(agent: &Agent, binary: &Path, _auto_allow: bool) -> ConfigStatus {
    let path = &agent.config;
    if !path.exists() {
        return ConfigStatus::Skipped("opencode.jsonc not found".into());
    }

    let txt = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => return ConfigStatus::Error(e.to_string()),
    };

    let stripped = strip_jsonc_comments(&txt);
    let mut config: Value = match serde_json::from_str(&stripped) {
        Ok(v) => v,
        Err(e) => return ConfigStatus::Error(format!("parse error: {e}")),
    };

    let binary_str = binary.to_string_lossy().to_string();

    // Check if already configured
    if let Some(existing_cmd) = config.pointer("/mcp/innate/command") {
        let already = existing_cmd
            .as_array()
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            == Some(&binary_str);
        if already {
            return ConfigStatus::Unchanged(path.clone());
        }
    }

    config
        .as_object_mut()
        .unwrap()
        .entry("mcp")
        .or_insert(json!({}))
        .as_object_mut()
        .unwrap()
        .insert(
            "innate".to_string(),
            json!({
                "type": "local",
                "command": [binary_str, "mcp"],
                "enabled": true
            }),
        );

    match write_json(path, &config) {
        Ok(()) => ConfigStatus::Updated(path.clone()),
        Err(e) => ConfigStatus::Error(e.to_string()),
    }
}

fn install_claude_skill() -> ConfigStatus {
    let dest_dir = home_dir()
        .join(".claude")
        .join("skills")
        .join("innate-memory");
    let dest = dest_dir.join("SKILL.md");

    if let Ok(existing) = std::fs::read_to_string(&dest) {
        if existing == SKILL_MD {
            return ConfigStatus::Unchanged(dest);
        }
    }

    if let Err(e) = std::fs::create_dir_all(&dest_dir) {
        return ConfigStatus::Error(e.to_string());
    }

    match std::fs::write(&dest, SKILL_MD) {
        Ok(()) => ConfigStatus::Updated(dest),
        Err(e) => ConfigStatus::Error(e.to_string()),
    }
}

// ── PATH installation ─────────────────────────────────────────────────────────

fn check_on_path() -> Option<PathBuf> {
    which_binary("innate")
}

fn install_to_path(current_exe: &Path) -> anyhow::Result<PathBuf> {
    let local_bin = home_dir().join(".local").join("bin");
    std::fs::create_dir_all(&local_bin)?;
    let dest = local_bin.join("innate");

    // Symlink preferred; fall back to copy.
    if dest.exists() || dest.is_symlink() {
        std::fs::remove_file(&dest)?;
    }

    #[cfg(unix)]
    std::os::unix::fs::symlink(current_exe, &dest)?;
    #[cfg(not(unix))]
    std::fs::copy(current_exe, &dest)?;

    Ok(dest)
}

fn path_has_local_bin() -> bool {
    std::env::var("PATH")
        .map(|p| p.split(':').any(|d| d.contains(".local/bin")))
        .unwrap_or(false)
}

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
    let on_path = check_on_path();
    let binary_path: PathBuf = if let Some(p) = &on_path {
        question("Install innate CLI on your PATH?");
        info(&format!(
            "Already on PATH {}",
            gray(&format!("({})", p.display()))
        ));
        sep();
        p.clone()
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
                        warn_line(&yellow(
                            "Add ~/.local/bin to PATH in your shell profile:\
                            \n│    export PATH=\"$HOME/.local/bin:$PATH\"",
                        ));
                    }
                    sep();
                    dest
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

    // ── 5. Apply configs ───────────────────────────────────────────────────
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
            match install_claude_skill() {
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
        }
    }
    sep();

    // ── 6. Quick start ─────────────────────────────────────────────────────
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

// ── Helpers ───────────────────────────────────────────────────────────────────

fn home_dir() -> PathBuf {
    dirs_next::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn tilde_path(p: &Path) -> String {
    let home = home_dir();
    if let Ok(rel) = p.strip_prefix(&home) {
        format!("~/{}", rel.display())
    } else {
        p.display().to_string()
    }
}

fn read_json(path: &Path) -> Option<Value> {
    let txt = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&txt).ok()
}

fn write_json(path: &Path, value: &Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let txt = serde_json::to_string_pretty(value)?;
    std::fs::write(path, txt + "\n")?;
    Ok(())
}

/// Strip `//` and `/* */` comments from a JSONC string.
fn strip_jsonc_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    let mut in_str = false;
    let mut escape = false;

    while let Some(c) = chars.next() {
        if escape {
            out.push(c);
            escape = false;
            continue;
        }
        if in_str {
            if c == '\\' {
                escape = true;
                out.push(c);
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            out.push(c);
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            continue;
        }
        if c == '/' {
            match chars.peek() {
                Some('/') => {
                    for nc in chars.by_ref() {
                        if nc == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                    continue;
                }
                Some('*') => {
                    chars.next();
                    while let Some(nc) = chars.next() {
                        if nc == '*' && chars.peek() == Some(&'/') {
                            chars.next();
                            break;
                        }
                    }
                    continue;
                }
                _ => {}
            }
        }
        out.push(c);
    }
    out
}

/// Remove all `[prefix.*]` TOML sections (and their keys) from a TOML string.
/// Used to replace an existing innate block when re-configuring.
fn strip_toml_section(toml: &str, section_prefix: &str) -> String {
    let mut out = String::new();
    let mut skip = false;
    for line in toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            // New section header — check if it belongs to the prefix we're stripping.
            let header = trimmed.trim_start_matches('[').trim_end_matches(']');
            skip = header == section_prefix || header.starts_with(&format!("{section_prefix}."));
        }
        if !skip {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}
