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
            .filter(|v| !v.is_null());

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
        // agent_coverage misconfiguration (design doc §5.1 / §7): hook/daemon NULL agent
        // is *by design*, so only an mcp/cli channel with NULL agent is a real signal —
        // typically a missing `INNATE_AGENT` env in the agent's MCP config. Fire only on
        // that channel to avoid the perma-misfire a global coverage check would cause.
        let mcpcli_null: i64 = count_query_params(
            &self.storage,
            "SELECT COUNT(*) FROM episodic_log
             WHERE event_source IN ('mcp','cli') AND agent IS NULL AND ts >= ?1",
            rusqlite::params![days_ago(&utc_now_iso(), 7)],
        )?;
        if mcpcli_null > 0 {
            suggestions.push(json!({
                "action": "set INNATE_AGENT in the agent's MCP env (innate install)",
                "reason": format!("{mcpcli_null} mcp/cli call(s) in 7d have NULL agent — agent attribution misconfigured")
            }));
        }

        // Intuition honesty (PRD §4): does high strength actually predict success, and is
        // the critic crying wolf? Only nudge once enough appraisals carry an outcome.
        let intuition = self.intuition_calibration(&metric_window_start)?;
        let appraisals = intuition
            .get("appraisals")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let mono_gap = intuition
            .get("monotonicity_gap")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let false_alarm = intuition
            .get("false_alarm_rate")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if appraisals >= 20 && mono_gap <= 0.0 {
            suggestions.push(json!({
                "action": "tune recall.w_* / situation.coarse_keys",
                "reason": "appraise strength may be noise — strong tier does not beat weak on task_ok"
            }));
        }
        if appraisals >= 20 && false_alarm >= 0.5 {
            suggestions.push(json!({
                "action": "review caution chunks / raise appraise.tier_strong",
                "reason": format!("intuition false-alarm rate {false_alarm} — strong cautions often end ok")
            }));
        }

        // Storage growth metrics — trace/log bloat is driven by recall/record
        // activity over time, independent of chunk count, so it is surfaced here
        // for monitoring before it becomes a problem.
        let usage_trace_total = count_query(&self.storage, "SELECT COUNT(*) FROM usage_trace")?;
        let episodic_log_total = count_query(&self.storage, "SELECT COUNT(*) FROM episodic_log")?;
        let page_count = count_query(&self.storage, "PRAGMA page_count")?;
        let page_size = count_query(&self.storage, "PRAGMA page_size")?;
        let db_size_bytes = page_count * page_size;

        // ── Observability (P1–P3b) — additive top-level blocks, see design doc §5.6.
        // Existing keys are never renamed/moved;未就绪的子块整块省略。
        let now_obs = utc_now_iso();
        let observability = self.observability_block(&now_obs)?;
        let operational = self.operational_block(&now_obs)?;
        let trends = self.trends_block(&now_obs)?;

        let mut out = json!({
            "schema_version": schema_version,
            "lib_id": lib_id,
            "last_agg_ts": last_agg,
            "chunks": {
                "total": total, "active": active, "pending": pending, "archived": archived,
                "pending_oldest_ts": pending_oldest_ts,
            },
            "storage": {
                "usage_trace_rows": usage_trace_total,
                "episodic_log_rows": episodic_log_total,
                "db_size_bytes": db_size_bytes,
                "db_size_mb": (db_size_bytes as f64 / 1_048_576.0 * 100.0).round() / 100.0,
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
            "intuition_calibration": intuition,
            "distill_cost_estimate": {"prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens},
            "recurring_sparks": recurring_sparks.len(),
            "recurring_spark_ids": recurring_spark_ids,
            "params": {
                "recall.w_content": self.w_content,
                "recall.w_trigger": self.w_trigger,
                "recall.w_context": self.w_context,
                "recall.w_activation": self.w_activation,
                "recall.w_spread": self.w_spread,
                "recall.top_k_candidates": self.top_k_candidates,
                "curate.low_conf_threshold": self.low_conf_threshold,
                "curate.low_conf_idle_days": self.low_conf_idle_days,
                "curate.repeat_select_min": self.repeat_select_min,
                "curate.never_used_age_days": self.never_used_age_days,
                "curate.promote_used_success_min": self.promote_used_success_min,
                "curate.promote_confidence_min": self.promote_confidence_min,
                "curate.screening_timeout_minutes": self.screening_timeout_minutes,
                "curate.open_ttl_days": self.open_ttl_days,
                "curate.log_compact_days": self.log_compact_days,
                "evolve.schedule_interval_hours": self.evolve_schedule_interval_hours,
            },
            "suggestions": suggestions
        });

        if let Some(obj) = out.as_object_mut() {
            obj.insert("observability".to_string(), observability);
            obj.insert("operational".to_string(), operational);
            if let Some(trends) = trends {
                obj.insert("trends".to_string(), trends);
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------------
    // Observability blocks (design doc §5.1 / §5.6)
    // ------------------------------------------------------------------

    /// P1 derived metrics: multi-window rates, per-channel agent coverage,
    /// zombie chunks, and approximate lifecycle transitions. Pure SQL, no new tables.
    fn observability_block(&self, now: &str) -> Result<Value> {
        let windows = json!({
            "1d": self.window_rates(&days_ago(now, 1))?,
            "7d": self.window_rates(&days_ago(now, 7))?,
            "30d": self.window_rates(&days_ago(now, 30))?,
        });

        // agent_coverage **by channel** — hook/daemon NULL agent is by design (see §5.1);
        // only mcp/cli NULL is a real misconfiguration signal.
        let mut by_source = serde_json::Map::new();
        let rows = self.storage.query_chunks_params(
            "SELECT COALESCE(event_source,'unknown') AS src, COUNT(*) AS total,
                    SUM(CASE WHEN agent IS NOT NULL THEN 1 ELSE 0 END) AS with_agent
             FROM episodic_log WHERE ts >= ?1 GROUP BY event_source",
            rusqlite::params![days_ago(now, 30)],
        )?;
        for r in &rows {
            let src = r
                .get("src")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let total = r.get("total").and_then(Value::as_i64).unwrap_or(0);
            let with_agent = r.get("with_agent").and_then(Value::as_i64).unwrap_or(0);
            by_source.insert(
                src,
                json!({
                    "total": total,
                    "with_agent": with_agent,
                    "agent_coverage": ratio(with_agent, total),
                }),
            );
        }

        // Per-dimension 7d rate breakdown (event_source / agent / top context_key) —
        // answers "which source/agent/context is degrading" (design doc §5.1).
        let cut7 = days_ago(now, 7);
        let by_dimension = json!({
            "event_source": { "agent_coverage": by_source,
                              "rates": self.dimension_rates("event_source", &cut7, 0)? },
            "agent": self.dimension_rates("agent", &cut7, 0)?,
            "context_key": self.dimension_rates("context_key", &cut7, 10)?,
        });

        // ── recall_pack (§5.1) ──
        let zombie: i64 = count_query_params(
            &self.storage,
            "SELECT COUNT(*) FROM chunks
             WHERE state!='archived' AND origin!='spark'
               AND selected_count >= ?1 AND used_count = 0",
            rusqlite::params![self.repeat_select_min],
        )?;
        // avg retrieved/selected per recall trace (recent usage_trace window).
        let avg_pack = self.storage.query_chunks_params(
            "SELECT
               AVG(r) AS avg_retrieved,
               AVG(s) AS avg_selected
             FROM (
               SELECT trace_id,
                 SUM(CASE WHEN event='retrieved' THEN 1 ELSE 0 END) AS r,
                 SUM(CASE WHEN event='selected'  THEN 1 ELSE 0 END) AS s
               FROM usage_trace WHERE ts >= ?1 GROUP BY trace_id)",
            rusqlite::params![cut7],
        )?;
        let avg_get = |k: &str| {
            avg_pack
                .first()
                .and_then(|r| r.get(k))
                .and_then(Value::as_f64)
                .map(|x| (x * 100.0).round() / 100.0)
                .unwrap_or(0.0)
        };
        // selected-but-never-used top offenders (zombie detail).
        let selected_unused_top = self.storage.query_chunks(
            "SELECT id, selected_count FROM chunks
             WHERE state!='archived' AND origin!='spark' AND used_count=0 AND selected_count>0
             ORDER BY selected_count DESC LIMIT 5",
        )?;
        // selected→used (chunk-level): fraction of ever-selected chunks never used.
        let sel_any: i64 = count_query(
            &self.storage,
            "SELECT COUNT(*) FROM chunks WHERE origin!='spark' AND selected_count>0",
        )?;
        let sel_unused: i64 = count_query(
            &self.storage,
            "SELECT COUNT(*) FROM chunks WHERE origin!='spark' AND selected_count>0 AND used_count=0",
        )?;
        // MRR proxy: mean reciprocal selected-rank of chunks that were actually used.
        let mrr_rows = self.storage.query_chunks_params(
            "SELECT AVG(1.0/ut.rank) AS mrr FROM usage_trace ut
             JOIN chunks c ON c.id = ut.chunk_id
             WHERE ut.event='selected' AND ut.rank IS NOT NULL AND ut.rank>0
               AND c.used_count>0 AND ut.ts >= ?1",
            rusqlite::params![cut7],
        )?;
        let used_rank_mrr = mrr_rows
            .first()
            .and_then(|r| r.get("mrr"))
            .and_then(Value::as_f64)
            .map(|x| (x * 1000.0).round() / 1000.0)
            .unwrap_or(0.0);
        // hook silence rate (§5.5): hook recalls returning known_none / all hook recalls.
        let hook_total: i64 = count_query_params(
            &self.storage,
            "SELECT COUNT(*) FROM episodic_log WHERE event_source='hook' AND ts >= ?1",
            rusqlite::params![cut7],
        )?;
        let hook_silent: i64 = count_query_params(
            &self.storage,
            "SELECT COUNT(*) FROM episodic_log
             WHERE event_source='hook' AND usage_state='known_none' AND ts >= ?1",
            rusqlite::params![cut7],
        )?;
        // selected rank distribution (§5.1): where in the shortlist selected chunks landed.
        let rank_hist = self.storage.query_chunks_params(
            "SELECT
               SUM(CASE WHEN rank=1 THEN 1 ELSE 0 END) AS r1,
               SUM(CASE WHEN rank BETWEEN 2 AND 3 THEN 1 ELSE 0 END) AS r2_3,
               SUM(CASE WHEN rank BETWEEN 4 AND 10 THEN 1 ELSE 0 END) AS r4_10,
               SUM(CASE WHEN rank > 10 THEN 1 ELSE 0 END) AS r11plus
             FROM usage_trace WHERE event='selected' AND rank IS NOT NULL AND ts >= ?1",
            rusqlite::params![cut7],
        )?;
        let rh = |k: &str| {
            rank_hist
                .first()
                .and_then(|r| r.get(k))
                .and_then(Value::as_i64)
                .unwrap_or(0)
        };
        // high-rank-unused anomaly: took a top-3 slot but the chunk was never used.
        let high_rank_unused = self.storage.query_chunks_params(
            "SELECT ut.chunk_id AS id, MIN(ut.rank) AS best_rank
             FROM usage_trace ut JOIN chunks c ON c.id = ut.chunk_id
             WHERE ut.event='selected' AND ut.rank<=3 AND c.used_count=0 AND ut.ts >= ?1
             GROUP BY ut.chunk_id ORDER BY best_rank LIMIT 5",
            rusqlite::params![cut7],
        )?;
        // low-rank-used anomaly: useful chunk that only surfaced deep (rank>10) — ranking
        // could promote it. Sample for debugging fused-score weights.
        let low_rank_used = self.storage.query_chunks_params(
            "SELECT ut.chunk_id AS id, MIN(ut.rank) AS best_rank
             FROM usage_trace ut JOIN chunks c ON c.id = ut.chunk_id
             WHERE ut.event='selected' AND ut.rank>10 AND c.used_count>0 AND ut.ts >= ?1
             GROUP BY ut.chunk_id ORDER BY best_rank DESC LIMIT 5",
            rusqlite::params![cut7],
        )?;

        // ── lifecycle (§5.1) ──
        let promotions: i64 = count_query_params(
            &self.storage,
            "SELECT COUNT(*) FROM chunks
             WHERE state='active' AND origin!='spark' AND state_updated_at >= ?1
               AND state_reason IN ('repeated_success','approved','restore')",
            rusqlite::params![cut7],
        )?;
        let evictions: i64 = count_query_params(
            &self.storage,
            "SELECT COUNT(*) FROM chunks
             WHERE state='archived' AND origin!='spark' AND state_updated_at >= ?1",
            rusqlite::params![cut7],
        )?;
        let pending_oldest = self.storage.query_chunks(
            "SELECT MIN(created_at) AS oldest FROM chunks WHERE state='pending' AND origin!='spark'",
        )?;
        let pending_oldest_ts = pending_oldest
            .first()
            .and_then(|r| r.get("oldest"))
            .and_then(Value::as_str)
            .map(|s| s.to_string());
        let gov_backlog = self.storage.query_chunks(
            "SELECT MIN(created_at) AS oldest FROM governance_proposals WHERE state='pending'",
        )?;
        let gov_backlog_oldest_ts = gov_backlog
            .first()
            .and_then(|r| r.get("oldest"))
            .and_then(Value::as_str)
            .map(|s| s.to_string());

        Ok(json!({
            "windows": windows,
            "by_dimension": by_dimension,
            "recall_pack": {
                "zombie_chunks": zombie,
                "avg_retrieved": avg_get("avg_retrieved"),
                "avg_selected": avg_get("avg_selected"),
                "selected_unused_rate": ratio(sel_unused, sel_any),
                "selected_unused_top": selected_unused_top,
                "used_rank_mrr": used_rank_mrr,
                "hook_silence_rate": ratio(hook_silent, hook_total),
                "selected_rank_distribution": {
                    "1": rh("r1"), "2-3": rh("r2_3"), "4-10": rh("r4_10"), "11+": rh("r11plus")
                },
                "high_rank_unused": high_rank_unused,
                "low_rank_used": low_rank_used,
            },
            "lifecycle": {
                "pending_oldest_ts": pending_oldest_ts,
                "governance_backlog_oldest_ts": gov_backlog_oldest_ts,
                "state_transition_approx": {
                    "promotions_7d": promotions,
                    "evictions_7d": evictions,
                    "note": "approx via state_updated_at/state_reason; not a strict rate"
                }
            }
        }))
    }

    /// Per-group rate breakdown over `episodic_log` newer than `cutoff`, grouped by
    /// `group_col` (event_source / agent / context_key). `top_n>0` caps to the busiest
    /// N groups (for high-cardinality context_key). NULL group → "(null)".
    fn dimension_rates(&self, group_col: &str, cutoff: &str, top_n: i64) -> Result<Value> {
        let limit = if top_n > 0 {
            format!(" ORDER BY recalls DESC LIMIT {top_n}")
        } else {
            String::new()
        };
        let sql = format!(
            "SELECT COALESCE({group_col},'(null)') AS g,
                    COUNT(*) AS recalls,
                    SUM(CASE WHEN usage_state='known_none' THEN 1 ELSE 0 END) AS empty,
                    SUM(CASE WHEN task_state='completed' THEN 1 ELSE 0 END) AS completed,
                    SUM(CASE WHEN outcome='ok' THEN 1 ELSE 0 END) AS ok,
                    SUM(CASE WHEN outcome IN ('ok','fail') THEN 1 ELSE 0 END) AS outcome_known,
                    SUM(CASE WHEN task_state='completed' AND usage_state!='unknown' THEN 1 ELSE 0 END) AS annotated
             FROM episodic_log WHERE ts >= ?1 GROUP BY {group_col}{limit}"
        );
        let rows = self.storage.query_chunks_params(&sql, rusqlite::params![cutoff])?;

        // selected→used per group: join usage_trace back to episodic_log for the dimension.
        let su_sql = format!(
            "SELECT COALESCE(el.{group_col},'(null)') AS g,
                    SUM(CASE WHEN ut.event='selected' THEN 1 ELSE 0 END) AS sel,
                    SUM(CASE WHEN ut.event='used' THEN 1 ELSE 0 END) AS used
             FROM usage_trace ut JOIN episodic_log el ON el.trace_id = ut.trace_id
             WHERE ut.ts >= ?1 GROUP BY el.{group_col}"
        );
        let su_rows = self.storage.query_chunks_params(&su_sql, rusqlite::params![cutoff])?;
        let mut su_map: std::collections::HashMap<String, (i64, i64)> = std::collections::HashMap::new();
        for r in &su_rows {
            let g = r.get("g").and_then(Value::as_str).unwrap_or("(null)").to_string();
            su_map.insert(
                g,
                (
                    r.get("sel").and_then(Value::as_i64).unwrap_or(0),
                    r.get("used").and_then(Value::as_i64).unwrap_or(0),
                ),
            );
        }
        // feedback coverage per group.
        let fb_sql = format!(
            "SELECT COALESCE(el.{group_col},'(null)') AS g,
                    COUNT(DISTINCT fe.trace_id) AS fb
             FROM feedback_events fe JOIN episodic_log el ON el.trace_id = fe.trace_id
             WHERE fe.ts >= ?1 GROUP BY el.{group_col}"
        );
        let fb_rows = self.storage.query_chunks_params(&fb_sql, rusqlite::params![cutoff])?;
        let mut fb_map: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        for r in &fb_rows {
            let g = r.get("g").and_then(Value::as_str).unwrap_or("(null)").to_string();
            fb_map.insert(g, r.get("fb").and_then(Value::as_i64).unwrap_or(0));
        }

        let mut out = serde_json::Map::new();
        for r in &rows {
            let g = r.get("g").and_then(Value::as_str).unwrap_or("(null)").to_string();
            let i = |k: &str| r.get(k).and_then(Value::as_i64).unwrap_or(0);
            let recalls = i("recalls");
            let completed = i("completed");
            let (sel, used) = su_map.get(&g).copied().unwrap_or((0, 0));
            let fb = fb_map.get(&g).copied().unwrap_or(0);
            out.insert(
                g,
                json!({
                    "recalls": recalls,
                    "empty_recall_rate": ratio(i("empty"), recalls),
                    "completed_rate": ratio(completed, recalls),
                    "task_success_rate": ratio(i("ok"), i("outcome_known")),
                    "usage_annotation_rate": ratio(i("annotated"), completed),
                    "selected_to_used_rate": ratio(used, sel),
                    "feedback_coverage": ratio(fb, completed),
                }),
            );
        }
        Ok(Value::Object(out))
    }

    /// Full rate metrics over `episodic_log` rows newer than `cutoff` (§5.1). The
    /// selected→used and feedback-coverage signals come from usage_trace/feedback_events
    /// (two cheap supplementary windowed scans), the rest from a single episodic_log scan.
    fn window_rates(&self, cutoff: &str) -> Result<Value> {
        let rows = self.storage.query_chunks_params(
            "SELECT COUNT(*) AS recalls,
                    SUM(CASE WHEN usage_state='known_none' THEN 1 ELSE 0 END) AS empty,
                    SUM(CASE WHEN task_state='completed' THEN 1 ELSE 0 END) AS completed,
                    SUM(CASE WHEN task_state='timed_out' THEN 1 ELSE 0 END) AS timed_out,
                    SUM(CASE WHEN outcome='ok' THEN 1 ELSE 0 END) AS ok,
                    SUM(CASE WHEN outcome IN ('ok','fail') THEN 1 ELSE 0 END) AS outcome_known,
                    SUM(CASE WHEN task_state='completed' AND usage_state!='unknown' THEN 1 ELSE 0 END) AS annotated
             FROM episodic_log WHERE ts >= ?1",
            rusqlite::params![cutoff],
        )?;
        let g = |k: &str| {
            rows.first()
                .and_then(|r| r.get(k))
                .and_then(Value::as_i64)
                .unwrap_or(0)
        };
        // selected→used (event counts) over the same window.
        let su = self.storage.query_chunks_params(
            "SELECT SUM(CASE WHEN event='selected' THEN 1 ELSE 0 END) AS sel,
                    SUM(CASE WHEN event='used' THEN 1 ELSE 0 END) AS used
             FROM usage_trace WHERE ts >= ?1",
            rusqlite::params![cutoff],
        )?;
        let sug = |k: &str| {
            su.first()
                .and_then(|r| r.get(k))
                .and_then(Value::as_i64)
                .unwrap_or(0)
        };
        let fb_traces: i64 = count_query_params(
            &self.storage,
            "SELECT COUNT(DISTINCT trace_id) FROM feedback_events WHERE ts >= ?1",
            rusqlite::params![cutoff],
        )?;
        let recalls = g("recalls");
        let completed = g("completed");
        Ok(json!({
            "recalls": recalls,
            "empty_recall_rate": ratio(g("empty"), recalls),
            "completed_rate": ratio(completed, recalls),
            "timeout_rate": ratio(g("timed_out"), recalls),
            "task_success_rate": ratio(g("ok"), g("outcome_known")),
            "usage_annotation_rate": ratio(g("annotated"), completed),
            "selected_to_used_rate": ratio(sug("used"), sug("sel")),
            "feedback_coverage": ratio(fb_traces, completed),
        }))
    }

    /// P1 daemon health (independent read-only connection) + P3a op aggregation.
    fn operational_block(&self, now: &str) -> Result<Value> {
        let daemon = crate::daemon::health(
            &crate::paths::daemon_state_path(),
            &crate::paths::daemon_pid_path(),
            now,
        );
        let mut block = serde_json::Map::new();
        block.insert("daemon".to_string(), daemon);
        // Omit `ops` entirely until operation_runs has data (table exists from 4.20 but
        // may be empty before instrumentation lands / on a fresh db).
        if self.storage.count_operation_runs().unwrap_or(0) > 0 {
            let rows = self.storage.operation_runs_since(&days_ago(now, 7))?;
            block.insert(
                "ops".to_string(),
                crate::storage::metrics::aggregate_ops(&rows),
            );
        }
        Ok(Value::Object(block))
    }

    /// P3b trend block: current snapshot + delta vs the nearest snapshot ≥7d old.
    /// Returns None when no snapshot exists yet (block omitted from inspect).
    fn trends_block(&self, now: &str) -> Result<Option<Value>> {
        let Some((cur_ts, cur_json)) = self.storage.latest_snapshot()? else {
            return Ok(None);
        };
        let cur: Value = serde_json::from_str(&cur_json).unwrap_or_else(|_| json!({}));
        let mut out = json!({ "current_ts": cur_ts, "current": cur.clone() });
        if let Some((base_ts, base_json)) =
            self.storage.snapshot_at_or_before(&days_ago(now, 7))?
        {
            let base: Value = serde_json::from_str(&base_json).unwrap_or_else(|_| json!({}));
            let mut delta = serde_json::Map::new();
            if let (Some(c), Some(b)) = (cur.as_object(), base.as_object()) {
                for (k, cv) in c {
                    if let (Some(cf), Some(bf)) = (cv.as_f64(), b.get(k).and_then(Value::as_f64)) {
                        delta.insert(k.clone(), json!(((cf - bf) * 1000.0).round() / 1000.0));
                    }
                }
            }
            out["baseline_ts"] = json!(base_ts);
            out["delta_vs_7d"] = Value::Object(delta);
        }
        Ok(Some(out))
    }

    /// Fused-score weights in effect, as a JSON object (for the recall-eval params
    /// snapshot so an eval run is self-describing and comparable over time).
    pub fn recall_weights(&self) -> Value {
        json!({
            "w_content": self.w_content,
            "w_trigger": self.w_trigger,
            "w_lexical": self.w_lexical,
            "w_context": self.w_context,
            "w_activation": self.w_activation,
            "w_spread": self.w_spread,
        })
    }

    /// Write a state-KPI snapshot row now and return the KPIs (CLI `metrics snapshot`).
    pub fn write_metric_snapshot(&self) -> Result<Value> {
        let now = utc_now_iso();
        let kpis = self.collect_kpis(&now)?;
        self.storage.insert_metric_snapshot(&now, &kpis.to_string())?;
        Ok(kpis)
    }

    /// State-KPI snapshot payload for `metric_snapshots` (written by curate at cycle
    /// end). Only numeric, stable KPIs — these can't be reconstructed from event logs.
    pub(crate) fn collect_kpis(&self, now: &str) -> Result<Value> {
        let active: i64 = count_query(
            &self.storage,
            "SELECT COUNT(*) FROM chunks WHERE state='active' AND origin!='spark'",
        )?;
        let pending: i64 = count_query(
            &self.storage,
            "SELECT COUNT(*) FROM chunks WHERE state='pending' AND origin!='spark'",
        )?;
        let zombie: i64 = count_query_params(
            &self.storage,
            "SELECT COUNT(*) FROM chunks
             WHERE state!='archived' AND origin!='spark'
               AND selected_count >= ?1 AND used_count = 0",
            rusqlite::params![self.repeat_select_min],
        )?;
        let debt_ratio = if active > 0 {
            (pending as f64 / active as f64 * 100.0).round() / 100.0
        } else {
            pending as f64
        };
        let w7 = self.window_rates(&days_ago(now, 7))?;
        let daemon = crate::daemon::health(
            &crate::paths::daemon_state_path(),
            &crate::paths::daemon_pid_path(),
            now,
        );
        Ok(json!({
            "active": active,
            "pending": pending,
            "knowledge_debt_ratio": debt_ratio,
            "zombie_chunks": zombie,
            "task_success_rate_7d": w7.get("task_success_rate").and_then(Value::as_f64).unwrap_or(0.0),
            "empty_recall_rate_7d": w7.get("empty_recall_rate").and_then(Value::as_f64).unwrap_or(0.0),
            "daemon_errors_24h": daemon.get("errors_24h").and_then(Value::as_i64).unwrap_or(0),
        }))
    }

    // ------------------------------------------------------------------
    // Intuition honesty metrics (PRD §4 / Spec §7)
    //
    // The core KPI is not recall but discrimination quality: "loud when it should
    // be, silent when it shouldn't." All inputs already exist — appraise persists
    // {valence, tier, strength} into episodic_log.recall_snapshot, and record fills
    // in `outcome`. We bucket appraisals by tier and check the actual task_ok rate.
    // ------------------------------------------------------------------

    fn intuition_calibration(&self, window_start: &str) -> Result<Value> {
        let rows = self.storage.query_chunks_params(
            "SELECT recall_snapshot, outcome FROM episodic_log
             WHERE ts >= ? AND recall_snapshot LIKE '%\"appraise\"%'",
            rusqlite::params![window_start],
        )?;

        // Per-tier accumulators: (n_total, n_with_outcome, ok, sum_strength_with_outcome).
        let mut buckets: std::collections::BTreeMap<String, [f64; 4]> =
            std::collections::BTreeMap::new();
        for tier in ["weak", "medium", "strong"] {
            buckets.insert(tier.to_string(), [0.0; 4]);
        }
        let mut total = 0_i64;
        let mut silent = 0_i64;
        let mut caution_strong = 0_i64;
        let mut caution_strong_false = 0_i64;

        for row in &rows {
            let snapshot = row
                .get("recall_snapshot")
                .and_then(Value::as_str)
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
            let Some(appraise) = snapshot.as_ref().and_then(|s| s.get("appraise")) else {
                continue;
            };
            let tier = appraise
                .get("tier")
                .and_then(Value::as_str)
                .unwrap_or("weak");
            let valence = appraise
                .get("valence")
                .and_then(Value::as_str)
                .unwrap_or("neutral");
            let strength = appraise
                .get("strength")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let outcome = row.get("outcome").and_then(Value::as_str);

            total += 1;
            if tier == "weak" || valence == "neutral" {
                silent += 1;
            }
            let has_outcome = matches!(outcome, Some("ok") | Some("fail"));
            let is_ok = outcome == Some("ok");
            if let Some(b) = buckets.get_mut(tier) {
                b[0] += 1.0;
                if has_outcome {
                    b[1] += 1.0;
                    b[3] += strength;
                    if is_ok {
                        b[2] += 1.0;
                    }
                }
            }
            if valence == "caution" && tier == "strong" && has_outcome {
                caution_strong += 1;
                if is_ok {
                    caution_strong_false += 1;
                }
            }
        }

        let hit_rate = |b: &[f64; 4]| if b[1] > 0.0 { b[2] / b[1] } else { 0.0 };
        let weak = buckets.get("weak").copied().unwrap_or([0.0; 4]);
        let strong = buckets.get("strong").copied().unwrap_or([0.0; 4]);
        let monotonicity_gap = hit_rate(&strong) - hit_rate(&weak);

        // ECE: evidence-weighted gap between mean strength and actual hit rate per bucket.
        let outcome_total: f64 = buckets.values().map(|b| b[1]).sum();
        let ece = if outcome_total > 0.0 {
            buckets
                .values()
                .filter(|b| b[1] > 0.0)
                .map(|b| {
                    let avg_strength = b[3] / b[1];
                    (b[1] / outcome_total) * (avg_strength - hit_rate(b)).abs()
                })
                .sum::<f64>()
        } else {
            0.0
        };

        let bucket_detail: Vec<Value> = ["weak", "medium", "strong"]
            .iter()
            .map(|tier| {
                let b = buckets.get(*tier).copied().unwrap_or([0.0; 4]);
                json!({
                    "tier": tier,
                    "n": b[0] as i64,
                    "n_with_outcome": b[1] as i64,
                    "avg_strength": if b[1] > 0.0 { (b[3] / b[1] * 1000.0).round() / 1000.0 } else { 0.0 },
                    "actual_hit_rate": (hit_rate(&b) * 1000.0).round() / 1000.0,
                })
            })
            .collect();

        // 方案 B —— verdict_log 仪表盘:可证伪的 ECE / 弃权率(头号体检指标)。
        // 与上面基于 recall_snapshot 的 tier-bucket 指标互补:verdict_log 直接用
        // emitted_conf 分桶 + observed 回填算 ECE,且把弃权率作为一等健康信号。
        let (vl_total, vl_abstained, vl_observed) =
            self.storage.verdict_log_overview().unwrap_or((0, 0, 0));
        let samples = self.storage.verdict_calibration_samples().unwrap_or_default();
        let bins = self.calibration_bins.max(2);
        let mut vhit = vec![0.0_f64; bins as usize];
        let mut vtot = vec![0.0_f64; bins as usize];
        // ECE 按 **emitted_conf** 分桶:衡量「声称置信度」的真实兑现率(校准映射重算
        // 则按 strength 分桶,见 curate::recompute_calibration_map —— 两者域不同)。
        for (_strength, conf, h) in &samples {
            let b = ((conf * bins as f64).floor() as i64).clamp(0, bins - 1) as usize;
            vtot[b] += 1.0;
            vhit[b] += *h;
        }
        let n_obs: f64 = vtot.iter().sum();
        let verdict_ece = if n_obs > 0.0 {
            (0..bins as usize)
                .filter(|&b| vtot[b] > 0.0)
                .map(|b| {
                    let claimed = (b as f64 + 0.5) / bins as f64;
                    let actual = vhit[b] / vtot[b];
                    (vtot[b] / n_obs) * (claimed - actual).abs()
                })
                .sum::<f64>()
        } else {
            0.0
        };

        Ok(json!({
            "appraisals": total,
            "monotonicity_gap": (monotonicity_gap * 1000.0).round() / 1000.0,
            "ece": (ece * 1000.0).round() / 1000.0,
            "false_alarm_rate": ratio(caution_strong_false, caution_strong),
            "silence_rate": ratio(silent, total),
            "buckets": bucket_detail,
            // 方案 B verdict_log 仪表盘
            "verdict_log": {
                "total": vl_total,
                "abstained": vl_abstained,
                "abstain_rate": ratio(vl_abstained, vl_total),
                "observed": vl_observed,
                "ece": (verdict_ece * 1000.0).round() / 1000.0,
            },
        }))
    }

    // ------------------------------------------------------------------
    // Public: rebuild_embeddings (evolve --rebuild-embeddings)
    // ------------------------------------------------------------------

    pub fn rebuild_embeddings(&self) -> Result<usize> {
        Ok(self.rebuild_embeddings_capped(None)?.0)
    }

    /// Bounded re-embed used by latency-sensitive callers (MCP/CLI `evolve
    /// --rebuild-embeddings`). When `max` is `Some(n)` at most `n` stale chunks
    /// are re-embedded this call, so a large backlog is chipped away across
    /// successive evolves instead of blocking one request on the whole queue
    /// (each re-embed is a network LLM round-trip). Returns
    /// `(rebuilt_this_call, remaining_stale_after)`. `None` rebuilds everything.
    pub fn rebuild_embeddings_capped(&self, max: Option<usize>) -> Result<(usize, usize)> {
        let meta_version = self
            .storage
            .get_meta("embed_version")?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(1);
        // Fetch chunks with embed_version=0 (failed writes) or below current meta version.
        let mut stale = self.storage.query_chunks_params(
            "SELECT id, content, trigger_desc, state_reason FROM chunks
             WHERE embed_version = 0 OR embed_version < ?",
            rusqlite::params![meta_version],
        )?;
        // Bound the batch: keep the first `max`, report the rest as remaining.
        let total_stale = stale.len();
        let remaining = match max {
            Some(n) if n < total_stale => {
                stale.truncate(n);
                total_stale - n
            }
            _ => 0,
        };
        // Bulk re-embed: drop the warm cache once so the per-row in-place upserts
        // stay no-ops (cold) and the loop runs O(N) instead of O(N²). The next
        // search reloads the rebuilt vectors from disk.
        self.storage.invalidate_vector_caches();
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

            let (cvec_res, tvec_res) = self.embed_pair(content, trigger, "rebuild");
            let cvec = match cvec_res {
                Ok(v) => v,
                Err(_) => continue,
            };
            let tvec = match tvec_res {
                Ok(v) => v,
                Err(_) => continue,
            };

            self.storage.begin_immediate()?;
            let r = (|| -> Result<()> {
                self.store_vec_content(id, &cvec)?;
                self.store_vec_trigger(id, &tvec)?;
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
        Ok((count, remaining))
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
