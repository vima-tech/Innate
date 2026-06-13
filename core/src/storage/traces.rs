use super::*;

impl Storage {
    #[allow(clippy::too_many_arguments)]
    pub fn insert_usage_trace(
        &self,
        trace_id: &str,
        chunk_id: Option<&str>,
        event: &str,
        strength: f64,
        similarity: Option<f64>,
        refine_mode: Option<&str>,
        tokens: Option<i64>,
        rank: Option<i64>,
        attribution: Option<&str>,
        source: &str,
        ts: &str,
    ) -> Result<usize> {
        let mut stmt = self.conn.prepare_cached(
            "INSERT OR IGNORE INTO usage_trace
             (trace_id, chunk_id, event, strength, similarity, refine_mode, tokens, rank, attribution, source, ts)
             VALUES (?,?,?,?,?,?,?,?,?,?,?)",
        )?;
        Ok(stmt.execute(
            params![trace_id, chunk_id, event, strength, similarity, refine_mode, tokens, rank, attribution, source, ts],
        )?)
    }

    pub fn replace_used_trace(
        &self,
        trace_id: &str,
        used_ids: &[String],
        strength: f64,
        attribution: &str,
        source: &str,
        ts: &str,
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM usage_trace WHERE trace_id=? AND event='used'",
            [trace_id],
        )?;
        for chunk_id in used_ids {
            self.insert_usage_trace(
                trace_id,
                Some(chunk_id),
                "used",
                strength,
                None,
                None,
                None,
                None,
                Some(attribution),
                source,
                ts,
            )?;
        }
        Ok(())
    }

    pub fn merge_used_trace(
        &self,
        trace_id: &str,
        used_ids: &[String],
        strength: f64,
        attribution: &str,
        source: &str,
        ts: &str,
    ) -> Result<()> {
        if used_ids.is_empty() {
            return Ok(());
        }
        let attribution_rank = |value: &str| match value {
            "explicit" => 3,
            "cited" => 2,
            "inferred" => 1,
            _ => 0,
        };

        // Batch-fetch all existing 'used' rows for this trace in one query
        // instead of one SELECT per chunk id.
        let placeholders = used_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT chunk_id, attribution FROM usage_trace
             WHERE trace_id=? AND event='used' AND chunk_id IN ({placeholders})"
        );
        let mut qparams: Vec<&str> = Vec::with_capacity(used_ids.len() + 1);
        qparams.push(trace_id);
        qparams.extend(used_ids.iter().map(String::as_str));
        let existing: HashMap<String, String> = {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(qparams.iter()), |r| {
                let id: String = r.get(0)?;
                let attr: Option<String> = r.get(1)?;
                Ok((id, attr.unwrap_or_else(|| "inferred".to_string())))
            })?;
            rows.collect::<rusqlite::Result<HashMap<_, _>>>()?
        };

        for chunk_id in used_ids {
            match existing.get(chunk_id) {
                Some(existing_attribution) => {
                    if attribution_rank(attribution) > attribution_rank(existing_attribution) {
                        self.conn.execute(
                            "UPDATE usage_trace
                             SET strength=?, attribution=?, source=?, ts=?
                             WHERE trace_id=? AND chunk_id=? AND event='used'",
                            params![strength, attribution, source, ts, trace_id, chunk_id],
                        )?;
                    }
                }
                None => {
                    self.insert_usage_trace(
                        trace_id,
                        Some(chunk_id),
                        "used",
                        strength,
                        None,
                        None,
                        None,
                        None,
                        Some(attribution),
                        source,
                        ts,
                    )?;
                }
            }
        }
        Ok(())
    }

    pub fn refresh_chunk_last_used(&self, chunk_id: &str, now: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE chunks
             SET last_used_at=COALESCE(
                   (SELECT MAX(ts) FROM usage_trace
                    WHERE chunk_id=? AND event='used'
                      AND ts > COALESCE(chunks.evidence_cutoff_at, '')),
                   last_used_base
                 ),
                 updated_at=?
             WHERE id=?",
            params![chunk_id, now, chunk_id],
        )?;
        Ok(())
    }

    pub fn get_outcome_for_trace(&self, trace_id: &str) -> Result<Option<String>> {
        let row = self.conn.query_row(
            "SELECT event FROM usage_trace
             WHERE trace_id=? AND event IN ('task_ok','task_fail') AND chunk_id IS NULL
             LIMIT 1",
            [trace_id],
            |r| r.get::<_, String>(0),
        );
        match row {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn purge_usage_trace(&self, before_ts: &str) -> Result<usize> {
        // Preserve compact attribution facts. They are required to replay corrections.
        let n = self.conn.execute(
            "DELETE FROM usage_trace
             WHERE ts < ?
             AND event IN ('retrieved','refined')
             AND NOT (event = 'retrieved'
                      AND chunk_id IN (SELECT id FROM chunks WHERE origin='spark'))",
            [before_ts],
        )?;
        Ok(n)
    }

    // ------------------------------------------------------------------
    // Episodic log
    // ------------------------------------------------------------------

    pub fn upsert_episodic_log(&self, log: &EpisodicLogRow) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO episodic_log
             (id, trace_id, lib_id, ts, query, recall_snapshot, output,
              output_summary, outcome, event_source, task_state, completed_at,
              usage_state, used_ids, used_attribution, used_complete, context_key, nomination, priority,
              distill_state, distill_note, distill_attempts, distill_last_failed_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,0,NULL)",
            params![
                log.id,
                log.trace_id,
                log.lib_id,
                log.ts,
                log.query,
                log.recall_snapshot,
                log.output,
                log.output_summary,
                log.outcome,
                log.event_source,
                log.task_state,
                log.completed_at,
                log.usage_state,
                log.used_ids,
                log.used_attribution,
                i64::from(log.used_complete),
                log.context_key,
                log.nomination,
                log.priority,
                log.distill_state,
                log.distill_note
            ],
        )?;
        Ok(())
    }

    pub fn get_episodic_log(&self, trace_id: &str) -> Result<Option<Value>> {
        let row = self.conn.query_row(
            "SELECT * FROM episodic_log WHERE trace_id=?",
            [trace_id],
            row_to_json,
        );
        match row {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn update_episodic_log_state(
        &self,
        trace_id: &str,
        state: &str,
        note: Option<&str>,
        outcome: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE episodic_log
             SET distill_state=?, distill_note=COALESCE(?,distill_note),
                 outcome=COALESCE(?,outcome),
                 distill_run_id=NULL, distill_locked_at=NULL
             WHERE trace_id=?",
            params![state, note, outcome, trace_id],
        )?;
        Ok(())
    }

    /// Patch content fields on an existing episodic_log row (補写: output_summary, nomination, etc.)
    pub fn patch_episodic_log_content(
        &self,
        trace_id: &str,
        query: Option<&str>,
        output: Option<&str>,
        output_summary: Option<&str>,
        nomination: Option<&str>,
        priority: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE episodic_log
             SET output_summary = COALESCE(?, output_summary),
                 nomination     = COALESCE(?, nomination),
                 output         = COALESCE(?, output),
                 query          = COALESCE(?, query),
                 priority       = MAX(priority, ?)
             WHERE trace_id = ?",
            params![
                output_summary,
                nomination,
                output,
                query,
                priority,
                trace_id
            ],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_trace_lifecycle(
        &self,
        trace_id: &str,
        task_state: &str,
        completed_at: Option<&str>,
        usage_state: Option<&str>,
        used_ids: Option<&str>,
        used_attribution: Option<&str>,
        used_complete: Option<bool>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE episodic_log
             SET task_state=?,
                 completed_at=COALESCE(?, completed_at),
                 usage_state=COALESCE(?, usage_state),
                 used_ids=COALESCE(?, used_ids),
                 used_attribution=COALESCE(?, used_attribution),
                 used_complete=COALESCE(?, used_complete)
             WHERE trace_id=?",
            params![
                task_state,
                completed_at,
                usage_state,
                used_ids,
                used_attribution,
                used_complete.map(i64::from),
                trace_id
            ],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_confidence_evidence(
        &self,
        id: &str,
        trace_id: Option<&str>,
        chunk_id: &str,
        kind: &str,
        target: f64,
        alpha: f64,
        reason: &str,
        context_key: Option<&str>,
        ts: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO confidence_evidence
             (id, trace_id, chunk_id, kind, target, alpha, reason, context_key, ts)
             VALUES (?,?,?,?,?,?,?,?,?)
             ON CONFLICT(trace_id, chunk_id, kind) WHERE trace_id IS NOT NULL
             DO UPDATE SET target=excluded.target, alpha=excluded.alpha,
                           reason=excluded.reason, context_key=excluded.context_key",
            params![
                id,
                trace_id,
                chunk_id,
                kind,
                target,
                alpha,
                reason,
                context_key,
                ts
            ],
        )?;
        Ok(())
    }

    pub fn delete_trace_confidence_evidence(&self, trace_id: &str, kinds: &[&str]) -> Result<()> {
        if kinds.is_empty() {
            return Ok(());
        }
        let placeholders = kinds.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "DELETE FROM confidence_evidence WHERE trace_id=? AND kind IN ({placeholders})"
        );
        let mut params: Vec<&str> = Vec::with_capacity(kinds.len() + 1);
        params.push(trace_id);
        params.extend_from_slice(kinds);
        self.conn
            .execute(&sql, rusqlite::params_from_iter(params.iter()))?;
        Ok(())
    }

    pub fn delete_chunk_trace_confidence_evidence(
        &self,
        trace_id: &str,
        chunk_id: &str,
        kind: &str,
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM confidence_evidence
             WHERE trace_id=? AND chunk_id=? AND kind=?",
            params![trace_id, chunk_id, kind],
        )?;
        Ok(())
    }

    pub fn confidence_evidence_for_chunk(&self, chunk_id: &str) -> Result<Vec<Value>> {
        self.query_json(
            "SELECT target, alpha, reason, ts, id
             FROM confidence_evidence WHERE chunk_id=?
             ORDER BY ts ASC,
                      CASE kind
                        WHEN 'outcome_ok' THEN 1
                        WHEN 'outcome_fail' THEN 1
                        WHEN 'selected_unused' THEN 2
                        WHEN 'feedback_up' THEN 3
                        WHEN 'feedback_down' THEN 3
                        WHEN 'decay' THEN 4
                        ELSE 5
                      END ASC,
                      kind ASC, id ASC",
            [chunk_id],
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_feedback_event(
        &self,
        id: &str,
        trace_id: &str,
        chunk_id: &str,
        signal: &str,
        strength: f64,
        source: &str,
        actor: Option<&str>,
        reason: Option<&str>,
        context_key: Option<&str>,
        ts: &str,
    ) -> Result<usize> {
        Ok(self.conn.execute(
            "INSERT OR IGNORE INTO feedback_events
             (id, trace_id, chunk_id, signal, strength, source, actor, reason, context_key, ts)
             VALUES (?,?,?,?,?,?,?,?,?,?)",
            params![
                id,
                trace_id,
                chunk_id,
                signal,
                strength,
                source,
                actor,
                reason,
                context_key,
                ts
            ],
        )?)
    }

    pub fn delete_feedback_event(
        &self,
        trace_id: &str,
        chunk_id: &str,
        signal: &str,
    ) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM feedback_events
             WHERE trace_id=? AND chunk_id=? AND signal=?",
            params![trace_id, chunk_id, signal],
        )?)
    }

    pub fn update_chunk_last_decayed_at(&self, id: &str, now: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE chunks SET last_decayed_at=?, updated_at=? WHERE id=?",
            params![now, now, id],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_context_stat(
        &self,
        chunk_id: &str,
        context_key: &str,
        success: i64,
        failure: i64,
        positive: i64,
        negative: i64,
        now: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO chunk_context_stats
             (chunk_id, context_key, success_count, failure_count,
              positive_feedback, negative_feedback, last_updated_at)
             VALUES (?,?,?,?,?,?,?)
             ON CONFLICT(chunk_id, context_key) DO UPDATE SET
               success_count=success_count+excluded.success_count,
               failure_count=failure_count+excluded.failure_count,
               positive_feedback=positive_feedback+excluded.positive_feedback,
               negative_feedback=negative_feedback+excluded.negative_feedback,
               last_updated_at=excluded.last_updated_at",
            params![
                chunk_id,
                context_key,
                success,
                failure,
                positive,
                negative,
                now
            ],
        )?;
        Ok(())
    }

    pub fn context_score(&self, chunk_id: &str, context_key: &str) -> Result<f64> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT success_count, failure_count, positive_feedback, negative_feedback
             FROM chunk_context_stats WHERE chunk_id=? AND context_key=?",
        )?;
        let row = stmt
            .query_row(params![chunk_id, context_key], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .optional()?;
        let Some((success, failure, positive, negative)) = row else {
            return Ok(0.0);
        };
        Ok(context_score_from_counts(success, failure, positive, negative))
    }

    /// Batch variant of `context_score`: one query for many chunk ids under a
    /// single context key. Chunks with no stats are absent from the map (score 0).
    pub fn context_scores_batch(
        &self,
        chunk_ids: &[&str],
        context_key: &str,
    ) -> Result<HashMap<String, f64>> {
        if chunk_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = chunk_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT chunk_id, success_count, failure_count, positive_feedback, negative_feedback
             FROM chunk_context_stats
             WHERE context_key=? AND chunk_id IN ({placeholders})"
        );
        let mut params: Vec<&str> = Vec::with_capacity(chunk_ids.len() + 1);
        params.push(context_key);
        params.extend_from_slice(chunk_ids);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let (id, success, failure, positive, negative) = row?;
            map.insert(
                id,
                context_score_from_counts(success, failure, positive, negative),
            );
        }
        Ok(map)
    }
}

/// Shared scoring math for `context_score` / `context_scores_batch`.
fn context_score_from_counts(success: i64, failure: i64, positive: i64, negative: i64) -> f64 {
    let wins = success as f64 + positive as f64 * 2.0;
    let losses = failure as f64 + negative as f64 * 2.0;
    let evidence = wins + losses;
    let posterior = (wins + 1.0) / (evidence + 2.0);
    let evidence_weight = (evidence / 5.0).min(1.0);
    (posterior - 0.5) * 2.0 * evidence_weight
}
