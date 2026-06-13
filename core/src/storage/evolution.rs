use super::*;

impl Storage {
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_governance_proposal(
        &self,
        id: &str,
        chunk_id: &str,
        proposal_type: &str,
        reason: &str,
        evidence_count: i64,
        evidence_score: f64,
        actor_count: i64,
        now: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO governance_proposals
             (id, chunk_id, proposal_type, reason, evidence_count,
              evidence_score, actor_count, state, created_at, updated_at)
             VALUES (?,?,?,?,?,?,?,'pending',?,?)
             ON CONFLICT(chunk_id, proposal_type) WHERE state='pending'
             DO UPDATE SET reason=excluded.reason,
                           evidence_count=excluded.evidence_count,
                           evidence_score=excluded.evidence_score,
                           actor_count=excluded.actor_count,
                           updated_at=excluded.updated_at",
            params![
                id,
                chunk_id,
                proposal_type,
                reason,
                evidence_count,
                evidence_score,
                actor_count,
                now,
                now
            ],
        )?;
        Ok(())
    }

    pub fn request_evolve(&self, id: &str, reason: &str, now: &str) -> Result<()> {
        self.request_evolve_at(id, reason, now, None)
    }

    pub fn request_evolve_at(
        &self,
        id: &str,
        reason: &str,
        now: &str,
        next_retry_at: Option<&str>,
    ) -> Result<()> {
        let priority = match reason {
            "governance_ready" => 100,
            "governance" => 80,
            "threshold" => 60,
            "distill_retry" => 50,
            "batch_continue" => 40,
            _ => 20,
        };
        self.conn.execute(
            "INSERT INTO evolve_requests(
               id, reason, state, requested_at, priority, next_retry_at
             )
             VALUES (?,?,'pending',?,?,?)
             ON CONFLICT(reason) WHERE state='pending'
             DO UPDATE SET
               priority=MAX(priority, excluded.priority),
               next_retry_at=CASE
                 WHEN excluded.next_retry_at IS NULL THEN NULL
                 WHEN evolve_requests.next_retry_at IS NULL THEN NULL
                 ELSE MIN(evolve_requests.next_retry_at, excluded.next_retry_at)
               END",
            params![id, reason, now, priority, next_retry_at],
        )?;
        Ok(())
    }

    pub fn claim_evolve_request_with_reason(
        &self,
        now: &str,
        stale_before: &str,
    ) -> Result<Option<EvolveRequestClaim>> {
        self.conn.execute(
            "UPDATE evolve_requests
             SET state='pending', leased_at=NULL, note='lease_recovered'
             WHERE state='running' AND leased_at < ?",
            [stale_before],
        )?;
        self.conn.execute(
            "UPDATE evolve_requests
             SET state='pending', leased_at=NULL, note='retry_failed'
             WHERE state='failed' AND attempts < 3
               AND COALESCE(next_retry_at, completed_at) < ?",
            [now],
        )?;
        Ok(self
            .conn
            .query_row(
                "UPDATE evolve_requests
             SET state='running', leased_at=?, attempts=attempts+1
             WHERE id=(
               SELECT id FROM evolve_requests
               WHERE state='pending'
                 AND (next_retry_at IS NULL OR next_retry_at <= ?)
               ORDER BY priority DESC, requested_at ASC LIMIT 1
             ) AND state='pending'
             RETURNING id, reason",
                params![now, now],
                |row| {
                    Ok(EvolveRequestClaim {
                        id: row.get(0)?,
                        reason: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn defer_evolve_request(&self, id: &str, note: &str, next_retry_at: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM evolve_requests
             WHERE state='pending'
               AND id!=?
               AND reason=(SELECT reason FROM evolve_requests WHERE id=?)",
            params![id, id],
        )?;
        self.conn.execute(
            "UPDATE evolve_requests
             SET state='pending', leased_at=NULL, completed_at=NULL,
                 note=?, next_retry_at=?
             WHERE id=?",
            params![note, next_retry_at, id],
        )?;
        Ok(())
    }

    pub fn finish_evolve_request(
        &self,
        id: &str,
        state: &str,
        note: Option<&str>,
        now: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE evolve_requests
             SET state=?, completed_at=?, note=?,
                 last_failed_at=CASE
                   WHEN ?='failed' THEN ?
                   ELSE last_failed_at
                 END,
                 next_retry_at=CASE WHEN ?='failed'
                   THEN strftime('%Y-%m-%dT%H:%M:%fZ', ?, '+5 minutes')
                   ELSE NULL END
             WHERE id=?",
            params![state, now, note, state, now, state, now, id],
        )?;
        Ok(())
    }

    pub fn finish_covered_evolve_requests(&self, requested_before: &str, now: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE evolve_requests
             SET state='completed', completed_at=?, note='covered_by_evolve', next_retry_at=NULL
             WHERE state='pending' AND requested_at <= ?",
            params![now, requested_before],
        )?;
        Ok(())
    }

    /// Update by primary-key id (used after distill where we have the row id, not trace_id).
    pub fn update_episodic_log_state_by_id(
        &self,
        id: &str,
        state: &str,
        note: Option<&str>,
        outcome: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE episodic_log
             SET distill_state=?, distill_note=COALESCE(?,distill_note),
                 outcome=COALESCE(?,outcome),
                 distill_run_id=NULL, distill_locked_at=NULL
             WHERE id=?",
            params![state, note, outcome, id],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn finish_distill_log(
        &self,
        id: &str,
        state: &str,
        note: Option<&str>,
        prompt_tokens: i64,
        completion_tokens: i64,
        accounted_at: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO distill_token_usage(
               log_id, prompt_tokens, completion_tokens, outcome, accounted_at
             ) VALUES (?,?,?,?,?)",
            params![id, prompt_tokens, completion_tokens, state, accounted_at],
        )?;
        self.conn.execute(
            "UPDATE episodic_log
             SET distill_state=?, distill_note=?,
                 distill_prompt_tokens=?, distill_completion_tokens=?,
                 distill_accounted_at=?,
                 distill_attempts=distill_attempts
                   + CASE WHEN ?='failed' THEN 1 ELSE 0 END,
                 distill_last_failed_at=CASE
                   WHEN ?='failed' THEN ?
                   ELSE distill_last_failed_at
                 END,
                 distill_run_id=NULL, distill_locked_at=NULL
             WHERE id=?",
            params![
                state,
                note,
                prompt_tokens,
                completion_tokens,
                accounted_at,
                state,
                state,
                accounted_at,
                id
            ],
        )?;
        Ok(())
    }

    /// Claim a batch of 'new' logs for distillation: mark them 'screening' atomically.
    /// Returns the claimed rows (with distill_run_id set to run_id).
    pub fn claim_distill_batch(
        &self,
        run_id: &str,
        limit: usize,
        locked_at: &str,
    ) -> Result<Vec<Value>> {
        // BEGIN IMMEDIATE must be held by caller; this is called inside a transaction.
        self.conn.execute(
            "UPDATE episodic_log
             SET distill_state='screening', distill_run_id=?, distill_locked_at=?
             WHERE id IN (
               SELECT id FROM episodic_log
               WHERE distill_state='new'
               ORDER BY priority DESC, ts ASC
               LIMIT ?
             )",
            params![run_id, locked_at, limit as i64],
        )?;
        self.query_json(
            "SELECT * FROM episodic_log WHERE distill_run_id=? AND distill_state='screening'",
            params![run_id],
        )
    }

    pub fn query_episodic_logs_open(&self, limit: usize) -> Result<Vec<Value>> {
        self.query_json(
            "SELECT * FROM episodic_log WHERE distill_state='new' ORDER BY priority DESC, ts ASC LIMIT ?",
            params![limit as i64],
        )
    }
}
