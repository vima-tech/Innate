use super::*;

impl KnowledgeBase {
    pub(crate) fn builtin_curate_impl(&self, scope: &CurateScope) -> Result<CurateReport> {
        self.measure("curate", None, None, || self.builtin_curate_inner(scope))
    }

    fn builtin_curate_inner(&self, scope: &CurateScope) -> Result<CurateReport> {
        let mut report = CurateReport::default();
        let now_iso = utc_now_iso();
        if scope.dry_run {
            // dry_run: compute report without writing
            let archived_count: i64 = count_query(&self.storage,
                "SELECT COUNT(*) FROM chunks WHERE origin!='spark' AND protected=0 AND state='active'")?;
            report.stats.insert("dry_run".to_string(), json!(true));
            report
                .stats
                .insert("eligible_for_governance".to_string(), json!(archived_count));
            return Ok(report);
        }

        // ── Step 1-4: aggregate (single BEGIN IMMEDIATE, half-open cutoff window) ──
        self.storage.begin_immediate()?;
        let agg_result = (|| -> Result<()> {
            let cutoff_ts = now_iso.clone();

            // 1. Rebuild post-4.12 success facts from retained attribution events.
            self.storage
                .conn_execute("DELETE FROM chunk_success_traces", rusqlite::params![])?;
            self.storage.conn_execute(
                "INSERT OR IGNORE INTO chunk_success_traces(chunk_id, trace_id, ts)
                 SELECT ut.chunk_id, ut.trace_id, MAX(ut.ts)
                 FROM usage_trace ut
                 WHERE ut.event = 'used'
                   AND ut.chunk_id IS NOT NULL
                   AND ut.ts > COALESCE((
                     SELECT evidence_cutoff_at FROM chunks c
                     WHERE c.id=ut.chunk_id
                   ), '')
                   AND (
                     EXISTS (SELECT 1 FROM usage_trace ok
                             WHERE ok.trace_id = ut.trace_id
                               AND ok.event = 'task_ok' AND ok.chunk_id IS NULL)
                     OR EXISTS (SELECT 1 FROM episodic_log el
                                WHERE el.trace_id = ut.trace_id AND el.outcome = 'ok')
                   )
                 GROUP BY ut.chunk_id, ut.trace_id",
                rusqlite::params![],
            )?;

            // 2. Derive success counts from migration baseline + replayable facts.
            self.storage.conn_execute(
                "WITH cst AS (
                   SELECT chunk_id, COUNT(*) AS cnt, MAX(ts) AS max_ts
                   FROM chunk_success_traces
                   GROUP BY chunk_id
                 )
                 UPDATE chunks SET
                   used_success_count = used_success_count_base
                     + COALESCE((SELECT cnt FROM cst WHERE cst.chunk_id=chunks.id), 0),
                   success_trace_ids_count = used_success_count_base
                     + COALESCE((SELECT cnt FROM cst WHERE cst.chunk_id=chunks.id), 0),
                   last_success_at = COALESCE(
                     (SELECT max_ts FROM cst WHERE cst.chunk_id=chunks.id),
                     last_success_at
                   )
                 WHERE origin!='spark'",
                rusqlite::params![],
            )?;

            // 3. Recompute selected/used counts and last-use from retained facts.
            self.storage.conn_execute(
                "UPDATE chunks SET
                   selected_count = selected_count_base + COALESCE(
                     (SELECT COUNT(*) FROM usage_trace
                      WHERE chunk_id = chunks.id AND event = 'selected'
                        AND ts > COALESCE(chunks.evidence_cutoff_at, '')), 0),
                   used_count = used_count_base + COALESCE(
                     (SELECT COUNT(*) FROM usage_trace
                      WHERE chunk_id = chunks.id AND event = 'used'
                        AND ts > COALESCE(chunks.evidence_cutoff_at, '')), 0),
                   last_used_at = COALESCE(
                     (SELECT MAX(ts) FROM usage_trace
                      WHERE chunk_id=chunks.id AND event='used'
                        AND ts > COALESCE(chunks.evidence_cutoff_at, '')),
                     last_used_base
                   )
                 WHERE origin!='spark'",
                rusqlite::params![],
            )?;

            // 4. Advance watermark and purge only verbose retrieval events.
            // Attributed facts remain replayable so repeated curate runs are idempotent
            // and a later correction can subtract the previous trace contribution.
            self.storage.set_meta("last_agg_ts", &cutoff_ts)?;
            self.storage.purge_usage_trace(&cutoff_ts)?;
            self.storage.commit()
        })();
        if agg_result.is_err() {
            let _ = self.storage.rollback();
            agg_result?;
        }

