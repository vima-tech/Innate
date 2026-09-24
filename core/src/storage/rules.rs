//! Storage for the living-knowledge model (schema 5.0): episodes (experience
//! material) and rule_validations (show → observe → verdict).
//!
//! Rules themselves stay in `chunks`; this module only covers the two tables a
//! rule's life is judged by.

use rusqlite::{params, OptionalExtension};
use serde_json::Value;

use super::{row_to_json, Storage};
use crate::errors::Result;

/// One episode: a stretch of struggle (failed command → attempts → recovery) or
/// a user correction, captured deterministically from a transcript.
#[derive(Debug, Clone, Default)]
pub struct EpisodeRow {
    pub id: String,
    pub kind: String,
    pub session_id: Option<String>,
    pub project: Option<String>,
    pub agent: Option<String>,
    pub ts: String,
    /// JSON array of signals.
    pub signals: String,
    pub trigger_text: String,
    pub attempts: Option<String>,
    pub resolution: Option<String>,
}

/// One row of a rule's validation history.
#[derive(Debug, Clone, Default)]
pub struct ValidationRow {
    pub id: String,
    pub chunk_id: String,
    pub rule_version: i64,
    pub verdict: String,
    pub observation: Option<String>,
    pub source: String,
    pub channel: Option<String>,
    pub session_id: Option<String>,
    pub project: Option<String>,
    pub trace_id: Option<String>,
    pub tool_use_id: Option<String>,
    pub created_at: String,
}

