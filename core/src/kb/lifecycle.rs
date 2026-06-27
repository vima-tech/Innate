use super::*;

impl KnowledgeBase {
    pub fn add(
        &self,
        content: &str,
        kind: &str,
        trigger_desc: Option<&str>,
        anti_trigger_desc: Option<&str>,
        source: &str,
        skill_name: Option<&str>,
    ) -> Result<String> {
        self.add_with_deps(
            content,
            kind,
            trigger_desc,
            anti_trigger_desc,
            source,
            skill_name,
            &[],
        )
    }

    /// Full-form writer: persist a chunk, its vectors, and all declared
    /// dependencies in a **single transaction**. Each dep is `(dst_chunk_id,
    /// kind)`; `kind` ∈ {`soft`,`hard`}. Dependency targets are validated to
    /// exist *inside* the transaction, so a bad dependency rolls back the whole
    /// write — the chunk is never persisted on its own.
    #[allow(clippy::too_many_arguments)]
    pub fn add_with_deps(
        &self,
        content: &str,
        kind: &str,
        trigger_desc: Option<&str>,
        anti_trigger_desc: Option<&str>,
        source: &str,
        skill_name: Option<&str>,
        deps: &[(String, String)],
    ) -> Result<String> {
        if !matches!(kind, "note" | "skill") {
            return Err(InnateError::InvalidState(format!("invalid kind: {kind}")));
        }
        for (_, dep_kind) in deps {
            if !matches!(dep_kind.as_str(), "soft" | "hard") {
                return Err(InnateError::InvalidState(format!(
                    "invalid dependency kind: {dep_kind} (expected soft|hard)"
                )));
            }
        }
        if !matches!(source, "chat" | "manual" | "doc" | "agent") {
            return Err(InnateError::InvalidState(format!(
                "invalid source: {source}"
            )));
        }

        let (content, action) = self.sanitize_content(content);
        if action == SanitizeAction::Discard {
            return Ok(String::new());
        }

        let trigger_clean = trigger_desc.and_then(|t| {
            let (cleaned, act) = self.sanitizer.sanitize(t);
            if act == SanitizeAction::Discard {
                None
            } else {
                Some(cleaned)
            }
        });
        let anti_trigger_clean = anti_trigger_desc.and_then(|t| {
            let (cleaned, act) = self.sanitizer.sanitize(t);
            if act == SanitizeAction::Discard {
                None
            } else {
                Some(cleaned)
            }
        });

        let h = content_hash(&content);
        if self.storage.is_hash_invalidated(&h)? {
            return Err(InnateError::InvalidState(
                "content hash is invalidated".into(),
            ));
        }

        // Idempotency check
        let existing = self.storage.query_chunks_params(
            "SELECT id FROM chunks WHERE content_hash=? AND origin!='spark' AND state IN ('active','pending') ORDER BY created_at ASC LIMIT 1",
            rusqlite::params![h],
        )?;
        if let Some(e) = existing.first() {
            if let Some(id) = e.get("id").and_then(Value::as_str).map(str::to_string) {
                // Content already exists. Don't silently drop newly-declared
                // dependencies: merge them into the existing chunk in one
                // transaction (edge insert is idempotent via INSERT OR IGNORE,
                // targets validated as in the fresh-write path).
                if !deps.is_empty() {
                    self.storage.begin_immediate()?;
                    let merge = (|| -> Result<()> {
                        for (dst, dep_kind) in deps {
                            if self.storage.get_chunk(dst)?.is_none() {
                                return Err(InnateError::ChunkNotFound(format!(
                                    "dependency target not found: {dst}"
                                )));
                            }
                            self.storage.insert_dep(&id, dst, dep_kind, None)?;
                        }
                        self.storage.commit()
                    })();
                    if merge.is_err() {
                        let _ = self.storage.rollback();
                    }
                    merge?;
                }
                return Ok(id);
            }
        }

        let now = utc_now_iso();
        let chunk_id = gen_uuid();
        let redacted = action == SanitizeAction::Redact;

        let (origin, state, conf, prot, init_state_reason) = if source == "agent" {
            (
                "captured",
                "pending",
                if redacted { 0.4 } else { 0.60 },
                0,
                "init:captured_agent",
            )
        } else if kind == "skill" {
            (
                "installed",
                "active",
                if redacted { 0.4 } else { 0.85 },
                1,
                "init:installed",
            )
        } else {
            (
                "captured",
                "active",
                if redacted { 0.4 } else { 0.60 },
                0,
                "init:captured",
            )
        };

        // Embedding — fall back to embedding_pending on failure.
        let trigger_str = trigger_clean.as_deref().unwrap_or(&content);
        let (cvec, tvec, embed_ver, final_state_reason) =
            match self.embed_pair(&content, trigger_str, "add") {
                (Ok(cv), Ok(tv)) => (cv, tv, 1i64, init_state_reason.to_string()),
                _ => (
                    vec![],
                    vec![],
                    0i64,
                    format!("embedding_pending:target={state}"),
                ),
            };

        let tokens = estimate_tokens(&content) as i64;
        let row = ChunkRow {
            id: chunk_id.clone(),
            skill_name: skill_name.map(str::to_string),
            content: content.clone(),
            trigger_desc: trigger_clean.clone(),
            anti_trigger_desc: anti_trigger_clean.clone(),
            content_hash: h,
            token_count: Some(tokens),
            origin: origin.to_string(),
            source: Some(source.to_string()),
            agent: agent_source(),
            protected: prot,
            state: state.to_string(),
            state_reason: Some(final_state_reason),
            confidence: conf,
            confidence_reason: Some(format!("init:{origin}")),
            version: 1,
            embed_version: embed_ver,
            created_at: now.clone(),
            updated_at: now.clone(),
            ..Default::default()
        };

        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            self.storage.insert_chunk(&row)?;
            if embed_ver > 0 {
                self.store_vec_content(&chunk_id, &cvec)?;
                self.store_vec_trigger(&chunk_id, &tvec)?;
            }
            // Dependencies are validated and written in the SAME transaction: a
            // missing target aborts the whole write so the chunk never lands
            // alone (no foreign keys, so existence is checked here explicitly).
            for (dst, dep_kind) in deps {
                if self.storage.get_chunk(dst)?.is_none() {
                    return Err(InnateError::ChunkNotFound(format!(
                        "dependency target not found: {dst}"
                    )));
                }
                self.storage.insert_dep(&chunk_id, dst, dep_kind, None)?;
            }
            self.storage.commit()
        })();
        if result.is_err() {
            let _ = self.storage.rollback();
        }
        result?;
        Ok(chunk_id)
    }

    /// Declare that chunk `src` depends on chunk `dst`.
    ///
    /// `kind` is `"hard"` (fail-closed: if `dst` is unavailable or archived at
    /// recall time the whole seed is dropped) or `"soft"` (a recall-time
    /// ranking bonus). Both chunks must exist. Idempotent — re-declaring the
    /// same edge is a no-op (`INSERT OR IGNORE`).
    pub fn add_dependency(&self, src: &str, dst: &str, kind: &str) -> Result<()> {
        if !matches!(kind, "soft" | "hard") {
            return Err(InnateError::InvalidState(format!(
                "invalid dependency kind: {kind} (expected soft|hard)"
            )));
        }
        if self.storage.get_chunk(src)?.is_none() {
            return Err(InnateError::ChunkNotFound(format!(
                "dependency source not found: {src}"
            )));
        }
        if self.storage.get_chunk(dst)?.is_none() {
            return Err(InnateError::ChunkNotFound(format!(
                "dependency target not found: {dst}"
            )));
        }
        self.storage.insert_dep(src, dst, kind, None)
    }

    // ------------------------------------------------------------------
    // Public API 4: spark
    // ------------------------------------------------------------------

    pub fn spark(
        &self,
        content: &str,
        trigger_desc: Option<&str>,
        anti_trigger_desc: Option<&str>,
    ) -> Result<String> {
        let (content, action) = self.sanitize_content(content);
        if action == SanitizeAction::Discard {
            return Ok(String::new());
        }

        let trigger_clean = trigger_desc.and_then(|t| {
            let (cleaned, act) = self.sanitizer.sanitize(t);
            if act == SanitizeAction::Discard {
                None
            } else {
                Some(cleaned)
            }
        });
        let anti_trigger_clean = anti_trigger_desc.and_then(|t| {
            let (cleaned, act) = self.sanitizer.sanitize(t);
            if act == SanitizeAction::Discard {
                None
            } else {
                Some(cleaned)
            }
        });

        let h = content_hash(&content);
        if self.storage.is_hash_invalidated(&h)? {
            return Err(InnateError::InvalidState(
                "content hash is invalidated".into(),
            ));
        }

        // Quick related recall (trace=false, no recursion risk)
        let related: Vec<String> = self
            .recall(RecallParams {
                query: &content,
                budget: 2000,
                top: Some(5),
                source: "sdk",
                ..Default::default()
            })
            .map(|r| {
                r.knowledge
                    .iter()
                    .filter_map(|c| c["id"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let now = utc_now_iso();
        let chunk_id = gen_uuid();
        let tokens = estimate_tokens(&content) as i64;

        let trigger_str = trigger_clean.as_deref().unwrap_or(&content);
        let (cvec, tvec, embed_ver, state_reason) =
            match self.embed_pair(&content, trigger_str, "spark") {
                (Ok(cv), Ok(tv)) => (cv, tv, 1i64, "init:spark".to_string()),
                _ => (
                    vec![],
                    vec![],
                    0i64,
                    "embedding_pending:target=active".to_string(),
                ),
            };

        let row = ChunkRow {
            id: chunk_id.clone(),
            content: content.clone(),
            trigger_desc: trigger_clean.clone(),
            anti_trigger_desc: anti_trigger_clean.clone(),
            content_hash: h,
            token_count: Some(tokens),
            origin: "spark".to_string(),
            agent: agent_source(),
            maturity: Some("seed".to_string()),
            related_ids: if related.is_empty() {
                None
            } else {
                Some(related.join(","))
            },
            state: "active".to_string(),
            state_reason: Some(state_reason),
            confidence: 0.5,
            version: 1,
            embed_version: embed_ver,
            created_at: now.clone(),
            updated_at: now.clone(),
            ..Default::default()
        };

        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            self.storage.insert_chunk(&row)?;
            if embed_ver > 0 {
                self.store_vec_content(&chunk_id, &cvec)?;
                self.store_vec_trigger(&chunk_id, &tvec)?;
            }
            self.storage.commit()
        })();
        if result.is_err() {
            let _ = self.storage.rollback();
        }
        result?;
        Ok(chunk_id)
    }

    // ------------------------------------------------------------------
    // Public API 5: mature_spark / promote_spark / drop_spark
    // ------------------------------------------------------------------

    pub fn mature_spark(&self, spark_id: &str, to: &str) -> Result<()> {
        let chunk = self
            .storage
            .get_chunk(spark_id)?
            .ok_or_else(|| InnateError::ChunkNotFound(spark_id.to_string()))?;
        if chunk.get("origin").and_then(Value::as_str) != Some("spark") {
            return Err(InnateError::ChunkNotFound(spark_id.to_string()));
        }
        let current = chunk
            .get("maturity")
            .and_then(Value::as_str)
            .unwrap_or("seed");
        let valid_next: &[&str] = match current {
            "seed" => &["sprouting"],
            "sprouting" => &["incubating"],
            _ => {
                return Err(InnateError::InvalidState(format!(
                    "spark {spark_id} already {current}"
                )))
            }
        };
        if current == to {
            return Ok(());
        }
        if !valid_next.contains(&to) {
            return Err(InnateError::InvalidState(format!(
                "invalid spark maturity transition: {current} -> {to}"
            )));
        }
        let now = utc_now_iso();
        self.storage.begin_immediate()?;
        let result = self
            .storage
            .query_chunks_params(
                "UPDATE chunks SET maturity=?, updated_at=? WHERE id=?",
                rusqlite::params![to, now, spark_id],
            )
            .and_then(|_| self.storage.commit());
        if result.is_err() {
            let _ = self.storage.rollback();
        }
        result.map(|_| ())
    }

    pub fn promote_spark(&self, spark_id: &str, to: &str) -> Result<String> {
        let spark = self
            .storage
            .get_chunk(spark_id)?
            .ok_or_else(|| InnateError::ChunkNotFound(spark_id.to_string()))?;
        if spark.get("origin").and_then(Value::as_str) != Some("spark") {
            return Err(InnateError::ChunkNotFound(spark_id.to_string()));
        }
        let maturity = spark.get("maturity").and_then(Value::as_str).unwrap_or("");
        if maturity == "promoted" || maturity == "dropped" {
            return Err(InnateError::InvalidState(format!(
                "spark {spark_id} already {maturity}"
            )));
        }
        if !matches!(to, "note" | "skill") {
            return Err(InnateError::InvalidState(format!(
                "invalid spark promotion target: {to}"
            )));
        }

        let content = spark.get("content").and_then(Value::as_str).unwrap_or("");
        let (content, action) = self.sanitize_content(content);
        if action == SanitizeAction::Discard {
            return Err(InnateError::InvalidState(
                "sanitize discard on promote".into(),
            ));
        }

        let promoted_hash = content_hash(&content);
        if self.storage.is_hash_invalidated(&promoted_hash)? {
            return Err(InnateError::InvalidState(
                "spark content hash is invalidated".into(),
            ));
        }

        let now = utc_now_iso();

        // Idempotency: existing non-spark chunk with same hash
        let existing = self.storage.query_chunks_params(
            "SELECT id FROM chunks WHERE content_hash=? AND origin!='spark' AND state IN ('active','pending') ORDER BY created_at ASC LIMIT 1",
            rusqlite::params![promoted_hash],
        )?;
        if let Some(e) = existing.first() {
            if let Some(id) = e.get("id").and_then(Value::as_str) {
                let id = id.to_string();
                self.storage.begin_immediate()?;
                let result = self
                    .storage
                    .query_chunks_params(
                        "UPDATE chunks SET maturity='promoted', updated_at=? WHERE id=?",
                        rusqlite::params![now, spark_id],
                    )
                    .and_then(|_| self.storage.commit());
                if result.is_err() {
                    let _ = self.storage.rollback();
                    result?;
                }
                return Ok(id);
            }
        }

        let (state, conf, prot, origin, state_reason) = if to == "skill" {
            ("active", 0.85, 1, "installed", "init:installed")
        } else {
            ("active", 0.60, 0, "captured", "init:captured")
        };

        let conf = if action == SanitizeAction::Redact {
            0.4_f64
        } else {
            conf
        };
        let new_id = gen_uuid();
        let trigger = spark.get("trigger_desc").and_then(Value::as_str);
        let anti = spark.get("anti_trigger_desc").and_then(Value::as_str);

        let row = ChunkRow {
            id: new_id.clone(),
            content: content.clone(),
            trigger_desc: trigger.map(str::to_string),
            anti_trigger_desc: anti.map(str::to_string),
            content_hash: promoted_hash,
            token_count: Some(estimate_tokens(&content) as i64),
            origin: origin.to_string(),
            source: Some("manual".to_string()),
            // 提升时继承 spark 创建时的 agent;旧 spark 缺列则回退当前 agent。
            agent: spark
                .get("agent")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(agent_source),
            protected: prot,
            state: state.to_string(),
            state_reason: Some(state_reason.to_string()),
            confidence: conf,
            confidence_reason: Some("manual_set".to_string()),
            parent_id: Some(spark_id.to_string()),
            version: 1,
            embed_version: 1,
            created_at: now.clone(),
            updated_at: now.clone(),
            ..Default::default()
        };

        let (cvec_res, tvec_res) = self.embed_pair(&content, trigger.unwrap_or(&content), "install");
        let cvec = cvec_res?;
        let tvec = tvec_res?;

        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            self.storage.insert_chunk(&row)?;
            self.store_vec_content(&new_id, &cvec)?;
            self.store_vec_trigger(&new_id, &tvec)?;
            self.storage.query_chunks_params(
                "UPDATE chunks SET maturity='promoted', updated_at=? WHERE id=?",
                rusqlite::params![now, spark_id],
            )?;
            self.storage.commit()
        })();
        if result.is_err() {
            let _ = self.storage.rollback();
        }
        result?;
        Ok(new_id)
    }

    pub fn drop_spark(&self, spark_id: &str, reason: &str) -> Result<()> {
        let spark = self
            .storage
            .get_chunk(spark_id)?
            .ok_or_else(|| InnateError::ChunkNotFound(spark_id.to_string()))?;
        if spark.get("origin").and_then(Value::as_str) != Some("spark") {
            return Err(InnateError::ChunkNotFound(spark_id.to_string()));
        }
        let maturity = spark.get("maturity").and_then(Value::as_str).unwrap_or("");
        if maturity == "promoted" {
            return Err(InnateError::InvalidState(format!(
                "spark {spark_id} already promoted"
            )));
        }
        if maturity == "dropped" {
            return Ok(());
        }
        let now = utc_now_iso();
        let reason_str = if reason.is_empty() {
            "dropped".to_string()
        } else {
            format!("dropped:{reason}")
        };
        self.storage.begin_immediate()?;
        let result = self
            .storage
            .query_chunks_params(
                "UPDATE chunks SET maturity='dropped', state_reason=?, updated_at=? WHERE id=?",
                rusqlite::params![reason_str, now, spark_id],
            )
            .and_then(|_| self.storage.commit());
        if result.is_err() {
            let _ = self.storage.rollback();
        }
        result.map(|_| ())
    }

    // ------------------------------------------------------------------
    // Public API 6: approve / archive / invalidate / restore
    // ------------------------------------------------------------------

    pub fn approve(&self, chunk_id: &str) -> Result<()> {
        let chunk = self
            .storage
            .get_chunk(chunk_id)?
            .ok_or_else(|| InnateError::ChunkNotFound(chunk_id.to_string()))?;
        if chunk.get("origin").and_then(Value::as_str) == Some("spark") {
            return Err(InnateError::InvalidState(
                "spark lifecycle uses promote_spark() or invalidate()".into(),
            ));
        }
        if chunk.get("state").and_then(Value::as_str) == Some("active") {
            return Ok(());
        }
        if chunk.get("state").and_then(Value::as_str) != Some("pending") {
            return Err(InnateError::InvalidState(
                "approve requires pending chunk".into(),
            ));
        }
        let now = utc_now_iso();
        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            self.storage
                .update_chunk_state(chunk_id, "active", Some("approved"), &now)?;
            self.storage.query_chunks_params(
                "UPDATE chunks SET confidence_reason='manual_set', updated_at=? WHERE id=?",
                rusqlite::params![now, chunk_id],
            )?;
            self.storage.commit()
        })();
        if result.is_err() {
            let _ = self.storage.rollback();
        }
        result
    }

    pub fn archive(&self, chunk_id: &str, reason: &str) -> Result<()> {
        let chunk = self
            .storage
            .get_chunk(chunk_id)?
            .ok_or_else(|| InnateError::ChunkNotFound(chunk_id.to_string()))?;
        if chunk.get("origin").and_then(Value::as_str) == Some("spark") {
            return Err(InnateError::InvalidState(
                "spark lifecycle uses drop_spark() or invalidate()".into(),
            ));
        }
        let now = utc_now_iso();
        self.storage.begin_immediate()?;
        let result = self
            .storage
            .update_chunk_state(chunk_id, "archived", Some(reason), &now)
            .and_then(|_| self.storage.commit());
        if result.is_err() {
            let _ = self.storage.rollback();
        }
        result
    }

    pub fn invalidate(&self, chunk_id: &str, reason: &str) -> Result<()> {
        let chunk = self
            .storage
            .get_chunk(chunk_id)?
            .ok_or_else(|| InnateError::ChunkNotFound(chunk_id.to_string()))?;
        let h = chunk
            .get("content_hash")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let now = utc_now_iso();
        let reason_str = if reason.is_empty() {
            "invalidated".to_string()
        } else {
            format!("invalidated:{reason}")
        };

        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            self.storage.query_chunks_params(
                "UPDATE chunks
                 SET state='archived', confidence=0.0, confidence_base=0.0,
                     confidence_reason='invalidated', state_reason=?,
                     state_updated_at=?, updated_at=?
                 WHERE id=?",
                rusqlite::params![reason_str, now, now, chunk_id],
            )?;
            self.storage.query_chunks_params(
                "UPDATE chunks
                 SET state='archived', confidence=0.0, confidence_base=0.0,
                     confidence_reason='invalidated',
                     state_reason='invalidated:same_hash',
                     state_updated_at=?, updated_at=?
                 WHERE content_hash=? AND id!=?",
                rusqlite::params![now, now, h, chunk_id],
            )?;
            self.storage.conn_execute(
                "DELETE FROM confidence_evidence
                 WHERE chunk_id IN (SELECT id FROM chunks WHERE content_hash=?)",
                rusqlite::params![h],
            )?;
            self.storage
                .insert_invalidated_hash(&h, Some(reason), &now)?;
            self.storage.commit()
        })();
        if result.is_err() {
            let _ = self.storage.rollback();
        }
        result
    }

    pub fn restore(&self, chunk_id: &str) -> Result<()> {
        let chunk = self
            .storage
            .get_chunk(chunk_id)?
            .ok_or_else(|| InnateError::ChunkNotFound(chunk_id.to_string()))?;
        let state = chunk.get("state").and_then(Value::as_str).unwrap_or("");
        if state == "active" {
            return Ok(());
        }
        if state != "archived" {
            return Err(InnateError::InvalidState(
                "restore requires archived chunk".into(),
            ));
        }
        let was_invalidated = chunk
            .get("state_reason")
            .and_then(Value::as_str)
            .map(|r| r.starts_with("invalidated"))
            .unwrap_or(false);
        let h = chunk
            .get("content_hash")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let now = utc_now_iso();

        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            self.storage
                .update_chunk_state(chunk_id, "active", Some("restore"), &now)?;
            if was_invalidated {
                self.storage.query_chunks_params(
                    "DELETE FROM invalidated_hashes WHERE content_hash=?",
                    rusqlite::params![h],
                )?;
            }
            self.storage.query_chunks_params(
                "UPDATE chunks
                 SET confidence_base=0.5, confidence=0.5,
                     confidence_reason='restore',
                     selected_count=0, selected_count_base=0,
                     used_count=0, used_count_base=0,
                     used_success_count=0, used_success_count_base=0,
                     success_trace_ids_count=0,
                     last_used_at=NULL, last_used_base=NULL,
                     last_success_at=NULL, last_decayed_at=NULL,
                     evidence_cutoff_at=?, updated_at=?
                 WHERE id=?",
                rusqlite::params![now, now, chunk_id],
            )?;
            self.storage.conn_execute(
                "DELETE FROM confidence_evidence WHERE chunk_id=?",
                rusqlite::params![chunk_id],
            )?;
            self.storage.conn_execute(
                "DELETE FROM chunk_success_traces WHERE chunk_id=?",
                rusqlite::params![chunk_id],
            )?;
            self.storage.conn_execute(
                "DELETE FROM chunk_context_stats_base WHERE chunk_id=?",
                rusqlite::params![chunk_id],
            )?;
            self.storage.conn_execute(
                "DELETE FROM chunk_context_stats WHERE chunk_id=?",
                rusqlite::params![chunk_id],
            )?;
            self.storage.conn_execute(
                "UPDATE governance_proposals
                 SET state='rejected', reason=reason || '; restored by user', updated_at=?
                 WHERE chunk_id=? AND state IN ('pending','accepted')",
                rusqlite::params![now, chunk_id],
            )?;
            self.storage.commit()
        })();
        if result.is_err() {
            let _ = self.storage.rollback();
        }
        result
    }

    // ------------------------------------------------------------------
    // Public API 7: evolve
    // ------------------------------------------------------------------
}
