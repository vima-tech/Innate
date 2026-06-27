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

        Ok(json!({
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