impl Storage {
    /// Insert an episode unless one with the same deterministic id exists.
    /// Returns whether a row was written.
    pub fn insert_episode(&self, e: &EpisodeRow, now: &str) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO episodes
             (id, kind, session_id, project, agent, ts, signals, trigger_text,
              attempts, resolution, state, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'new',?11)",
            params![
                e.id,
                e.kind,
                e.session_id,
                e.project,
                e.agent,
                e.ts,
                e.signals,
                e.trigger_text,
                e.attempts,
                e.resolution,
                now
            ],
        )?;
        Ok(n > 0)
    }

    /// Episodes still waiting to become (part of) a rule, oldest first.
    pub fn open_episodes(&self, limit: i64) -> Result<Vec<Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, session_id, project, agent, ts, signals, trigger_text,
                    attempts, resolution, (embedding IS NOT NULL) AS has_embedding
             FROM episodes WHERE state='new' ORDER BY ts ASC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], row_to_json)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn set_episode_embedding(&self, id: &str, blob: &[u8]) -> Result<()> {
        self.conn.execute(
            "UPDATE episodes SET embedding=?1 WHERE id=?2",
            params![blob, id],
        )?;
        Ok(())
    }

    pub fn episode_embedding(&self, id: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row("SELECT embedding FROM episodes WHERE id=?1", [id], |r| {
                r.get::<_, Option<Vec<u8>>>(0)
            })
            .optional()?
            .flatten())
    }

    /// Mark episodes as consumed: `ruled` (with the rule they produced) or
    /// `no_rule` (the writer found nothing transferable in them).
    pub fn mark_episodes(&self, ids: &[String], state: &str, rule_id: Option<&str>) -> Result<()> {
        for id in ids {
            self.conn.execute(
                "UPDATE episodes SET state=?1, rule_id=?2 WHERE id=?3",
                params![state, rule_id, id],
            )?;
        }
        Ok(())
    }

    /// Episodes that never recurred within the window stop being cluster
    /// candidates. They are kept as material, not deleted.
    pub fn expire_episodes_before(&self, cutoff: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE episodes SET state='expired' WHERE state='new' AND ts < ?1",
            [cutoff],
        )?)
    }

    pub fn insert_validation(&self, v: &ValidationRow) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO rule_validations
             (id, chunk_id, rule_version, verdict, observation, source, channel,
              session_id, project, trace_id, tool_use_id, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                v.id,
                v.chunk_id,
                v.rule_version,
                v.verdict,
                v.observation,
                v.source,
                v.channel,
                v.session_id,
                v.project,
                v.trace_id,
                v.tool_use_id,
                v.created_at
            ],
        )?;
        Ok(())
    }

    /// Whether `chunk_id` was already shown in `session_id` (action-time recall
    /// shows a rule at most once per session).
    pub fn shown_in_session(&self, chunk_id: &str, session_id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM rule_validations
             WHERE chunk_id=?1 AND session_id=?2 AND source='hook'",
            params![chunk_id, session_id],
            |r| r.get::<_, i64>(0),
        )? > 0)
    }

    /// `shown` rows of a session that still wait for their observation.
    pub fn shown_awaiting_observation(&self, session_id: &str) -> Result<Vec<Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, chunk_id, channel, tool_use_id, created_at
             FROM rule_validations
             WHERE session_id=?1 AND verdict='shown' ORDER BY created_at",
        )?;
        let rows = stmt.query_map([session_id], row_to_json)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn set_observation(&self, id: &str, observation: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE rule_validations SET verdict='observed', observation=?1
             WHERE id=?2 AND verdict='shown'",
            params![observation, id],
        )?;
        Ok(())
    }

    /// Observations waiting for the offline judge, oldest first.
    pub fn observations_to_judge(&self, limit: i64) -> Result<Vec<Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT v.id, v.chunk_id, v.observation, v.channel, c.content, c.signals
             FROM rule_validations v JOIN chunks c ON c.id = v.chunk_id
             WHERE v.verdict='observed' AND c.state != 'archived'
             ORDER BY v.created_at LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], row_to_json)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn set_judged_verdict(&self, id: &str, verdict: &str, now: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE rule_validations SET verdict=?1, source='judge', judged_at=?2 WHERE id=?3",
            params![verdict, now, id],
        )?;
        Ok(())
    }

    /// `shown` rows whose observation never arrived (the session ended without
    /// a Stop hook reading it) become `unknown` after `cutoff`.
    pub fn expire_unobserved_before(&self, cutoff: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE rule_validations SET verdict='unknown'
             WHERE verdict='shown' AND created_at < ?1",
            [cutoff],
        )?)
    }

    /// Support rows that count toward maturity for the rule's current version:
    /// after the rule was born, from sessions that did not give birth to it.
    /// Returns `(session_or_trace, project)` pairs, one per independent session.
    pub fn independent_supports(
        &self,
        chunk_id: &str,
        version: i64,
    ) -> Result<Vec<(String, Option<String>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(v.session_id, v.trace_id, v.id) AS unit, MAX(v.project) AS project
             FROM rule_validations v JOIN chunks c ON c.id = v.chunk_id
             WHERE v.chunk_id=?1 AND v.rule_version=?2 AND v.verdict='supported'
               AND v.created_at > c.created_at
               AND (v.session_id IS NULL OR v.session_id NOT IN (
                     SELECT session_id FROM episodes
                     WHERE rule_id=?1 AND session_id IS NOT NULL))
             GROUP BY unit",
        )?;
        let rows = stmt.query_map(params![chunk_id, version], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Unresolved counterexamples for a rule, newest first.
    pub fn open_contradictions(&self, chunk_id: &str) -> Result<Vec<Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, observation, session_id, project, created_at FROM rule_validations
             WHERE chunk_id=?1 AND verdict='contradicted' AND resolved_at IS NULL
             ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([chunk_id], row_to_json)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Rules (not archived) carrying at least one unresolved counterexample.
    pub fn rules_with_open_contradictions(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT v.chunk_id FROM rule_validations v
             JOIN chunks c ON c.id = v.chunk_id
             WHERE v.verdict='contradicted' AND v.resolved_at IS NULL
               AND c.state != 'archived'",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn resolve_contradictions(&self, chunk_id: &str, now: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE rule_validations SET resolved_at=?1
             WHERE chunk_id=?2 AND verdict='contradicted' AND resolved_at IS NULL",
            params![now, chunk_id],
        )?;
        Ok(())
    }

    /// Rules (active or pending, non-spark) with signals, for action-time
    /// matching. Small by construction: signals are short strings.
    pub fn rule_signals(&self) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, state, signals FROM chunks
             WHERE state IN ('active','pending') AND origin != 'spark'
               AND signals IS NOT NULL AND signals != '[]'",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Active rules still lacking signals (born before 5.0), oldest first.
    pub fn rules_missing_signals(&self, limit: i64) -> Result<Vec<Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, trigger_desc FROM chunks
             WHERE state='active' AND origin != 'spark' AND signals IS NULL
             ORDER BY created_at LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], row_to_json)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn set_chunk_signals(&self, id: &str, signals_json: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE chunks SET signals=?1 WHERE id=?2",
            params![signals_json, id],
        )?;
        Ok(())
    }

    /// Counts for `inspect`: episodes by state and validations by verdict in
    /// the window.
    pub fn rule_loop_counts(&self, since: &str) -> Result<Value> {
        let mut episodes = serde_json::Map::new();
        let mut stmt = self
            .conn
            .prepare("SELECT state, COUNT(*) FROM episodes GROUP BY state")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (k, n) = row?;
            episodes.insert(k, Value::from(n));
        }
        let mut verdicts = serde_json::Map::new();
        let mut stmt = self.conn.prepare(
            "SELECT verdict, COUNT(*) FROM rule_validations WHERE created_at >= ?1 GROUP BY verdict",
        )?;
        for row in stmt.query_map([since], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })? {
            let (k, n) = row?;
            verdicts.insert(k, Value::from(n));
        }
        Ok(serde_json::json!({ "episodes": episodes, "validations": verdicts }))
    }
}
