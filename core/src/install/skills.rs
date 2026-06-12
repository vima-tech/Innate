use super::{agents::ConfigStatus, *};

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
pub(super) fn install_commands() -> Vec<(String, ConfigStatus)> {
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
pub(super) fn remove_commands() -> Vec<(String, ConfigStatus)> {
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
    // The lock file is innate's own metadata — reset a malformed root instead of panicking.
    if !lock.is_object() {
        lock = json!({"version": 3, "skills": {}});
    }

    if installing {
        let now = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let installed_at = lock
            .pointer("/skills/innate-memory/installedAt")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| now.clone());

        let skills = lock
            .as_object_mut()
            .unwrap()
            .entry("skills")
            .or_insert(json!({}));
        // Same defensive reset for a malformed "skills" field.
        if !skills.is_object() {
            *skills = json!({});
        }
        skills.as_object_mut().unwrap().insert(
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

pub(super) fn install_skill() -> ConfigStatus {
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
        if let Err(e) =
            std::fs::create_dir_all(&claude_link).and_then(|_| std::fs::write(&dest, SKILL_MD))
        {
            return ConfigStatus::Error(e.to_string());
        }
    }

    update_skill_lock(true);
    ConfigStatus::Updated(skill_file)
}

// ── Uninstall helpers ────────────────────────────────────────────────────────

pub(super) fn remove_skill() -> ConfigStatus {
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
