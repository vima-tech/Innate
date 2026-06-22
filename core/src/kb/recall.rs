use super::*;

/// Parameters for [`KnowledgeBase::recall`].
///
/// Borrowed, `Default`-able: construct with `RecallParams { query, budget, source, ..Default::default() }`.
/// Empty-string defaults are normalized inside `recall`: `expand_deps` empty → `"false"`,
/// `refine_mode` empty → `"off"`.
#[derive(Debug, Clone, Default)]
pub struct RecallParams<'a> {
    pub query: &'a str,
    pub budget: usize,
    pub trace: bool,
    pub include_sparks: bool,
    pub top: Option<usize>,
    pub source: &'a str,
    pub expand_deps: &'a str, // "false" | "direct" | "closure"
    pub allow_trim: bool,     // if true, invoke Refiner::trim when block doesn't fit
    pub refine_mode: &'a str, // "off" | "trim" | "adapt" — recorded in trace
    /// Relevance gate: drop candidates whose fused score is below this value
    /// **before** packing/trace, so the trace only records knowledge that was
    /// actually surfaced. `None` disables the gate. Used by always-on hooks
    /// (UserPromptSubmit / SessionStart) to stay high-frequency without noise.
    pub min_score: Option<f64>,
    /// Session trace mode: open an episodic log for later record-correlation but
    /// write **no** per-chunk `retrieved`/`selected` usage events. Used by the
    /// daemon, which recalls only to obtain a `trace_id` and discards the
    /// knowledge without ever placing it in a model context. `selected` must
    /// strictly mean "entered the model context", so a caller that does not
    /// inject the result must set this. Defaults to `false` (full injection).
    pub session_only: bool,
}

impl KnowledgeBase {
    pub fn recall(&self, params: RecallParams<'_>) -> Result<RecallResult> {
        let RecallParams {
            query,
            budget,
            trace,
            include_sparks,
            top,
            source,
            expand_deps,
            allow_trim,
            refine_mode,
            min_score,
            session_only,
        } = params;
        let expand_deps = if expand_deps.is_empty() {
            "false"
        } else {
            expand_deps
        };
        let refine_mode = if refine_mode.is_empty() {
            "off"
        } else {
            refine_mode
        };
        validate_source(source)?;
        let trace_id = gen_uuid();
        let now = utc_now_iso();

        // Calibration path: derive the context_key from a Situation. A bare query degrades
        // exactly to the legacy `content_hash(normalize_query(query))`, so recall stays
        // zero-regression while sharing one key derivation with appraise (Spec §2.2).
        let situation = Situation::from_query(query);
        let context_key = situation.context_key(&self.situation_coarse_keys);

        let (q_content, q_trigger) = self
            .embedding
            .embed_both(query)
            .map_err(|e| InnateError::EmbeddingUnavailable(e.to_string()))?;

        // ANN candidates (non-spark)
        let mut candidates = self.ann_candidates(&q_content, &q_trigger)?;
        self.apply_soft_dep_bonus(&mut candidates)?;

        // Score + anti-trigger penalty
        let mut scored = self.score_candidates(candidates, query, &context_key, &now)?;

        // Relevance gate — drop sub-threshold candidates before packing/trace so the
        // trace records only what was actually surfaced (keeps selected→used stats clean).
        if let Some(min) = min_score {
            scored.retain(|(fused, _)| *fused >= min);
        }

        // First-fit pack with dep expansion
        let (selected, skipped, skipped_reasons) =
            self.pack(&scored, budget, expand_deps, allow_trim, query)?;

        let depth_skipped: Vec<String> = skipped_reasons
            .iter()
            .filter(|(_, r)| r.as_str() == "dep_depth_limit")
            .map(|(id, _)| id.clone())
            .collect();

        // Density refill
        let mut selected = selected;
        if self.density_refill {
            selected = self.density_refill(selected, &skipped, budget);
        }

        let limited = limit_knowledge(selected, top);
        let visible = if refine_mode == "adapt" {
            self.refiner
                .refine(limited.clone(), Some(budget))
                .unwrap_or(limited)
        } else {
            limited
        };

        // Sparks
        let sparks = if include_sparks {
            self.recall_sparks(&q_content, &q_trigger)?
        } else {
            vec![]
        };

        if trace {
            self.write_recall_trace(
                &trace_id,
                query,
                &context_key,
                &scored,
                &visible,
                &sparks,
                &depth_skipped,
                &skipped_reasons,
                refine_mode,
                source,
                &now,
                session_only,
            )?;
        }

        let empty = visible.is_empty() && sparks.is_empty();
        Ok(RecallResult {
            knowledge: visible,
            sparks,
            trace_id,
            empty,
            depth_skipped,
            skipped_reasons,
        })
    }

