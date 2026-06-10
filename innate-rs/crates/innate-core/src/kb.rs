//! KnowledgeBase — all 8 Public APIs.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::embedding::{EmbeddingProvider, DummyEmbeddingProvider};
use crate::errors::{InnateError, Result};
use crate::refine::{Distiller, HeuristicDistiller, NullRefiner, Refiner};
use crate::storage::{ChunkRow, EpisodicLogRow, Storage};
use crate::utils::{
    content_hash, default_sanitize, estimate_tokens, gen_uuid, pack_embedding, utc_now_iso,
};

// ---------------------------------------------------------------------------
// Tuning defaults
// ---------------------------------------------------------------------------

const W_CONTENT: f64 = 0.65;
const W_TRIGGER: f64 = 0.25;
const W_CONFIDENCE: f64 = 0.10;
const TOP_K_CANDIDATES: usize = 20;
const ANTI_TRIGGER_PENALTY: f64 = 0.6;
const DENSITY_REFILL: bool = true;

const LOW_CONF_THRESHOLD: f64 = 0.25;
const LOW_CONF_IDLE_DAYS: i64 = 60;
const REPEAT_SELECT_MIN: i64 = 10;
const REPEAT_SELECT_CONF_MAX: f64 = 0.5;
const NEVER_USED_AGE_DAYS: i64 = 30;
const OPEN_TTL_DAYS: i64 = 7;
const SCREENING_TIMEOUT_MINUTES: i64 = 30;
const PROMOTE_USED_SUCCESS_MIN: i64 = 3;
const PROMOTE_CONFIDENCE_MIN: f64 = 0.65;
const EVOLVE_THRESHOLD: i64 = 5;
const DISTILL_BATCH_SIZE: usize = 20;

// ---------------------------------------------------------------------------
// Public result types
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct RecallResult {
    pub knowledge: Vec<Value>,
    pub sparks: Vec<Value>,
    pub trace_id: String,
    pub empty: bool,
    pub depth_skipped: Vec<String>,
    pub skipped_reasons: HashMap<String, String>,
}

#[derive(Debug, Default)]
pub struct CurateReport {
    pub archived: Vec<String>,
    pub deduped: Vec<String>,
    pub decayed: Vec<String>,
    pub cycles: Vec<Vec<String>>,
    pub orphans: Vec<String>,
    pub recovered: Vec<String>,
    pub warnings: Vec<String>,
    pub stats: HashMap<String, Value>,
}

// ---------------------------------------------------------------------------
// KnowledgeBase
// ---------------------------------------------------------------------------

pub struct KnowledgeBase {
    pub storage: Storage,
    embedding: Arc<dyn EmbeddingProvider>,
    refiner: Arc<dyn Refiner>,
    distiller: Arc<dyn Distiller>,
    sanitize: Option<fn(&str) -> Option<String>>,

    // Tuning params (loaded from meta at init)
    w_content: f64,
    w_trigger: f64,
    w_confidence: f64,
    top_k_candidates: usize,
    anti_trigger_penalty: f64,
    density_refill: bool,

    low_conf_threshold: f64,
    low_conf_idle_days: i64,
    repeat_select_min: i64,
    repeat_select_conf_max: f64,
    never_used_age_days: i64,
    open_ttl_days: i64,
    screening_timeout_minutes: i64,
    promote_used_success_min: i64,
    promote_confidence_min: f64,
    evolve_threshold: i64,
    distill_batch_size: usize,
}

