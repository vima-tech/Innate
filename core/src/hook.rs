#[derive(Subcommand)]
pub enum HookCommands {
    /// Process a Claude Code Stop hook payload from stdin and record session events
    Stop,
}
fn extract_content_text(content: Option<&serde_json::Value>) -> String {
    match content {
        None => String::new(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn run_hook_stop() -> anyhow::Result<()> {
    use std::io::{Read, Write};

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;

    let data: serde_json::Value = serde_json::from_str(&input).unwrap_or(serde_json::Value::Null);

    let empty = vec![];
    let transcript = data
        .get("transcript")
        .or_else(|| data.get("messages"))
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);

    let mut query = String::new();
    let mut summary = String::new();

    for m in transcript.iter().rev() {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if query.is_empty() && role == "user" {
            query = extract_content_text(m.get("content"))
                .chars()
                .take(200)
                .collect();
        }
        if summary.is_empty() && role == "assistant" {
            summary = extract_content_text(m.get("content"))
                .chars()
                .take(400)
                .collect();
        }
        if !query.is_empty() && !summary.is_empty() {
            break;
        }
    }

    let mut events: Vec<serde_json::Value> = Vec::new();
    if !query.is_empty() {
        events.push(json!({"event_type": "session_start", "query": query.trim()}));
    }
    if !summary.is_empty() {
        events.push(json!({"event_type": "tool_success", "output_summary": summary.trim(), "outcome": "ok"}));
    }
    events.push(json!({"event_type": "session_end"}));

    let log_path = dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".innate")
        .join("sessions")
        .join("session.log");

    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    for event in &events {
        writeln!(file, "{}", serde_json::to_string(event)?)?;
    }

    Ok(())
}

pub(crate) fn run_command(action: &HookCommands) -> anyhow::Result<()> {
    match action {
        HookCommands::Stop => run_hook_stop(),
    }
}
use std::path::PathBuf;

use clap::Subcommand;
use serde_json::json;