        // ── Step 2: recover_logs ──
        self.storage.begin_immediate()?;
        let recover_result = (|| -> Result<()> {
            // Stale screening rows → 'failed' (not 'open'), note = 'screening_timeout:<run_id>'.
            let screening_cutoff = minutes_ago(&now_iso, self.screening_timeout_minutes);
            let stale = self.storage.query_chunks_params(
                "SELECT id, distill_run_id FROM episodic_log
                 WHERE distill_state='screening' AND distill_locked_at < ?",
                rusqlite::params![screening_cutoff],
            )?;
            for row in &stale {
                let id = row.get("id").and_then(Value::as_str).unwrap_or("");
                let run_id = row
                    .get("distill_run_id")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let note = format!("screening_timeout:{run_id}");
                self.storage.conn_execute(
                    "UPDATE episodic_log
                 SET distill_state='failed', distill_note=?,
                     distill_attempts=distill_attempts+1,
                     distill_last_failed_at=?,
                     distill_run_id=NULL, distill_locked_at=NULL
                 WHERE id=?",
                    rusqlite::params![note, now_iso, id],
                )?;
                report.recovered.push(id.to_string());
                report
                    .warnings
                    .push(format!("stale screening recovered as failed: {id}"));
            }

            // Open logs past TTL → discarded (insufficient material, never record'd).
            let open_ttl_cutoff = days_ago(&now_iso, self.open_ttl_days);
            self.storage.conn_execute(
                "UPDATE episodic_log
                 SET distill_state='discarded', distill_note='no_record_timeout',
                     task_state='timed_out', completed_at=?
                 WHERE distill_state='open' AND ts < ?",
                rusqlite::params![now_iso, open_ttl_cutoff],
            )?;
            self.storage.commit()
        })();
        if recover_result.is_err() {
            let _ = self.storage.rollback();
            recover_result?;
        }

