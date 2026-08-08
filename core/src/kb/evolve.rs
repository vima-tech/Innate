use super::*;

impl KnowledgeBase {
    pub fn evolve(&self, trigger: &str) -> Result<Value> {
        self.measure("evolve", None, None, || self.evolve_inner(trigger))
    }

    fn evolve_inner(&self, trigger: &str) -> Result<Value> {
        if !matches!(trigger, "manual" | "scheduled" | "threshold") {
            return Err(InnateError::InvalidState(format!(
                "invalid evolve trigger: {trigger}"
            )));
        }
        let evolve_started_at = utc_now_iso();
        let retry_cutoff = minutes_ago(&evolve_started_at, 5);
        let recovered_failed = self.storage.conn_execute_count(
            "UPDATE episodic_log
             SET distill_state='new', distill_note='retry_failed',
                 distill_locked_at=NULL, distill_run_id=NULL
             WHERE distill_state='failed'
               AND distill_attempts < 3
               AND COALESCE(distill_last_failed_at, distill_accounted_at, ts) < ?",
            rusqlite::params![retry_cutoff],
        )?;
        if recovered_failed > 0 {
            self.storage
                .request_evolve(&gen_uuid(), "distill_retry", &evolve_started_at)?;
        }
        if trigger == "scheduled" {
            let age_cutoff = hours_ago(&evolve_started_at, self.evolve_schedule_interval_hours);
            let aged_new = count_query_params(
                &self.storage,
                "SELECT COUNT(*) FROM episodic_log
                 WHERE distill_state='new' AND ts <= ?",
                rusqlite::params![age_cutoff],
            )?;
            if aged_new > 0 {
                self.storage
                    .request_evolve(&gen_uuid(), "scheduled", &evolve_started_at)?;
            }
        }
        let request = self.storage.claim_evolve_request_with_reason(
            &evolve_started_at,
            &minutes_ago(&evolve_started_at, self.screening_timeout_minutes),
        )?;
        let request_id = request.as_ref().map(|claim| claim.id.as_str());
        let request_reason = request.as_ref().map(|claim| claim.reason.as_str());
        // Issue 7: scheduled without pending request → still run curate (time-based maintenance).
        if trigger == "scheduled" && request_id.is_none() {
            let curator = Arc::clone(&self.curator);
            let curate = curator.run(self, &CurateScope::default())?;
            return Ok(json!({
                "distilled": 0,
                "curate": self.format_curate_report(&curate),
                "skipped": "no_evolve_request"
            }));
        }

        // Issue 8: threshold / token-limit gates should not suppress curate.
        if trigger == "threshold" {
            let rows = self.storage.query_chunks(
                "SELECT COUNT(*) AS cnt FROM episodic_log WHERE distill_state='new'",
            )?;
            let cnt = rows
                .first()
                .and_then(|r| r.get("cnt"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            if cnt < self.evolve_threshold {
                let curator = Arc::clone(&self.curator);
                let curate = curator.run(self, &CurateScope::default())?;
                if let Some(id) = request_id {
                    if matches!(request_reason, Some("governance" | "governance_ready")) {
                        self.storage.finish_evolve_request(
                            id,
                            "completed",
                            Some("curate_only"),
                            &utc_now_iso(),
                        )?;
                    } else {
                        self.storage.defer_evolve_request(
                            id,
                            "below_threshold",
                            &hours_after(
                                &evolve_started_at,
                                self.evolve_schedule_interval_hours.max(1),
                            ),
                        )?;
                    }
                }
                return Ok(json!({
                    "distilled": 0,
                    "curate": self.format_curate_report(&curate),
                    "skipped": "below_threshold"
                }));
            }
        }

        if trigger != "manual" {
            if let Some(limit) = self
                .storage
                .get_meta("max_distill_tokens_per_period")?
                .and_then(|value| value.parse::<i64>().ok())
                .filter(|value| *value > 0)
            {
                let period_start = self.distill_token_period_start(&evolve_started_at)?;
                let rows = self.storage.query_chunks_params(
                    "SELECT COALESCE(SUM(prompt_tokens + completion_tokens),0) AS used
                     FROM distill_token_usage
                     WHERE accounted_at >= ?",
                    rusqlite::params![period_start],
                )?;
                let used_tokens = rows
                    .first()
                    .and_then(|row| row.get("used"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                if used_tokens >= limit {
                    let curator = Arc::clone(&self.curator);
                    let curate = curator.run(self, &CurateScope::default())?;
                    if let Some(id) = request_id {
                        if matches!(request_reason, Some("governance" | "governance_ready")) {
                            self.storage.finish_evolve_request(
                                id,
                                "completed",
                                Some("curate_only"),
                                &utc_now_iso(),
                            )?;
                        } else {
                            self.storage.defer_evolve_request(
                                id,
                                "distill_token_limit",
                                &hours_after(&evolve_started_at, 1),
                            )?;
                        }
                    }
                    return Ok(json!({
                        "distilled": 0,
                        "curate": self.format_curate_report(&curate),
                        "skipped": "distill_token_limit",
                        "distill_tokens_used": used_tokens,
                        "distill_token_limit": limit,
                        "period_start": period_start,
                    }));
                }
            }
        }

        let result = (|| -> Result<Value> {
            let distill = self.measure("distill", None, None, || self.distill_batch())?;
            let curator = Arc::clone(&self.curator);
            let curate = curator.run(self, &CurateScope::default())?;
            Ok(json!({
                "distilled": distill.distilled,
                "distill_failed": distill.failed,
                "curate": self.format_curate_report(&curate),
            }))
        })();
        if let Some(id) = request_id {
            let (state, note) = match &result {
                Ok(_) => ("completed", None),
                Err(error) => ("failed", Some(error.to_string())),
            };
            self.storage
                .finish_evolve_request(id, state, note.as_deref(), &utc_now_iso())?;
        }
        if result.is_ok() {
            self.storage
                .finish_covered_evolve_requests(&evolve_started_at, &utc_now_iso())?;
        }

        let failed_remaining = count_query(
            &self.storage,
            "SELECT COUNT(*) FROM episodic_log
             WHERE distill_state='failed' AND distill_attempts < 3",
        )?;
        if failed_remaining > 0 {
            self.storage.request_evolve_at(
                &gen_uuid(),
                "distill_retry",
                &utc_now_iso(),
                Some(&minutes_after(&utc_now_iso(), 5)),
            )?;
        }

        // Issue 9: if more 'new' logs remain after this batch, self-queue so the next evolve drains them.
        if result.is_ok() {
            let remaining = count_query(
                &self.storage,
                "SELECT COUNT(*) FROM episodic_log WHERE distill_state='new'",
            )?;
            if remaining > 0 {
                let _ = self
                    .storage
                    .request_evolve(&gen_uuid(), "batch_continue", &utc_now_iso());
            }
        }
        result
    }

    fn format_curate_report(&self, curate: &CurateReport) -> Value {
        json!({
            "archived": curate.archived.len(),
            "promoted": curate.promoted.len(),
            "deduped": curate.deduped.len(),
            "decayed": curate.decayed.len(),
            "recovered": curate.recovered.len(),
            "orphans": curate.orphans.len(),
            "warnings": curate.warnings,
        })
    }

    fn distill_batch(&self) -> Result<DistillBatchReport> {
        let run_id = gen_uuid();
        let now = utc_now_iso();

        // Atomically claim a batch of 'new' logs → mark 'screening'.
        self.storage.begin_immediate()?;
        let logs = match self
            .storage
            .claim_distill_batch(&run_id, self.distill_batch_size, &now)
        {
            Ok(l) => {
                self.storage.commit()?;
                l
            }
            Err(e) => {
                let _ = self.storage.rollback();
                return Err(e);
            }
        };

        let mut chunks_by_log: HashMap<String, Vec<DistilledChunk>> = HashMap::new();
        let mut failed_logs = HashSet::new();
        let mut distill_errors = Vec::new();
        for log in &logs {
            let log_id = log.get("id").and_then(Value::as_str).unwrap_or("");
            let context_key = log.get("context_key").and_then(Value::as_str);
            let related_logs: Vec<Value> = logs
                .iter()
                .filter(|other| {
                    other.get("id").and_then(Value::as_str) == Some(log_id)
                        || (context_key.is_some()
                            && other.get("context_key").and_then(Value::as_str) == context_key)
                })
                .cloned()
                .collect();
            match self.distiller.distill_with_context(log, &related_logs) {
                Ok(chunks) => {
                    if chunks.iter().any(|chunk| chunk.source_log_id != log_id) {
                        let error = "distiller returned a chunk for an unknown source log";
                        failed_logs.insert(log_id.to_string());
                        distill_errors.push(format!("{log_id}: {error}"));
                        self.finish_distill_log(
                            log_id,
                            "failed",
                            Some(&format!("distill_failed:{error}")),
                            estimate_distill_prompt_tokens(log, &related_logs),
                            0,
                        )?;
                        continue;
                    }
                    chunks_by_log.insert(log_id.to_string(), chunks);
                }
                Err(error) => {
                    let note = format!("distill_failed:{error}");
                    failed_logs.insert(log_id.to_string());
                    distill_errors.push(format!("{log_id}: {error}"));
                    self.finish_distill_log(
                        log_id,
                        "failed",
                        Some(&note),
                        estimate_distill_prompt_tokens(log, &related_logs),
                        0,
                    )?;
                }
            }
        }

        let mut count = 0;
        let provenance = self.distiller.provenance();
        for log in &logs {
            let log_id = log.get("id").and_then(Value::as_str).unwrap_or("");
            if failed_logs.contains(log_id) {
                continue;
            }
            let context_key = log.get("context_key").and_then(Value::as_str);
            let related_logs: Vec<Value> = logs
                .iter()
                .filter(|other| {
                    other.get("id").and_then(Value::as_str) == Some(log_id)
                        || (context_key.is_some()
                            && other.get("context_key").and_then(Value::as_str) == context_key)
                })
                .cloned()
                .collect();
            let prompt_tokens = estimate_distill_prompt_tokens(log, &related_logs);
            let chunks = chunks_by_log.remove(log_id).unwrap_or_default();
            let completion_tokens = chunks
                .iter()
                .map(estimate_distilled_chunk_tokens)
                .sum::<i64>();
            if chunks.is_empty() {
                self.finish_distill_log(
                    log_id,
                    "discarded",
                    Some("insufficient_material"),
                    prompt_tokens,
                    completion_tokens,
                )?;
                continue;
            }

            // Prepare all chunks + embeddings outside the write transaction so
            // slow embedding calls do not hold an exclusive SQLite lock.
            // Supports N >= 1 chunks per log (e.g. multi-concept LLM distillation).
            // Bad individual chunks are skipped; valid siblings still survive.
            struct PreparedChunk {
                row: ChunkRow,
                cvec_bytes: Vec<u8>,
                tvec_bytes: Vec<u8>,
            }
            let mut prepared: Vec<PreparedChunk> = Vec::with_capacity(chunks.len());
            let mut embedding_failures = 0_usize;
            for dc in chunks {
                let (content, action) = self.sanitize_content(&dc.content);
                if action == SanitizeAction::Discard {
                    continue; // skip this chunk, try others
                }
                let h = content_hash(&content);
                if self.storage.is_hash_invalidated(&h)? {
                    continue; // skip invalidated content, try others
                }
                let redacted = action == SanitizeAction::Redact;
                let conf = if redacted { 0.4 } else { 0.55 };
                let now2 = utc_now_iso();
                let chunk_id = gen_uuid();
                let tokens = estimate_tokens(&content) as i64;
                // Short label for the web UI's row-skill slot; fall back to the
                // canonical trigger phrase when the distiller produced none.
                let skill_name = dc
                    .skill_name
                    .clone()
                    .or_else(|| dc.trigger_desc.clone())
                    .filter(|s| !s.trim().is_empty());
                // Per-chunk fallback provenance wins over the batch-level provider,
                // so heuristic-fallback chunks are tagged accurately.
                let distill_provider = dc
                    .provider_override
                    .clone()
                    .or_else(|| provenance.provider.clone());
                let row = ChunkRow {
                    id: chunk_id,
                    skill_name,
                    content: content.clone(),
                    trigger_desc: dc.trigger_desc.clone(),
                    anti_trigger_desc: dc.anti_trigger_desc,
                    content_hash: h,
                    token_count: Some(tokens),
                    origin: "distilled".to_string(),
                    // 蒸馏 chunk 继承源 episodic_log 的 agent(经验来自哪个 agent),
                    // 而非运行 evolve 的进程(可能是 daemon/cron)。
                    agent: log
                        .get("agent")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    distilled_from: Some(dc.source_log_id),
                    distill_provider,
                    distill_model: provenance.model.clone(),
                    distill_prompt_version: provenance.prompt_version.clone(),
                    state: "pending".to_string(),
                    state_reason: Some("init:distilled".to_string()),
                    confidence: conf,
                    confidence_reason: Some("init:distilled".to_string()),
                    version: 1,
                    embed_version: 1,
                    created_at: now2.clone(),
                    updated_at: now2,
                    ..Default::default()
                };
                let (cvec_res, tvec_res) = self.embed_pair(
                    &content,
                    row.trigger_desc.as_deref().unwrap_or(&content),
                    "distill",
                );
                let cvec = match cvec_res {
                    Ok(v) if v.len() == self.embedding.content_dim() => v,
                    // A failed or wrong-dimension embedding is deferred (not
                    // written): a dim mismatch would be silently dropped at
                    // search time, so it must never reach storage.
                    _ => {
                        embedding_failures += 1;
                        continue;
                    }
                };
                let tvec = match tvec_res {
                    Ok(v) if v.len() == self.embedding.trigger_dim() => v,
                    _ => {
                        embedding_failures += 1;
                        continue;
                    }
                };
                prepared.push(PreparedChunk {
                    row,
                    cvec_bytes: pack_embedding(&cvec),
                    tvec_bytes: pack_embedding(&tvec),
                });
            }

            if prepared.is_empty() {
                let note = if embedding_failures > 0 {
                    "embedding_failed"
                } else {
                    "all_chunks_filtered"
                };
                self.finish_distill_log(
                    log_id,
                    if embedding_failures > 0 {
                        "failed"
                    } else {
                        "discarded"
                    },
                    Some(note),
                    prompt_tokens,
                    completion_tokens,
                )?;
                if embedding_failures > 0 {
                    failed_logs.insert(log_id.to_string());
                }
                continue;
            }

            // Write all chunks, vectors, token accounting, and terminal log state atomically.
            let accounted_at = utc_now_iso();
            self.storage.begin_immediate()?;
            let write_result = (|| -> Result<()> {
                for pc in &prepared {
                    self.storage.insert_chunk(&pc.row)?;
                    self.storage
                        .insert_vec_content(&pc.row.id, &pc.cvec_bytes)?;
                    self.storage
                        .insert_vec_trigger(&pc.row.id, &pc.tvec_bytes)?;
                }
                let note = (embedding_failures > 0)
                    .then(|| format!("partial_embedding_failures:{embedding_failures}"));
                self.storage.finish_distill_log(
                    log_id,
                    "distilled",
                    note.as_deref(),
                    prompt_tokens,
                    completion_tokens,
                    &accounted_at,
                )?;
                self.storage.commit()
            })();
            if let Err(error) = write_result {
                let _ = self.storage.rollback();
                let note = format!("distill_write_failed:{error}");
                self.finish_distill_log(
                    log_id,
                    "failed",
                    Some(&note),
                    prompt_tokens,
                    completion_tokens,
                )?;
                failed_logs.insert(log_id.to_string());
                continue;
            }
            count += 1;
        }
        if !distill_errors.is_empty() {
            // Log failures but do not abort: successful chunks are already committed and
            // failed logs are marked 'failed' for bounded retry. Returning Ok preserves
            // evolve request state and allows finish_covered_evolve_requests to run.
            eprintln!(
                "[innate] distillation partial failure ({} log(s)): {}",
                distill_errors.len(),
                distill_errors.join("; ")
            );
        }
        Ok(DistillBatchReport {
            distilled: count,
            failed: failed_logs.len(),
        })
    }

    fn finish_distill_log(
        &self,
        log_id: &str,
        state: &str,
        note: Option<&str>,
        prompt_tokens: i64,
        completion_tokens: i64,
    ) -> Result<()> {
        let accounted_at = utc_now_iso();
        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            self.storage.finish_distill_log(
                log_id,
                state,
                note,
                prompt_tokens,
                completion_tokens,
                &accounted_at,
            )?;
            self.storage.commit()
        })();
        if result.is_err() {
            let _ = self.storage.rollback();
        }
        result
    }

    pub(super) fn distill_token_period_start(&self, now: &str) -> Result<String> {
        let window_hours = self
            .storage
            .get_meta("evolve.distill_token_window_hours")?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(24)
            .max(1);
        Ok(hours_ago(now, window_hours))
    }
}
