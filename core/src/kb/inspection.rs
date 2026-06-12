use super::*;

impl KnowledgeBase {
    pub fn inspect(&self) -> Result<Value> {
        let total: i64 = count_query(
            &self.storage,
            "SELECT COUNT(*) FROM chunks WHERE origin!='spark'",
        )?;
        let active: i64 = count_query(
            &self.storage,
            "SELECT COUNT(*) FROM chunks WHERE state='active' AND origin!='spark'",
        )?;
        let pending: i64 = count_query(
            &self.storage,
            "SELECT COUNT(*) FROM chunks WHERE state='pending' AND origin!='spark'",
        )?;
        let archived: i64 = count_query(
            &self.storage,
            "SELECT COUNT(*) FROM chunks WHERE state='archived' AND origin!='spark'",
        )?;
        let sparks: i64 = count_query(
            &self.storage,
            "SELECT COUNT(*) FROM chunks WHERE origin='spark' AND state!='archived'",
        )?;
        let open_logs: i64 = count_query(
            &self.storage,
            "SELECT COUNT(*) FROM episodic_log WHERE distill_state='open'",
        )?;
        let new_logs: i64 = count_query(
            &self.storage,
            "SELECT COUNT(*) FROM episodic_log WHERE distill_state='new'",
        )?;
        let embed_rebuild: i64 = count_query(&self.storage,
            "SELECT COUNT(*) FROM chunks WHERE embed_version=0 OR embed_version < (SELECT COALESCE(CAST(value AS INTEGER),1) FROM meta WHERE key='embed_version')")?;
        let schema_version = self.storage.get_meta_or("schema_version", "?");
        let lib_id = self.storage.get_meta_or("lib_id", "?");
        let last_agg = self.storage.get_meta_or("last_agg_ts", "never");

        let metric_window_start = days_ago(&utc_now_iso(), 30);
        let trace_metrics = self.storage.query_chunks_params(
            "SELECT COUNT(*) AS total,
                    SUM(CASE WHEN task_state='completed' THEN 1 ELSE 0 END) AS completed,
                    SUM(CASE WHEN task_state='timed_out' THEN 1 ELSE 0 END) AS timed_out,
                    SUM(CASE WHEN task_state='completed' AND usage_state!='unknown'
                             THEN 1 ELSE 0 END) AS usage_known,
                    SUM(CASE WHEN task_state='completed' AND usage_state='known_some'
                             THEN 1 ELSE 0 END) AS usage_some,
                    SUM(CASE WHEN task_state='completed'
                                  AND outcome IN ('ok','fail')
                             THEN 1 ELSE 0 END) AS outcome_known,
                    SUM(CASE WHEN outcome='ok' THEN 1 ELSE 0 END) AS succeeded
             FROM episodic_log WHERE ts >= ?",
            rusqlite::params![metric_window_start],
        )?;
        let trace_row = trace_metrics.first();
        let trace_total = trace_row
            .and_then(|row| row.get("total"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let trace_completed = trace_row
            .and_then(|row| row.get("completed"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let trace_timed_out = trace_row
            .and_then(|row| row.get("timed_out"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let usage_known = trace_row
            .and_then(|row| row.get("usage_known"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let usage_some = trace_row
            .and_then(|row| row.get("usage_some"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let succeeded = trace_row
            .and_then(|row| row.get("succeeded"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let outcome_known = trace_row
            .and_then(|row| row.get("outcome_known"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let usage_rows = self.storage.query_chunks_params(
            "SELECT recall_snapshot, used_ids FROM episodic_log
             WHERE task_state='completed'
               AND usage_state!='unknown' AND used_complete=1
               AND recall_snapshot IS NOT NULL AND used_ids IS NOT NULL
               AND ts >= ?",
            rusqlite::params![metric_window_start],
        )?;
        let mut selected_total = 0_i64;
        let mut selected_used = 0_i64;
        for row in usage_rows {
            let selected: HashSet<String> = row
                .get("recall_snapshot")
                .and_then(Value::as_str)
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                .and_then(|snapshot| snapshot.get("selected").cloned())
                .and_then(|value| serde_json::from_value::<Vec<String>>(value).ok())
                .unwrap_or_default()
                .into_iter()
                .collect();
            let used: HashSet<String> = row
                .get("used_ids")
                .and_then(Value::as_str)
                .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
                .unwrap_or_default()
                .into_iter()
                .collect();
            selected_total += selected.len() as i64;
            selected_used += selected.intersection(&used).count() as i64;
        }
        let feedback_count = count_query_params(
            &self.storage,
            "SELECT COUNT(*) FROM feedback_events WHERE ts >= ?",
            rusqlite::params![metric_window_start],
        )?;
        let feedback_traces = count_query_params(
            &self.storage,
            "SELECT COUNT(DISTINCT f.trace_id)
             FROM feedback_events f
             JOIN episodic_log e ON e.trace_id=f.trace_id
             WHERE f.ts >= ? AND e.ts >= ? AND e.task_state='completed'",
            rusqlite::params![metric_window_start, metric_window_start],
        )?;
        let pending_evolve = count_query(
            &self.storage,
            "SELECT COUNT(*) FROM evolve_requests WHERE state IN ('pending','running')",
        )?;
        let governance_pending = count_query(
            &self.storage,
            "SELECT COUNT(*) FROM governance_proposals WHERE state='pending'",
        )?;
        let failed_evolve = count_query_params(
            &self.storage,
            "SELECT COUNT(*) FROM evolve_requests
             WHERE last_failed_at >= ?",
            rusqlite::params![metric_window_start],
        )?;
        let failed_distill = count_query_params(
            &self.storage,
            "SELECT COUNT(*) FROM episodic_log
             WHERE distill_last_failed_at >= ?",
            rusqlite::params![metric_window_start],
        )?;
        let confidence_buckets = self.storage.query_chunks(&format!(
            "SELECT
               SUM(CASE WHEN confidence < 0.25 THEN 1 ELSE 0 END) AS low,
               SUM(CASE WHEN confidence >= 0.25 AND confidence < {0} THEN 1 ELSE 0 END) AS medium,
               SUM(CASE WHEN confidence >= {0} THEN 1 ELSE 0 END) AS high
             FROM chunks WHERE origin!='spark' AND state!='archived'",
            self.promote_confidence_min
        ))?;
        let confidence_row = confidence_buckets.first();

        // P3-A: oldest pending chunk timestamp — surfaces long-lived pending debt.
        let pending_oldest_ts = self.storage.query_chunks(
            "SELECT MIN(created_at) AS oldest FROM chunks WHERE state='pending' AND origin!='spark'",
        )?.into_iter().next()
            .and_then(|r| r.get("oldest").cloned())
            .and_then(|v| if v.is_null() { None } else { Some(v) });

        // Health signal 1: knowledge debt ratio.
        // Zombie = active chunks with middling confidence (stuck, neither good nor bad)
        // that are at least 14d old and have been used at least once.
        // "never-recalled old" chunks are handled by curate 3c (never_used archive).
        let zombie_cutoff = days_ago(&utc_now_iso(), 14);
        let zombie: i64 = count_query_params(
            &self.storage,
            "SELECT COUNT(*) FROM chunks
             WHERE origin!='spark' AND state='active'
               AND confidence >= 0.4 AND confidence <= 0.6
               AND last_used_at IS NOT NULL
               AND created_at < ?",
            rusqlite::params![zombie_cutoff],
        )?;
        let debt_numerator = pending + zombie;
        let debt_denominator = active.max(1);
        let debt_ratio = debt_numerator as f64 / debt_denominator as f64;

        // Health signal 5: stale screening count
        let screening_cutoff = minutes_ago(&utc_now_iso(), self.screening_timeout_minutes);
        let stale_screening: i64 = count_query_params(
            &self.storage,
            "SELECT COUNT(*) FROM episodic_log
             WHERE distill_state='screening' AND distill_locked_at < ?",
            rusqlite::params![screening_cutoff],
        )?;

        // Health signal 4: actual Distill cost within the configured rolling window.
        let distill_period_start = self.distill_token_period_start(&utc_now_iso())?;
        let distill_cost = self.storage.query_chunks_params(
            "SELECT COALESCE(SUM(prompt_tokens),0) AS pt,
                    COALESCE(SUM(completion_tokens),0) AS ct
             FROM distill_token_usage
             WHERE accounted_at >= ?",
            rusqlite::params![distill_period_start],
        )?;
        let prompt_tokens = distill_cost
            .first()
            .and_then(|r| r.get("pt"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let completion_tokens = distill_cost
            .first()
            .and_then(|r| r.get("ct"))
            .and_then(Value::as_i64)
            .unwrap_or(0);

        // Health signal 2: sparks that have been recalled often (soft incubation threshold = 5)
        let spark_threshold: i64 = self
            .storage
            .get_meta("curate.soft_mature_threshold")
            .ok()
            .flatten()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(5);
        let recurring_sparks = self.storage.query_chunks_params(
            "SELECT ut.chunk_id, COUNT(*) AS cnt,
                    c.content, c.trigger_desc, c.maturity
             FROM usage_trace ut
             JOIN chunks c ON c.id = ut.chunk_id
             WHERE ut.event='retrieved'
               AND c.origin='spark'
             GROUP BY ut.chunk_id HAVING cnt >= ?",
            rusqlite::params![spark_threshold],
        )?;
        let recurring_spark_ids: Vec<Value> = recurring_sparks
            .iter()
            .map(|r| {
                json!({
                    "id": r.get("chunk_id").and_then(Value::as_str).unwrap_or(""),
                    "retrieved_count": r.get("cnt").and_then(Value::as_i64).unwrap_or(0),
                    "maturity": r.get("maturity").and_then(Value::as_str).unwrap_or(""),
                    "content_preview": r.get("content").and_then(Value::as_str).unwrap_or("")
                        .chars().take(80).collect::<String>(),
                })
            })
            .collect();

        let mut suggestions: Vec<Value> = Vec::new();
        if embed_rebuild > 0 {
            suggestions.push(json!({"action": "innate evolve --rebuild-embeddings", "reason": format!("{embed_rebuild} chunk(s) missing embeddings")}));
        }
        if new_logs > 0 {
            suggestions.push(json!({"action": "innate evolve --trigger manual", "reason": format!("{new_logs} episodic log(s) ready to distill")}));
        }
        if pending > 0 {
            suggestions.push(json!({"action": "innate approve <id>  # or innate archive <id>", "reason": format!("{pending} pending chunk(s) awaiting review")}));
        }
        if !recurring_spark_ids.is_empty() {
            suggestions.push(json!({"action": "innate promote-spark <id> --to note", "reason": format!("{} spark(s) recalled ≥{spark_threshold}× — consider promoting", recurring_spark_ids.len())}));
        }
        if stale_screening > 0 {
            suggestions.push(json!({"action": "innate evolve --trigger manual", "reason": format!("{stale_screening} episodic log(s) stuck in screening")}));
        }
        if governance_pending > 0 {
            suggestions.push(json!({
                "action": "review governance_proposals",
                "reason": format!("{governance_pending} chunk(s) have repeated negative feedback")
            }));
        }

        Ok(json!({
            "schema_version": schema_version,
            "lib_id": lib_id,
            "last_agg_ts": last_agg,
            "chunks": {
                "total": total, "active": active, "pending": pending, "archived": archived,
                "pending_oldest_ts": pending_oldest_ts,
            },
            "sparks": sparks,
            "episodic_log": {"open": open_logs, "new": new_logs},
            "embed_rebuild_queue": embed_rebuild,
            "knowledge_debt_ratio": (debt_ratio * 100.0).round() / 100.0,
            "stale_screening_count": stale_screening,
            "feedback_loop": {
                "trace_completion_rate": ratio(trace_completed, trace_total),
                "usage_annotation_rate": ratio(usage_known, trace_completed),
                "trace_use_rate": ratio(usage_some, usage_known),
                "selected_to_used_rate": ratio(selected_used, selected_total),
                "task_success_rate": ratio(succeeded, outcome_known),
                "feedback_coverage": ratio(feedback_traces, trace_completed),
                "feedback_events": feedback_count,
                "timed_out_traces": trace_timed_out,
                "pending_evolve_requests": pending_evolve,
                "failed_evolve_requests_30d": failed_evolve,
                "failed_distill_logs_30d": failed_distill,
                "pending_governance_proposals": governance_pending,
                "window_days": 30,
                "confidence_distribution": {
                    "low": confidence_row.and_then(|row| row.get("low")).and_then(Value::as_i64).unwrap_or(0),
                    "medium": confidence_row.and_then(|row| row.get("medium")).and_then(Value::as_i64).unwrap_or(0),
                    "high": confidence_row.and_then(|row| row.get("high")).and_then(Value::as_i64).unwrap_or(0),
                }
            },
            "distill_cost_estimate": {"prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens},
            "recurring_sparks": recurring_sparks.len(),
            "recurring_spark_ids": recurring_spark_ids,
            "params": {
                "recall.w_content": self.w_content,
                "recall.w_trigger": self.w_trigger,
                "recall.w_context": self.w_context,
                "recall.top_k_candidates": self.top_k_candidates,
                "curate.low_conf_threshold": self.low_conf_threshold,
                "curate.low_conf_idle_days": self.low_conf_idle_days,
                "curate.repeat_select_min": self.repeat_select_min,
                "curate.never_used_age_days": self.never_used_age_days,
                "curate.promote_used_success_min": self.promote_used_success_min,
                "curate.promote_confidence_min": self.promote_confidence_min,
                "curate.screening_timeout_minutes": self.screening_timeout_minutes,
                "curate.open_ttl_days": self.open_ttl_days,
                "evolve.schedule_interval_hours": self.evolve_schedule_interval_hours,
            },
            "suggestions": suggestions
        }))
    }

    // ------------------------------------------------------------------
    // Public: rebuild_embeddings (evolve --rebuild-embeddings)
    // ------------------------------------------------------------------

    pub fn rebuild_embeddings(&self) -> Result<usize> {
        let meta_version = self
            .storage
            .get_meta("embed_version")?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(1);
        // Fetch chunks with embed_version=0 (failed writes) or below current meta version.
        let stale = self.storage.query_chunks_params(
            "SELECT id, content, trigger_desc, state_reason FROM chunks
             WHERE embed_version = 0 OR embed_version < ?",
            rusqlite::params![meta_version],
        )?;
        let mut count = 0;
        for row in &stale {
            let id = match row.get("id").and_then(Value::as_str) {
                Some(v) => v,
                None => continue,
            };
            let content = row.get("content").and_then(Value::as_str).unwrap_or("");
            let trigger = row
                .get("trigger_desc")
                .and_then(Value::as_str)
                .unwrap_or(content);
            let state_reason = row
                .get("state_reason")
                .and_then(Value::as_str)
                .unwrap_or("");

            let cvec = match self.embedding.embed_content(content) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let tvec = match self.embedding.embed_trigger(trigger) {
                Ok(v) => v,
                Err(_) => continue,
            };

            self.storage.begin_immediate()?;
            let r = (|| -> Result<()> {
                self.storage
                    .insert_vec_content(id, &pack_embedding(&cvec))?;
                self.storage
                    .insert_vec_trigger(id, &pack_embedding(&tvec))?;
                // Restore intended state if encoded in state_reason.
                let new_reason = if state_reason.starts_with("embedding_pending:target=") {
                    let target_state = state_reason.trim_start_matches("embedding_pending:target=");
                    let now = utc_now_iso();
                    self.storage.update_chunk_state(
                        id,
                        target_state,
                        Some("embedding_rebuilt"),
                        &now,
                    )?;
                    "embedding_rebuilt".to_string()
                } else {
                    "embedding_rebuilt".to_string()
                };
                let now = utc_now_iso();
                self.storage.conn_execute(
                    "UPDATE chunks SET embed_version=?, state_reason=?, updated_at=? WHERE id=?",
                    rusqlite::params![meta_version, new_reason, now, id],
                )?;
                self.storage.commit()
            })();
            if r.is_err() {
                let _ = self.storage.rollback();
            } else {
                count += 1;
            }
        }
        Ok(count)
    }

    // ------------------------------------------------------------------
    // Public: inspect_id (inspect <chunk_id> or <trace_id>)
    // ------------------------------------------------------------------

    pub fn inspect_id(&self, id: &str) -> Result<Value> {
        // Try as chunk_id first, then as trace_id.
        if let Some(chunk) = self.storage.get_chunk(id)? {
            let traces = self.storage.query_chunks_params(
                "SELECT * FROM usage_trace WHERE chunk_id=? ORDER BY ts DESC LIMIT 20",
                rusqlite::params![id],
            )?;
            let derived = self.storage.query_chunks_params(
                "SELECT id, state, confidence FROM chunks WHERE distilled_from IN (
                   SELECT id FROM episodic_log WHERE trace_id IN (
                     SELECT trace_id FROM usage_trace WHERE chunk_id=?
                   )
                 ) LIMIT 10",
                rusqlite::params![id],
            )?;
            return Ok(json!({
                "kind": "chunk",
                "chunk": chunk,
                "recent_traces": traces,
                "derived_chunks": derived,
            }));
        }
        // Try as trace_id.
        if let Some(log) = self.storage.get_episodic_log(id)? {
            let traces = self.storage.query_chunks_params(
                "SELECT * FROM usage_trace WHERE trace_id=? ORDER BY ts ASC",
                rusqlite::params![id],
            )?;
            return Ok(json!({
                "kind": "trace",
                "episodic_log": log,
                "usage_traces": traces,
            }));
        }
        Err(InnateError::ChunkNotFound(id.to_string()))
    }

    // ------------------------------------------------------------------
    // Sanitize
    // ------------------------------------------------------------------

    pub(super) fn sanitize_content(&self, content: &str) -> (String, SanitizeAction) {
        self.sanitizer.sanitize(content)
    }
}
