#[derive(Subcommand)]
pub enum HookCommands {
    /// Process a Claude Code Stop hook payload from stdin and record session events
    Stop,
    /// UserPromptSubmit hook: recall relevant knowledge for the prompt and print it to
    /// stdout (injected into context). Relevance-gated so it stays quiet when nothing fits.
    Prompt,
    /// SessionStart hook: warm up context with high-relevance project knowledge.
    SessionStart,
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

    // The real Stop/SubagentStop payload carries `transcript_path` (a .jsonl file, one message
    // per line nested under "message"), NOT an inline transcript array. Read it to recover the
    // user query and to detect whether innate_recall was actually used this session.
    let transcript_text = data
        .get("transcript_path")
        .and_then(|v| v.as_str())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();

    // Only treat this as a knowledge-using session if innate_recall was actually *invoked*.
    // A bare substring match would false-positive on the tool's name in system-reminder tool
    // listings, so require a tool_use block that names it (transcript lines, or inline payload).
    let recall_used = transcript_text
        .lines()
        .any(|l| l.contains("tool_use") && l.contains("innate_recall"))
        || (input.contains("tool_use") && input.contains("innate_recall"));

    // Summary: the payload hands us the last assistant message directly — prefer it.
    let mut summary: String = data
        .get("last_assistant_message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .chars()
        .take(400)
        .collect();
    let mut query = String::new();

    // Newest-first scan of the transcript file for the user query (and assistant fallback).
    for line in transcript_text.lines().rev() {
        if !query.is_empty() && !summary.is_empty() {
            break;
        }
        let Ok(m) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let role = m
            .pointer("/message/role")
            .and_then(|r| r.as_str())
            .unwrap_or("");
        let content = m.pointer("/message/content");
        if query.is_empty() && role == "user" {
            let q = extract_content_text(content);
            if !q.trim().is_empty() {
                query = q.chars().take(200).collect();
            }
        }
        if summary.is_empty() && role == "assistant" {
            summary = extract_content_text(content).chars().take(400).collect();
        }
    }

    // Backward-compat: older payloads and tests pass an inline transcript/messages array.
    if query.is_empty() || summary.is_empty() {
        let empty = vec![];
        let transcript = data
            .get("transcript")
            .or_else(|| data.get("messages"))
            .and_then(|v| v.as_array())
            .unwrap_or(&empty);
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
    }

    let mut events: Vec<serde_json::Value> = Vec::new();
    if !query.is_empty() {
        events.push(json!({"event_type": "session_start", "query": query.trim()}));
    }
    // outcome=unknown (not ok): the Stop hook cannot know which chunks were actually used or
    // whether they helped. Leave the authoritative ok/fail + per-chunk feedback to the agent's
    // explicit innate_record call; this coarse signal must not inflate confidence on its own.
    if !summary.is_empty() && recall_used {
        events.push(json!({"event_type": "tool_success", "output_summary": summary.trim(), "outcome": "unknown"}));
    }
    events.push(json!({"event_type": "session_end"}));

    let log_path = crate::paths::session_log_path();

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

pub(crate) fn run_command(action: &HookCommands, db_path: &Path) -> anyhow::Result<()> {
    match action {
        HookCommands::Stop => run_hook_stop(),
        // Recall hooks are auxiliary and must never break the session: on any error we
        // swallow it and exit cleanly so the harness keeps going.
        HookCommands::Prompt => {
            let _ = run_hook_recall(db_path, HookKind::Prompt);
            Ok(())
        }
        HookCommands::SessionStart => {
            let _ = run_hook_recall(db_path, HookKind::SessionStart);
            Ok(())
        }
    }
}

#[derive(Clone, Copy)]
enum HookKind {
    Prompt,
    SessionStart,
}

/// Default relevance gate for always-on recall hooks. Relevance scores roughly span [0, ~1.05]
/// (weights: content .55 + trigger .25 + confidence .10 + context .15). 0.40 keeps strong
/// semantic matches and drops weak ones. The gate runs before the pending lifecycle penalty;
/// final ranking still applies that penalty. Override with `INNATE_HOOK_MIN_SCORE`.
const DEFAULT_HOOK_MIN_SCORE: f64 = 0.40;

/// UserPromptSubmit / SessionStart hook: recall relevant knowledge and print it to stdout so
/// Claude Code injects it into the conversation. Relevance-gated so it stays silent when nothing
/// fits — high frequency without noise.
fn run_hook_recall(db_path: &Path, kind: HookKind) -> anyhow::Result<()> {
    use std::io::Read;

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let data: serde_json::Value = serde_json::from_str(&input).unwrap_or(serde_json::Value::Null);

    // Derive the recall query. UserPromptSubmit carries the user's prompt; SessionStart has no
    // query, so warm up from the project directory name as a coarse canonical project intent.
    let query: String = match kind {
        HookKind::Prompt => data
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .chars()
            .take(500)
            .collect(),
        HookKind::SessionStart => {
            let cwd = data
                .get("cwd")
                .and_then(|v| v.as_str())
                .or_else(|| data.get("workspace").and_then(|v| v.as_str()))
                .unwrap_or("");
            std::path::Path::new(cwd)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string()
        }
    };
    if query.trim().is_empty() {
        return Ok(());
    }

    let min_score = std::env::var("INNATE_HOOK_MIN_SCORE")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(DEFAULT_HOOK_MIN_SCORE);

    let kb = crate::open_kb(db_path)?;
    let result = kb.recall(RecallParams {
        query: &query,
        budget: 4000,
        trace: true,
        include_sparks: false,
        top: Some(5),
        source: "hook",
        expand_deps: "false",
        allow_trim: false,
        refine_mode: "off",
        min_score: Some(min_score),
        session_only: false,
        rerank: false,
    })?;

    if result.knowledge.is_empty() {
        return Ok(());
    }

    // Stdout becomes context. Be explicit that these are recalled chunks and that the agent
    // must cite the IDs it actually uses in innate_record — this is what keeps feedback precise.
    let mut out = String::new();
    out.push_str("<innate-recall>\n");
    out.push_str(&format!(
        "Innate recalled {} relevant knowledge chunk(s). Apply what helps; \
         when you finish, call innate_record(trace_id, outcome, used=[ids you actually applied], \
         feedback_up/down=[ids that helped/misled]).\n\n",
        result.knowledge.len()
    ));
    for c in &result.knowledge {
        let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let content = c.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let conf = c.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.0);
        out.push_str(&format!("- [{id}] (confidence {conf:.2}) {content}\n"));
    }
    out.push_str(&format!("\ntrace_id: {}\n", result.trace_id));
    out.push_str("</innate-recall>");
    println!("{out}");

    Ok(())
}

use crate::kb::RecallParams;
use clap::Subcommand;
use serde_json::json;
use std::path::Path;
