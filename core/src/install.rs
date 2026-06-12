//! `innate install` — interactive setup wizard (clack-style TUI).
//!
//! No extra dependencies — uses only what Innate already pulls in.
//! Configures Claude Code, Codex CLI, and opencode to use innate's MCP server.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::{json, Value};

const SKILL_MD: &str = include_str!("../assets/SKILL.md");

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

/// Free-text input prompt. Returns the user's input, or `default` if empty.
fn prompt_text(prompt: &str, default: &str, hint: &str) -> String {
    if hint.is_empty() {
        question(&format!("{prompt} {}", dim(&format!("(default: {default})"))));
    } else {
        question(&format!("{prompt} {}", dim(hint)));
    }
    print!("{}  {} ", gray("│"), dim("▶"));
    io::stdout().flush().ok();
    let line = read_line().trim().to_string();
    let result = if line.is_empty() {
        default.to_string()
    } else {
        line
    };
    info(&green(if result.is_empty() { "skipped" } else { &result }));
    sep();
    result
}

/// Masked API key input — same as prompt_text but prints "••••••••" in the result line.
fn prompt_secret(prompt: &str, hint: &str) -> String {
    question(&format!("{prompt} {}", dim(hint)));
    print!("{}  {} ", gray("│"), dim("▶"));
    io::stdout().flush().ok();
    let line = read_line().trim().to_string();
    let display = if line.is_empty() {
        "skipped"
    } else {
        "••••••••"
    };
    info(&green(display));
    sep();
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

    // Claude Code: global = ~/.claude.json (User MCPs), project = .claude/settings.json
    let claude_global = home.join(".claude.json");
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

fn binary_name() -> &'static str {
    if cfg!(windows) { "innate.exe" } else { "innate" }
}

fn path_sep() -> char {
    if cfg!(windows) { ';' } else { ':' }
}