        // ── Steps 3-7: governance (archive, dedupe, decay, promote, cycle) ──
        let scope_origin = scope.origin.clone();
        let scope_skill = scope.skill_name.clone();
        self.storage.begin_immediate()?;
        let gov_result = (|| -> Result<()> {
            // New feedback refreshes its chunk synchronously in Record. Curate only needs
            // to age pending proposals; scanning every historical feedback row is unbounded.
            let governance_chunks = self.storage.query_chunks(
                "SELECT DISTINCT chunk_id FROM governance_proposals WHERE state='pending'",
            )?;
            for row in governance_chunks {
                if let Some(chunk_id) = row.get("chunk_id").and_then(Value::as_str) {
                    self.refresh_governance_evidence(chunk_id, &now_iso)?;
                }
            }

            // ── 3a. Archive: low_confidence — only blocks that HAVE been used ──
            let low_conf_cutoff = days_ago(&now_iso, self.low_conf_idle_days);
            let low_conf = self.storage.query_chunks_params(
                "SELECT id FROM chunks
                 WHERE origin!='spark' AND protected=0 AND state IN ('active','pending')
                   AND last_used_at IS NOT NULL
                   AND confidence < ?
                   AND last_used_at < ?
                   AND (? IS NULL OR origin=?)
                   AND (? IS NULL OR skill_name=?)",
                rusqlite::params![
                    self.low_conf_threshold,
                    low_conf_cutoff,
                    scope_origin,
                    scope_origin,
                    scope_skill,
                    scope_skill
                ],
            )?;
            for c in &low_conf {
                if let Some(id) = c.get("id").and_then(Value::as_str) {
                    self.storage.update_chunk_state(
                        id,
                        "archived",
                        Some("low_confidence"),
                        &now_iso,
                    )?;
                    report.archived.push(id.to_string());
                }
            }

            // ── 3b. Archive: repeated_selected_unused ──
            let rep_sel = self.storage.query_chunks_params(
                "SELECT id FROM chunks
                 WHERE origin!='spark' AND protected=0 AND state IN ('active','pending')
                   AND selected_count >= ? AND used_count = 0 AND confidence < ?
                   AND (? IS NULL OR origin=?)
                   AND (? IS NULL OR skill_name=?)",
                rusqlite::params![
                    self.repeat_select_min,
                    self.repeat_select_conf_max,
                    scope_origin,
                    scope_origin,
                    scope_skill,
                    scope_skill
                ],
            )?;
            for c in &rep_sel {
                if let Some(id) = c.get("id").and_then(Value::as_str) {
                    if !report.archived.contains(&id.to_string()) {
                        self.storage.update_chunk_state(
                            id,
                            "archived",
                            Some("repeated_selected_unused"),
                            &now_iso,
                        )?;
                        report.archived.push(id.to_string());
                    }
                }
            }

            // ── 3c. Archive: never_used — no successful use before the age limit ──
            let never_used_cutoff = days_ago(&now_iso, self.never_used_age_days);
            let never_used = self.storage.query_chunks_params(
                "SELECT id FROM chunks
                 WHERE origin!='spark' AND protected=0 AND state IN ('active','pending')
                   AND used_count = 0
                   AND COALESCE(evidence_cutoff_at, created_at) < ?
                   AND (? IS NULL OR origin=?)
                   AND (? IS NULL OR skill_name=?)",
                rusqlite::params![
                    never_used_cutoff,
                    scope_origin,
                    scope_origin,
                    scope_skill,
                    scope_skill
                ],
            )?;
            for c in &never_used {
                if let Some(id) = c.get("id").and_then(Value::as_str) {
                    if !report.archived.contains(&id.to_string()) {
                        self.storage.update_chunk_state(
                            id,
                            "archived",
                            Some("never_used"),
                            &now_iso,
                        )?;
                        report.archived.push(id.to_string());
                    }
                }
            }

            // ── 3d. Archive: governance_proposal ──
            // Chunks whose pending governance proposals have accumulated enough evidence
            // are archived and the proposals accepted atomically.
            let gov_proposals = self.storage.query_chunks_params(
                "SELECT DISTINCT chunk_id FROM governance_proposals
                 WHERE state='pending'
                   AND evidence_score >= ? AND actor_count >= 2",
                rusqlite::params![self.governance_archive_threshold as f64],
            )?;
            for c in &gov_proposals {
                if let Some(cid) = c.get("chunk_id").and_then(Value::as_str) {
                    let already_archived = report.archived.contains(&cid.to_string());
                    let eligible = !already_archived
                        && self
                            .storage
                            .get_chunk(cid)?
                            .map(|ch| {
                                ch.get("origin").and_then(Value::as_str) != Some("spark")
                                    && ch.get("protected").and_then(Value::as_i64).unwrap_or(0) == 0
                                    && matches!(
                                        ch.get("state").and_then(Value::as_str),
                                        Some("active") | Some("pending")
                                    )
                            })
                            .unwrap_or(false);
                    if eligible {
                        self.storage.update_chunk_state(
                            cid,
                            "archived",
                            Some("governance_proposal"),
                            &now_iso,
                        )?;
                        report.archived.push(cid.to_string());
                        self.storage.conn_execute(
                            "UPDATE governance_proposals
                             SET state='accepted', updated_at=?
                             WHERE chunk_id=? AND state='pending'",
                            rusqlite::params![now_iso, cid],
                        )?;
                    } else {
                        self.storage.conn_execute(
                            "UPDATE governance_proposals
                             SET state='rejected', updated_at=?
                             WHERE chunk_id=? AND state='pending'",
                            rusqlite::params![now_iso, cid],
                        )?;
                    }
                }
            }

            // ── 3d2. Expire stale governance proposals (insufficient evidence, too old) ──
            // Proposals that never accumulate enough evidence cause repeated evolve cycles.
            // Reject them after governance_proposal_max_age_days so they stop triggering.
            let proposal_expiry_cutoff = days_ago(&now_iso, self.governance_proposal_max_age_days);
            self.storage.conn_execute(
                "UPDATE governance_proposals
                 SET state='rejected', updated_at=?
                 WHERE state='pending'
                   AND evidence_score < ?
                   AND created_at < ?",
                rusqlite::params![
                    now_iso,
                    self.governance_archive_threshold as f64,
                    proposal_expiry_cutoff
                ],
            )?;

            // ── 3e. Archive: sustained_negative_feedback ──
            // Chunks with too many negative feedback events are archived regardless of
            // how long they've been idle, giving feedback a direct archival path.
            let neg_feedback_chunks = self.storage.query_chunks_params(
                "SELECT p.chunk_id FROM governance_proposals p
                 JOIN chunks c ON c.id = p.chunk_id
                 WHERE c.origin!='spark' AND c.protected=0
                   AND c.state IN ('active','pending')
                   AND p.state='pending'
                   AND p.evidence_score >= ? AND p.actor_count >= 2
                   AND (? IS NULL OR c.origin=?)
                   AND (? IS NULL OR c.skill_name=?)
                 GROUP BY p.chunk_id",
                rusqlite::params![
                    self.negative_feedback_archive_threshold as f64,
                    scope_origin,
                    scope_origin,
                    scope_skill,
                    scope_skill
                ],
            )?;
            for c in &neg_feedback_chunks {
                if let Some(cid) = c.get("chunk_id").and_then(Value::as_str) {
                    if !report.archived.contains(&cid.to_string()) {
                        self.storage.update_chunk_state(
                            cid,
                            "archived",
                            Some("sustained_negative_feedback"),
                            &now_iso,
                        )?;
                        report.archived.push(cid.to_string());
                    }
                }
            }

            // ── 3f. Archive: sustained_task_failure ──
            // Covers both active and pending chunks: a pending chunk recalled repeatedly
            // but never producing successful tasks also has no other archive path.
            let high_fail_chunks = self.storage.query_chunks_params(
                "SELECT id FROM chunks
                 WHERE origin!='spark' AND protected=0 AND state IN ('active','pending')
                   AND used_count >= ?
                   AND CAST(used_success_count AS REAL) / CAST(used_count AS REAL) < ?
                   AND confidence < ?
                   AND (? IS NULL OR origin=?)
                   AND (? IS NULL OR skill_name=?)",
                rusqlite::params![
                    self.failure_min_uses,
                    self.failure_max_success_rate,
                    self.failure_confidence_max,
                    scope_origin,
                    scope_origin,
                    scope_skill,
                    scope_skill
                ],
            )?;
            for c in &high_fail_chunks {
                if let Some(cid) = c.get("id").and_then(Value::as_str) {
                    if !report.archived.contains(&cid.to_string()) {
                        self.storage.update_chunk_state(
                            cid,
                            "archived",
                            Some("sustained_task_failure"),
                            &now_iso,
                        )?;
                        report.archived.push(cid.to_string());
                    }
                }
            }

            // ── 4. Dedupe: same content_hash — keep protected or highest confidence ──
            let dupes = self.storage.query_chunks_params(
                "SELECT content_hash FROM chunks
                 WHERE origin!='spark' AND state IN ('active','pending')
                   AND (? IS NULL OR origin=?)
                   AND (? IS NULL OR skill_name=?)
                 GROUP BY content_hash HAVING COUNT(*) > 1",
                rusqlite::params![scope_origin, scope_origin, scope_skill, scope_skill],
            )?;
            for d in &dupes {
                if let Some(h) = d.get("content_hash").and_then(Value::as_str) {
                    let group = self.storage.query_chunks_params(
                        "SELECT id, confidence, protected FROM chunks
                         WHERE content_hash=? AND origin!='spark' AND state IN ('active','pending')
                           AND (? IS NULL OR origin=?)
                           AND (? IS NULL OR skill_name=?)
                         ORDER BY protected DESC, confidence DESC",
                        rusqlite::params![h, scope_origin, scope_origin, scope_skill, scope_skill],
                    )?;
                    let canonical_id = group
                        .first()
                        .and_then(|row| row.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    for row in group.iter().skip(1) {
                        let id = row.get("id").and_then(Value::as_str).unwrap_or("");
                        let reason = format!("duplicate:{canonical_id}");
                        self.storage
                            .update_chunk_state(id, "archived", Some(&reason), &now_iso)?;
                        self.storage.conn_execute(
                            "UPDATE chunks SET parent_id=?, updated_at=? WHERE id=?",
                            rusqlite::params![canonical_id, now_iso, id],
                        )?;
                        report.deduped.push(id.to_string());
                    }
                }
            }

            // ── 5. Decay: confidence time-decay for idle non-spark non-protected active chunks ──
            // Issue 6: Use last_decayed_at as the delta reference to avoid compounding.
            // Each curate only applies the INCREMENTAL decay since the previous run, so the
            // 90-day half-life is preserved regardless of how often curate runs.
            let decay_candidates = self.storage.query_chunks_params(
                "SELECT id, confidence, last_used_at, last_decayed_at FROM chunks
                 WHERE origin!='spark' AND protected=0 AND state IN ('active','pending')
                   AND last_used_at IS NOT NULL
                   AND (? IS NULL OR origin=?)
                   AND (? IS NULL OR skill_name=?)",
                rusqlite::params![scope_origin, scope_origin, scope_skill, scope_skill],
            )?;
            for c in &decay_candidates {
                let id = match c.get("id").and_then(Value::as_str) {
                    Some(v) => v,
                    None => continue,
                };
                let conf = c.get("confidence").and_then(Value::as_f64).unwrap_or(0.5);
                let last_used = c
                    .get("last_used_at")
                    .and_then(Value::as_str)
                    .unwrap_or(&now_iso);
                // Use last_decayed_at (or last_used_at) as the reference for incremental delta.
                let decay_ref = c
                    .get("last_decayed_at")
                    .and_then(Value::as_str)
                    .filter(|s| *s > last_used)
                    .unwrap_or(last_used);
                let delta_days = iso_days_diff(&now_iso, decay_ref);
                if delta_days <= 0 {
                    continue;
                }
                let floor = self.decay_floor;
                let decay_alpha = 1.0 - 0.5_f64.powf(delta_days as f64 / 90.0);
                let new_conf = conf + decay_alpha * (floor - conf);
                if (new_conf - conf).abs() > 0.001 {
                    let note = format!("decay:{delta_days}d");
                    self.storage.upsert_confidence_evidence(
                        &gen_uuid(),
                        None,
                        id,
                        "decay",
                        floor,
                        decay_alpha,
                        &note,
                        None,
                        &now_iso,
                        "observed",
                    )?;
                    self.recompute_chunk_confidence(id, &now_iso)?;
                    self.storage.update_chunk_last_decayed_at(id, &now_iso)?;
                    report.decayed.push(id.to_string());
                }
            }

            // ── 6. Promote: pending → active when three-guard criteria met ──
            let promotable = self.storage.query_chunks_params(
                "SELECT id FROM chunks
                 WHERE state='pending' AND origin!='spark'
                   AND used_success_count >= ?
                   AND success_trace_ids_count >= ?
                   AND confidence >= ?
                   AND (? IS NULL OR origin=?)
                   AND (? IS NULL OR skill_name=?)",
                rusqlite::params![
                    self.promote_used_success_min,
                    // Both counters are derived from the same trace-deduped
                    // aggregate, so they share one threshold; a literal here
                    // would silently pin the gate above a lower configured min.
                    self.promote_used_success_min,
                    self.promote_confidence_min,
                    scope_origin,
                    scope_origin,
                    scope_skill,
                    scope_skill
                ],
            )?;
            for c in &promotable {
                if let Some(id) = c.get("id").and_then(Value::as_str) {
                    self.storage.update_chunk_state(
                        id,
                        "active",
                        Some("repeated_success"),
                        &now_iso,
                    )?;
                    report.promoted.push(id.to_string());
                }
            }

            // ── 7. Cycle/orphan detection (report only, no auto-fix) ──
            let all_deps = self
                .storage
                .query_chunks("SELECT src, dst FROM deps WHERE kind='hard'")?;
            let cycles = detect_cycles(&all_deps);
            report.cycles = cycles;
            let orphan_rows = self.storage.query_chunks_params(
                "SELECT d.src, d.dst, s.id AS src_exists, t.id AS dst_exists
                 FROM deps d
                 LEFT JOIN chunks s ON s.id=d.src
                 LEFT JOIN chunks t ON t.id=d.dst
                 WHERE d.kind='hard'
                   AND (? IS NULL OR s.origin=?)
                   AND (? IS NULL OR s.skill_name=?)",
                rusqlite::params![scope_origin, scope_origin, scope_skill, scope_skill],
            )?;
            let mut orphans = HashSet::new();
            for row in orphan_rows {
                if row.get("src_exists").is_none_or(Value::is_null) {
                    if let Some(id) = row.get("src").and_then(Value::as_str) {
                        orphans.insert(id.to_string());
                    }
                }
                if row.get("dst_exists").is_none_or(Value::is_null) {
                    if let Some(id) = row.get("dst").and_then(Value::as_str) {
                        orphans.insert(id.to_string());
                    }
                }
            }
            report.orphans = orphans.into_iter().collect();
            report.orphans.sort();

            // ── 8. Full context_stats rebuild — periodic correction pass.
            // record() only does targeted per-chunk updates; curate keeps the whole
            // table accurate by rebuilding from all sources once per cycle.
            self.rebuild_context_stats(&now_iso)?;

            self.storage.commit()
        })();
        if gov_result.is_err() {
            let _ = self.storage.rollback();
            gov_result?;
        }

        // ── Step 8: compact old terminal logs while preserving trace identity. ──
        // The compact row keeps attribution corrections and audit joins possible without
        // retaining potentially large raw outputs indefinitely.
        self.storage.begin_immediate()?;
        let purge_cutoff = days_ago(&now_iso, self.log_compact_days);
        let purge_result = self
            .storage
            .conn_execute(
                "UPDATE episodic_log
                 SET query=NULL, recall_snapshot=NULL, output=NULL, output_summary=NULL,
                     nomination=NULL,
                     distill_note=COALESCE(distill_note, 'compacted')
                 WHERE distill_state IN ('distilled','discarded','failed')
                   AND ts < ?",
                rusqlite::params![purge_cutoff],
            )
            .and_then(|_| self.storage.commit());
        if purge_result.is_err() {
            let _ = self.storage.rollback();
            purge_result?;
        }

        // ── Step 9: prune completed/failed evolve_requests older than 30 days ──
        // Prevents unbounded table growth that would slow COUNT(*) queries.
        self.storage.begin_immediate()?;
        let evolve_req_cutoff = days_ago(&now_iso, 30);
        let prune_req_result = self
            .storage
            .conn_execute(
                "DELETE FROM evolve_requests
                 WHERE state IN ('completed','failed') AND requested_at < ?",
                rusqlite::params![evolve_req_cutoff],
            )
            .and_then(|_| self.storage.commit());
        if prune_req_result.is_err() {
            let _ = self.storage.rollback();
            prune_req_result?;
        }

        // 方案 E:重算校准映射(分桶查表)。从 verdict_log 的 observed 回填中,按声称
        // 置信度分桶,算每桶实际命中率,供 appraise emit 时把 raw_conf → calibrated。
        // 数据不足时不写(保持恒等),避免少数样本带偏。非致命:失败不阻断 curate。
        if let Ok(n) = self.recompute_calibration_map(&now_iso) {
            report
                .stats
                .insert("calibration_samples".to_string(), json!(n));
        }

        // 方案 I(误差驱动分区衰减)—— Phase 2,刻意留桩:需要 Phase 1 的 verdict_log
        // 校准数据按 situation_sig 分区累积后才有意义。当前仍用 90 天时间半衰期
        // (上方 decay 步),verdict_log.situation_sig 已是其未来数据源。详见实施文档 §6/§7.6。

        // 可观测性 P3b:curate 末尾写一行状态 KPI 快照,供 inspect().trends 算周环比。
        // 节流:仅当最近快照缺失或 ≥20h 前才写,避免高频 curate 灌满 metric_snapshots。
        // 非致命:快照失败绝不阻断 curate(主流程已完成)。
        let _ = self.maybe_write_metric_snapshot(&now_iso);

        // 运行事件 retention(P3a):清理早于 metrics.retain_days 的 operation_runs 行,
        // 防无界增长拖慢聚合(参照 usage_trace 的 purge)。非致命。
        let retain_cutoff = days_ago(&now_iso, self.metrics_retain_days);
        let _ = self.storage.purge_operation_runs(&retain_cutoff);

        Ok(report)
    }

