//! `innate install` — interactive setup wizard (clack-style TUI).
//!
//! No extra dependencies — uses only what Innate already pulls in.
//! Configures Claude Code, Codex CLI, and opencode to use innate's MCP server.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::{json, Value};

const SKILL_MD: &str = include_str!("../../assets/SKILL.md");

mod agents;
mod path;
mod settings;
mod skills;
mod ui;
mod uninstall;
mod wizard;

pub use uninstall::run_uninstall;
pub use wizard::run_install;

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

/// Read a JSON config file whose root must be an object.
/// A missing file yields an empty object; an unreadable, unparseable, or
/// non-object file yields `Err` so an existing user config is never
/// silently replaced by a rewrite.
fn read_json_object(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        Ok(txt) => match serde_json::from_str::<Value>(&txt) {
            Err(e) => Err(format!(
                "cannot parse {}: {e} — fix the file and re-run",
                path.display()
            )),
            Ok(v) if !v.is_object() => {
                Err(format!("{}: root is not a JSON object", path.display()))
            }
            Ok(v) => Ok(v),
        },
    }
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