    pub(super) fn ann_candidates(
        &self,
        q_content: &[f32],
        q_trigger: &[f32],
    ) -> Result<HashMap<String, CandidateInfo>> {
        let embed_version = self
            .storage
            .get_meta("embed_version")?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(1);

        let content_res = self
            .storage
            .search_vec_content(q_content, self.top_k_candidates * 2)?;
        let trigger_res = self
            .storage
            .search_vec_trigger(q_trigger, self.top_k_candidates * 2)?;

        // Collect unique ids and batch-fetch all chunks in two queries instead of N individual ones.
        let all_ids: Vec<&str> = {
            let mut seen = HashSet::new();
            content_res
                .iter()
                .chain(trigger_res.iter())
                .map(|(id, _)| id.as_str())
                .filter(|id| seen.insert(*id))
                .collect()
        };
        let chunks = self.storage.get_chunks_by_ids(&all_ids)?;

        let mut candidates: HashMap<String, CandidateInfo> = HashMap::new();

        for (cid, sim) in &content_res {
            if let Some(chunk) = chunks.get(cid) {
                if chunk_is_valid_for_recall(chunk, embed_version) {
                    let e = candidates
                        .entry(cid.clone())
                        .or_insert_with(|| CandidateInfo {
                            chunk: chunk.clone(),
                            sim_content: 0.0,
                            sim_trigger: 0.0,
                        });
                    e.sim_content = e.sim_content.max(*sim);
                }
            }
        }
        for (cid, sim) in &trigger_res {
            if let Some(chunk) = chunks.get(cid) {
                if chunk_is_valid_for_recall(chunk, embed_version) {
                    let e = candidates
                        .entry(cid.clone())
                        .or_insert_with(|| CandidateInfo {
                            chunk: chunk.clone(),
                            sim_content: 0.0,
                            sim_trigger: 0.0,
                        });
                    e.sim_trigger = e.sim_trigger.max(*sim);
                }
            }
        }
        Ok(candidates)
    }

    pub(super) fn apply_soft_dep_bonus(
        &self,
        candidates: &mut HashMap<String, CandidateInfo>,
    ) -> Result<()> {
        // Collect non-spark candidate ids and batch-fetch their outgoing deps
        // in a single query (was one get_deps per candidate).
        let src_ids: Vec<String> = candidates
            .iter()
            .filter(|(_, info)| info.chunk.get("origin").and_then(Value::as_str) != Some("spark"))
            .map(|(cid, _)| cid.clone())
            .collect();
        if src_ids.is_empty() {
            return Ok(());
        }
        let src_refs: Vec<&str> = src_ids.iter().map(String::as_str).collect();
        let deps_map = self.storage.get_deps_batch(&src_refs)?;

        // Gather distinct soft-dep targets and batch-fetch them in one query
        // (was one get_chunk per soft edge).
        let mut target_ids: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for deps in deps_map.values() {
            for (dst, kind, _) in deps {
                if kind == "soft" && seen.insert(dst.clone()) {
                    target_ids.push(dst.clone());
                }
            }
        }
        if target_ids.is_empty() {
            return Ok(());
        }
        let target_refs: Vec<&str> = target_ids.iter().map(String::as_str).collect();
        let targets = self.storage.get_chunks_by_ids(&target_refs)?;

        for src in &src_ids {
            let Some(deps) = deps_map.get(src) else {
                continue;
            };
            for (dst, kind, _) in deps {
                if kind != "soft" {
                    continue;
                }
                let Some(target) = targets.get(dst) else {
                    continue;
                };
                if target.get("state").and_then(Value::as_str) == Some("archived") {
                    continue;
                }
                if target.get("origin").and_then(Value::as_str) == Some("spark") {
                    continue;
                }
                let e = candidates
                    .entry(dst.clone())
                    .or_insert_with(|| CandidateInfo {
                        chunk: target.clone(),
                        sim_content: 0.0,
                        sim_trigger: 0.0,
                    });
                e.sim_content = (e.sim_content + 0.05).min(1.0);
            }
        }
        Ok(())
    }

