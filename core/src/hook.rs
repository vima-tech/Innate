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

/// Scan `text` for the 36-character UUIDs that follow each occurrence of
/// `marker`, in order and de-duplicated.
///
/// Deliberately hand-rolled: the crate has no `regex` dependency, and the
/// shapes we look for are fixed-width.
pub(crate) fn uuids_after(text: &str, marker: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tail in text.split(marker).skip(1) {
        let candidate: String = tail.chars().take(36).collect();
        if is_uuid(&candidate) && !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    b.iter().enumerate().all(|(i, c)| {
        if matches!(i, 8 | 13 | 18 | 23) {
            *c == b'-'
        } else {
            c.is_ascii_hexdigit()
        }
    })
}

/// Decoded message text from a Claude Code transcript (.jsonl, one message per
/// line), optionally restricted to one role.
///
/// Decoding matters: on disk a message body is a JSON string, so a newline in
/// the text is the two characters `\\n`, not a line break. Scanning the raw file
/// for a marker that spans a newline therefore never matches — which is exactly
/// how the "did the agent quote this chunk id" check silently found nothing.
pub(crate) fn role_text(transcript: &str, role: Option<&str>) -> String {
    let mut out = String::new();
    for line in transcript.lines() {
        let Ok(m) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(role) = role {
            if m.pointer("/message/role").and_then(|r| r.as_str()) != Some(role) {
                continue;
            }
        }
        out.push_str(&extract_content_text(m.pointer("/message/content")));
        out.push('\n');
    }
    out
}

/// Close every trace this session's recall hooks opened.
///
/// Without this, a hook recall was a dead end: it wrote `selected` rows and an
/// episodic log, and then nothing ever closed the trace. 47.5% of them expired
/// as `timed_out` and 98.5% of all selections produced no feedback at all,
/// because the only closing path was the agent voluntarily calling
/// `innate_record` over MCP.
///
/// The transcript is its own session state — `run_hook_recall` prints
/// `trace_id: <uuid>` into the context it injects — so no side table is needed.
///
/// Two deliberate restraints:
///   * `outcome` stays `None`. The Stop hook cannot know whether the task
///     succeeded, and a guessed outcome would move confidence. Completing the
///     lifecycle is enough to make the log distillable (`record` gates
///     distillation on the session having *finished with material*, not on a
///     confidence-bearing outcome).
///   * usage is claimed only for chunk ids the assistant actually wrote in its
///     own output, and only as `cited` (the middle attribution strength), with
///     `used_complete=false` so it merges with — never overwrites — whatever
///     the agent recorded explicitly.
fn close_session_traces(db_path: &Path, transcript: &str, summary: &str) -> anyhow::Result<usize> {
    let trace_ids = uuids_after(transcript, "trace_id: ");
    if trace_ids.is_empty() {
        return Ok(0);
    }
    // Chunk ids the hook offered this session, as printed by `run_hook_recall`
    // (`- [<uuid>] (confidence …`). Read from decoded message text, not the raw
    // file — see `role_text`.
    let offered = uuids_after(&role_text(transcript, None), "- [");
    let assistant = role_text(transcript, Some("assistant"));
    let cited: Vec<String> = offered
        .into_iter()
        .filter(|id| assistant.contains(id.as_str()))
        .collect();

    let kb = crate::open_kb(db_path)?;
    let summary = summary.trim();
    let last = trace_ids.len().saturating_sub(1);
    let mut closed = 0usize;
    for (i, trace_id) in trace_ids.iter().enumerate() {
        // Only touch traces this library actually knows about.
        if kb
            .storage
            .get_episodic_log(trace_id)
            .ok()
            .flatten()
            .is_none()
        {
            continue;
        }
        // The summary describes the end of the session, so it is attached to the
        // session's last trace only. Giving the same text to all of a session's
        // traces would hand the distiller N copies of one session and inflate an
        // already-saturated pending queue; the earlier traces close as completed
        // without material, which is what actually happened.
        let material = (i == last && !summary.is_empty()).then_some(summary);
        let result = kb.record(RecordParams {
            trace_id,
            output_summary: material,
            used: (!cited.is_empty()).then_some(cited.as_slice()),
            used_attribution: "cited",
            used_complete: Some(false),
            task_state: Some("completed"),
            source: "hook",
            ..Default::default()
        });
        if result.is_ok() {
            closed += 1;
        }
    }
    Ok(closed)
}

fn run_hook_stop(db_path: &Path) -> anyhow::Result<()> {
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

    // Best-effort: a Stop hook must never fail the session, and the session.log
    // events above (which trigger evolve) have already been written.
    let _ = close_session_traces(db_path, &transcript_text, &summary);

    Ok(())
}

pub(crate) fn run_command(action: &HookCommands, db_path: &Path) -> anyhow::Result<()> {
    match action {
        HookCommands::Stop => run_hook_stop(db_path),
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

/// Network budget for a hook's query embedding, in milliseconds.
///
/// Measured p50 for the call is ~2 s and the observed worst case was 19 s
/// across retries — paid in front of the user, on every prompt. One attempt
/// with this ceiling bounds the damage; past it the hook falls back to the
/// local lexical channel.
const HOOK_EMBED_TIMEOUT_MS: u64 = 2500;

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

    // A recall hook runs in front of the user on every prompt, so it gets a
    // tight network budget instead of the background defaults (3 × 30 s). The
    // caller can still override either value explicitly.
    if std::env::var_os("INNATE_HTTP_TIMEOUT_MS").is_none() {
        std::env::set_var("INNATE_HTTP_TIMEOUT_MS", HOOK_EMBED_TIMEOUT_MS.to_string());
    }
    if std::env::var_os("INNATE_HTTP_MAX_ATTEMPTS").is_none() {
        std::env::set_var("INNATE_HTTP_MAX_ATTEMPTS", "1");
    }

    let kb = crate::open_kb(db_path)?;
    // SessionStart has no user prompt — its "query" is the project directory
    // name. There is nothing for an embedding to add to a bare folder name, so
    // it skips the remote call outright.
    let lexical_only = matches!(kind, HookKind::SessionStart);
    let recall = |lexical_only: bool| {
        kb.recall(RecallParams {
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
            lexical_only,
        })
    };
    let result = match recall(lexical_only) {
        Ok(result) => result,
        // Degrade, don't disappear: if the embedding endpoint is slow or down,
        // the lexical/BM25 channel still answers, locally and instantly.
        Err(crate::errors::InnateError::EmbeddingUnavailable(_)) if !lexical_only => {
            recall(true)?
        }
        Err(e) => return Err(e.into()),
    };

    if result.knowledge.is_empty() {
        return Ok(());
    }

    // Stdout becomes context. Be explicit that these are recalled chunks and that the agent
    // must cite the IDs it actually uses in innate_record — this is what keeps feedback precise.
    let mut out = String::new();
    out.push_str("<innate-recall>\n");
    // The record instruction spells out the failure case explicitly. Left
    // implicit, agents record only successes: the live library held 412
    // `task_ok` events and zero `task_fail`, which made `task_success_rate` a
    // constant 1.0 and starved every negative-evidence rule (decay,
    // sustained_task_failure archiving) of input.
    out.push_str(&format!(
        "Innate recalled {} relevant knowledge chunk(s). Apply what helps; \
         when you finish, call innate_record(trace_id, outcome, used=[ids you actually applied], \
         feedback_up/down=[ids that helped/misled]). Record outcome=\"fail\" when the task did \
         not succeed, and feedback_down for any chunk that was wrong or misleading — negative \
         results are as valuable as positive ones and are the only way bad knowledge decays.\n\n",
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

use crate::kb::{RecallParams, RecordParams};
use clap::Subcommand;
use serde_json::json;
use std::path::Path;