impl KnowledgeBase {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(db_path, None, None, None)
    }

    pub fn open_with(
        db_path: impl AsRef<Path>,
        embedding: Option<Arc<dyn EmbeddingProvider>>,
        refiner: Option<Arc<dyn Refiner>>,
        distiller: Option<Arc<dyn Distiller>>,
    ) -> Result<Self> {
        let embedding = embedding
            .unwrap_or_else(|| Arc::new(DummyEmbeddingProvider::default()));
        let refiner = refiner.unwrap_or_else(|| Arc::new(NullRefiner));
        let distiller = distiller.unwrap_or_else(|| Arc::new(HeuristicDistiller));

        let storage = Storage::open(
            db_path,
            embedding.content_dim(),
            embedding.trigger_dim(),
        )?;

        let mut kb = Self {
            storage,
            embedding,
            refiner,
            distiller,
            sanitize: Some(|s| default_sanitize(s)),
            w_content: W_CONTENT,
            w_trigger: W_TRIGGER,
            w_confidence: W_CONFIDENCE,
            top_k_candidates: TOP_K_CANDIDATES,
            anti_trigger_penalty: ANTI_TRIGGER_PENALTY,
            density_refill: DENSITY_REFILL,
            low_conf_threshold: LOW_CONF_THRESHOLD,
            low_conf_idle_days: LOW_CONF_IDLE_DAYS,
            repeat_select_min: REPEAT_SELECT_MIN,
            repeat_select_conf_max: REPEAT_SELECT_CONF_MAX,
            never_used_age_days: NEVER_USED_AGE_DAYS,
            open_ttl_days: OPEN_TTL_DAYS,
            screening_timeout_minutes: SCREENING_TIMEOUT_MINUTES,
            promote_used_success_min: PROMOTE_USED_SUCCESS_MIN,
            promote_confidence_min: PROMOTE_CONFIDENCE_MIN,
            evolve_threshold: EVOLVE_THRESHOLD,
            distill_batch_size: DISTILL_BATCH_SIZE,
        };
        kb.init_meta()?;
        kb.load_params()?;
        Ok(kb)
    }

    fn init_meta(&self) -> Result<()> {
        let lib_id = gen_uuid();
        let content_dim = self.embedding.content_dim().to_string();
        let trigger_dim = self.embedding.trigger_dim().to_string();
        let defaults: &[(&str, &str)] = &[
            ("lib_id", &lib_id),
            ("lib_role", "personal"),
            ("schema_version", "4.5.1"),
            ("content_dim", &content_dim),
            ("trigger_dim", &trigger_dim),
            ("embed_model", "DummyEmbeddingProvider"),
            ("embed_version", "1"),
            ("last_agg_ts", "1970-01-01T00:00:00.000Z"),
            ("recall.w_content", "0.65"),
            ("recall.w_trigger", "0.25"),
            ("recall.w_confidence", "0.10"),
            ("recall.top_k_candidates", "20"),
            ("recall.anti_trigger_penalty", "0.6"),
            ("recall.density_refill", "true"),
            ("curate.low_conf_threshold", "0.25"),
            ("curate.low_conf_idle_days", "60"),
            ("curate.repeat_select_min", "10"),
            ("curate.repeat_select_conf_max", "0.5"),
            ("curate.never_used_age_days", "30"),
            ("curate.open_ttl_days", "7"),
            ("curate.screening_timeout_minutes", "30"),
            ("curate.promote_used_success_min", "3"),
            ("curate.promote_confidence_min", "0.65"),
            ("evolve.threshold_new_count", "5"),
            ("evolve.distill_batch_size", "20"),
        ];
        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            for (k, v) in defaults {
                if self.storage.get_meta(k)?.is_none() {
                    self.storage.set_meta(k, v)?;
                }
            }
            self.storage.commit()
        })();
        if result.is_err() { let _ = self.storage.rollback(); }
        result
    }

    fn load_params(&mut self) -> Result<()> {
        let f = |k: &str, d: f64| -> f64 {
            self.storage.get_meta(k).ok().flatten()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        let i = |k: &str, d: i64| -> i64 {
            self.storage.get_meta(k).ok().flatten()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        let b = |k: &str, d: bool| -> bool {
            self.storage.get_meta(k).ok().flatten()
                .map(|v| v.to_lowercase() == "true")
                .unwrap_or(d)
        };
        self.w_content = f("recall.w_content", W_CONTENT);
        self.w_trigger = f("recall.w_trigger", W_TRIGGER);
        self.w_confidence = f("recall.w_confidence", W_CONFIDENCE);
        self.top_k_candidates = i("recall.top_k_candidates", TOP_K_CANDIDATES as i64) as usize;
        self.anti_trigger_penalty = f("recall.anti_trigger_penalty", ANTI_TRIGGER_PENALTY);
        self.density_refill = b("recall.density_refill", DENSITY_REFILL);
        self.low_conf_threshold = f("curate.low_conf_threshold", LOW_CONF_THRESHOLD);
        self.low_conf_idle_days = i("curate.low_conf_idle_days", LOW_CONF_IDLE_DAYS);
        self.repeat_select_min = i("curate.repeat_select_min", REPEAT_SELECT_MIN);
        self.repeat_select_conf_max = f("curate.repeat_select_conf_max", REPEAT_SELECT_CONF_MAX);
        self.never_used_age_days = i("curate.never_used_age_days", NEVER_USED_AGE_DAYS);
        self.open_ttl_days = i("curate.open_ttl_days", OPEN_TTL_DAYS);
        self.screening_timeout_minutes = i("curate.screening_timeout_minutes", SCREENING_TIMEOUT_MINUTES);
        self.promote_used_success_min = i("curate.promote_used_success_min", PROMOTE_USED_SUCCESS_MIN);
        self.promote_confidence_min = f("curate.promote_confidence_min", PROMOTE_CONFIDENCE_MIN);
        self.evolve_threshold = i("evolve.threshold_new_count", EVOLVE_THRESHOLD);
        self.distill_batch_size = i("evolve.distill_batch_size", DISTILL_BATCH_SIZE as i64) as usize;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Public API 1: recall
    // ------------------------------------------------------------------

    pub fn recall(
        &self,
        query: &str,
        budget: usize,
        trace: bool,
        include_sparks: bool,
        top: Option<usize>,
        source: &str,
    ) -> Result<RecallResult> {
        validate_source(source)?;
        let trace_id = gen_uuid();
        let now = utc_now_iso();

        let q_content = self.embedding.embed_content(query)
            .map_err(|e| InnateError::EmbeddingUnavailable(e.to_string()))?;
        let q_trigger = self.embedding.embed_trigger(query)
            .map_err(|e| InnateError::EmbeddingUnavailable(e.to_string()))?;

        // ANN candidates (non-spark)
        let mut candidates = self.ann_candidates(&q_content, &q_trigger)?;
        self.apply_soft_dep_bonus(&mut candidates)?;

        // Score + anti-trigger penalty
        let scored = self.score_candidates(candidates, query);

        // First-fit pack
        let (selected, skipped, skipped_reasons) = self.pack(&scored, budget)?;

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

        let visible = limit_knowledge(selected.clone(), top);

        // Sparks
        let sparks = if include_sparks {
            self.recall_sparks(&q_content, &q_trigger)?
        } else {
            vec![]
        };

        if trace {
            self.write_recall_trace(
                &trace_id, query, &scored, &visible, &sparks,
                &depth_skipped, &skipped_reasons, source, &now,
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

    fn ann_candidates(&self, q_content: &[f32], q_trigger: &[f32]) -> Result<HashMap<String, CandidateInfo>> {
        let embed_version = self.storage.get_meta("embed_version")?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(1);

        let content_res = self.storage.search_vec_content(q_content, self.top_k_candidates * 2)?;
        let trigger_res = self.storage.search_vec_trigger(q_trigger, self.top_k_candidates * 2)?;

        let mut candidates: HashMap<String, CandidateInfo> = HashMap::new();

        for (cid, sim) in &content_res {
            if let Some(chunk) = self.storage.get_chunk(cid)? {
                if chunk_is_valid_for_recall(&chunk, embed_version) {
                    let e = candidates.entry(cid.clone()).or_insert_with(|| CandidateInfo {
                        chunk: chunk.clone(),
                        sim_content: 0.0,
                        sim_trigger: 0.0,
                    });
                    e.sim_content = e.sim_content.max(*sim);
                }
            }
        }
        for (cid, sim) in &trigger_res {
            if let Some(chunk) = self.storage.get_chunk(cid)? {
                if chunk_is_valid_for_recall(&chunk, embed_version) {
                    let e = candidates.entry(cid.clone()).or_insert_with(|| CandidateInfo {
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

    fn apply_soft_dep_bonus(&self, candidates: &mut HashMap<String, CandidateInfo>) -> Result<()> {
        let ids: Vec<String> = candidates.keys().cloned().collect();
        for cid in ids {
            if candidates[&cid].chunk.get("origin").and_then(Value::as_str) == Some("spark") {
                continue;
            }
            let deps = self.storage.get_deps(&cid)?;
            for (dst, kind, _) in &deps {
                if kind != "soft" { continue; }
                if let Some(target) = self.storage.get_chunk(dst)? {
                    if target.get("state").and_then(Value::as_str) == Some("archived") { continue; }
                    if target.get("origin").and_then(Value::as_str) == Some("spark") { continue; }
                    let e = candidates.entry(dst.clone()).or_insert_with(|| CandidateInfo {
                        chunk: target,
                        sim_content: 0.0,
                        sim_trigger: 0.0,
                    });
                    e.sim_content = (e.sim_content + 0.05).min(1.0);
                }
            }
        }
        Ok(())
    }

    fn score_candidates(
        &self,
        candidates: HashMap<String, CandidateInfo>,
        query: &str,
    ) -> Vec<(f64, Value)> {
        let mut scored: Vec<(f64, Value)> = candidates
            .into_values()
            .map(|info| {
                let conf = info.chunk.get("confidence")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.5);
                let mut fused = self.w_content * info.sim_content as f64
                    + self.w_trigger * info.sim_trigger as f64
                    + self.w_confidence * conf;
                let anti = info.chunk.get("anti_trigger_desc")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !anti.is_empty() && anti_trigger_hit(query, anti) {
                    fused *= self.anti_trigger_penalty;
                }
                let mut chunk = info.chunk;
                chunk["_fused_score"] = json!(fused);
                (fused, chunk)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(self.top_k_candidates);
        scored
    }

    fn pack(
        &self,
        scored: &[(f64, Value)],
        budget: usize,
    ) -> Result<(Vec<Value>, Vec<(Vec<Value>, f64, usize)>, HashMap<String, String>)> {
        let mut selected: Vec<Value> = vec![];
        let mut skipped: Vec<(Vec<Value>, f64, usize)> = vec![];
        let mut skipped_reasons: HashMap<String, String> = HashMap::new();
        let mut used_ids: HashSet<String> = HashSet::new();
        let mut used_tokens: usize = 0;

        for (fused, chunk) in scored {
            let cid = chunk["id"].as_str().unwrap_or("").to_string();
            if used_ids.contains(&cid) { continue; }

            let block = vec![chunk.clone()]; // No dep expansion in base implementation
            let new_block: Vec<Value> = block.iter()
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
            } else {
                skipped.push((block, *fused, cost));
            }
        }
        Ok((selected, skipped, skipped_reasons))
    }

    fn density_refill(
        &self,
        mut selected: Vec<Value>,
        skipped: &[(Vec<Value>, f64, usize)],
        budget: usize,
    ) -> Vec<Value> {
        let used_tokens = block_cost(&selected);
        if used_tokens >= budget { return selected; }

        let selected_ids: HashSet<String> = selected.iter()
            .filter_map(|c| c["id"].as_str().map(str::to_string))
            .collect();

        let mut density_items: Vec<(f64, Vec<Value>, usize)> = skipped.iter()
            .filter_map(|(block, fscore, _)| {
                let block: Vec<Value> = block.iter()
                    .filter(|b| !selected_ids.contains(b["id"].as_str().unwrap_or("")))
                    .cloned()
                    .collect();
                if block.is_empty() { return None; }
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
        let embed_version = self.storage.get_meta("embed_version")?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(1);

        let content_res = self.storage.search_vec_content(q_content, self.top_k_candidates)?;
        let trigger_res = self.storage.search_vec_trigger(q_trigger, self.top_k_candidates)?;

        let mut spark_scores: HashMap<String, (f32, Value)> = HashMap::new();
        for (cid, sim) in content_res.iter().chain(trigger_res.iter()) {
            if let Some(chunk) = self.storage.get_chunk(cid)? {
                if chunk.get("origin").and_then(Value::as_str) != Some("spark") { continue; }
                if chunk.get("state").and_then(Value::as_str) == Some("archived") { continue; }
                let maturity = chunk.get("maturity").and_then(Value::as_str).unwrap_or("");
                if maturity == "promoted" || maturity == "dropped" { continue; }
                let ev = chunk.get("embed_version").and_then(Value::as_i64).unwrap_or(1);
                if ev < embed_version { continue; }
                let entry = spark_scores.entry(cid.clone()).or_insert_with(|| (*sim, chunk.clone()));
                if *sim > entry.0 { *entry = (*sim, chunk); }
            }
        }
        let mut sparks: Vec<(f32, Value)> = spark_scores.into_values().collect();
        sparks.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(sparks.into_iter().take(self.top_k_candidates).map(|(_, c)| c).collect())
    }

    fn write_recall_trace(
        &self,
        trace_id: &str,
        query: &str,
        scored: &[(f64, Value)],
        visible: &[Value],
        sparks: &[Value],
        depth_skipped: &[String],
        skipped_reasons: &HashMap<String, String>,
        source: &str,
        now: &str,
    ) -> Result<()> {
        let lib_id = self.storage.lib_id()?;
        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            for (rank, (_, chunk)) in scored.iter().enumerate() {
                let cid = chunk["id"].as_str().unwrap_or("");
                let sim = chunk.get("_fused_score").and_then(Value::as_f64);
                self.storage.insert_usage_trace(
                    trace_id, Some(cid), "retrieved", 1.0, sim, None,
                    Some((rank + 1) as i64), source, now,
                )?;
            }
            for (rank, chunk) in visible.iter().enumerate() {
                let cid = chunk["id"].as_str().unwrap_or("");
                self.storage.insert_usage_trace(
                    trace_id, Some(cid), "selected", 1.0, None, None,
                    Some((rank + 1) as i64), source, now,
                )?;
            }
            let snapshot = json!({
                "retrieved": scored.iter().map(|(_, c)| c["id"].as_str().unwrap_or("")).collect::<Vec<_>>(),
                "selected": visible.iter().map(|c| c["id"].as_str().unwrap_or("")).collect::<Vec<_>>(),
                "sparks": sparks.iter().map(|c| c["id"].as_str().unwrap_or("")).collect::<Vec<_>>(),
                "depth_skipped": depth_skipped,
                "skipped_reasons": skipped_reasons,
            });
            let log = EpisodicLogRow {
                id: gen_uuid(),
                trace_id: trace_id.to_string(),
                lib_id,
                ts: now.to_string(),
                query: Some(query.to_string()),
                recall_snapshot: Some(snapshot.to_string()),
                event_source: source.to_string(),
                distill_state: "open".to_string(),
                ..Default::default()
            };
            self.storage.upsert_episodic_log(&log)?;
            self.storage.commit()
        })();
        if result.is_err() { let _ = self.storage.rollback(); }
        result
    }

    // ------------------------------------------------------------------
    // Public API 2: record
    // ------------------------------------------------------------------

    pub fn record(
        &self,
        trace_id: &str,
        query: Option<&str>,
        output: Option<&str>,
        output_summary: Option<&str>,
        outcome: Option<&str>,
        used: Option<&[String]>,
        feedback_up: Option<&[String]>,
        feedback_down: Option<&[String]>,
        nomination: Option<&str>,
        priority: i64,
        source: &str,
    ) -> Result<()> {
        if let Some(o) = outcome {
            if !matches!(o, "ok" | "fail" | "unknown") {
                return Err(InnateError::InvalidState(format!("invalid outcome: {o}")));
            }
        }
        validate_source(source)?;
        let effective_priority = if nomination.is_some() && priority == 0 { 1 } else { priority };
        let now = utc_now_iso();
        let lib_id = self.storage.lib_id()?;

        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            let log = self.storage.get_episodic_log(trace_id)?;
            let mut is_fresh_insert = false;
            let log = match log {
                Some(l) => l,
                None => {
                    let row = EpisodicLogRow {
                        id: gen_uuid(),
                        trace_id: trace_id.to_string(),
                        lib_id,
                        ts: now.clone(),
                        query: query.map(str::to_string).or_else(|| Some(String::new())),
                        output: output.map(str::to_string),
                        output_summary: output_summary.map(str::to_string),
                        outcome: outcome.map(str::to_string),
                        event_source: source.to_string(),
                        nomination: nomination.map(str::to_string),
                        priority: effective_priority,
                        distill_state: "open".to_string(),
                        ..Default::default()
                    };
                    self.storage.upsert_episodic_log(&row)?;
                    is_fresh_insert = true;
                    self.storage.get_episodic_log(trace_id)?.unwrap()
                }
            };

            let existing_outcome = log.get("outcome").and_then(Value::as_str).map(str::to_string);
            if let Some(new_outcome) = outcome {
                if let Some(ref ex) = existing_outcome {
                    if ex != new_outcome {
                        return Err(InnateError::OutcomeConflict {
                            trace_id: trace_id.to_string(),
                            existing: ex.clone(),
                            requested: new_outcome.to_string(),
                        });
                    }
                }
            }

            // usage_trace: used
            if let Some(used_ids) = used {
                for cid in used_ids {
                    self.storage.insert_usage_trace(
                        trace_id, Some(cid), "used", 0.3, None, None, None, source, &now,
                    )?;
                    self.storage.update_chunk_last_used(cid, &now)?;
                }
            }

            // usage_trace: task_ok / task_fail
            if let Some(o) = outcome {
                if matches!(o, "ok" | "fail") {
                    let event = if o == "ok" { "task_ok" } else { "task_fail" };
                    let strength = if event == "task_fail" { 0.15 } else { 1.0 };
                    self.storage.insert_usage_trace(
                        trace_id, None, event, strength, None, None, None, source, &now,
                    )?;
                }
            }

            // confidence implicit update
            if outcome.is_some() && (is_fresh_insert || existing_outcome.is_none()) {
                self.apply_outcome_implicit(trace_id, outcome.unwrap(), used, &now)?;
            }

            // feedback
            if let Some(ups) = feedback_up {
                for cid in ups {
                    self.update_confidence(cid, 1.0, 1.0, "user_up", &now)?;
                    self.storage.update_chunk_last_used(cid, &now)?;
                }
            }
            if let Some(downs) = feedback_down {
                for cid in downs {
                    self.update_confidence(cid, 0.0, 1.0, "user_down", &now)?;
                }
            }

            // Update episodic log
            let current_state = log.get("distill_state").and_then(Value::as_str).unwrap_or("open");
            let outcome_completed = outcome.is_some()
                || existing_outcome.is_some();
            let new_state = if current_state == "open" && outcome_completed {
                let has_material = output_summary.is_some()
                    || nomination.is_some()
                    || output.is_some()
                    || log.get("output_summary").and_then(Value::as_str).is_some()
                    || log.get("nomination").and_then(Value::as_str).is_some()
                    || log.get("output").and_then(Value::as_str).is_some()
                    || (used.map(|u| !u.is_empty()).unwrap_or(false) && outcome.map(|o| o != "unknown").unwrap_or(false));
                if has_material { Some("new") } else { Some("discarded") }
            } else {
                None
            };
            if let Some(state) = new_state {
                let note = if state == "discarded" { Some("insufficient_material") } else { None };
                let outcome_str = outcome.map(str::to_string);
                self.storage.update_episodic_log_state(
                    trace_id, state, note, outcome_str.as_deref(),
                )?;
            } else if outcome.is_some() {
                let outcome_str = outcome.map(str::to_string);
                self.storage.update_episodic_log_state(
                    trace_id, current_state, None, outcome_str.as_deref(),
                )?;
            }

            self.storage.commit()
        })();
        if result.is_err() { let _ = self.storage.rollback(); }
        result
    }

    fn apply_outcome_implicit(
        &self,
        trace_id: &str,
        outcome: &str,
        used: Option<&[String]>,
        now: &str,
    ) -> Result<()> {
        let used_set: HashSet<&str> = used.map(|u| u.iter().map(String::as_str).collect())
            .unwrap_or_default();
        let (target, strength, reason) = if outcome == "ok" {
            (1.0, 0.3, "agent_used")
        } else {
            (0.0, 0.15, "task_fail")
        };
        for cid in &used_set {
            self.update_confidence(cid, target, strength, reason, now)?;
        }
        // selected but not used: very weak downgrade
        let selected_rows = self.storage.query_chunks_params(
            "SELECT chunk_id FROM usage_trace WHERE trace_id=? AND event='selected' AND chunk_id IS NOT NULL",
            rusqlite::params![trace_id],
        )?;
        for row in selected_rows {
            if let Some(cid) = row.get("chunk_id").and_then(Value::as_str) {
                if !used_set.contains(cid) {
                    self.update_confidence(cid, 0.3, 0.1, "selected_unused", now)?;
                }
            }
        }
        Ok(())
    }

    fn update_confidence(
        &self,
        chunk_id: &str,
        target: f64,
        strength: f64,
        reason: &str,
        now: &str,
    ) -> Result<()> {
        let chunk = match self.storage.get_chunk(chunk_id)? {
            Some(c) => c,
            None => return Ok(()),
        };
        if chunk.get("origin").and_then(Value::as_str) == Some("spark") {
            return Ok(());
        }
        let alpha = 0.2_f64;
        let conf = chunk.get("confidence").and_then(Value::as_f64).unwrap_or(0.5);
        let effective_alpha = alpha * strength;
        let new_conf = (conf + effective_alpha * (target - conf)).clamp(0.0, 1.0);
        self.storage.update_chunk_confidence(chunk_id, new_conf, Some(reason), now)?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Public API 3: add
    // ------------------------------------------------------------------

    pub fn add(
        &self,
        content: &str,
        kind: &str,
        trigger_desc: Option<&str>,
        anti_trigger_desc: Option<&str>,
        source: &str,
        skill_name: Option<&str>,
    ) -> Result<String> {
        if !matches!(kind, "note" | "skill") {
            return Err(InnateError::InvalidState(format!("invalid kind: {kind}")));
        }
        if !matches!(source, "chat" | "manual" | "doc" | "agent") {
            return Err(InnateError::InvalidState(format!("invalid source: {source}")));
        }

        let (content, action) = self.sanitize_content(content);
        if action == "discard" { return Ok(String::new()); }

        let h = content_hash(&content);
        if self.storage.is_hash_invalidated(&h)? {
            return Err(InnateError::InvalidState("content hash is invalidated".into()));
        }

        // Idempotency check
        let existing = self.storage.query_chunks_params(
            "SELECT id FROM chunks WHERE content_hash=? AND origin!='spark' AND state IN ('active','pending') ORDER BY created_at ASC LIMIT 1",
            rusqlite::params![h],
        )?;
        if let Some(e) = existing.first() {
            if let Some(id) = e.get("id").and_then(Value::as_str) {
                return Ok(id.to_string());
            }
        }

        let now = utc_now_iso();
        let chunk_id = gen_uuid();
        let sanitized = action == "redact";

        let (origin, state, conf, prot, state_reason) = if source == "agent" {
            ("captured", "pending", if sanitized { 0.4 } else { 0.60 }, 0, "init:captured_agent")
        } else if kind == "skill" {
            ("installed", "active", if sanitized { 0.4 } else { 0.85 }, 1, "init:installed")
        } else {
            ("captured", "active", if sanitized { 0.4 } else { 0.60 }, 0, "init:captured")
        };

        let tokens = estimate_tokens(&content) as i64;
        let row = ChunkRow {
            id: chunk_id.clone(),
            skill_name: skill_name.map(str::to_string),
            content: content.clone(),
            trigger_desc: trigger_desc.map(str::to_string),
            anti_trigger_desc: anti_trigger_desc.map(str::to_string),
            content_hash: h,
            token_count: Some(tokens),
            origin: origin.to_string(),
            source: Some(source.to_string()),
            protected: prot,
            state: state.to_string(),
            state_reason: Some(state_reason.to_string()),
            confidence: conf,
            confidence_reason: Some(format!("init:{origin}")),
            version: 1,
            embed_version: 1,
            created_at: now.clone(),
            updated_at: now.clone(),
            ..Default::default()
        };

        let cvec = self.embedding.embed_content(&content)?;
        let tvec = self.embedding.embed_trigger(trigger_desc.unwrap_or(&content))?;

        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            self.storage.insert_chunk(&row)?;
            self.storage.insert_vec_content(&chunk_id, &pack_embedding(&cvec))?;
            self.storage.insert_vec_trigger(&chunk_id, &pack_embedding(&tvec))?;
            self.storage.commit()
        })();
        if result.is_err() { let _ = self.storage.rollback(); }
        result?;
        Ok(chunk_id)
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
        if action == "discard" { return Ok(String::new()); }

        let h = content_hash(&content);
        if self.storage.is_hash_invalidated(&h)? {
            return Err(InnateError::InvalidState("content hash is invalidated".into()));
        }

        // Quick related recall (trace=false, no recursion risk)
        let related: Vec<String> = self.recall(&content, 2000, false, false, Some(5), "sdk")
            .map(|r| r.knowledge.iter().filter_map(|c| c["id"].as_str().map(str::to_string)).collect())
            .unwrap_or_default();

        let now = utc_now_iso();
        let chunk_id = gen_uuid();
        let tokens = estimate_tokens(&content) as i64;
        let row = ChunkRow {
            id: chunk_id.clone(),
            content: content.clone(),
            trigger_desc: trigger_desc.map(str::to_string),
            anti_trigger_desc: anti_trigger_desc.map(str::to_string),
            content_hash: h,
            token_count: Some(tokens),
            origin: "spark".to_string(),
            maturity: Some("seed".to_string()),
            related_ids: if related.is_empty() { None } else { Some(related.join(",")) },
            state: "active".to_string(),
            confidence: 0.5,
            version: 1,
            embed_version: 1,
            created_at: now.clone(),
            updated_at: now.clone(),
            ..Default::default()
        };

        let cvec = self.embedding.embed_content(&content)?;
        let tvec = self.embedding.embed_trigger(trigger_desc.unwrap_or(&content))?;

        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            self.storage.insert_chunk(&row)?;
            self.storage.insert_vec_content(&chunk_id, &pack_embedding(&cvec))?;
            self.storage.insert_vec_trigger(&chunk_id, &pack_embedding(&tvec))?;
            self.storage.commit()
        })();
        if result.is_err() { let _ = self.storage.rollback(); }
        result?;
        Ok(chunk_id)
    }

    // ------------------------------------------------------------------
    // Public API 5: mature_spark / promote_spark / drop_spark
    // ------------------------------------------------------------------

    pub fn mature_spark(&self, spark_id: &str, to: &str) -> Result<()> {
        let chunk = self.storage.get_chunk(spark_id)?
            .ok_or_else(|| InnateError::ChunkNotFound(spark_id.to_string()))?;
        if chunk.get("origin").and_then(Value::as_str) != Some("spark") {
            return Err(InnateError::ChunkNotFound(spark_id.to_string()));
        }
        let current = chunk.get("maturity").and_then(Value::as_str).unwrap_or("seed");
        let valid_next: &[&str] = match current {
            "seed" => &["sprouting"],
            "sprouting" => &["incubating"],
            _ => return Err(InnateError::InvalidState(format!("spark {spark_id} already {current}"))),
        };
        if current == to { return Ok(()); }
        if !valid_next.contains(&to) {
            return Err(InnateError::InvalidState(format!("invalid spark maturity transition: {current} -> {to}")));
        }
        let now = utc_now_iso();
        self.storage.query_chunks_params(
            "UPDATE chunks SET maturity=?, updated_at=? WHERE id=?",
            rusqlite::params![to, now, spark_id],
        )?;
        self.storage.commit()?;
        Ok(())
    }

    pub fn promote_spark(&self, spark_id: &str, to: &str) -> Result<String> {
        let spark = self.storage.get_chunk(spark_id)?
            .ok_or_else(|| InnateError::ChunkNotFound(spark_id.to_string()))?;
        if spark.get("origin").and_then(Value::as_str) != Some("spark") {
            return Err(InnateError::ChunkNotFound(spark_id.to_string()));
        }
        let maturity = spark.get("maturity").and_then(Value::as_str).unwrap_or("");
        if maturity == "promoted" || maturity == "dropped" {
            return Err(InnateError::InvalidState(format!("spark {spark_id} already {maturity}")));
        }
        if !matches!(to, "note" | "skill") {
            return Err(InnateError::InvalidState(format!("invalid spark promotion target: {to}")));
        }

        let content = spark.get("content").and_then(Value::as_str).unwrap_or("");
        let (content, action) = self.sanitize_content(content);
        if action == "discard" {
            return Err(InnateError::InvalidState("sanitize discard on promote".into()));
        }

        let promoted_hash = content_hash(&content);
        if self.storage.is_hash_invalidated(&promoted_hash)? {
            return Err(InnateError::InvalidState("spark content hash is invalidated".into()));
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
                self.storage.query_chunks_params(
                    "UPDATE chunks SET maturity='promoted', updated_at=? WHERE id=?",
                    rusqlite::params![now, spark_id],
                )?;
                self.storage.commit()?;
                return Ok(id);
            }
        }

        let (state, conf, prot, origin, state_reason) = if to == "skill" {
            ("active", 0.85, 1, "installed", "init:installed")
        } else {
            ("active", 0.60, 0, "captured", "init:captured")
        };

        let conf = if action == "redact" { 0.4_f64 } else { conf };
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

        let cvec = self.embedding.embed_content(&content)?;
        let tvec = self.embedding.embed_trigger(trigger.unwrap_or(&content))?;

        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            self.storage.insert_chunk(&row)?;
            self.storage.insert_vec_content(&new_id, &pack_embedding(&cvec))?;
            self.storage.insert_vec_trigger(&new_id, &pack_embedding(&tvec))?;
            self.storage.query_chunks_params(
                "UPDATE chunks SET maturity='promoted', updated_at=? WHERE id=?",
                rusqlite::params![now, spark_id],
            )?;
            self.storage.commit()
        })();
        if result.is_err() { let _ = self.storage.rollback(); }
        result?;
        Ok(new_id)
    }

    pub fn drop_spark(&self, spark_id: &str, reason: &str) -> Result<()> {
        let spark = self.storage.get_chunk(spark_id)?
            .ok_or_else(|| InnateError::ChunkNotFound(spark_id.to_string()))?;
        if spark.get("origin").and_then(Value::as_str) != Some("spark") {
            return Err(InnateError::ChunkNotFound(spark_id.to_string()));
        }
        let maturity = spark.get("maturity").and_then(Value::as_str).unwrap_or("");
        if maturity == "promoted" {
            return Err(InnateError::InvalidState(format!("spark {spark_id} already promoted")));
        }
        if maturity == "dropped" { return Ok(()); }
        let now = utc_now_iso();
        let reason_str = if reason.is_empty() { "dropped".to_string() } else { format!("dropped:{reason}") };
        self.storage.query_chunks_params(
            "UPDATE chunks SET maturity='dropped', state_reason=?, updated_at=? WHERE id=?",
            rusqlite::params![reason_str, now, spark_id],
        )?;
        self.storage.commit()?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Public API 6: approve / archive / invalidate / restore
    // ------------------------------------------------------------------

    pub fn approve(&self, chunk_id: &str) -> Result<()> {
        let chunk = self.storage.get_chunk(chunk_id)?
            .ok_or_else(|| InnateError::ChunkNotFound(chunk_id.to_string()))?;
        if chunk.get("origin").and_then(Value::as_str) == Some("spark") {
            return Err(InnateError::InvalidState("spark lifecycle uses promote_spark() or invalidate()".into()));
        }
        if chunk.get("state").and_then(Value::as_str) == Some("active") { return Ok(()); }
        if chunk.get("state").and_then(Value::as_str) != Some("pending") {
            return Err(InnateError::InvalidState("approve requires pending chunk".into()));
        }
        let now = utc_now_iso();
        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            self.storage.update_chunk_state(chunk_id, "active", Some("approved"), &now)?;
            self.storage.query_chunks_params(
                "UPDATE chunks SET confidence_reason='manual_set', updated_at=? WHERE id=?",
                rusqlite::params![now, chunk_id],
            )?;
            self.storage.commit()
        })();
        if result.is_err() { let _ = self.storage.rollback(); }
        result
    }

    pub fn archive(&self, chunk_id: &str, reason: &str) -> Result<()> {
        let chunk = self.storage.get_chunk(chunk_id)?
            .ok_or_else(|| InnateError::ChunkNotFound(chunk_id.to_string()))?;
        if chunk.get("origin").and_then(Value::as_str) == Some("spark") {
            return Err(InnateError::InvalidState("spark lifecycle uses drop_spark() or invalidate()".into()));
        }
        let now = utc_now_iso();
        self.storage.update_chunk_state(chunk_id, "archived", Some(reason), &now)?;
        self.storage.commit()?;
        Ok(())
    }

    pub fn invalidate(&self, chunk_id: &str, reason: &str) -> Result<()> {
        let chunk = self.storage.get_chunk(chunk_id)?
            .ok_or_else(|| InnateError::ChunkNotFound(chunk_id.to_string()))?;
        let h = chunk.get("content_hash").and_then(Value::as_str).unwrap_or("").to_string();
        let now = utc_now_iso();
        let reason_str = if reason.is_empty() { "invalidated".to_string() } else { format!("invalidated:{reason}") };

        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            self.storage.query_chunks_params(
                "UPDATE chunks SET state='archived', confidence=0.0, state_reason=?, state_updated_at=?, updated_at=? WHERE id=?",
                rusqlite::params![reason_str, now, now, chunk_id],
            )?;
            self.storage.query_chunks_params(
                "UPDATE chunks SET state='archived', confidence=0.0, state_reason='invalidated:same_hash', state_updated_at=?, updated_at=? WHERE content_hash=? AND id!=?",
                rusqlite::params![now, now, h, chunk_id],
            )?;
            self.storage.insert_invalidated_hash(&h, Some(reason), &now)?;
            self.storage.commit()
        })();
        if result.is_err() { let _ = self.storage.rollback(); }
        result
    }

    pub fn restore(&self, chunk_id: &str) -> Result<()> {
        let chunk = self.storage.get_chunk(chunk_id)?
            .ok_or_else(|| InnateError::ChunkNotFound(chunk_id.to_string()))?;
        let state = chunk.get("state").and_then(Value::as_str).unwrap_or("");
        if state == "active" { return Ok(()); }
        if state != "archived" {
            return Err(InnateError::InvalidState("restore requires archived chunk".into()));
        }
        let was_invalidated = chunk.get("state_reason")
            .and_then(Value::as_str)
            .map(|r| r.starts_with("invalidated"))
            .unwrap_or(false);
        let h = chunk.get("content_hash").and_then(Value::as_str).unwrap_or("").to_string();
        let now = utc_now_iso();

        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            self.storage.update_chunk_state(chunk_id, "active", Some("restore"), &now)?;
            if was_invalidated {
                self.storage.query_chunks_params(
                    "DELETE FROM invalidated_hashes WHERE content_hash=?",
                    rusqlite::params![h],
                )?;
            }
            self.storage.query_chunks_params(
                "UPDATE chunks SET confidence_reason='restore', updated_at=? WHERE id=?",
                rusqlite::params![now, chunk_id],
            )?;
            self.storage.commit()
        })();
        if result.is_err() { let _ = self.storage.rollback(); }
        result
    }

    // ------------------------------------------------------------------
    // Public API 7: evolve
    // ------------------------------------------------------------------

    pub fn evolve(&self, trigger: &str) -> Result<Value> {
        if !matches!(trigger, "manual" | "scheduled" | "threshold") {
            return Err(InnateError::InvalidState(format!("invalid evolve trigger: {trigger}")));
        }

        // Threshold check
        if trigger == "threshold" {
            let rows = self.storage.query_chunks(
                "SELECT COUNT(*) AS cnt FROM episodic_log WHERE distill_state='new'"
            )?;
            let cnt = rows.first().and_then(|r| r.get("cnt")).and_then(Value::as_i64).unwrap_or(0);
            if cnt < self.evolve_threshold {
                return Ok(json!({"distilled": 0, "curate": null}));
            }
        }

        let distilled = self.distill_batch()?;
        let curate = self.builtin_curate()?;

        Ok(json!({
            "distilled": distilled,
            "curate": {
                "archived": curate.archived.len(),
                "deduped": curate.deduped.len(),
                "decayed": curate.decayed.len(),
                "recovered": curate.recovered.len(),
                "orphans": curate.orphans.len(),
                "warnings": curate.warnings,
            }
        }))
    }

    fn distill_batch(&self) -> Result<usize> {
        let logs = self.storage.query_episodic_logs_open(self.distill_batch_size)?;
        let mut count = 0;
        for log in &logs {
            let trace_id = log.get("trace_id").and_then(Value::as_str).unwrap_or("");
            let chunks = self.distiller.distill(std::slice::from_ref(log))?;
            if chunks.is_empty() {
                self.storage.update_episodic_log_state(trace_id, "discarded", Some("insufficient_material"), None)?;
                continue;
            }
            for dc in chunks {
                let (content, action) = self.sanitize_content(&dc.content);
                if action == "discard" { continue; }
                let h = content_hash(&content);
                if self.storage.is_hash_invalidated(&h)? { continue; }
                let now = utc_now_iso();
                let chunk_id = gen_uuid();
                let tokens = estimate_tokens(&content) as i64;
                let row = ChunkRow {
                    id: chunk_id.clone(),
                    content: content.clone(),
                    trigger_desc: dc.trigger_desc,
                    anti_trigger_desc: dc.anti_trigger_desc,
                    content_hash: h,
                    token_count: Some(tokens),
                    origin: "distilled".to_string(),
                    distilled_from: Some(dc.source_log_id),
                    state: "pending".to_string(),
                    state_reason: Some("distilled".to_string()),
                    confidence: 0.5,
                    confidence_reason: Some("init:distilled".to_string()),
                    version: 1,
                    embed_version: 1,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                    ..Default::default()
                };
                let cvec = self.embedding.embed_content(&content)?;
                let tvec = self.embedding.embed_trigger(
                    row.trigger_desc.as_deref().unwrap_or(&content)
                )?;
                self.storage.begin_immediate()?;
                let r = (|| -> Result<()> {
                    self.storage.insert_chunk(&row)?;
                    self.storage.insert_vec_content(&chunk_id, &pack_embedding(&cvec))?;
                    self.storage.insert_vec_trigger(&chunk_id, &pack_embedding(&tvec))?;
                    self.storage.commit()
                })();
                if r.is_err() { let _ = self.storage.rollback(); r?; }
                count += 1;
            }
            let log_id = log.get("id").and_then(Value::as_str).unwrap_or(trace_id);
            self.storage.update_episodic_log_state(log_id, "distilled", None, None)?;
        }
        Ok(count)
    }

    fn builtin_curate(&self) -> Result<CurateReport> {
        let mut report = CurateReport::default();
        let now_iso = utc_now_iso();

        // Step 1-4: aggregate (BEGIN IMMEDIATE atomic)
        self.storage.begin_immediate()?;
        let agg_result = (|| -> Result<()> {
            let cutoff_ts = now_iso.clone();

            // 1. aggregate_success_traces
            self.storage.execute(
                "INSERT OR IGNORE INTO chunk_success_traces(chunk_id, trace_id, ts)
                 SELECT ut.chunk_id, ut.trace_id, ut.ts
                 FROM usage_trace ut
                 INNER JOIN usage_trace ok ON ok.trace_id = ut.trace_id
                    AND ok.event = 'task_ok' AND ok.chunk_id IS NULL
                 WHERE ut.event = 'selected' AND ut.chunk_id IS NOT NULL"
            )?;

            // 2. aggregate_success_counts
            self.storage.execute(
                "UPDATE chunks SET
                   used_success_count = (SELECT COUNT(*) FROM chunk_success_traces WHERE chunk_id = chunks.id),
                   success_trace_ids_count = (SELECT COUNT(*) FROM chunk_success_traces WHERE chunk_id = chunks.id),
                   last_success_at = (SELECT MAX(ts) FROM chunk_success_traces WHERE chunk_id = chunks.id)
                 WHERE origin != 'spark'"
            )?;

            // 3. aggregate_counters from usage_trace
            self.storage.execute(
                "UPDATE chunks SET
                   selected_count = COALESCE((SELECT COUNT(*) FROM usage_trace WHERE chunk_id = chunks.id AND event = 'selected'), 0),
                   used_count = COALESCE((SELECT COUNT(*) FROM usage_trace WHERE chunk_id = chunks.id AND event = 'used'), 0)
                 WHERE origin != 'spark'"
            )?;

            // 4. Write last_agg_ts and purge
            self.storage.set_meta("last_agg_ts", &cutoff_ts)?;
            self.storage.purge_usage_trace(&cutoff_ts)?;
            self.storage.commit()
        })();
        if agg_result.is_err() { let _ = self.storage.rollback(); agg_result?; }

        // Post-aggregate governance in one transaction
        self.storage.begin_immediate()?;
        let gov_result = (|| -> Result<()> {
            // Archive: low-confidence + idle
            let low_conf_cutoff = days_ago(&now_iso, self.low_conf_idle_days);
            let low_conf = self.storage.query_chunks_params(
                "SELECT id FROM chunks WHERE origin!='spark' AND state='active'
                 AND confidence < ? AND (last_used_at IS NULL OR last_used_at < ?)",
                rusqlite::params![self.low_conf_threshold, low_conf_cutoff],
            )?;
            for c in &low_conf {
                if let Some(id) = c.get("id").and_then(Value::as_str) {
                    self.storage.update_chunk_state(id, "archived", Some("low_conf_idle"), &now_iso)?;
                    report.archived.push(id.to_string());
                }
            }

            // Archive: never used + too old
            let never_used_cutoff = days_ago(&now_iso, self.never_used_age_days);
            let never_used = self.storage.query_chunks_params(
                "SELECT id FROM chunks WHERE origin!='spark' AND state='active'
                 AND used_count=0 AND selected_count >= ? AND created_at < ?",
                rusqlite::params![self.repeat_select_min, never_used_cutoff],
            )?;
            for c in &never_used {
                if let Some(id) = c.get("id").and_then(Value::as_str) {
                    if !report.archived.contains(&id.to_string()) {
                        self.storage.update_chunk_state(id, "archived", Some("repeat_select_never_used"), &now_iso)?;
                        report.archived.push(id.to_string());
                    }
                }
            }

            // Recover stale-screening logs → open
            let screening_cutoff = minutes_ago(&now_iso, self.screening_timeout_minutes);
            self.storage.query_chunks_params(
                "UPDATE episodic_log SET distill_state='open', distill_locked_at=NULL
                 WHERE distill_state='screening' AND distill_locked_at < ?",
                rusqlite::params![screening_cutoff],
            )?;

            // Purge old open logs (TTL)
            let open_ttl_cutoff = days_ago(&now_iso, self.open_ttl_days);
            self.storage.query_chunks_params(
                "UPDATE episodic_log SET distill_state='discarded', distill_note='open_ttl_expired'
                 WHERE distill_state='open' AND ts < ?",
                rusqlite::params![open_ttl_cutoff],
            )?;

            // Confidence-promote pending chunks
            let promotable = self.storage.query_chunks_params(
                "SELECT id FROM chunks WHERE state='pending' AND origin!='spark'
                 AND used_success_count >= ? AND confidence >= ?",
                rusqlite::params![self.promote_used_success_min, self.promote_confidence_min],
            )?;
            for c in &promotable {
                if let Some(id) = c.get("id").and_then(Value::as_str) {
                    self.storage.update_chunk_state(id, "active", Some("auto_promoted"), &now_iso)?;
                }
            }

            self.storage.commit()
        })();
        if gov_result.is_err() { let _ = self.storage.rollback(); gov_result?; }

        Ok(report)
    }

    // ------------------------------------------------------------------
    // Public API 8: inspect
    // ------------------------------------------------------------------

    pub fn inspect(&self) -> Result<Value> {
        let total: i64 = count_query(&self.storage, "SELECT COUNT(*) FROM chunks WHERE origin!='spark'")?;
        let active: i64 = count_query(&self.storage, "SELECT COUNT(*) FROM chunks WHERE state='active' AND origin!='spark'")?;
        let pending: i64 = count_query(&self.storage, "SELECT COUNT(*) FROM chunks WHERE state='pending' AND origin!='spark'")?;
        let archived: i64 = count_query(&self.storage, "SELECT COUNT(*) FROM chunks WHERE state='archived' AND origin!='spark'")?;
        let sparks: i64 = count_query(&self.storage, "SELECT COUNT(*) FROM chunks WHERE origin='spark' AND state!='archived'")?;
        let open_logs: i64 = count_query(&self.storage, "SELECT COUNT(*) FROM episodic_log WHERE distill_state='open'")?;
        let new_logs: i64 = count_query(&self.storage, "SELECT COUNT(*) FROM episodic_log WHERE distill_state='new'")?;
        let embed_rebuild: i64 = count_query(&self.storage,
            "SELECT COUNT(*) FROM chunks WHERE embed_version < (SELECT COALESCE(value,1) FROM meta WHERE key='embed_version')")?;
        let schema_version = self.storage.get_meta_or("schema_version", "?");
        let lib_id = self.storage.get_meta_or("lib_id", "?");
        let last_agg = self.storage.get_meta_or("last_agg_ts", "never");

        Ok(json!({
            "schema_version": schema_version,
            "lib_id": lib_id,
            "last_agg_ts": last_agg,
            "chunks": {"total": total, "active": active, "pending": pending, "archived": archived},
            "sparks": sparks,
            "episodic_log": {"open": open_logs, "new": new_logs},
            "embed_rebuild_queue": embed_rebuild,
            "params": {
                "recall.w_content": self.w_content,
                "recall.w_trigger": self.w_trigger,
                "recall.top_k_candidates": self.top_k_candidates,
                "curate.low_conf_threshold": self.low_conf_threshold,
                "curate.promote_confidence_min": self.promote_confidence_min,
            }
        }))
    }

    // ------------------------------------------------------------------
    // Sanitize
    // ------------------------------------------------------------------

    fn sanitize_content(&self, content: &str) -> (String, &'static str) {
        match self.sanitize {
            None => (content.to_string(), "allow"),
            Some(f) => match f(content) {
                Some(c) => (c, "allow"),
                None => (String::new(), "discard"),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct CandidateInfo {
    chunk: Value,
    sim_content: f32,
    sim_trigger: f32,
}

fn chunk_is_valid_for_recall(chunk: &Value, embed_version: i64) -> bool {
    chunk.get("state").and_then(Value::as_str) != Some("archived")
        && chunk.get("origin").and_then(Value::as_str) != Some("spark")
        && chunk.get("embed_version").and_then(Value::as_i64).unwrap_or(1) >= embed_version
}

fn anti_trigger_hit(query: &str, anti: &str) -> bool {
    let q_lower = query.to_lowercase();
    anti.to_lowercase().split(',').any(|part| {
        let p = part.trim();
        !p.is_empty() && q_lower.contains(p)
    })
}

fn block_cost(block: &[Value]) -> usize {
    block.iter().map(|b| {
        b.get("token_count").and_then(Value::as_u64).map(|t| t as usize)
            .unwrap_or_else(|| {
                estimate_tokens(b.get("content").and_then(Value::as_str).unwrap_or(""))
                    .max(100)
            })
    }).sum()
}

fn limit_knowledge(knowledge: Vec<Value>, top: Option<usize>) -> Vec<Value> {
    match top {
        None => knowledge,
        Some(0) => vec![],
        Some(n) => knowledge.into_iter().take(n).collect(),
    }
}

fn validate_source(source: &str) -> Result<()> {
    if !matches!(source, "sdk" | "cli" | "hook" | "daemon" | "augmented") {
        return Err(InnateError::InvalidState(format!("invalid event source: {source}")));
    }
    Ok(())
}

fn count_query(storage: &Storage, sql: &str) -> Result<i64> {
    Ok(storage.query_chunks(sql)?
        .first()
        .and_then(|r| r.as_object())
        .and_then(|m| m.values().next())
        .and_then(Value::as_i64)
        .unwrap_or(0))
}

fn days_ago(now_iso: &str, days: i64) -> String {
    // Approximate: subtract days from ISO string via chrono
    use chrono::{DateTime, Duration, Utc};
    if let Ok(t) = now_iso.parse::<DateTime<Utc>>() {
        let cutoff = t - Duration::days(days);
        return cutoff.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    }
    now_iso.to_string()
}

fn minutes_ago(now_iso: &str, minutes: i64) -> String {
    use chrono::{DateTime, Duration, Utc};
    if let Ok(t) = now_iso.parse::<DateTime<Utc>>() {
        let cutoff = t - Duration::minutes(minutes);
        return cutoff.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    }
    now_iso.to_string()
}