    fn score_candidates(
        &self,
        candidates: HashMap<String, CandidateInfo>,
        query: &str,
        context_key: &str,
        now: &str,
    ) -> Result<Vec<(f64, Value)>> {
        // Batch-fetch context scores for all candidates in one query
        // (was one context_score lookup per candidate).
        let cand_ids: Vec<String> = candidates
            .values()
            .filter_map(|info| {
                info.chunk
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        let cand_refs: Vec<&str> = cand_ids.iter().map(String::as_str).collect();
        // 方案 D 与 recall 解耦:recall 恒用中性 Laplace 先验,不读 intuition.* 旋钮。
        let ctx_scores = self.storage.context_scores_batch(
            &cand_refs,
            context_key,
            RECALL_PRIOR_M,
            RECALL_BASE_RATE,
        )?;

        let mut scored: Vec<(f64, Value)> = Vec::with_capacity(candidates.len());
        for info in candidates.into_values() {
            let conf = info
                .chunk
                .get("confidence")
                .and_then(Value::as_f64)
                .unwrap_or(0.5);
            let chunk_id = info.chunk.get("id").and_then(Value::as_str).unwrap_or("");
            let context_score = ctx_scores.get(chunk_id).copied().unwrap_or(0.0);
            // ACT-R base-level activation: recency × frequency from usage history.
            // Zero for never-used chunks, so freshly-added knowledge is unaffected.
            let used_count = info
                .chunk
                .get("used_count")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let last_used_at = info.chunk.get("last_used_at").and_then(Value::as_str);
            let activation = actr_activation(used_count, last_used_at, now);
            let mut fused = self.w_content * info.sim_content as f64
                + self.w_trigger * info.sim_trigger as f64
                + self.w_confidence * conf
                + self.w_context * context_score
                + self.w_activation * activation;
            if info.chunk.get("state").and_then(Value::as_str) == Some("pending") {
                fused *= PENDING_RECALL_PENALTY;
            }
            let anti = info
                .chunk
                .get("anti_trigger_desc")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !anti.is_empty() && anti_trigger_hit(query, anti) {
                fused *= self.anti_trigger_penalty;
            }
            let mut chunk = info.chunk;
            chunk["_context_score"] = json!(context_score);
            chunk["_activation"] = json!(activation);
            chunk["_fused_score"] = json!(fused);
            scored.push((fused, chunk));
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(self.top_k_candidates);
        Ok(scored)
    }

    fn pack(
        &self,
        scored: &[(f64, Value)],
        budget: usize,
        expand_deps: &str,
        allow_trim: bool,
        query: &str,
    ) -> Result<PackResult> {
        let mut selected: Vec<Value> = vec![];
        let mut skipped: Vec<(Vec<Value>, f64, usize)> = vec![];
        let mut skipped_reasons: HashMap<String, String> = HashMap::new();
        let mut used_ids: HashSet<String> = HashSet::new();
        let mut used_tokens: usize = 0;

        for (fused, chunk) in scored {
            let cid = chunk["id"].as_str().unwrap_or("").to_string();
            if used_ids.contains(&cid) {
                continue;
            }

            // Build block with dep expansion; fail-closed on dep issues.
            let (block, dep_skip_reason) = self.build_dep_block(chunk, expand_deps)?;
            if let Some(reason) = dep_skip_reason {
                skipped_reasons.insert(cid, reason);
                continue;
            }

            let new_block: Vec<Value> = block
                .iter()
                .filter(|b| !used_ids.contains(b["id"].as_str().unwrap_or("")))
                .cloned()
                .collect();
            let cost = block_cost(&new_block);

            if used_tokens + cost <= budget {
                for b in &block {
                    let bid = b["id"].as_str().unwrap_or("").to_string();
                    if !used_ids.contains(&bid) {
                        let mut b = b.clone();
                        b["_fused_score"] = json!(fused);
                        selected.push(b);
                        used_ids.insert(bid);
                    }
                }
                used_tokens += cost;
            } else if allow_trim {
                // Attempt refiner trim — NullRefiner returns None (no-op).
                if let Some(trimmed) =
                    self.refiner
                        .trim(&block, query, budget.saturating_sub(used_tokens))
                {
                    let trim_cost = block_cost(&trimmed);
                    if used_tokens + trim_cost <= budget {
                        for b in &trimmed {
                            let bid = b["id"].as_str().unwrap_or("").to_string();
                            if !used_ids.contains(&bid) {
                                let mut b = b.clone();
                                b["_fused_score"] = json!(fused);
                                b["_trimmed"] = json!(true);
                                selected.push(b);
                                used_ids.insert(bid);
                            }
                        }
                        used_tokens += trim_cost;
                        continue;
                    }
                }
                skipped.push((block, *fused, cost));
            } else {
                skipped.push((block, *fused, cost));
            }
        }
        Ok((selected, skipped, skipped_reasons))
    }

    /// Expand a seed chunk into a block according to `expand_deps`.
    /// Returns `(block, Some(skip_reason))` if the block should be discarded (fail-closed).
    fn build_dep_block(
        &self,
        seed: &Value,
        expand_deps: &str,
    ) -> Result<(Vec<Value>, Option<String>)> {
        if expand_deps == "false" || expand_deps.is_empty() {
            return Ok((vec![seed.clone()], None));
        }
        let seed_id = seed["id"].as_str().unwrap_or("");
        match expand_deps {
            "direct" => {
                let deps = self.storage.get_deps(seed_id)?;
                let mut block = vec![seed.clone()];
                for (dep_id, kind, _) in &deps {
                    if kind != "hard" {
                        continue;
                    }
                    match self.validate_hard_dep(dep_id)? {
                        Some(chunk) => block.push(chunk),
                        None => return Ok((vec![], Some("hard_dep_unavailable".to_string()))),
                    }
                }
                Ok((block, None))
            }
            "closure" => {
                let mut block = vec![seed.clone()];
                let mut visited: HashSet<String> = [seed_id.to_string()].into();
                match self.expand_hard_closure(seed_id, &mut visited, &mut block, 0, 3)? {
                    Some(reason) => Ok((vec![], Some(reason))),
                    None => Ok((block, None)),
                }
            }
            _ => Ok((vec![seed.clone()], None)),
        }
    }

    /// Returns the chunk if the hard dep is usable, None if it should cause fail-closed.
    fn validate_hard_dep(&self, dep_id: &str) -> Result<Option<Value>> {
        match self.storage.get_chunk(dep_id)? {
            None => Ok(None),
            Some(chunk) => {
                let state = chunk.get("state").and_then(Value::as_str).unwrap_or("");
                let origin = chunk.get("origin").and_then(Value::as_str).unwrap_or("");
                let embed_v = chunk
                    .get("embed_version")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                if state == "archived" || origin == "spark" || embed_v == 0 {
                    Ok(None)
                } else {
                    Ok(Some(chunk))
                }
            }
        }
    }

    /// BFS hard-dep expansion up to `max_depth`. Returns Some(reason) on fail-closed.
    fn expand_hard_closure(
        &self,
        id: &str,
        visited: &mut HashSet<String>,
        block: &mut Vec<Value>,
        depth: usize,
        max_depth: usize,
    ) -> Result<Option<String>> {
        if depth >= max_depth {
            return Ok(Some("dep_depth_limit".to_string()));
        }
        let deps = self.storage.get_deps(id)?;
        for (dep_id, kind, _) in &deps {
            if kind != "hard" {
                continue;
            }
            if visited.contains(dep_id) {
                continue;
            } // cycle guard
            visited.insert(dep_id.clone());
            match self.validate_hard_dep(dep_id)? {
                None => return Ok(Some("hard_dep_unavailable".to_string())),
                Some(chunk) => {
                    block.push(chunk);
                    if let Some(reason) =
                        self.expand_hard_closure(dep_id, visited, block, depth + 1, max_depth)?
                    {
                        return Ok(Some(reason));
                    }
                }
            }
        }
        Ok(None)
    }

    fn density_refill(
        &self,
        mut selected: Vec<Value>,
        skipped: &[(Vec<Value>, f64, usize)],
        budget: usize,
    ) -> Vec<Value> {
        let used_tokens = block_cost(&selected);
        if used_tokens >= budget {
            return selected;
        }

        let selected_ids: HashSet<String> = selected
            .iter()
            .filter_map(|c| c["id"].as_str().map(str::to_string))
            .collect();

        let mut density_items: Vec<(f64, Vec<Value>, usize)> = skipped
            .iter()
            .filter_map(|(block, fscore, _)| {
                let block: Vec<Value> = block
                    .iter()
                    .filter(|b| !selected_ids.contains(b["id"].as_str().unwrap_or("")))
                    .cloned()
                    .collect();
                if block.is_empty() {
                    return None;
                }
                let cost = block_cost(&block);
                let density = fscore / cost.max(1) as f64;
                Some((density, block, cost))
            })
            .collect();
        density_items.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut used_tokens = block_cost(&selected);
        let mut added_ids: HashSet<String> = selected_ids;
        for (_, block, cost) in density_items {
            if used_tokens + cost <= budget {
                for b in block {
                    let bid = b["id"].as_str().unwrap_or("").to_string();
                    if !added_ids.contains(&bid) {
                        selected.push(b);
                        added_ids.insert(bid);
                    }
                }
                used_tokens += cost;
            }
        }
        selected
    }

    fn recall_sparks(&self, q_content: &[f32], q_trigger: &[f32]) -> Result<Vec<Value>> {
        let embed_version = self
            .storage
            .get_meta("embed_version")?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(1);

        let content_res = self
            .storage
            .search_vec_content(q_content, self.top_k_candidates)?;
        let trigger_res = self
            .storage
            .search_vec_trigger(q_trigger, self.top_k_candidates)?;

        // Batch-fetch all candidate chunk IDs (mirrors the pattern in ann_candidates).
        let all_ids: Vec<&str> = {
            let mut seen = HashSet::new();
            content_res
                .iter()
                .chain(trigger_res.iter())
                .map(|(id, _)| id.as_str())
                .filter(|id| seen.insert(*id))
                .collect()
        };
        let chunks = self.storage.get_chunks_by_ids(&all_ids)?;

        let mut spark_scores: HashMap<String, (f32, Value)> = HashMap::new();
        for (cid, sim) in content_res.iter().chain(trigger_res.iter()) {
            if let Some(chunk) = chunks.get(cid) {
                if chunk.get("origin").and_then(Value::as_str) != Some("spark") {
                    continue;
                }
                if chunk.get("state").and_then(Value::as_str) == Some("archived") {
                    continue;
                }
                let maturity = chunk.get("maturity").and_then(Value::as_str).unwrap_or("");
                if maturity == "promoted" || maturity == "dropped" {
                    continue;
                }
                let ev = chunk
                    .get("embed_version")
                    .and_then(Value::as_i64)
                    .unwrap_or(1);
                if ev < embed_version {
                    continue;
                }
                let entry = spark_scores
                    .entry(cid.clone())
                    .or_insert_with(|| (*sim, chunk.clone()));
                if *sim > entry.0 {
                    *entry = (*sim, chunk.clone());
                }
            }
        }
        let mut sparks: Vec<(f32, Value)> = spark_scores.into_values().collect();
        sparks.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(sparks
            .into_iter()
            .take(self.top_k_candidates)
            .map(|(_, c)| c)
            .collect())
    }

    #[allow(clippy::too_many_arguments)]
    fn write_recall_trace(
        &self,
        trace_id: &str,
        query: &str,
        context_key: &str,
        scored: &[(f64, Value)],
        visible: &[Value],
        sparks: &[Value],
        depth_skipped: &[String],
        skipped_reasons: &HashMap<String, String>,
        refine_mode: &str,
        source: &str,
        now: &str,
        session_only: bool,
    ) -> Result<()> {
        let lib_id = self.storage.lib_id()?;
        // `selected` must strictly mean "entered the model context". Record the
        // per-chunk retrieved/selected/refined events only when the result is
        // actually surfaced: skip them for empty results (nothing surfaced) and
        // for session-only recalls (daemon discards the knowledge). The episodic
        // log is still written in both cases — an empty result as a terminal
        // `known_none`/`discarded` row (no-answer telemetry, never `open`), a
        // session recall as an `open` row for later record-correlation.
        let is_empty = visible.is_empty() && sparks.is_empty();
        let record_selection = !is_empty && !session_only;
        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            if record_selection {
                for (rank, (_, chunk)) in scored.iter().enumerate() {
                    let cid = chunk["id"].as_str().unwrap_or("");
                    let sim = chunk.get("_fused_score").and_then(Value::as_f64);
                    // For dep-skipped seeds, record their skip reason as refine_mode.
                    let rm = skipped_reasons
                        .get(cid)
                        .map(|r| format!("skipped:{r}"))
                        .or_else(|| {
                            if refine_mode != "off" && !refine_mode.is_empty() {
                                Some(refine_mode.to_string())
                            } else {
                                None
                            }
                        });
                    self.storage.insert_usage_trace(
                        trace_id,
                        Some(cid),
                        "retrieved",
                        1.0,
                        sim,
                        rm.as_deref(),
                        None,
                        Some((rank + 1) as i64),
                        None,
                        source,
                        now,
                    )?;
                }
                for (rank, chunk) in visible.iter().enumerate() {
                    let cid = chunk["id"].as_str().unwrap_or("");
                    self.storage.insert_usage_trace(
                        trace_id,
                        Some(cid),
                        "selected",
                        1.0,
                        None,
                        None,
                        None,
                        Some((rank + 1) as i64),
                        None,
                        source,
                        now,
                    )?;
                    // Write 'refined' event for chunks that came through the trim path.
                    if chunk
                        .get("_trimmed")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        self.storage.insert_usage_trace(
                            trace_id,
                            Some(cid),
                            "refined",
                            1.0,
                            None,
                            Some("trim"),
                            None,
                            Some((rank + 1) as i64),
                            None,
                            source,
                            now,
                        )?;
                    }
                }
                // Write 'retrieved' events for sparks (for recurring-spark count tracking).
                for (rank, chunk) in sparks.iter().enumerate() {
                    let cid = chunk["id"].as_str().unwrap_or("");
                    self.storage.insert_usage_trace(
                        trace_id,
                        Some(cid),
                        "retrieved",
                        1.0,
                        None,
                        Some("spark"),
                        None,
                        Some((rank + 1) as i64),
                        None,
                        source,
                        now,
                    )?;
                }
            }
            // The snapshot mirrors what was surfaced: empty for known_none and
            // session-only recalls so no chunk is credited with a selection.
            let snapshot = json!({
                "retrieved": if record_selection { scored.iter().map(|(_, c)| c["id"].as_str().unwrap_or("")).collect::<Vec<_>>() } else { vec![] },
                "selected": if record_selection { visible.iter().map(|c| c["id"].as_str().unwrap_or("")).collect::<Vec<_>>() } else { vec![] },
                "sparks": if record_selection { sparks.iter().map(|c| c["id"].as_str().unwrap_or("")).collect::<Vec<_>>() } else { vec![] },
                "depth_skipped": depth_skipped,
                "skipped_reasons": skipped_reasons,
                "session_only": session_only,
            });
            // Empty recall → terminal known_none/discarded (kept out of the `open`
            // pool that feeds trace-completion stats). Otherwise `open` for the
            // record() outcome transition (incl. session-only daemon traces).
            // `usage_state='known_none'` is the no-answer signal; `task_state`
            // stays 'recalled' (both bounded by schema CHECK constraints).
            let (usage_state, distill_state) = if is_empty {
                ("known_none", "discarded")
            } else {
                ("unknown", "open")
            };
            let log = EpisodicLogRow {
                id: gen_uuid(),
                trace_id: trace_id.to_string(),
                lib_id,
                ts: now.to_string(),
                query: Some(query.to_string()),
                recall_snapshot: Some(snapshot.to_string()),
                event_source: source.to_string(),
                task_state: "recalled".to_string(),
                usage_state: usage_state.to_string(),
                context_key: Some(context_key.to_string()),
                distill_state: distill_state.to_string(),
                ..Default::default()
            };
            self.storage.upsert_episodic_log(&log)?;
            self.storage.commit()
        })();
        if result.is_err() {
            let _ = self.storage.rollback();
        }
        result
    }
}