    /// Write a `metric_snapshots` row if the most recent one is missing or ≥20h old.
    /// Throttled so frequent daemon-triggered curates don't flood the table.
    fn maybe_write_metric_snapshot(&self, now: &str) -> Result<()> {
        if let Some((last_ts, _)) = self.storage.latest_snapshot()? {
            // 20h throttle window via the shared cutoff helper (fractional days unsupported,
            // so compare against `now - ~0.83d` by using an hours-aware check on the ISO ts).
            let cutoff = hours_ago(now, 20);
            if last_ts.as_str() >= cutoff.as_str() {
                return Ok(());
            }
        }
        let kpis = self.collect_kpis(now)?;
        self.storage.insert_metric_snapshot(now, &kpis.to_string())
    }

    /// 方案 E:用 verdict_log 的实际命中率重写 calibration_map。
    /// 返回参与重算的 observed 样本数。样本太少(< 2×bins)则不动 map(保持恒等)。
    fn recompute_calibration_map(&self, now: &str) -> Result<i64> {
        let samples = self.storage.verdict_calibration_samples()?;
        let bins = self.calibration_bins.max(2);
        let min_samples = 2 * bins;
        if (samples.len() as i64) < min_samples {
            return Ok(samples.len() as i64);
        }
        // 累加每桶 (命中数, 总数)。桶 = floor(strength * bins),clamp 到 [0, bins-1]。
        // **按 strength 分桶**:emit 时 `calibrate_confidence` 用原始 strength 查表,
        // 故映射键域必须是 strength,而非已塑形的 emitted_conf(否则键域漂移、自指偏差)。
        let mut hit = vec![0.0_f64; bins as usize];
        let mut tot = vec![0.0_f64; bins as usize];
        for (strength, _conf, h) in &samples {
            let b = ((strength * bins as f64).floor() as i64).clamp(0, bins - 1) as usize;
            tot[b] += 1.0;
            hit[b] += *h;
        }
        let mut buckets: Vec<(f64, f64, f64, i64)> = Vec::with_capacity(bins as usize);
        for b in 0..bins as usize {
            let lo = b as f64 / bins as f64;
            let hi = (b as f64 + 1.0) / bins as f64;
            // 空桶回退到桶中点(无观测则恒等,不引入偏差)。
            let rate = if tot[b] > 0.0 {
                hit[b] / tot[b]
            } else {
                (lo + hi) / 2.0
            };
            buckets.push((lo, hi, rate, tot[b] as i64));
        }
        self.storage.begin_immediate()?;
        let r = self.storage.replace_calibration_map(&buckets, now);
        let r = r.and_then(|_| self.storage.commit());
        if r.is_err() {
            let _ = self.storage.rollback();
            r?;
        }
        Ok(samples.len() as i64)
    }

    // ------------------------------------------------------------------
    // Public API 8: inspect
    // ------------------------------------------------------------------
}
