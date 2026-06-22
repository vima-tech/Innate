use super::{ui::gray, *};

pub(super) struct Agent {
    pub(super) id: &'static str,
    pub(super) label: String, // display name with "(detected)" suffix if found
    pub(super) detected: bool,
    pub(super) config: PathBuf,
}

pub(super) fn detect_agents(global: bool) -> Vec<Agent> {
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

pub(super) fn binary_name() -> &'static str {
    if cfg!(windows) {
        "innate.exe"
    } else {
        "innate"
    }
}

pub(super) fn path_sep() -> char {
    if cfg!(windows) {
        ';'
    } else {
        ':'
    }
}

pub(super) fn which_binary(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    std::env::var("PATH").ok().and_then(|path| {
        path.split(path_sep()).find_map(|dir| {
            let p = PathBuf::from(dir).join(&exe);
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
pub(super) enum ConfigStatus {
    Updated(PathBuf),
    Unchanged(PathBuf),
    Skipped(String),
    Error(String),
}

pub(super) fn configure_claude(agent: &Agent, binary: &Path, auto_allow: bool) -> ConfigStatus {
    let path = &agent.config;
    let mut settings: Value = match read_json_object(path) {
        Ok(v) => v,
        Err(e) => return ConfigStatus::Error(e),
    };

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
    // Backfill the agent-source env on re-run even when command/permissions match.
    let env_ok = settings
        .pointer("/mcpServers/innate/env/INNATE_AGENT")
        .and_then(Value::as_str)
        == Some("claude-code");

    if existing_cmd == binary_str && already_allowed && env_ok {
        return ConfigStatus::Unchanged(path.clone());
    }

    // Set mcpServers.innate (root is an object — guaranteed by read_json_object)
    let root = settings.as_object_mut().unwrap();
    let Some(mcp_servers) = root
        .entry("mcpServers")
        .or_insert(json!({}))
        .as_object_mut()
    else {
        return ConfigStatus::Error(format!(
            "{}: \"mcpServers\" is not an object",
            path.display()
        ));
    };
    mcp_servers.insert(
        "innate".to_string(),
        json!({
            "type": "stdio",
            "command": binary_str,
            "args": ["mcp"],
            // Agent-source dimension: tags recalled/recorded/distilled knowledge
            // with the driving agent product (innate utils::agent_source).
            "env": {"INNATE_AGENT": "claude-code"}
        }),
    );

    // Set permissions.allow
    if auto_allow {
        let Some(permissions) = root
            .entry("permissions")
            .or_insert(json!({}))
            .as_object_mut()
        else {
            return ConfigStatus::Error(format!(
                "{}: \"permissions\" is not an object",
                path.display()
            ));
        };
        let Some(arr) = permissions
            .entry("allow")
            .or_insert(json!([]))
            .as_array_mut()
        else {
            return ConfigStatus::Error(format!(
                "{}: \"permissions.allow\" is not an array",
                path.display()
            ));
        };
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

pub(super) fn configure_codex(agent: &Agent, binary: &Path, auto_allow: bool) -> ConfigStatus {
    let path = &agent.config;
    if !path.parent().map(|p| p.exists()).unwrap_or(false) {
        return ConfigStatus::Skipped("~/.codex/ not found — install Codex CLI first".to_string());
    }

    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let binary_str = binary.to_string_lossy();

    // Check if already configured
    let already = existing.contains("[mcp_servers.innate]");
    if already {
        // Check if command matches and the agent-source env is already present.
        if existing.contains(&format!("command = \"{binary_str}\""))
            && existing.contains("INNATE_AGENT")
        {
            return ConfigStatus::Unchanged(path.clone());
        }
    }

    let mut addition = format!(
        "\n[mcp_servers.innate]\ncommand = \"{binary_str}\"\nargs = [\"mcp\"]\nenv = {{ INNATE_AGENT = \"codex\" }}\n"
    );

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

pub(super) fn configure_opencode(agent: &Agent, binary: &Path, _auto_allow: bool) -> ConfigStatus {
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

    // Check if already configured (command matches AND agent-source env present).
    if let Some(existing_cmd) = config.pointer("/mcp/innate/command") {
        let cmd_ok = existing_cmd
            .as_array()
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            == Some(&binary_str);
        let env_ok = config
            .pointer("/mcp/innate/environment/INNATE_AGENT")
            .and_then(Value::as_str)
            == Some("opencode");
        if cmd_ok && env_ok {
            return ConfigStatus::Unchanged(path.clone());
        }
    }

    let Some(root) = config.as_object_mut() else {
        return ConfigStatus::Error(format!("{}: root is not a JSON object", path.display()));
    };
    let Some(mcp) = root.entry("mcp").or_insert(json!({})).as_object_mut() else {
        return ConfigStatus::Error(format!("{}: \"mcp\" is not an object", path.display()));
    };
    mcp.insert(
        "innate".to_string(),
        json!({
            "type": "local",
            "command": [binary_str, "mcp"],
            "enabled": true,
            // Agent-source dimension (innate utils::agent_source).
            "environment": {"INNATE_AGENT": "opencode"}
        }),
    );

    match write_json(path, &config) {
        Ok(()) => ConfigStatus::Updated(path.clone()),
        Err(e) => ConfigStatus::Error(e.to_string()),
    }
}
pub(super) fn remove_claude_config(config_path: &Path) -> ConfigStatus {
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

pub(super) fn remove_codex_config() -> ConfigStatus {
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

pub(super) fn remove_opencode_config() -> ConfigStatus {
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

/// Append a Stop hook entry to the Claude Code settings at `config_path`.
/// Uses the provided `binary` path so no Python interpreter is required.
pub(super) fn configure_claude_stop_hook(config_path: &Path, binary: &Path) -> ConfigStatus {
    configure_claude_hook(config_path, binary, "Stop", "hook stop")
}

/// Append a UserPromptSubmit hook that recalls relevant knowledge for every prompt.
/// This is the high-frequency, relevance-gated recall trigger (see `innate hook prompt`).
pub(super) fn configure_claude_prompt_hook(config_path: &Path, binary: &Path) -> ConfigStatus {
    configure_claude_hook(config_path, binary, "UserPromptSubmit", "hook prompt")
}

/// Append a SubagentStop hook so Task-tool subagents also feed session events to the daemon.
/// Reuses the same `hook stop` handler — the subagent stop payload carries the same transcript
/// reference, plus `agent_id`/`agent_type` for attribution.
pub(super) fn configure_claude_subagent_stop_hook(
    config_path: &Path,
    binary: &Path,
) -> ConfigStatus {
    configure_claude_hook(config_path, binary, "SubagentStop", "hook stop")
}

/// Append a SessionStart hook that warms up context with high-relevance project knowledge.
pub(super) fn configure_claude_session_start_hook(
    config_path: &Path,
    binary: &Path,
) -> ConfigStatus {
    configure_claude_hook(config_path, binary, "SessionStart", "hook session-start")
}

/// Generic Claude Code hook installer: append `<binary> <subcommand>` to `hooks.<event>`.
/// Idempotent — skips if the exact command is already present. No Python interpreter required.
fn configure_claude_hook(
    config_path: &Path,
    binary: &Path,
    event: &str,
    subcommand: &str,
) -> ConfigStatus {
    let mut settings: Value = match read_json_object(config_path) {
        Ok(v) => v,
        Err(e) => return ConfigStatus::Error(e),
    };

    // Quote the binary path in case it contains spaces.
    let binary_str = binary.to_string_lossy();
    let cmd = if binary_str.contains(' ') {
        format!("\"{binary_str}\" {subcommand}")
    } else {
        format!("{binary_str} {subcommand}")
    };

    // Idempotence check — skip if the command is already present.
    let already = settings
        .pointer(&format!("/hooks/{event}"))
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

    // Root is an object — guaranteed by read_json_object.
    let Some(hooks_map) = settings
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert(json!({}))
        .as_object_mut()
    else {
        return ConfigStatus::Error(format!(
            "{}: \"hooks\" is not an object",
            config_path.display()
        ));
    };

    let Some(event_arr) = hooks_map.entry(event).or_insert(json!([])).as_array_mut() else {
        return ConfigStatus::Error(format!(
            "{}: \"hooks.{event}\" is not an array",
            config_path.display()
        ));
    };

    event_arr.push(json!({
        "hooks": [{"type": "command", "command": cmd}]
    }));

    match write_json(config_path, &settings) {
        Ok(()) => ConfigStatus::Updated(config_path.to_path_buf()),
        Err(e) => ConfigStatus::Error(e.to_string()),
    }
}
