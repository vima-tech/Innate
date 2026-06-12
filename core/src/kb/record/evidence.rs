use super::super::*;

impl KnowledgeBase {
    pub(super) fn validate_trace_attribution(
        &self,
        trace_id: &str,
        chunk_ids: Option<&[String]>,
        field: &str,
    ) -> Result<()> {
        let Some(chunk_ids) = chunk_ids else {
            return Ok(());
        };
        if chunk_ids.is_empty() {
            return Ok(());
        }

        let log = self.storage.get_episodic_log(trace_id)?.ok_or_else(|| {
            InnateError::InvalidState(format!(
                "{field} requires a trace created by recall: {trace_id}"
            ))
        })?;
        let mut attributable = HashSet::new();
        if let Some(raw) = log.get("recall_snapshot").and_then(Value::as_str) {
            if let Ok(snapshot) = serde_json::from_str::<Value>(raw) {
                if let Some(ids) = snapshot.get("selected").and_then(Value::as_array) {
                    attributable.extend(ids.iter().filter_map(Value::as_str).map(str::to_string));
                }
            }
        }
        let rows = self.storage.query_chunks_params(
            "SELECT DISTINCT chunk_id FROM usage_trace
             WHERE trace_id=? AND chunk_id IS NOT NULL
               AND event='selected'",
            rusqlite::params![trace_id],
        )?;
        attributable.extend(rows.iter().filter_map(|row| {
            row.get("chunk_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        }));

        for chunk_id in chunk_ids {
            if self.storage.get_chunk(chunk_id)?.is_none() || !attributable.contains(chunk_id) {
                return Err(InnateError::InvalidState(format!(
                    "{field} chunk {chunk_id} was not attributable to trace {trace_id}"
                )));
            }
        }
        Ok(())
    }

    pub(super) fn replace_selected_unused_evidence(
        &self,
        trace_id: &str,
        used_ids: &[String],
        now: &str,
    ) -> Result<()> {
        let old_rows = self.storage.query_chunks_params(
            "SELECT DISTINCT chunk_id FROM confidence_evidence
             WHERE trace_id=? AND kind='selected_unused'",
            rusqlite::params![trace_id],
        )?;
        let mut affected: HashSet<String> = old_rows
            .iter()
            .filter_map(|row| {
                row.get("chunk_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        self.storage
            .delete_trace_confidence_evidence(trace_id, &["selected_unused"])?;

        let used_set: HashSet<&str> = used_ids.iter().map(String::as_str).collect();
        let context_key = self.storage.get_episodic_log(trace_id)?.and_then(|log| {
            log.get("context_key")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        let selected_rows = self.storage.query_chunks_params(
            "SELECT chunk_id FROM usage_trace
             WHERE trace_id=? AND event='selected' AND chunk_id IS NOT NULL",
            rusqlite::params![trace_id],
        )?;
        for row in selected_rows {
            if let Some(chunk_id) = row.get("chunk_id").and_then(Value::as_str) {
                if !used_set.contains(chunk_id) {
                    self.upsert_trace_confidence_evidence(
                        trace_id,
                        chunk_id,
                        "selected_unused",
                        0.0,
                        0.08,
                        "selected_unused",
                        context_key.as_deref(),
                        now,
                        false,
                    )?;
                    affected.insert(chunk_id.to_string());
                }
            }
        }
        for chunk_id in affected {
            self.recompute_chunk_confidence(&chunk_id, now)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn upsert_trace_confidence_evidence(
        &self,
        trace_id: &str,
        chunk_id: &str,
        kind: &str,
        target: f64,
        strength: f64,
        reason: &str,
        context_key: Option<&str>,
        now: &str,
        explicit: bool,
    ) -> Result<()> {
        let chunk = match self.storage.get_chunk(chunk_id)? {
            Some(chunk) => chunk,
            None => return Ok(()),
        };
        if chunk.get("origin").and_then(Value::as_str) == Some("spark") {
            return Ok(());
        }
        let recency_weight = if explicit {
            const KAPPA: f64 = 0.5;
            const WINDOW_DAYS: f64 = 14.0;
            let gap_days = chunk
                .get("last_used_at")
                .and_then(Value::as_str)
                .map(|ts| iso_days_diff(now, ts) as f64)
                .unwrap_or(0.0);
            (1.0 + KAPPA * (-(gap_days / WINDOW_DAYS) * std::f64::consts::LN_2).exp()).min(1.5)
        } else {
            1.0
        };
        let alpha = (0.2 * strength * recency_weight).clamp(0.0, 1.0);
        self.storage.upsert_confidence_evidence(
            &gen_uuid(),
            Some(trace_id),
            chunk_id,
            kind,
            target,
            alpha,
            reason,
            context_key,
            now,
        )?;
        self.recompute_chunk_confidence(chunk_id, now)
    }

    pub(in crate::kb) fn recompute_chunk_confidence(
        &self,
        chunk_id: &str,
        now: &str,
    ) -> Result<()> {
        let Some(chunk) = self.storage.get_chunk(chunk_id)? else {
            return Ok(());
        };
        let mut confidence = chunk
            .get("confidence_base")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| {
                chunk
                    .get("confidence")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.5)
            });
        let mut reason = chunk
            .get("confidence_reason")
            .and_then(Value::as_str)
            .unwrap_or("base")
            .to_string();
        for evidence in self.storage.confidence_evidence_for_chunk(chunk_id)? {
            let target = evidence
                .get("target")
                .and_then(Value::as_f64)
                .unwrap_or(0.5);
            let alpha = evidence
                .get("alpha")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            confidence = (confidence + alpha * (target - confidence)).clamp(0.0, 1.0);
            reason = evidence
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("evidence")
                .to_string();
        }
        self.storage.conn_execute(
            "UPDATE chunks SET confidence=?, confidence_reason=?, updated_at=? WHERE id=?",
            rusqlite::params![confidence, reason, now, chunk_id],
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn replace_outcome_evidence(
        &self,
        trace_id: &str,
        outcome: &str,
        used: Option<&[String]>,
        used_complete: bool,
        now: &str,
    ) -> Result<()> {
        let old_rows = self.storage.query_chunks_params(
            "SELECT DISTINCT chunk_id FROM confidence_evidence
             WHERE trace_id=? AND kind IN ('outcome_ok','outcome_fail','selected_unused')",
            rusqlite::params![trace_id],
        )?;
        let mut affected: HashSet<String> = old_rows
            .iter()
            .filter_map(|row| {
                row.get("chunk_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        self.storage.delete_trace_confidence_evidence(
            trace_id,
            &["outcome_ok", "outcome_fail", "selected_unused"],
        )?;

        let used_ids = used.unwrap_or_default();
        let used_set: HashSet<&str> = used_ids.iter().map(String::as_str).collect();
        let attribution_divisor = used_ids.len().max(1) as f64;
        let context_key = self.storage.get_episodic_log(trace_id)?.and_then(|log| {
            log.get("context_key")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        for chunk_id in used_ids {
            let attribution = self
                .storage
                .query_chunks_params(
                    "SELECT strength, attribution FROM usage_trace
                     WHERE trace_id=? AND chunk_id=? AND event='used'",
                    rusqlite::params![trace_id, chunk_id],
                )?
                .into_iter()
                .next();
            let base_strength = attribution
                .as_ref()
                .and_then(|row| row.get("strength"))
                .and_then(Value::as_f64)
                .unwrap_or(0.15)
                / attribution_divisor;
            let attribution_reason = attribution
                .as_ref()
                .and_then(|row| row.get("attribution"))
                .and_then(Value::as_str)
                .unwrap_or("inferred");
            let (kind, target, strength, reason) = if outcome == "ok" {
                ("outcome_ok", 1.0, base_strength, attribution_reason)
            } else {
                ("outcome_fail", 0.0, base_strength * 0.5, "task_fail")
            };
            self.upsert_trace_confidence_evidence(
                trace_id,
                chunk_id,
                kind,
                target,
                strength,
                reason,
                context_key.as_deref(),
                now,
                false,
            )?;
            affected.insert(chunk_id.clone());
        }

        if used_complete {
            let selected_rows = self.storage.query_chunks_params(
                "SELECT chunk_id FROM usage_trace
                 WHERE trace_id=? AND event='selected' AND chunk_id IS NOT NULL",
                rusqlite::params![trace_id],
            )?;
            for row in selected_rows {
                if let Some(chunk_id) = row.get("chunk_id").and_then(Value::as_str) {
                    if !used_set.contains(chunk_id) {
                        self.upsert_trace_confidence_evidence(
                            trace_id,
                            chunk_id,
                            "selected_unused",
                            0.0,
                            0.08,
                            "selected_unused",
                            context_key.as_deref(),
                            now,
                            false,
                        )?;
                        affected.insert(chunk_id.to_string());
                    }
                }
            }
        }

        for chunk_id in affected {
            self.recompute_chunk_confidence(&chunk_id, now)?;
        }
        Ok(())
    }

    pub(in crate::kb) fn rebuild_context_stats(&self, now: &str) -> Result<()> {
        self.storage
            .conn_execute("DELETE FROM chunk_context_stats", rusqlite::params![])?;
        self.storage.conn_execute(
            "INSERT INTO chunk_context_stats
             (chunk_id, context_key, success_count, failure_count,
              positive_feedback, negative_feedback, last_updated_at)
             SELECT chunk_id, context_key, success_count, failure_count,
                    positive_feedback, negative_feedback, ?
             FROM chunk_context_stats_base",
            rusqlite::params![now],
        )?;
        self.storage.conn_execute(
            "INSERT INTO chunk_context_stats
             (chunk_id, context_key, success_count, failure_count,
              positive_feedback, negative_feedback, last_updated_at)
             SELECT chunk_id, context_key,
                    SUM(CASE WHEN kind='outcome_ok' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN kind='outcome_fail' THEN 1 ELSE 0 END),
                    0, 0, ?
             FROM confidence_evidence
             WHERE context_key IS NOT NULL AND kind IN ('outcome_ok','outcome_fail')
             GROUP BY chunk_id, context_key
             ON CONFLICT(chunk_id, context_key) DO UPDATE SET
               success_count=success_count+excluded.success_count,
               failure_count=failure_count+excluded.failure_count,
               last_updated_at=excluded.last_updated_at",
            rusqlite::params![now],
        )?;
        self.storage.conn_execute(
            "INSERT INTO chunk_context_stats
             (chunk_id, context_key, success_count, failure_count,
              positive_feedback, negative_feedback, last_updated_at)
             SELECT fe.chunk_id, fe.context_key, 0, 0,
                    SUM(CASE WHEN fe.signal='up' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN fe.signal='down' THEN 1 ELSE 0 END), ?
             FROM feedback_events fe
             WHERE fe.context_key IS NOT NULL
               AND fe.ts > COALESCE((
                 SELECT c.evidence_cutoff_at FROM chunks c
                 WHERE c.id = fe.chunk_id
               ), '')
             GROUP BY fe.chunk_id, fe.context_key
             ON CONFLICT(chunk_id, context_key) DO UPDATE SET
               positive_feedback=positive_feedback+excluded.positive_feedback,
               negative_feedback=negative_feedback+excluded.negative_feedback,
               last_updated_at=excluded.last_updated_at",
            rusqlite::params![now],
        )
    }

    /// Targeted context_stats rebuild for only the specified chunk_ids.
    /// Used by record() to avoid a full-table rebuild on every call.
    /// curate() still calls the full rebuild_context_stats() for periodic accuracy.
    pub(super) fn rebuild_context_stats_for(
        &self,
        chunk_ids: &HashSet<String>,
        now: &str,
    ) -> Result<()> {
        if chunk_ids.is_empty() {
            return Ok(());
        }
        for chunk_id in chunk_ids {
            self.storage.conn_execute(
                "DELETE FROM chunk_context_stats WHERE chunk_id=?",
                rusqlite::params![chunk_id],
            )?;
            self.storage.conn_execute(
                "INSERT OR IGNORE INTO chunk_context_stats
                 (chunk_id, context_key, success_count, failure_count,
                  positive_feedback, negative_feedback, last_updated_at)
                 SELECT chunk_id, context_key, success_count, failure_count,
                        positive_feedback, negative_feedback, ?
                 FROM chunk_context_stats_base WHERE chunk_id=?",
                rusqlite::params![now, chunk_id],
            )?;
            self.storage.conn_execute(
                "INSERT INTO chunk_context_stats
                 (chunk_id, context_key, success_count, failure_count,
                  positive_feedback, negative_feedback, last_updated_at)
                 SELECT chunk_id, context_key,
                        SUM(CASE WHEN kind='outcome_ok' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN kind='outcome_fail' THEN 1 ELSE 0 END),
                        0, 0, ?
                 FROM confidence_evidence
                 WHERE context_key IS NOT NULL AND kind IN ('outcome_ok','outcome_fail')
                   AND chunk_id=?
                 GROUP BY chunk_id, context_key
                 ON CONFLICT(chunk_id, context_key) DO UPDATE SET
                   success_count=success_count+excluded.success_count,
                   failure_count=failure_count+excluded.failure_count,
                   last_updated_at=excluded.last_updated_at",
                rusqlite::params![now, chunk_id],
            )?;
            self.storage.conn_execute(
                "INSERT INTO chunk_context_stats
                 (chunk_id, context_key, success_count, failure_count,
                  positive_feedback, negative_feedback, last_updated_at)
                 SELECT chunk_id, context_key, 0, 0,
                        SUM(CASE WHEN signal='up' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN signal='down' THEN 1 ELSE 0 END), ?
                 FROM feedback_events
                 WHERE context_key IS NOT NULL AND chunk_id=?
                   AND ts > COALESCE((
                     SELECT evidence_cutoff_at FROM chunks
                     WHERE id=?
                   ), '')
                 GROUP BY chunk_id, context_key
                 ON CONFLICT(chunk_id, context_key) DO UPDATE SET
                   positive_feedback=positive_feedback+excluded.positive_feedback,
                   negative_feedback=negative_feedback+excluded.negative_feedback,
                   last_updated_at=excluded.last_updated_at",
                rusqlite::params![now, chunk_id, chunk_id],
            )?;
        }
        Ok(())
    }

    pub(in crate::kb) fn refresh_governance_evidence(
        &self,
        chunk_id: &str,
        now: &str,
    ) -> Result<()> {
        let rows = self.storage.query_chunks_params(
            "SELECT actor AS actor_key,
                    signal, strength, ts
             FROM feedback_events
             WHERE chunk_id=? AND actor IS NOT NULL AND actor!=''
               AND ts > COALESCE((
                 SELECT evidence_cutoff_at FROM chunks
                 WHERE id=?
               ), '')",
            rusqlite::params![chunk_id, chunk_id],
        )?;
        let mut actor_contributions: HashMap<String, f64> = HashMap::new();
        for row in rows {
            let actor = row
                .get("actor_key")
                .and_then(Value::as_str)
                .unwrap_or("anonymous")
                .to_string();
            let age_days = row
                .get("ts")
                .and_then(Value::as_str)
                .map(|ts| iso_days_diff(now, ts).max(0) as f64)
                .unwrap_or(0.0);
            let recency_weight = 0.5_f64.powf(age_days / 90.0);
            let strength = row.get("strength").and_then(Value::as_f64).unwrap_or(0.0);
            let signed = if row.get("signal").and_then(Value::as_str) == Some("down") {
                strength
            } else {
                -strength
            };
            *actor_contributions.entry(actor).or_default() += signed * recency_weight;
        }
        let mut score = 0.0_f64;
        let mut actor_count = 0_i64;
        for contribution in actor_contributions.values().copied() {
            let contribution = contribution.clamp(-1.0, 1.0);
            score += contribution;
            if contribution > 0.0 {
                actor_count += 1;
            }
        }
        let score = score.max(0.0);
        if score >= 2.0 && actor_count >= 2 {
            self.storage.upsert_governance_proposal(
                &gen_uuid(),
                chunk_id,
                "review_applicability",
                "Weighted negative feedback",
                score.ceil() as i64,
                score,
                actor_count,
                now,
            )?;
        } else {
            self.storage.conn_execute(
                "UPDATE governance_proposals
                 SET state='rejected', evidence_count=?, evidence_score=?, actor_count=?, updated_at=?
                 WHERE chunk_id=? AND state='pending'",
                rusqlite::params![score.ceil() as i64, score, actor_count, now, chunk_id],
            )?;
        }
        Ok(())
    }

    pub(super) fn enqueue_evolve_if_needed(&self, now: &str) -> Result<()> {
        let ready = count_query(
            &self.storage,
            "SELECT COUNT(*) FROM episodic_log WHERE distill_state='new'",
        )?;
        let oldest = self
            .storage
            .query_chunks("SELECT MIN(ts) AS oldest FROM episodic_log WHERE distill_state='new'")?
            .first()
            .and_then(|row| row.get("oldest"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let age_due = oldest
            .as_deref()
            .is_some_and(|ts| ts <= hours_ago(now, self.evolve_schedule_interval_hours).as_str());
        let governance_pending = count_query(
            &self.storage,
            "SELECT COUNT(*) FROM governance_proposals WHERE state='pending'",
        )?;
        // A chunk whose proposal already has enough evidence should trigger evolve
        // immediately — don't wait for 3 different pending proposals.
        let governance_ready = count_query_params(
            &self.storage,
            "SELECT COUNT(*) FROM governance_proposals
             WHERE state='pending'
               AND evidence_score >= ? AND actor_count >= 2",
            rusqlite::params![self.governance_archive_threshold as f64],
        )?;
        if ready >= self.evolve_threshold
            || (ready > 0 && age_due)
            || governance_pending >= self.governance_evolve_threshold
            || governance_ready > 0
        {
            let reason = if ready >= self.evolve_threshold {
                "threshold"
            } else if governance_ready > 0 {
                "governance_ready"
            } else if governance_pending >= self.governance_evolve_threshold {
                "governance"
            } else {
                "scheduled"
            };
            self.storage.request_evolve(&gen_uuid(), reason, now)?;
        }
        Ok(())
    }
}
