//! Observability blocks (design doc §5.1 / §5.6), state-KPI snapshots and
//! the `metrics delta` view. Extracted from `inspection/mod.rs`, which grew
//! past the repo's per-file line limit.

use super::super::*;

impl KnowledgeBase {
    // ------------------------------------------------------------------
    // Observability blocks (design doc §5.1 / §5.6)
    // ------------------------------------------------------------------

    /// P1 derived metrics: multi-window rates, per-channel agent coverage,
    /// zombie chunks, and approximate lifecycle transitions. Pure SQL, no new tables.
    pub(super) fn observability_block(&self, now: &str) -> Result<Value> {
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
    pub(super) fn dimension_rates(&self, group_col: &str, cutoff: &str, top_n: i64) -> Result<Value> {
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
    pub(super) fn window_rates(&self, cutoff: &str) -> Result<Value> {
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
    pub(super) fn operational_block(&self, now: &str) -> Result<Value> {
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
    pub(super) fn trends_block(&self, now: &str) -> Result<Option<Value>> {
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

    /// KPI baseline vs now, as a flat row list ready for a table (CLI `metrics`).
    ///
    /// `inspect.trends` answers the same question with a fixed 7-day window and
    /// buries it inside a large document. Tuning work asks it repeatedly, with
    /// different windows, and wants only the moved numbers — so this is its own
    /// entry point.
    ///
    /// A metric present now but missing from the baseline (added by a newer
    /// build) reports a null delta rather than pretending the old value was 0.
    pub fn metrics_delta(&self, days: i64) -> Result<Value> {
        let now = utc_now_iso();
        let Some((cur_ts, cur_json)) = self.storage.latest_snapshot()? else {
            return Ok(json!({
                "current_ts": Value::Null,
                "baseline_ts": Value::Null,
                "baseline": Value::Null,
                "window_days": days,
                "metrics": [],
            }));
        };
        let cur: Value = serde_json::from_str(&cur_json).unwrap_or_else(|_| json!({}));
        let baseline = self
            .storage
            .snapshot_at_or_before(&days_ago(&now, days.max(0)))?;
        let (base_ts, base) = match baseline {
            Some((ts, raw)) => (
                json!(ts),
                serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| json!({})),
            ),
            None => (Value::Null, Value::Null),
        };

        let mut metrics = Vec::new();
        if let Some(cur_obj) = cur.as_object() {
            for (key, cur_val) in cur_obj {
                let base_val = base.get(key).cloned().unwrap_or(Value::Null);
                let delta = match (cur_val.as_f64(), base_val.as_f64()) {
                    (Some(c), Some(b)) => json!(((c - b) * 1000.0).round() / 1000.0),
                    _ => Value::Null,
                };
                metrics.push(json!({
                    "metric": key,
                    "baseline": base_val,
                    "current": cur_val,
                    "delta": delta,
                }));
            }
        }
        metrics.sort_by(|a, b| a["metric"].as_str().cmp(&b["metric"].as_str()));
        Ok(json!({
            "current_ts": cur_ts,
            "baseline_ts": base_ts,
            "baseline": base,
            "window_days": days,
            "metrics": metrics,
        }))
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
        // Background cost. `evolve`/`curate` runs are the dominant CPU draw when
        // the scheduler is misconfigured (39,383 of each in one 30-day window,
        // ~22 CPU-hours, for 414 distilled chunks) and nothing surfaced it.
        let day_ago = days_ago(now, 1);
        let op_runs_24h = |op: &str| -> i64 {
            count_query_params(
                &self.storage,
                "SELECT COUNT(*) FROM operation_runs WHERE op=?1 AND started_at >= ?2",
                rusqlite::params![op, day_ago],
            )
            .unwrap_or(0)
        };
        let record_total_24h = op_runs_24h("record");
        let record_errors_24h = count_query_params(
            &self.storage,
            "SELECT COUNT(*) FROM operation_runs
             WHERE op='record' AND status!='ok' AND started_at >= ?1",
            rusqlite::params![day_ago],
        )
        .unwrap_or(0);
        // Latency the user actually feels: the recall hook runs in front of
        // every prompt.
        let hook_p95_ms = self
            .storage
            .query_chunks_params(
                "SELECT duration_ms FROM operation_runs
                 WHERE op='hook_recall' AND started_at >= ?1
                 ORDER BY duration_ms",
                rusqlite::params![days_ago(now, 7)],
            )
            .map(|rows| {
                let d: Vec<i64> = rows
                    .iter()
                    .filter_map(|r| r.get("duration_ms").and_then(Value::as_i64))
                    .collect();
                if d.is_empty() {
                    0
                } else {
                    d[(((d.len() - 1) as f64) * 0.95).round() as usize]
                }
            })
            .unwrap_or(0);
        let llm_ok_24h = crate::llm_trace::health(24)
            .ok()
            .and_then(|h| {
                h.pointer("/by_kind/chat/success_rate")
                    .and_then(Value::as_f64)
            })
            .unwrap_or(-1.0);
        let db_size_mb = self
            .storage
            .db_size_bytes()
            .map(|b| (b as f64 / 1_048_576.0 * 100.0).round() / 100.0)
            .unwrap_or(0.0);

        Ok(json!({
            "active": active,
            "pending": pending,
            "knowledge_debt_ratio": debt_ratio,
            "zombie_chunks": zombie,
            "task_success_rate_7d": w7.get("task_success_rate").and_then(Value::as_f64).unwrap_or(0.0),
            "empty_recall_rate_7d": w7.get("empty_recall_rate").and_then(Value::as_f64).unwrap_or(0.0),
            "daemon_errors_24h": daemon.get("errors_24h").and_then(Value::as_i64).unwrap_or(0),
            // ── Added so a re-test can measure each lever directly ──
            // Feedback loop (B): was 46.1% timeout / 2.5% selected→used.
            "trace_timeout_rate_7d": w7.get("timeout_rate").and_then(Value::as_f64).unwrap_or(0.0),
            "completed_rate_7d": w7.get("completed_rate").and_then(Value::as_f64).unwrap_or(0.0),
            "selected_to_used_rate_7d": w7.get("selected_to_used_rate").and_then(Value::as_f64).unwrap_or(0.0),
            "feedback_coverage_7d": w7.get("feedback_coverage").and_then(Value::as_f64).unwrap_or(0.0),
            // record rejection (B1): was 16% of MCP records.
            "record_error_rate_24h": ratio(record_errors_24h, record_total_24h),
            // Background cost (A1/A2): was ~1,300 evolve + 1,300 curate per day.
            "evolve_runs_24h": op_runs_24h("evolve"),
            "curate_runs_24h": op_runs_24h("curate"),
            "hook_recall_runs_24h": op_runs_24h("hook_recall"),
            // Hook latency (C3): was p95 ≈ 2.4 s with a 12 s tail.
            "hook_recall_p95_ms_7d": hook_p95_ms,
            // Endpoint health (A3/D3): -1 = no calls in the window.
            "llm_chat_success_rate_24h": llm_ok_24h,
            // Storage (A5): was 52.76 MB.
            "db_size_mb": db_size_mb,
        }))
    }
}