fn which_binary(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    std::env::var("PATH").ok().and_then(|path| {
        path.split(path_sep()).find_map(|dir| {
            let p = PathBuf::from(dir).join(&exe);
            if p.exists() { Some(p) } else { None }
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
                "type": "stdio",
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

// ── Slash-command install ─────────────────────────────────────────────────────

/// A slash command definition parsed from a `command` fenced block in SKILL.md.
struct SkillCommand {
    name: String,
    body: String,
}

/// Parse every ` ```command … ``` ` block from `skill_md`.
/// Each block has a `name: <n>` header before a bare `---` separator; body follows after.
fn parse_skill_commands(skill_md: &str) -> Vec<SkillCommand> {
    let mut cmds = Vec::new();
    let lines: Vec<&str> = skill_md.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == "```command" {
            i += 1;
            let block_start = i;
            while i < lines.len() && lines[i].trim() != "```" {
                i += 1;
            }
            let block_lines = &lines[block_start..i];
            if let Some(sep) = block_lines.iter().position(|l| l.trim() == "---") {
                let mut name = String::new();
                for line in &block_lines[..sep] {
                    if let Some(v) = line.strip_prefix("name:") {
                        name = v.trim().to_string();
                    }
                }
                if !name.is_empty() {
                    cmds.push(SkillCommand {
                        name,
                        body: block_lines[sep + 1..].join("\n"),
                    });
                }
            }
        }
        i += 1;
    }
    cmds
}

/// Install slash commands to `~/.claude/commands/`. Returns one status per command.
fn install_commands() -> Vec<(String, ConfigStatus)> {
    let commands_dir = home_dir().join(".claude").join("commands");
    if let Err(e) = std::fs::create_dir_all(&commands_dir) {
        return vec![("*".into(), ConfigStatus::Error(e.to_string()))];
    }
    parse_skill_commands(SKILL_MD)
        .into_iter()
        .map(|cmd| {
            let path = commands_dir.join(format!("{}.md", cmd.name));
            let current = std::fs::read_to_string(&path).unwrap_or_default();
            if current == cmd.body {
                return (cmd.name, ConfigStatus::Unchanged(path));
            }
            match std::fs::write(&path, &cmd.body) {
                Ok(()) => (cmd.name, ConfigStatus::Updated(path)),
                Err(e) => (cmd.name, ConfigStatus::Error(e.to_string())),
            }
        })
        .collect()
}

/// Remove slash commands from `~/.claude/commands/` (uninstall path).
fn remove_commands() -> Vec<(String, ConfigStatus)> {
    let commands_dir = home_dir().join(".claude").join("commands");
    parse_skill_commands(SKILL_MD)
        .into_iter()
        .map(|cmd| {
            let path = commands_dir.join(format!("{}.md", cmd.name));
            if !path.exists() {
                return (cmd.name, ConfigStatus::Skipped("not installed".into()));
            }
            match std::fs::remove_file(&path) {
                Ok(()) => (cmd.name, ConfigStatus::Updated(path)),
                Err(e) => (cmd.name, ConfigStatus::Error(e.to_string())),
            }
        })
        .collect()
}

fn skill_content_hash() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    SKILL_MD.hash(&mut h);
    let v = h.finish();
    format!("{v:016x}{:016x}{:08x}", !v, 0u32)
}

fn update_skill_lock(installing: bool) {
    let lock_path = home_dir().join(".agents").join(".skill-lock.json");
    let mut lock: Value =
        read_json(&lock_path).unwrap_or_else(|| json!({"version": 3, "skills": {}}));

    if installing {
        let now = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let installed_at = lock
            .pointer("/skills/innate-memory/installedAt")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| now.clone());

        lock.as_object_mut()
            .unwrap()
            .entry("skills")
            .or_insert(json!({}))
            .as_object_mut()
            .unwrap()
            .insert(
                "innate-memory".to_string(),
                json!({
                    "source": "local",
                    "sourceType": "local",
                    "sourceUrl": null,
                    "skillPath": "skills/innate-memory/SKILL.md",
                    "skillFolderHash": skill_content_hash(),
                    "installedAt": installed_at,
                    "updatedAt": now,
                }),
            );
    } else if let Some(skills) = lock.pointer_mut("/skills") {
        if let Some(obj) = skills.as_object_mut() {
            obj.remove("innate-memory");
        }
    }

    let _ = write_json(&lock_path, &lock);
}

fn install_skill() -> ConfigStatus {
    let agents_dir = home_dir()
        .join(".agents")
        .join("skills")
        .join("innate-memory");
    let skill_file = agents_dir.join("SKILL.md");
    let claude_link = home_dir()
        .join(".claude")
        .join("skills")
        .join("innate-memory");

    // Already current?
    let up_to_date = std::fs::read_to_string(&skill_file)
        .map(|s| s == SKILL_MD)
        .unwrap_or(false)
        && (claude_link.is_symlink() || claude_link.exists());
    if up_to_date {
        return ConfigStatus::Unchanged(skill_file);
    }

    // Write SKILL.md into ~/.agents/skills/innate-memory/
    if let Err(e) = std::fs::create_dir_all(&agents_dir) {
        return ConfigStatus::Error(e.to_string());
    }
    if let Err(e) = std::fs::write(&skill_file, SKILL_MD) {
        return ConfigStatus::Error(e.to_string());
    }

    // Ensure ~/.claude/skills/ exists
    let claude_skills = home_dir().join(".claude").join("skills");
    if let Err(e) = std::fs::create_dir_all(&claude_skills) {
        return ConfigStatus::Error(e.to_string());
    }

    // Remove existing link/dir at ~/.claude/skills/innate-memory
    if claude_link.is_symlink() || claude_link.exists() {
        let _ = std::fs::remove_file(&claude_link);
        let _ = std::fs::remove_dir_all(&claude_link);
    }

    // Symlink ~/.claude/skills/innate-memory -> ../../.agents/skills/innate-memory
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        if let Err(e) = symlink("../../.agents/skills/innate-memory", &claude_link) {
            return ConfigStatus::Error(format!("symlink: {e}"));
        }
    }
    #[cfg(not(unix))]
    {
        // Windows fallback: copy directly
        let dest = claude_link.join("SKILL.md");
        if let Err(e) = std::fs::create_dir_all(&claude_link)
            .and_then(|_| std::fs::write(&dest, SKILL_MD))
        {
            return ConfigStatus::Error(e.to_string());
        }
    }

    update_skill_lock(true);
    ConfigStatus::Updated(skill_file)
}

// ── Uninstall helpers ────────────────────────────────────────────────────────

fn remove_claude_config(config_path: &Path) -> ConfigStatus {
    if !config_path.exists() {
        return ConfigStatus::Skipped("not found".into());
    }
    let mut settings: Value = match read_json(config_path) {
        Some(v) => v,
        None => return ConfigStatus::Skipped("could not parse".into()),
    };
    let mut changed = false;

    if let Some(mcp) = settings.pointer_mut("/mcpServers") {
        if let Some(obj) = mcp.as_object_mut() {
            if obj.remove("innate").is_some() {
                changed = true;
            }
        }
    }
    if let Some(allow) = settings.pointer_mut("/permissions/allow") {
        if let Some(arr) = allow.as_array_mut() {
            let before = arr.len();
            arr.retain(|v| {
                !v.as_str()
                    .map(|s| s.starts_with("mcp__innate__"))
                    .unwrap_or(false)
            });
            if arr.len() != before {
                changed = true;
            }
        }
    }

    if !changed {
        return ConfigStatus::Unchanged(config_path.to_path_buf());
    }
    match write_json(config_path, &settings) {
        Ok(()) => ConfigStatus::Updated(config_path.to_path_buf()),
        Err(e) => ConfigStatus::Error(e.to_string()),
    }
}

fn remove_codex_config() -> ConfigStatus {
    let path = home_dir().join(".codex").join("config.toml");
    if !path.exists() {
        return ConfigStatus::Skipped("~/.codex/config.toml not found".into());
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => return ConfigStatus::Error(e.to_string()),
    };
    if !content.contains("[mcp_servers.innate]") {
        return ConfigStatus::Unchanged(path);
    }
    let stripped = strip_toml_section(&content, "mcp_servers.innate");
    match std::fs::write(&path, stripped) {
        Ok(()) => ConfigStatus::Updated(path),
        Err(e) => ConfigStatus::Error(e.to_string()),
    }
}

fn remove_opencode_config() -> ConfigStatus {
    let path = home_dir()
        .join(".config")
        .join("opencode")
        .join("opencode.jsonc");
    if !path.exists() {
        return ConfigStatus::Skipped("opencode.jsonc not found".into());
    }
    let txt = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return ConfigStatus::Error(e.to_string()),
    };
    let stripped = strip_jsonc_comments(&txt);
    let mut config: Value = match serde_json::from_str(&stripped) {
        Ok(v) => v,
        Err(e) => return ConfigStatus::Error(format!("parse error: {e}")),
    };
    let removed = config
        .pointer_mut("/mcp")
        .and_then(Value::as_object_mut)
        .and_then(|obj| obj.remove("innate"))
        .is_some();
    if !removed {
        return ConfigStatus::Unchanged(path);
    }
    match write_json(&path, &config) {
        Ok(()) => ConfigStatus::Updated(path),
        Err(e) => ConfigStatus::Error(e.to_string()),
    }
}

