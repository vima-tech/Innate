//! The living-knowledge rule loop (schema 5.0).
//!
//! ```text
//! transcript ──capture──▶ episodes ──recurrence──▶ candidate rule (pending)
//!                                                     │ shown (prompt / pre-tool / tool-error)
//!                                                     ▼
//!                          observation ──judge / agent verdict──▶ supported │ contradicted
//!                                                     │                        │
//!               ≥2 independent supports, ≥1 other project, no open counterexample
//!                                                     ▼                        ▼
//!                                              mature (active)      suspend → revise / retire
//! ```
//!
//! Rules stay in `chunks`; episodes and validations have their own tables
//! (`storage/rules.rs`). Design: docs/Innate-活知识库重构-整体改进方案-v1.md.

use super::*;
use crate::storage::{EpisodeRow, ValidationRow};

mod evolve;
mod maturity;

/// Verdicts an agent may report for a rule it saw.
const AGENT_VERDICTS: &[&str] = &["applied", "supported", "contradicted", "irrelevant"];

/// A verdict on one rule, as reported by the agent through `record`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct RuleVerdict {
    pub chunk_id: String,
    /// `applied` | `supported` | `contradicted` | `irrelevant`
    pub verdict: String,
    /// What was observed. Required for `supported` and `contradicted`: a verdict
    /// without an observation is an opinion, not evidence.
    #[serde(default)]
    pub observation: Option<String>,
}

/// A verdict `record` did not store, and why.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RejectedVerdict {
    pub chunk_id: String,
    pub reason: String,
}

/// What one Stop-hook capture wrote.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct CaptureReport {
    pub episodes: usize,
    pub observations: usize,
}

/// How many following tool calls make up an observation, per channel.
fn observation_calls(channel: &str) -> usize {
    match channel {
        "pre_tool" => 3,
        "tool_error" => 4,
        _ => 6,
    }
}

impl KnowledgeBase {
    /// Store the agent's verdicts on rules for `trace_id`. Invalid entries are
    /// returned, never fatal — same contract as `record`'s attribution filter.
    pub(crate) fn record_verdicts(
        &self,
        trace_id: &str,
        verdicts: &[RuleVerdict],
        session_hint: Option<&str>,
        project_hint: Option<&str>,
    ) -> Result<Vec<RejectedVerdict>> {
        let log = self.storage.get_episodic_log(trace_id)?;
        let from_log = |key: &str| {
            log.as_ref()
                .and_then(|l| l.get(key))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let session = session_hint
            .map(str::to_string)
            .or_else(|| from_log("session_id"));
        let project = project_hint
            .map(str::to_string)
            .or_else(|| from_log("project"))
            .or_else(crate::project::current_project);
        let now = utc_now_iso();
        let mut rejected = Vec::new();
        for v in verdicts {
            let reject = |reason: &str| RejectedVerdict {
                chunk_id: v.chunk_id.clone(),
                reason: reason.to_string(),
            };
            if !AGENT_VERDICTS.contains(&v.verdict.as_str()) {
                rejected.push(reject("invalid_verdict"));
                continue;
            }
            let observation = v
                .observation
                .as_deref()
                .map(|o| crate::utils::redact_secrets(o.trim()).0)
                .filter(|o| !o.is_empty());
            if matches!(v.verdict.as_str(), "supported" | "contradicted") && observation.is_none() {
                rejected.push(reject("observation_required"));
                continue;
            }
            let Some(chunk) = self.storage.get_chunk(&v.chunk_id)? else {
                rejected.push(reject("unknown_chunk"));
                continue;
            };
            if chunk.get("origin").and_then(Value::as_str) == Some("spark") {
                rejected.push(reject("spark_not_a_rule"));
                continue;
            }
            self.storage.insert_validation(&ValidationRow {
                id: gen_uuid(),
                chunk_id: v.chunk_id.clone(),
                rule_version: chunk.get("version").and_then(Value::as_i64).unwrap_or(1),
                verdict: v.verdict.clone(),
                observation,
                source: "agent".to_string(),
                session_id: session.clone(),
                project: project.clone(),
                trace_id: Some(trace_id.to_string()),
                created_at: now.clone(),
                ..Default::default()
            })?;
        }
        Ok(rejected)
    }

    /// A hook placed `chunk_id` in the model's context. The Stop hook later
    /// attaches what happened next, and the judge reads it.
    pub fn mark_shown(
        &self,
        chunk_id: &str,
        channel: &str,
        session_id: Option<&str>,
        project: Option<&str>,
        tool_use_id: Option<&str>,
    ) -> Result<()> {
        let version = self
            .storage
            .get_chunk(chunk_id)?
            .and_then(|c| c.get("version").and_then(Value::as_i64))
            .unwrap_or(1);
        self.storage.insert_validation(&ValidationRow {
            id: gen_uuid(),
            chunk_id: chunk_id.to_string(),
            rule_version: version,
            verdict: "shown".to_string(),
            source: "hook".to_string(),
            channel: Some(channel.to_string()),
            session_id: session_id.map(str::to_string),
            project: project.map(str::to_string),
            tool_use_id: tool_use_id.map(str::to_string),
            created_at: utc_now_iso(),
            ..Default::default()
        })
    }

    /// Stop hook: persist this session's episodes and attach observations to
    /// the rules shown in it. Idempotent — the Stop hook sees the whole
    /// transcript every turn; episode ids are deterministic and an observation
    /// is attached only to rows still in `shown`.
    pub fn capture_session(&self, transcript: &str) -> Result<CaptureReport> {
        let t = crate::episodes::parse(transcript);
        let project = t
            .cwd
            .as_deref()
            .map(std::path::Path::new)
            .and_then(crate::project::project_of);
        let agent = crate::utils::agent_source();
        let now = utc_now_iso();
        let mut report = CaptureReport::default();
        let episodes = crate::episodes::struggles(&t)
            .into_iter()
            .chain(crate::episodes::corrections(&t));
        for e in episodes {
            let row = EpisodeRow {
                id: e.id,
                kind: e.kind.to_string(),
                session_id: t.session_id.clone(),
                project: project.clone(),
                agent: agent.clone(),
                ts: e.ts.unwrap_or_else(|| now.clone()),
                signals: serde_json::to_string(&e.signals).unwrap_or_else(|_| "[]".into()),
                trigger_text: e.trigger,
                attempts: e.attempts,
                resolution: e.resolution,
            };
            if self.storage.insert_episode(&row, &now)? {
                report.episodes += 1;
            }
        }
        if let Some(session) = t.session_id.as_deref() {
            for shown in self.storage.shown_awaiting_observation(session)? {
                let channel = shown
                    .get("channel")
                    .and_then(Value::as_str)
                    .unwrap_or("prompt");
                let tool_use_id = shown.get("tool_use_id").and_then(Value::as_str);
                let created_at = shown.get("created_at").and_then(Value::as_str);
                let obs = crate::episodes::observation_after(
                    &t,
                    tool_use_id,
                    created_at,
                    observation_calls(channel),
                );
                if let (Some(obs), Some(id)) = (obs, shown.get("id").and_then(Value::as_str)) {
                    self.storage.set_observation(id, &obs)?;
                    report.observations += 1;
                }
            }
        }
        Ok(report)
    }
}