fn remove_skill() -> ConfigStatus {
    let agents_dir = home_dir()
        .join(".agents")
        .join("skills")
        .join("innate-memory");
    let claude_link = home_dir()
        .join(".claude")
        .join("skills")
        .join("innate-memory");

    let mut removed = false;

    if agents_dir.exists() {
        match std::fs::remove_dir_all(&agents_dir) {
            Ok(()) => removed = true,
            Err(e) => return ConfigStatus::Error(e.to_string()),
        }
    }

    if claude_link.is_symlink() || claude_link.exists() {
        if claude_link.is_symlink() {
            let _ = std::fs::remove_file(&claude_link);
        } else {
            let _ = std::fs::remove_dir_all(&claude_link);
        }
        removed = true;
    }

    if !removed {
        return ConfigStatus::Skipped("skill not installed".into());
    }

    update_skill_lock(false);
    ConfigStatus::Updated(agents_dir)
}

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
            warn_line(&format!(
                "{}: \x1b[31mError — {e}\x1b[0m",
                bold("opencode")
            ));
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

// ── PATH installation ─────────────────────────────────────────────────────────

fn check_on_path() -> Option<PathBuf> {
    which_binary("innate")
}

fn install_to_path(current_exe: &Path) -> anyhow::Result<PathBuf> {
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

fn path_has_local_bin() -> bool {
    let local_bin = home_dir().join(".local").join("bin");
    std::env::var("PATH")
        .map(|p| p.split(path_sep()).any(|d| PathBuf::from(d) == local_bin))
        .unwrap_or(false)
}

fn write_path_to_profiles() -> Vec<PathBuf> {
    #[cfg(windows)]
    { write_path_windows() }
    #[cfg(not(windows))]
    { write_path_unix() }
}

/// Linux / macOS: append export line to every shell profile that exists and
/// doesn't already mention `.local/bin`.  Returns the list of files written.
#[cfg(not(windows))]
fn write_path_unix() -> Vec<PathBuf> {
    let home = home_dir();
    let export_line = r#"export PATH="$HOME/.local/bin:$PATH""#;
    let block = format!("\n# innate\n{export_line}\n");
    // .zprofile covers macOS login shells (Catalina+); .zshrc covers interactive
    let profiles = [".bashrc", ".zshrc", ".zprofile", ".bash_profile", ".profile"];
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
                            info(&dim("Open a new terminal for the PATH change to take effect"));
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
            match configure_claude_stop_hook(&agent.config, &binary_path) {
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

// ── LLM configuration step ────────────────────────────────────────────────────

fn configure_llm_interactive() {
    use crate::settings::{self, EmbeddingConfig, LlmConfig};

    let want = prompt_confirm(
        "Configure an LLM for smarter knowledge distillation? (optional)",
        false,
    );
    if !want {
        return;
    }

    // Load existing settings (preserve any already-set values).
    let mut s = settings::load();

    // ── Provider ──────────────────────────────────────────────────────────
    let provider_idx = prompt_select(
        "LLM API format:",
        &["OpenAI  (ChatGPT / DeepSeek / Ollama / …)", "Anthropic  (Claude)"],
    );
    let provider = if provider_idx == 1 {
        "anthropic".to_string()
    } else {
        "openai".to_string()
    };

    // ── Base URL (only ask when using OpenAI format, to allow custom endpoints) ──
    let default_base_url = match provider.as_str() {
        "anthropic" => "https://api.anthropic.com",
        _ => "https://api.openai.com/v1",
    };
    let base_url_raw = prompt_text(
        "Base URL",
        default_base_url,
        "(leave blank for default)",
    );
    let base_url = if base_url_raw == default_base_url || base_url_raw.is_empty() {
        None
    } else {
        Some(base_url_raw)
    };

    // ── Model ID ──────────────────────────────────────────────────────────
    let default_model = match provider.as_str() {
        "anthropic" => "claude-haiku-4-5-20251001",
        _ => "gpt-4o-mini",
    };
    let model_id = prompt_text("Model ID", default_model, "");

    // ── API key ───────────────────────────────────────────────────────────
    let env_hint = match provider.as_str() {
        "anthropic" => "(stored 0600; or set ANTHROPIC_API_KEY env var to skip)",
        _ => "(stored 0600; or set OPENAI_API_KEY env var to skip)",
    };
    let api_key_raw = prompt_secret("API key", env_hint);
    let api_key = if api_key_raw.is_empty() {
        None
    } else {
        Some(api_key_raw)
    };

    let llm_cfg = LlmConfig {
        provider: provider.clone(),
        base_url: base_url.clone(),
        model_id: model_id.clone(),
        api_key: api_key.clone(),
    };

    // ── Optional connection test ──────────────────────────────────────────
    let do_test = prompt_confirm("Test LLM connection now?", true);
    if do_test {
        info("Testing connection…");
        match crate::llm::test_llm(&llm_cfg) {
            Ok(msg) => result_line(&format!("LLM OK — {msg}")),
            Err(e) => warn_line(&format!("LLM test failed: {e}")),
        }
    }

    s.llm = Some(llm_cfg);

    // ── Embedding model (optional, defaults to same API as LLM) ──────────
    let want_embed = prompt_confirm(
        "Configure an embedding model too? (enables semantic recall — optional)",
        false,
    );
    if want_embed {
        let embed_default_url = base_url.clone().unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let embed_base_url_raw = prompt_text(
            "Embedding base URL",
            &embed_default_url,
            "(leave blank to reuse LLM base URL)",
        );
        let embed_base_url = if embed_base_url_raw == embed_default_url || embed_base_url_raw.is_empty() {
            base_url.clone()
        } else {
            Some(embed_base_url_raw)
        };

        let embed_model = prompt_text(
            "Embedding model ID",
            "text-embedding-3-small",
            "",
        );

        let embed_key_raw = prompt_secret(
            "Embedding API key",
            "(leave blank to reuse LLM API key)",
        );
        let embed_key = if embed_key_raw.is_empty() {
            api_key.clone()
        } else {
            Some(embed_key_raw)
        };

        let embed_cfg = EmbeddingConfig {
            provider: "openai".to_string(),
            base_url: embed_base_url,
            model_id: embed_model.clone(),
            api_key: embed_key.clone(),
            dim: 1536,
        };

        let do_test_embed = prompt_confirm("Test embedding connection now?", true);
        if do_test_embed {
            info("Testing embedding…");
            match crate::llm::test_embedding(&embed_cfg) {
                Ok(dim) => result_line(&format!("Embedding OK — dim={dim} model={embed_model}")),
                Err(e) => warn_line(&format!("Embedding test failed: {e}")),
            }
        }

        s.embedding = Some(embed_cfg);
    }

    // ── Save settings ─────────────────────────────────────────────────────
    match settings::save(&s) {
        Ok(()) => result_line(&format!(
            "LLM config saved to {}",
            bold(&settings::settings_path().display().to_string())
        )),
        Err(e) => warn_line(&format!("Could not save settings: {e}")),
    }
    sep();
}

// ── Daemon configuration step ─────────────────────────────────────────────────

fn configure_daemon_interactive() {
    use crate::settings::{self, DaemonConfig};

    // Auto-create the default hooks directory.
    let hooks_dir = home_dir().join(".innate").join("sessions");
    let _ = std::fs::create_dir_all(&hooks_dir);
    let default_watch = "~/.innate/sessions".to_string();

    let want = prompt_confirm(
        "Enable daemon auto-collection? (auto-evolves knowledge after each session)",
        true,
    );
    if !want {
        return;
    }

    info(&format!(
        "Default watch dir: {}  (created automatically)",
        bold(&default_watch)
    ));
    info("Add more directories below, or press Enter to use the default only.");
    sep();

    let mut watch_dirs: Vec<String> = vec![default_watch];
    loop {
        let dir = prompt_text(
            "Additional watch directory",
            "",
            "(leave blank to finish, ~ is expanded)",
        );
        if dir.is_empty() {
            break;
        }
        watch_dirs.push(dir.clone());
        result_line(&format!("Added: {dir}"));
    }

    let mut s = settings::load();
    s.daemon = Some(DaemonConfig {
        watch_dirs,
        auto_start: true,
    });

    match settings::save(&s) {
        Ok(()) => result_line(&format!(
            "Daemon config saved to {}",
            bold(&settings::settings_path().display().to_string())
        )),
        Err(e) => warn_line(&format!("Could not save daemon config: {e}")),
    }
    sep();
}

/// Append a Stop hook entry to the Claude Code settings at `config_path`.
/// Uses the provided `binary` path so no Python interpreter is required.
fn configure_claude_stop_hook(config_path: &Path, binary: &Path) -> ConfigStatus {
    let mut settings: Value = read_json(config_path).unwrap_or(json!({}));

    // Quote the binary path in case it contains spaces.
    let binary_str = binary.to_string_lossy();
    let cmd = if binary_str.contains(' ') {
        format!("\"{}\" hook stop", binary_str)
    } else {
        format!("{} hook stop", binary_str)
    };

    // Idempotence check — skip if the command is already present.
    let already = settings
        .pointer("/hooks/Stop")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter().any(|h| {
                h.pointer("/hooks")
                    .and_then(Value::as_array)
                    .map(|cmds| {
                        cmds.iter()
                            .any(|c| c.get("command").and_then(Value::as_str) == Some(&cmd))
                    })
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);

    if already {
        return ConfigStatus::Unchanged(config_path.to_path_buf());
    }

    let hooks_map = settings
        .as_object_mut()
        .expect("settings is object")
        .entry("hooks")
        .or_insert(json!({}))
        .as_object_mut()
        .expect("hooks is object");

    let stop_arr = hooks_map
        .entry("Stop")
        .or_insert(json!([]))
        .as_array_mut()
        .expect("Stop is array");

    stop_arr.push(json!({
        "hooks": [{"type": "command", "command": cmd}]
    }));

    match write_json(config_path, &settings) {
        Ok(()) => ConfigStatus::Updated(config_path.to_path_buf()),
        Err(e) => ConfigStatus::Error(e.to_string()),
    }
}
