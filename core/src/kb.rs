//! KnowledgeBase — all 8 Public APIs.

/// Return type for pack(): (selected_chunks, skipped_groups, skip_reasons)
type PackResult = (
    Vec<Value>,
    Vec<(Vec<Value>, f64, usize)>,
    std::collections::HashMap<String, String>,
);

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::embedding::{DummyEmbeddingProvider, EmbeddingProvider};
use crate::errors::{InnateError, Result};
use crate::refine::{
    DefaultSanitizer, DistilledChunk, Distiller, HeuristicDistiller, NullRefiner, Refiner,
    Sanitizer,
};
use crate::storage::{ChunkRow, EpisodicLogRow, Storage};
use crate::utils::{
    content_hash, estimate_tokens, gen_uuid, pack_embedding, utc_now_iso, SanitizeAction,
};

// ---------------------------------------------------------------------------
// Tuning defaults
// ---------------------------------------------------------------------------

const W_CONTENT: f64 = 0.55;
const W_TRIGGER: f64 = 0.25;
const W_CONFIDENCE: f64 = 0.10;
const W_CONTEXT: f64 = 0.15;
const TOP_K_CANDIDATES: usize = 20;
const ANTI_TRIGGER_PENALTY: f64 = 0.6;
const DENSITY_REFILL: bool = true;

const LOW_CONF_THRESHOLD: f64 = 0.25;
const LOW_CONF_IDLE_DAYS: i64 = 60;
const REPEAT_SELECT_MIN: i64 = 10;
const REPEAT_SELECT_CONF_MAX: f64 = 0.5;
const NEVER_USED_AGE_DAYS: i64 = 30;
const OPEN_TTL_DAYS: i64 = 14;
const SCREENING_TIMEOUT_MINUTES: i64 = 30;
const PROMOTE_USED_SUCCESS_MIN: i64 = 3;
const PROMOTE_CONFIDENCE_MIN: f64 = 0.60;
const DECAY_FLOOR: f64 = 0.20;
const EVOLVE_THRESHOLD: i64 = 5;
const DISTILL_BATCH_SIZE: usize = 20;
const PENDING_RECALL_PENALTY: f64 = 0.60;
const GOVERNANCE_ARCHIVE_THRESHOLD: i64 = 3;
const NEGATIVE_FEEDBACK_ARCHIVE_THRESHOLD: i64 = 5;
const GOVERNANCE_EVOLVE_THRESHOLD: i64 = 3;
const FAILURE_MIN_USES: i64 = 5;
const FAILURE_MAX_SUCCESS_RATE: f64 = 0.20;
const FAILURE_CONFIDENCE_MAX: f64 = 0.35;

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

#[derive(Debug, Default)]
struct DistillBatchReport {
    distilled: usize,
    failed: usize,
}

/// Scope for a single Curate run — allows limiting governance to a subset of chunks.
#[derive(Debug, Default, Clone)]
pub struct CurateScope {
    /// If set, only process chunks with this origin (e.g. "distilled").
    pub origin: Option<String>,
    /// If set, only process chunks belonging to this skill.
    pub skill_name: Option<String>,
    /// When true, compute the report but do not write any changes.
    pub dry_run: bool,
}

/// Replaceable governance interface (§二·六). Inject via `KnowledgeBase::open_with`.
/// Default implementation: `BuiltinCurator`.
pub trait Curator: Send + Sync {
    fn run(&self, kb: &KnowledgeBase, scope: &CurateScope) -> Result<CurateReport>;
}

/// Built-in curator — implements the full §四 governance pipeline.
pub struct BuiltinCurator;

impl Curator for BuiltinCurator {
    fn run(&self, kb: &KnowledgeBase, scope: &CurateScope) -> Result<CurateReport> {
        kb.builtin_curate_impl(scope)
    }
}

// ---------------------------------------------------------------------------
// KnowledgeBase
// ---------------------------------------------------------------------------

pub struct KnowledgeBase {
    pub storage: Storage,
    embedding: Arc<dyn EmbeddingProvider>,
    refiner: Arc<dyn Refiner>,
    distiller: Arc<dyn Distiller>,
    curator: Arc<dyn Curator>,
    sanitizer: Arc<dyn Sanitizer>,

    // Tuning params (loaded from meta at init)
    w_content: f64,
    w_trigger: f64,
    w_confidence: f64,
    w_context: f64,
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
    decay_floor: f64,
    evolve_threshold: i64,
    distill_batch_size: usize,
    evolve_schedule_interval_hours: i64,
    governance_archive_threshold: i64,
    negative_feedback_archive_threshold: i64,
    governance_evolve_threshold: i64,
    governance_proposal_max_age_days: i64,
    failure_min_uses: i64,
    failure_max_success_rate: f64,
    failure_confidence_max: f64,
}

impl KnowledgeBase {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(db_path, None, None, None, None, None)
    }

    pub fn open_with(
        db_path: impl AsRef<Path>,
        embedding: Option<Arc<dyn EmbeddingProvider>>,
        refiner: Option<Arc<dyn Refiner>>,
        distiller: Option<Arc<dyn Distiller>>,
        curator: Option<Arc<dyn Curator>>,
        sanitizer: Option<Arc<dyn Sanitizer>>,
    ) -> Result<Self> {
        let embedding = embedding.unwrap_or_else(|| Arc::new(DummyEmbeddingProvider::default()));
        let refiner = refiner.unwrap_or_else(|| Arc::new(NullRefiner));
        let distiller = distiller.unwrap_or_else(|| Arc::new(HeuristicDistiller));
        let curator = curator.unwrap_or_else(|| Arc::new(BuiltinCurator));
        let sanitizer = sanitizer.unwrap_or_else(|| Arc::new(DefaultSanitizer));

        let storage = Storage::open(db_path, embedding.content_dim(), embedding.trigger_dim())?;

        let mut kb = Self {
            storage,
            embedding,
            refiner,
            distiller,
            curator,
            sanitizer,
            w_content: W_CONTENT,
            w_trigger: W_TRIGGER,
            w_confidence: W_CONFIDENCE,
            w_context: W_CONTEXT,
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
            decay_floor: DECAY_FLOOR,
            evolve_threshold: EVOLVE_THRESHOLD,
            distill_batch_size: DISTILL_BATCH_SIZE,
            evolve_schedule_interval_hours: 6,
            governance_archive_threshold: GOVERNANCE_ARCHIVE_THRESHOLD,
            negative_feedback_archive_threshold: NEGATIVE_FEEDBACK_ARCHIVE_THRESHOLD,
            governance_evolve_threshold: GOVERNANCE_EVOLVE_THRESHOLD,
            governance_proposal_max_age_days: 30,
            failure_min_uses: FAILURE_MIN_USES,
            failure_max_success_rate: FAILURE_MAX_SUCCESS_RATE,
            failure_confidence_max: FAILURE_CONFIDENCE_MAX,
        };
        kb.init_meta()?;
        kb.load_params()?;
        Ok(kb)
    }

    fn init_meta(&self) -> Result<()> {
        let lib_id = gen_uuid();
        let content_dim = self.embedding.content_dim().to_string();
        let trigger_dim = self.embedding.trigger_dim().to_string();
        let embed_model = self.embedding.model_name();

        for (key, expected) in [
            ("content_dim", self.embedding.content_dim()),
            ("trigger_dim", self.embedding.trigger_dim()),
        ] {
            if let Some(stored) = self.storage.get_meta(key)? {
                let actual = stored.parse::<usize>().map_err(|_| {
                    InnateError::Other(format!("invalid {key} metadata value: {stored}"))
                })?;
                if actual != expected {
                    return Err(InnateError::Other(format!(
                        "{key} mismatch: database uses {actual}, embedding provider uses {expected}"
                    )));
                }
            }
        }

        let defaults: &[(&str, &str)] = &[
            ("lib_id", &lib_id),
            ("lib_role", "personal"),
            ("schema_version", "4.14"),
            ("content_dim", &content_dim),
            ("trigger_dim", &trigger_dim),
            ("embed_model", embed_model),
            ("embed_version", "1"),
            ("vector_revision", "0"),
            ("last_agg_ts", "1970-01-01T00:00:00.000Z"),
            ("recall.w_content", "0.55"),
            ("recall.w_trigger", "0.25"),
            ("recall.w_confidence", "0.10"),
            ("recall.w_context", "0.15"),
            ("recall.top_k_candidates", "20"),
            ("recall.anti_trigger_penalty", "0.6"),
            ("recall.density_refill", "true"),
            ("curate.low_conf_threshold", "0.25"),
            ("curate.low_conf_idle_days", "60"),
            ("curate.repeat_select_min", "10"),
            ("curate.repeat_select_conf_max", "0.5"),
            ("curate.never_used_age_days", "30"),
            ("curate.open_ttl_days", "14"),
            ("curate.screening_timeout_minutes", "30"),
            ("curate.promote_used_success_min", "3"),
            ("curate.promote_confidence_min", "0.60"),
            ("curate.decay_floor", "0.20"),
            ("evolve.threshold_new_count", "5"),
            ("evolve.distill_batch_size", "20"),
            ("evolve.schedule_interval_hours", "6"),
            ("curate.soft_mature_threshold", "5"),
            ("evolve.distill_token_window_hours", "24"),
            ("curate.governance_archive_threshold", "3"),
            ("curate.negative_feedback_archive_threshold", "5"),
            ("evolve.governance_pending_threshold", "3"),
            ("curate.governance_proposal_max_age_days", "30"),
            ("curate.failure_min_uses", "5"),
            ("curate.failure_max_success_rate", "0.20"),
            ("curate.failure_confidence_max", "0.35"),
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
        if result.is_err() {
            let _ = self.storage.rollback();
        }
        result
    }

    fn load_params(&mut self) -> Result<()> {
        let f = |k: &str, d: f64| -> f64 {
            self.storage
                .get_meta(k)
                .ok()
                .flatten()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        let i = |k: &str, d: i64| -> i64 {
            self.storage
                .get_meta(k)
                .ok()
                .flatten()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        let b = |k: &str, d: bool| -> bool {
            self.storage
                .get_meta(k)
                .ok()
                .flatten()
                .map(|v| v.to_lowercase() == "true")
                .unwrap_or(d)
        };
        self.w_content = f("recall.w_content", W_CONTENT);
        self.w_trigger = f("recall.w_trigger", W_TRIGGER);
        self.w_confidence = f("recall.w_confidence", W_CONFIDENCE);
        self.w_context = f("recall.w_context", W_CONTEXT);
        self.top_k_candidates =
            i("recall.top_k_candidates", TOP_K_CANDIDATES as i64).max(1) as usize;
        self.anti_trigger_penalty = f("recall.anti_trigger_penalty", ANTI_TRIGGER_PENALTY);
        self.density_refill = b("recall.density_refill", DENSITY_REFILL);
        self.low_conf_threshold = f("curate.low_conf_threshold", LOW_CONF_THRESHOLD);
        self.low_conf_idle_days = i("curate.low_conf_idle_days", LOW_CONF_IDLE_DAYS);
        self.repeat_select_min = i("curate.repeat_select_min", REPEAT_SELECT_MIN);
        self.repeat_select_conf_max = f("curate.repeat_select_conf_max", REPEAT_SELECT_CONF_MAX);
        self.never_used_age_days = i("curate.never_used_age_days", NEVER_USED_AGE_DAYS);
        self.open_ttl_days = i("curate.open_ttl_days", OPEN_TTL_DAYS);
        self.screening_timeout_minutes = i(
            "curate.screening_timeout_minutes",
            SCREENING_TIMEOUT_MINUTES,
        );
        self.promote_used_success_min =
            i("curate.promote_used_success_min", PROMOTE_USED_SUCCESS_MIN);
        self.promote_confidence_min = f("curate.promote_confidence_min", PROMOTE_CONFIDENCE_MIN);
        self.decay_floor = f("curate.decay_floor", DECAY_FLOOR).clamp(0.0, 0.4);
        self.evolve_threshold = i("evolve.threshold_new_count", EVOLVE_THRESHOLD);
        self.distill_batch_size =
            i("evolve.distill_batch_size", DISTILL_BATCH_SIZE as i64) as usize;
        self.evolve_schedule_interval_hours = i("evolve.schedule_interval_hours", 6).max(1);
        self.governance_archive_threshold =
            i("curate.governance_archive_threshold", GOVERNANCE_ARCHIVE_THRESHOLD).max(1);
        self.negative_feedback_archive_threshold = i(
            "curate.negative_feedback_archive_threshold",
            NEGATIVE_FEEDBACK_ARCHIVE_THRESHOLD,
        )
        .max(1);
        self.governance_evolve_threshold =
            i("evolve.governance_pending_threshold", GOVERNANCE_EVOLVE_THRESHOLD).max(1);
        self.governance_proposal_max_age_days =
            i("curate.governance_proposal_max_age_days", 30).max(1);
        self.failure_min_uses = i("curate.failure_min_uses", FAILURE_MIN_USES).max(1);
        self.failure_max_success_rate = f(
            "curate.failure_max_success_rate",
            FAILURE_MAX_SUCCESS_RATE,
        )
        .clamp(0.0, 1.0);
        self.failure_confidence_max =
            f("curate.failure_confidence_max", FAILURE_CONFIDENCE_MAX).clamp(0.0, 1.0);
        Ok(())
    }

    // ------------------------------------------------------------------
    // Public API 1: recall
    // ------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn recall(
        &self,
        query: &str,
        budget: usize,
        trace: bool,
        include_sparks: bool,
        top: Option<usize>,
        source: &str,
        expand_deps: &str, // "false" | "direct" | "closure"
        allow_trim: bool,  // if true, invoke Refiner::trim when block doesn't fit
        refine_mode: &str, // "off" | "trim" | "adapt" — recorded in trace
    ) -> Result<RecallResult> {
        validate_source(source)?;
        let trace_id = gen_uuid();
        let now = utc_now_iso();

        let q_content = self
            .embedding
            .embed_content(query)
            .map_err(|e| InnateError::EmbeddingUnavailable(e.to_string()))?;
        let q_trigger = self
            .embedding
            .embed_trigger(query)
            .map_err(|e| InnateError::EmbeddingUnavailable(e.to_string()))?;

        // ANN candidates (non-spark)
        let mut candidates = self.ann_candidates(&q_content, &q_trigger)?;
        self.apply_soft_dep_bonus(&mut candidates)?;

        // Score + anti-trigger penalty
        let scored = self.score_candidates(candidates, query)?;

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
                &scored,
                &visible,
                &sparks,
                &depth_skipped,
                &skipped_reasons,
                refine_mode,
                source,
                &now,
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

    fn ann_candidates(
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

    fn apply_soft_dep_bonus(&self, candidates: &mut HashMap<String, CandidateInfo>) -> Result<()> {
        let ids: Vec<String> = candidates.keys().cloned().collect();
        for cid in ids {
            if candidates[&cid].chunk.get("origin").and_then(Value::as_str) == Some("spark") {
                continue;
            }
            let deps = self.storage.get_deps(&cid)?;
            for (dst, kind, _) in &deps {
                if kind != "soft" {
                    continue;
                }
                if let Some(target) = self.storage.get_chunk(dst)? {
                    if target.get("state").and_then(Value::as_str) == Some("archived") {
                        continue;
                    }
                    if target.get("origin").and_then(Value::as_str) == Some("spark") {
                        continue;
                    }
                    let e = candidates
                        .entry(dst.clone())
                        .or_insert_with(|| CandidateInfo {
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
    ) -> Result<Vec<(f64, Value)>> {
        let context_key = content_hash(&normalize_query(query));
        let mut scored: Vec<(f64, Value)> = Vec::with_capacity(candidates.len());
        for info in candidates.into_values() {
            let conf = info
                .chunk
                .get("confidence")
                .and_then(Value::as_f64)
                .unwrap_or(0.5);
            let chunk_id = info.chunk.get("id").and_then(Value::as_str).unwrap_or("");
            let context_score = self.storage.context_score(chunk_id, &context_key)?;
            let mut fused = self.w_content * info.sim_content as f64
                + self.w_trigger * info.sim_trigger as f64
                + self.w_confidence * conf
                + self.w_context * context_score;
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
        scored: &[(f64, Value)],
        visible: &[Value],
        sparks: &[Value],
        depth_skipped: &[String],
        skipped_reasons: &HashMap<String, String>,
        refine_mode: &str,
        source: &str,
        now: &str,
    ) -> Result<()> {
        let lib_id = self.storage.lib_id()?;
        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
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
                task_state: "recalled".to_string(),
                usage_state: "unknown".to_string(),
                context_key: Some(content_hash(&normalize_query(query))),
                distill_state: "open".to_string(),
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

    // ------------------------------------------------------------------
    // Public API 2: record
    // ------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
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
        self.record_detailed(
            trace_id,
            query,
            output,
            output_summary,
            outcome,
            used,
            "explicit",
            true,
            feedback_up,
            feedback_down,
            "user",
            None,
            None,
            nomination,
            priority,
            None,
            source,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_detailed(
        &self,
        trace_id: &str,
        query: Option<&str>,
        output: Option<&str>,
        output_summary: Option<&str>,
        outcome: Option<&str>,
        used: Option<&[String]>,
        used_attribution: &str,
        used_complete: bool,
        feedback_up: Option<&[String]>,
        feedback_down: Option<&[String]>,
        feedback_kind: &str,
        feedback_actor: Option<&str>,
        feedback_reason: Option<&str>,
        nomination: Option<&str>,
        priority: i64,
        task_state: Option<&str>,
        source: &str,
    ) -> Result<()> {
        let dedupe_ids = |ids: &[String]| {
            let mut seen = HashSet::new();
            ids.iter()
                .filter(|id| seen.insert((*id).clone()))
                .cloned()
                .collect::<Vec<_>>()
        };
        let normalized_used = used.map(dedupe_ids);
        let normalized_feedback_up = feedback_up.map(dedupe_ids);
        let normalized_feedback_down = feedback_down.map(dedupe_ids);
        let used = normalized_used.as_deref();
        let feedback_up = normalized_feedback_up.as_deref();
        let feedback_down = normalized_feedback_down.as_deref();

        if let Some(o) = outcome {
            if !matches!(o, "ok" | "fail" | "unknown") {
                return Err(InnateError::InvalidState(format!("invalid outcome: {o}")));
            }
        }
        if !matches!(used_attribution, "explicit" | "cited" | "inferred") {
            return Err(InnateError::InvalidState(format!(
                "invalid used attribution: {used_attribution}"
            )));
        }
        if !matches!(feedback_kind, "user" | "judge") {
            return Err(InnateError::InvalidState(format!(
                "invalid feedback kind: {feedback_kind}"
            )));
        }
        if let Some(state) = task_state {
            if !matches!(
                state,
                "recalled" | "running" | "completed" | "abandoned" | "timed_out"
            ) {
                return Err(InnateError::InvalidState(format!(
                    "invalid task state: {state}"
                )));
            }
        }
        validate_source(source)?;
        if let (Some(ups), Some(downs)) = (feedback_up, feedback_down) {
            let down_set: HashSet<&str> = downs.iter().map(String::as_str).collect();
            if let Some(chunk_id) = ups.iter().find(|id| down_set.contains(id.as_str())) {
                return Err(InnateError::InvalidState(format!(
                    "conflicting feedback for chunk {chunk_id}"
                )));
            }
        }
        let effective_priority = if nomination.is_some() && priority == 0 {
            1
        } else {
            priority
        };
        let now = utc_now_iso();
        let lib_id = self.storage.lib_id()?;

        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            let log = self.storage.get_episodic_log(trace_id)?;
            let mut is_fresh_insert = false;
            let log = match log {
                Some(l) => l,
                None => {
                    let used_ids = used.map(serde_json::to_string).transpose()?;
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
                        task_state: if matches!(outcome, Some("ok") | Some("fail")) {
                            "completed".to_string()
                        } else {
                            task_state.unwrap_or("running").to_string()
                        },
                        completed_at: matches!(outcome, Some("ok") | Some("fail"))
                            .then(|| now.clone()),
                        usage_state: usage_state(used).to_string(),
                        used_ids,
                        used_attribution: used.map(|_| used_attribution.to_string()),
                        used_complete,
                        context_key: query.map(|q| content_hash(&normalize_query(q))),
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
            self.validate_trace_attribution(trace_id, used, "used")?;
            self.validate_trace_attribution(trace_id, feedback_up, "feedback_up")?;
            self.validate_trace_attribution(trace_id, feedback_down, "feedback_down")?;

            let existing_outcome = log
                .get("outcome")
                .and_then(Value::as_str)
                .map(str::to_string);
            // usage_trace: used
            let effective_used_attribution = if used.is_some() {
                used_attribution
            } else {
                log.get("used_attribution")
                    .and_then(Value::as_str)
                    .unwrap_or(used_attribution)
            };
            let used_strength = match effective_used_attribution {
                "explicit" => 0.3,
                "cited" => 0.25,
                "inferred" => 0.15,
                _ => unreachable!(),
            };
            let existing_used_ids: Vec<String> = log
                .get("used_ids")
                .and_then(Value::as_str)
                .and_then(|raw| serde_json::from_str(raw).ok())
                .unwrap_or_default();
            let existing_used_complete = log
                .get("used_complete")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                != 0;
            let effective_used_complete = used_complete || existing_used_complete;
            let effective_used_ids = used.map(|reported| {
                if used_complete {
                    reported.to_vec()
                } else {
                    let mut merged = existing_used_ids.clone();
                    let mut seen: HashSet<String> = merged.iter().cloned().collect();
                    merged.extend(
                        reported
                            .iter()
                            .filter(|id| seen.insert((*id).clone()))
                            .cloned(),
                    );
                    merged
                }
            });
            if let Some(used_ids) = effective_used_ids.as_deref() {
                let previously_used: HashSet<String> = existing_used_ids.iter().cloned().collect();
                if used_complete {
                    self.storage.replace_used_trace(
                        trace_id,
                        used_ids,
                        used_strength,
                        used_attribution,
                        source,
                        &now,
                    )?;
                } else if let Some(reported) = used {
                    self.storage.merge_used_trace(
                        trace_id,
                        reported,
                        used_strength,
                        used_attribution,
                        source,
                        &now,
                    )?;
                }
                let affected: HashSet<String> = previously_used
                    .into_iter()
                    .chain(used_ids.iter().cloned())
                    .collect();
                for cid in affected {
                    self.storage.refresh_chunk_last_used(&cid, &now)?;
                }
            }

            // usage_trace: task_ok / task_fail
            if let Some(o) = outcome {
                if matches!(o, "ok" | "fail") {
                    let event = if o == "ok" { "task_ok" } else { "task_fail" };
                    let strength = if event == "task_fail" { 0.15 } else { 1.0 };
                    self.storage.conn_execute(
                        "DELETE FROM usage_trace
                         WHERE trace_id=? AND event IN ('task_ok','task_fail')
                           AND chunk_id IS NULL",
                        rusqlite::params![trace_id],
                    )?;
                    self.storage.insert_usage_trace(
                        trace_id, None, event, strength, None, None, None, None, None, source, &now,
                    )?;
                }
            }

            // Rebuild trace-derived evidence whenever either side of the pair arrives.
            // This makes `outcome → used` and `used → outcome` equivalent and lets a
            // later complete usage declaration replace an earlier one safely.
            let effective_outcome = outcome
                .filter(|value| *value != "unknown")
                .or(existing_outcome.as_deref().filter(|value| *value != "unknown"));
            if let Some(o @ ("ok" | "fail")) = effective_outcome {
                if used.is_some()
                    || (outcome.is_some_and(|value| value != "unknown")
                        && existing_outcome.as_deref() != outcome)
                {
                    let fallback_ids: Vec<String>;
                    let effective_used: Option<&[String]> = if effective_used_ids.is_some() {
                        effective_used_ids.as_deref()
                    } else {
                        fallback_ids = log
                            .get("used_ids")
                            .and_then(Value::as_str)
                            .and_then(|s| serde_json::from_str(s).ok())
                            .unwrap_or_default();
                        if fallback_ids.is_empty() {
                            None
                        } else {
                            Some(&fallback_ids)
                        }
                    };
                    let effective_complete = if used.is_some() {
                        effective_used_complete
                    } else {
                        log.get("usage_state").and_then(Value::as_str) != Some("unknown")
                            && log
                                .get("used_complete")
                            .and_then(Value::as_i64)
                            .unwrap_or(1)
                            != 0
                    };
                    self.replace_outcome_evidence(
                        trace_id,
                        o,
                        effective_used,
                        effective_complete,
                        &now,
                    )?;
                }
            } else if used.is_some() && effective_used_complete {
                self.replace_selected_unused_evidence(
                    trace_id,
                    effective_used_ids.as_deref().unwrap_or_default(),
                    &now,
                )?;
            }

            let context_key = log
                .get("context_key")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| query.map(|q| content_hash(&normalize_query(q))));
            let feedback_strength = if feedback_kind == "judge" { 0.6 } else { 1.0 };

            // Persist feedback facts before reducing them into confidence.
            // INSERT OR IGNORE: skip all derived updates for duplicate (trace_id, chunk_id, signal).
            // Track affected chunks so we only rebuild their context_stats (not the full table).
            let mut context_affected: HashSet<String> = HashSet::new();
            if let Some(used_ids) = effective_used_ids.as_deref() {
                for cid in used_ids {
                    context_affected.insert(cid.clone());
                }
            }
            if let Some(ups) = feedback_up {
                for cid in ups {
                    let corrected = self.storage
                        .delete_feedback_event(trace_id, cid, "down")?;
                    self.storage
                        .delete_chunk_trace_confidence_evidence(trace_id, cid, "feedback_down")?;
                    let inserted = self.storage.insert_feedback_event(
                        &gen_uuid(),
                        trace_id,
                        cid,
                        "up",
                        feedback_strength,
                        source,
                        feedback_actor,
                        feedback_reason,
                        context_key.as_deref(),
                        &now,
                    )?;
                    if inserted > 0 {
                        self.upsert_trace_confidence_evidence(
                            trace_id,
                            cid,
                            "feedback_up",
                            1.0,
                            feedback_strength,
                            if feedback_kind == "judge" {
                                "judge_up"
                            } else {
                                "user_up"
                            },
                            context_key.as_deref(),
                            &now,
                            true,
                        )?;
                        self.storage.update_chunk_last_used(cid, &now)?;
                        self.refresh_governance_evidence(cid, &now)?;
                        context_affected.insert(cid.clone());
                    } else if corrected > 0 {
                        self.recompute_chunk_confidence(cid, &now)?;
                        self.refresh_governance_evidence(cid, &now)?;
                        context_affected.insert(cid.clone());
                    }
                }
            }
            if let Some(downs) = feedback_down {
                for cid in downs {
                    let corrected = self.storage
                        .delete_feedback_event(trace_id, cid, "up")?;
                    self.storage
                        .delete_chunk_trace_confidence_evidence(trace_id, cid, "feedback_up")?;
                    let inserted = self.storage.insert_feedback_event(
                        &gen_uuid(),
                        trace_id,
                        cid,
                        "down",
                        feedback_strength,
                        source,
                        feedback_actor,
                        feedback_reason,
                        context_key.as_deref(),
                        &now,
                    )?;
                    if inserted > 0 {
                        self.upsert_trace_confidence_evidence(
                            trace_id,
                            cid,
                            "feedback_down",
                            0.0,
                            feedback_strength,
                            if feedback_kind == "judge" {
                                "judge_down"
                            } else {
                                "user_down"
                            },
                            context_key.as_deref(),
                            &now,
                            true,
                        )?;
                        self.refresh_governance_evidence(cid, &now)?;
                        context_affected.insert(cid.clone());
                    } else if corrected > 0 {
                        self.recompute_chunk_confidence(cid, &now)?;
                        self.refresh_governance_evidence(cid, &now)?;
                        context_affected.insert(cid.clone());
                    }
                }
            }
            // Targeted rebuild — only update context_stats for chunks touched in this call.
            self.rebuild_context_stats_for(&context_affected, &now)?;

            // Fill in content fields (補写: output_summary, nomination, output, query) on existing log.
            if !is_fresh_insert {
                self.storage.patch_episodic_log_content(
                    trace_id,
                    query,
                    output,
                    output_summary,
                    nomination,
                    effective_priority,
                )?;
            }

            let lifecycle_state = if effective_outcome.is_some() {
                "completed"
            } else {
                task_state.unwrap_or_else(|| {
                    log.get("task_state")
                        .and_then(Value::as_str)
                        .unwrap_or("running")
                })
            };
            let used_ids_json = effective_used_ids
                .as_deref()
                .map(serde_json::to_string)
                .transpose()?;
            self.storage.update_trace_lifecycle(
                trace_id,
                lifecycle_state,
                (lifecycle_state == "completed").then_some(now.as_str()),
                effective_used_ids
                    .as_deref()
                    .map(|ids| usage_state(Some(ids))),
                used_ids_json.as_deref(),
                used.map(|_| used_attribution),
                used.map(|_| effective_used_complete),
            )?;

            // Update episodic log
            let current_state = log
                .get("distill_state")
                .and_then(Value::as_str)
                .unwrap_or("open");
            let lifecycle_completed = lifecycle_state == "completed";
            let has_material = output_summary.is_some()
                || nomination.is_some()
                || output.is_some()
                || log.get("output_summary").and_then(Value::as_str).is_some()
                || log.get("nomination").and_then(Value::as_str).is_some()
                || log.get("output").and_then(Value::as_str).is_some();
            let retryable_discard = current_state == "discarded"
                && matches!(
                    log.get("distill_note").and_then(Value::as_str),
                    Some("insufficient_material" | "abandoned" | "timed_out")
                );
            let new_state = if current_state == "open"
                && matches!(lifecycle_state, "abandoned" | "timed_out")
            {
                Some("discarded")
            } else if lifecycle_completed && (current_state == "open" || retryable_discard) {
                if has_material {
                    Some("new")
                } else {
                    Some("discarded")
                }
            } else {
                None
            };
            if let Some(state) = new_state {
                let note = if state == "discarded" {
                    Some(if matches!(lifecycle_state, "abandoned" | "timed_out") {
                        lifecycle_state
                    } else {
                        "insufficient_material"
                    })
                } else {
                    None
                };
                let outcome_str = outcome.map(str::to_string);
                self.storage.update_episodic_log_state(
                    trace_id,
                    state,
                    note,
                    outcome_str.as_deref(),
                )?;
                if state == "new" && retryable_discard {
                    self.storage.conn_execute(
                        "UPDATE episodic_log SET distill_note=NULL WHERE trace_id=?",
                        rusqlite::params![trace_id],
                    )?;
                }
            } else if outcome.is_some() {
                let outcome_str = outcome.map(str::to_string);
                self.storage.update_episodic_log_state(
                    trace_id,
                    current_state,
                    None,
                    outcome_str.as_deref(),
                )?;
            }

            self.storage.commit()
        })();
        if result.is_err() {
            let _ = self.storage.rollback();
        }
        result?;
        self.enqueue_evolve_if_needed(&now)?;
        Ok(())
    }

    fn validate_trace_attribution(
        &self,
        trace_id: &str,
        chunk_ids: Option<&[String]>,
        field: &str,
    ) -> Result<()> {
        let Some(chunk_ids) = chunk_ids else {
            return Ok(());
        };
        if chunk_ids.is_empty() {
            return Ok(());
        }

        let log = self.storage.get_episodic_log(trace_id)?.ok_or_else(|| {
            InnateError::InvalidState(format!(
                "{field} requires a trace created by recall: {trace_id}"
            ))
        })?;
        let mut attributable = HashSet::new();
        if let Some(raw) = log.get("recall_snapshot").and_then(Value::as_str) {
            if let Ok(snapshot) = serde_json::from_str::<Value>(raw) {
                if let Some(ids) = snapshot.get("selected").and_then(Value::as_array) {
                    attributable.extend(ids.iter().filter_map(Value::as_str).map(str::to_string));
                }
            }
        }
        let rows = self.storage.query_chunks_params(
            "SELECT DISTINCT chunk_id FROM usage_trace
             WHERE trace_id=? AND chunk_id IS NOT NULL
               AND event='selected'",
            rusqlite::params![trace_id],
        )?;
        attributable.extend(rows.iter().filter_map(|row| {
            row.get("chunk_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        }));

        for chunk_id in chunk_ids {
            if self.storage.get_chunk(chunk_id)?.is_none() || !attributable.contains(chunk_id) {
                return Err(InnateError::InvalidState(format!(
                    "{field} chunk {chunk_id} was not attributable to trace {trace_id}"
                )));
            }
        }
        Ok(())
    }

    fn replace_selected_unused_evidence(
        &self,
        trace_id: &str,
        used_ids: &[String],
        now: &str,
    ) -> Result<()> {
        let old_rows = self.storage.query_chunks_params(
            "SELECT DISTINCT chunk_id FROM confidence_evidence
             WHERE trace_id=? AND kind='selected_unused'",
            rusqlite::params![trace_id],
        )?;
        let mut affected: HashSet<String> = old_rows
            .iter()
            .filter_map(|row| row.get("chunk_id").and_then(Value::as_str).map(str::to_string))
            .collect();
        self.storage
            .delete_trace_confidence_evidence(trace_id, &["selected_unused"])?;

        let used_set: HashSet<&str> = used_ids.iter().map(String::as_str).collect();
        let context_key = self.storage.get_episodic_log(trace_id)?.and_then(|log| {
            log.get("context_key")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        let selected_rows = self.storage.query_chunks_params(
            "SELECT chunk_id FROM usage_trace
             WHERE trace_id=? AND event='selected' AND chunk_id IS NOT NULL",
            rusqlite::params![trace_id],
        )?;
        for row in selected_rows {
            if let Some(chunk_id) = row.get("chunk_id").and_then(Value::as_str) {
                if !used_set.contains(chunk_id) {
                    self.upsert_trace_confidence_evidence(
                        trace_id,
                        chunk_id,
                        "selected_unused",
                        0.0,
                        0.08,
                        "selected_unused",
                        context_key.as_deref(),
                        now,
                        false,
                    )?;
                    affected.insert(chunk_id.to_string());
                }
            }
        }
        for chunk_id in affected {
            self.recompute_chunk_confidence(&chunk_id, now)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn upsert_trace_confidence_evidence(
        &self,
        trace_id: &str,
        chunk_id: &str,
        kind: &str,
        target: f64,
        strength: f64,
        reason: &str,
        context_key: Option<&str>,
        now: &str,
        explicit: bool,
    ) -> Result<()> {
        let chunk = match self.storage.get_chunk(chunk_id)? {
            Some(chunk) => chunk,
            None => return Ok(()),
        };
        if chunk.get("origin").and_then(Value::as_str) == Some("spark") {
            return Ok(());
        }
        let recency_weight = if explicit {
            const KAPPA: f64 = 0.5;
            const WINDOW_DAYS: f64 = 14.0;
            let gap_days = chunk
                .get("last_used_at")
                .and_then(Value::as_str)
                .map(|ts| iso_days_diff(now, ts) as f64)
                .unwrap_or(0.0);
            (1.0
                + KAPPA
                    * (-(gap_days / WINDOW_DAYS) * std::f64::consts::LN_2).exp())
            .min(1.5)
        } else {
            1.0
        };
        let alpha = (0.2 * strength * recency_weight).clamp(0.0, 1.0);
        self.storage.upsert_confidence_evidence(
            &gen_uuid(),
            Some(trace_id),
            chunk_id,
            kind,
            target,
            alpha,
            reason,
            context_key,
            now,
        )?;
        self.recompute_chunk_confidence(chunk_id, now)
    }

    fn recompute_chunk_confidence(&self, chunk_id: &str, now: &str) -> Result<()> {
        let Some(chunk) = self.storage.get_chunk(chunk_id)? else {
            return Ok(());
        };
        let mut confidence = chunk
            .get("confidence_base")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| {
                chunk
                    .get("confidence")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.5)
            });
        let mut reason = chunk
            .get("confidence_reason")
            .and_then(Value::as_str)
            .unwrap_or("base")
            .to_string();
        for evidence in self.storage.confidence_evidence_for_chunk(chunk_id)? {
            let target = evidence
                .get("target")
                .and_then(Value::as_f64)
                .unwrap_or(0.5);
            let alpha = evidence
                .get("alpha")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            confidence = (confidence + alpha * (target - confidence)).clamp(0.0, 1.0);
            reason = evidence
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("evidence")
                .to_string();
        }
        self.storage.conn_execute(
            "UPDATE chunks SET confidence=?, confidence_reason=?, updated_at=? WHERE id=?",
            rusqlite::params![confidence, reason, now, chunk_id],
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn replace_outcome_evidence(
        &self,
        trace_id: &str,
        outcome: &str,
        used: Option<&[String]>,
        used_complete: bool,
        now: &str,
    ) -> Result<()> {
        let old_rows = self.storage.query_chunks_params(
            "SELECT DISTINCT chunk_id FROM confidence_evidence
             WHERE trace_id=? AND kind IN ('outcome_ok','outcome_fail','selected_unused')",
            rusqlite::params![trace_id],
        )?;
        let mut affected: HashSet<String> = old_rows
            .iter()
            .filter_map(|row| row.get("chunk_id").and_then(Value::as_str).map(str::to_string))
            .collect();
        self.storage.delete_trace_confidence_evidence(
            trace_id,
            &["outcome_ok", "outcome_fail", "selected_unused"],
        )?;

        let used_ids = used.unwrap_or_default();
        let used_set: HashSet<&str> = used_ids.iter().map(String::as_str).collect();
        let attribution_divisor = used_ids.len().max(1) as f64;
        let context_key = self.storage.get_episodic_log(trace_id)?.and_then(|log| {
            log.get("context_key")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        for chunk_id in used_ids {
            let attribution = self
                .storage
                .query_chunks_params(
                    "SELECT strength, attribution FROM usage_trace
                     WHERE trace_id=? AND chunk_id=? AND event='used'",
                    rusqlite::params![trace_id, chunk_id],
                )?
                .into_iter()
                .next();
            let base_strength = attribution
                .as_ref()
                .and_then(|row| row.get("strength"))
                .and_then(Value::as_f64)
                .unwrap_or(0.15)
                / attribution_divisor;
            let attribution_reason = attribution
                .as_ref()
                .and_then(|row| row.get("attribution"))
                .and_then(Value::as_str)
                .unwrap_or("inferred");
            let (kind, target, strength, reason) = if outcome == "ok" {
                ("outcome_ok", 1.0, base_strength, attribution_reason)
            } else {
                ("outcome_fail", 0.0, base_strength * 0.5, "task_fail")
            };
            self.upsert_trace_confidence_evidence(
                trace_id,
                chunk_id,
                kind,
                target,
                strength,
                reason,
                context_key.as_deref(),
                now,
                false,
            )?;
            affected.insert(chunk_id.clone());
        }

        if used_complete {
            let selected_rows = self.storage.query_chunks_params(
                "SELECT chunk_id FROM usage_trace
                 WHERE trace_id=? AND event='selected' AND chunk_id IS NOT NULL",
                rusqlite::params![trace_id],
            )?;
            for row in selected_rows {
                if let Some(chunk_id) = row.get("chunk_id").and_then(Value::as_str) {
                    if !used_set.contains(chunk_id) {
                        self.upsert_trace_confidence_evidence(
                            trace_id,
                            chunk_id,
                            "selected_unused",
                            0.0,
                            0.08,
                            "selected_unused",
                            context_key.as_deref(),
                            now,
                            false,
                        )?;
                        affected.insert(chunk_id.to_string());
                    }
                }
            }
        }

        for chunk_id in affected {
            self.recompute_chunk_confidence(&chunk_id, now)?;
        }
        Ok(())
    }

    fn rebuild_context_stats(&self, now: &str) -> Result<()> {
        self.storage.conn_execute(
            "DELETE FROM chunk_context_stats",
            rusqlite::params![],
        )?;
        self.storage.conn_execute(
            "INSERT INTO chunk_context_stats
             (chunk_id, context_key, success_count, failure_count,
              positive_feedback, negative_feedback, last_updated_at)
             SELECT chunk_id, context_key, success_count, failure_count,
                    positive_feedback, negative_feedback, ?
             FROM chunk_context_stats_base",
            rusqlite::params![now],
        )?;
        self.storage.conn_execute(
            "INSERT INTO chunk_context_stats
             (chunk_id, context_key, success_count, failure_count,
              positive_feedback, negative_feedback, last_updated_at)
             SELECT chunk_id, context_key,
                    SUM(CASE WHEN kind='outcome_ok' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN kind='outcome_fail' THEN 1 ELSE 0 END),
                    0, 0, ?
             FROM confidence_evidence
             WHERE context_key IS NOT NULL AND kind IN ('outcome_ok','outcome_fail')
             GROUP BY chunk_id, context_key
             ON CONFLICT(chunk_id, context_key) DO UPDATE SET
               success_count=success_count+excluded.success_count,
               failure_count=failure_count+excluded.failure_count,
               last_updated_at=excluded.last_updated_at",
            rusqlite::params![now],
        )?;
        self.storage.conn_execute(
            "INSERT INTO chunk_context_stats
             (chunk_id, context_key, success_count, failure_count,
              positive_feedback, negative_feedback, last_updated_at)
             SELECT fe.chunk_id, fe.context_key, 0, 0,
                    SUM(CASE WHEN fe.signal='up' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN fe.signal='down' THEN 1 ELSE 0 END), ?
             FROM feedback_events fe
             WHERE fe.context_key IS NOT NULL
               AND fe.ts > COALESCE((
                 SELECT c.evidence_cutoff_at FROM chunks c
                 WHERE c.id = fe.chunk_id
               ), '')
             GROUP BY fe.chunk_id, fe.context_key
             ON CONFLICT(chunk_id, context_key) DO UPDATE SET
               positive_feedback=positive_feedback+excluded.positive_feedback,
               negative_feedback=negative_feedback+excluded.negative_feedback,
               last_updated_at=excluded.last_updated_at",
            rusqlite::params![now],
        )
    }

    /// Targeted context_stats rebuild for only the specified chunk_ids.
    /// Used by record() to avoid a full-table rebuild on every call.
    /// curate() still calls the full rebuild_context_stats() for periodic accuracy.
    fn rebuild_context_stats_for(&self, chunk_ids: &HashSet<String>, now: &str) -> Result<()> {
        if chunk_ids.is_empty() {
            return Ok(());
        }
        for chunk_id in chunk_ids {
            self.storage.conn_execute(
                "DELETE FROM chunk_context_stats WHERE chunk_id=?",
                rusqlite::params![chunk_id],
            )?;
            self.storage.conn_execute(
                "INSERT OR IGNORE INTO chunk_context_stats
                 (chunk_id, context_key, success_count, failure_count,
                  positive_feedback, negative_feedback, last_updated_at)
                 SELECT chunk_id, context_key, success_count, failure_count,
                        positive_feedback, negative_feedback, ?
                 FROM chunk_context_stats_base WHERE chunk_id=?",
                rusqlite::params![now, chunk_id],
            )?;
            self.storage.conn_execute(
                "INSERT INTO chunk_context_stats
                 (chunk_id, context_key, success_count, failure_count,
                  positive_feedback, negative_feedback, last_updated_at)
                 SELECT chunk_id, context_key,
                        SUM(CASE WHEN kind='outcome_ok' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN kind='outcome_fail' THEN 1 ELSE 0 END),
                        0, 0, ?
                 FROM confidence_evidence
                 WHERE context_key IS NOT NULL AND kind IN ('outcome_ok','outcome_fail')
                   AND chunk_id=?
                 GROUP BY chunk_id, context_key
                 ON CONFLICT(chunk_id, context_key) DO UPDATE SET
                   success_count=success_count+excluded.success_count,
                   failure_count=failure_count+excluded.failure_count,
                   last_updated_at=excluded.last_updated_at",
                rusqlite::params![now, chunk_id],
            )?;
            self.storage.conn_execute(
                "INSERT INTO chunk_context_stats
                 (chunk_id, context_key, success_count, failure_count,
                  positive_feedback, negative_feedback, last_updated_at)
                 SELECT chunk_id, context_key, 0, 0,
                        SUM(CASE WHEN signal='up' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN signal='down' THEN 1 ELSE 0 END), ?
                 FROM feedback_events
                 WHERE context_key IS NOT NULL AND chunk_id=?
                   AND ts > COALESCE((
                     SELECT evidence_cutoff_at FROM chunks
                     WHERE id=?
                   ), '')
                 GROUP BY chunk_id, context_key
                 ON CONFLICT(chunk_id, context_key) DO UPDATE SET
                   positive_feedback=positive_feedback+excluded.positive_feedback,
                   negative_feedback=negative_feedback+excluded.negative_feedback,
                   last_updated_at=excluded.last_updated_at",
                rusqlite::params![now, chunk_id, chunk_id],
            )?;
        }
        Ok(())
    }

    fn refresh_governance_evidence(&self, chunk_id: &str, now: &str) -> Result<()> {
        let rows = self.storage.query_chunks_params(
            "SELECT actor AS actor_key,
                    signal, strength, ts
             FROM feedback_events
             WHERE chunk_id=? AND actor IS NOT NULL AND actor!=''
               AND ts > COALESCE((
                 SELECT evidence_cutoff_at FROM chunks
                 WHERE id=?
               ), '')",
            rusqlite::params![chunk_id, chunk_id],
        )?;
        let mut actor_contributions: HashMap<String, f64> = HashMap::new();
        for row in rows {
            let actor = row
                .get("actor_key")
                .and_then(Value::as_str)
                .unwrap_or("anonymous")
                .to_string();
            let age_days = row
                .get("ts")
                .and_then(Value::as_str)
                .map(|ts| iso_days_diff(now, ts).max(0) as f64)
                .unwrap_or(0.0);
            let recency_weight = 0.5_f64.powf(age_days / 90.0);
            let strength = row.get("strength").and_then(Value::as_f64).unwrap_or(0.0);
            let signed = if row.get("signal").and_then(Value::as_str) == Some("down") {
                strength
            } else {
                -strength
            };
            *actor_contributions.entry(actor).or_default() += signed * recency_weight;
        }
        let mut score = 0.0_f64;
        let mut actor_count = 0_i64;
        for contribution in actor_contributions.values().copied() {
            let contribution = contribution.clamp(-1.0, 1.0);
            score += contribution;
            if contribution > 0.0 {
                actor_count += 1;
            }
        }
        let score = score.max(0.0);
        if score >= 2.0 && actor_count >= 2 {
            self.storage.upsert_governance_proposal(
                &gen_uuid(),
                chunk_id,
                "review_applicability",
                "Weighted negative feedback",
                score.ceil() as i64,
                score,
                actor_count,
                now,
            )?;
        } else {
            self.storage.conn_execute(
                "UPDATE governance_proposals
                 SET state='rejected', evidence_count=?, evidence_score=?, actor_count=?, updated_at=?
                 WHERE chunk_id=? AND state='pending'",
                rusqlite::params![score.ceil() as i64, score, actor_count, now, chunk_id],
            )?;
        }
        Ok(())
    }

    fn enqueue_evolve_if_needed(&self, now: &str) -> Result<()> {
        let ready = count_query(
            &self.storage,
            "SELECT COUNT(*) FROM episodic_log WHERE distill_state='new'",
        )?;
        let oldest = self
            .storage
            .query_chunks("SELECT MIN(ts) AS oldest FROM episodic_log WHERE distill_state='new'")?
            .first()
            .and_then(|row| row.get("oldest"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let age_due = oldest
            .as_deref()
            .is_some_and(|ts| ts <= hours_ago(now, self.evolve_schedule_interval_hours).as_str());
        let governance_pending = count_query(
            &self.storage,
            "SELECT COUNT(*) FROM governance_proposals WHERE state='pending'",
        )?;
        // A chunk whose proposal already has enough evidence should trigger evolve
        // immediately — don't wait for 3 different pending proposals.
        let governance_ready = count_query_params(
            &self.storage,
            "SELECT COUNT(*) FROM governance_proposals
             WHERE state='pending'
               AND evidence_score >= ? AND actor_count >= 2",
            rusqlite::params![self.governance_archive_threshold as f64],
        )?;
        if ready >= self.evolve_threshold
            || (ready > 0 && age_due)
            || governance_pending >= self.governance_evolve_threshold
            || governance_ready > 0
        {
            let reason = if ready >= self.evolve_threshold {
                "threshold"
            } else if governance_ready > 0 {
                "governance_ready"
            } else if governance_pending >= self.governance_evolve_threshold {
                "governance"
            } else {
                "scheduled"
            };
            self.storage.request_evolve(&gen_uuid(), reason, now)?;
        }
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
            if let Some(id) = e.get("id").and_then(Value::as_str) {
                return Ok(id.to_string());
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
        let (cvec, tvec, embed_ver, final_state_reason) = match (
            self.embedding.embed_content(&content),
            self.embedding.embed_trigger(trigger_str),
        ) {
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
                self.storage
                    .insert_vec_content(&chunk_id, &pack_embedding(&cvec))?;
                self.storage
                    .insert_vec_trigger(&chunk_id, &pack_embedding(&tvec))?;
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
            .recall(
                &content,
                2000,
                false,
                false,
                Some(5),
                "sdk",
                "false",
                false,
                "off",
            )
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
        let (cvec, tvec, embed_ver, state_reason) = match (
            self.embedding.embed_content(&content),
            self.embedding.embed_trigger(trigger_str),
        ) {
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
                self.storage
                    .insert_vec_content(&chunk_id, &pack_embedding(&cvec))?;
                self.storage
                    .insert_vec_trigger(&chunk_id, &pack_embedding(&tvec))?;
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
            self.storage
                .insert_vec_content(&new_id, &pack_embedding(&cvec))?;
            self.storage
                .insert_vec_trigger(&new_id, &pack_embedding(&tvec))?;
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

    pub fn evolve(&self, trigger: &str) -> Result<Value> {
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
            self.storage.request_evolve(
                &gen_uuid(),
                "distill_retry",
                &evolve_started_at,
            )?;
        }
        if trigger == "scheduled" {
            let age_cutoff = hours_ago(
                &evolve_started_at,
                self.evolve_schedule_interval_hours,
            );
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
            let distill = self.distill_batch()?;
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
                let _ = self.storage.request_evolve(&gen_uuid(), "batch_continue", &utc_now_iso());
            }
        }
        result
    }

    fn format_curate_report(&self, curate: &CurateReport) -> Value {
        json!({
            "archived": curate.archived.len(),
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
                let row = ChunkRow {
                    id: chunk_id,
                    content: content.clone(),
                    trigger_desc: dc.trigger_desc.clone(),
                    anti_trigger_desc: dc.anti_trigger_desc,
                    content_hash: h,
                    token_count: Some(tokens),
                    origin: "distilled".to_string(),
                    distilled_from: Some(dc.source_log_id),
                    distill_provider: provenance.provider.clone(),
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
                let cvec = match self.embedding.embed_content(&content) {
                    Ok(v) => v,
                    Err(_) => {
                        embedding_failures += 1;
                        continue;
                    }
                };
                let tvec = match self
                    .embedding
                    .embed_trigger(row.trigger_desc.as_deref().unwrap_or(&content))
                {
                    Ok(v) => v,
                    Err(_) => {
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
                    self.storage.insert_vec_content(&pc.row.id, &pc.cvec_bytes)?;
                    self.storage.insert_vec_trigger(&pc.row.id, &pc.tvec_bytes)?;
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

    fn distill_token_period_start(&self, now: &str) -> Result<String> {
        let window_hours = self
            .storage
            .get_meta("evolve.distill_token_window_hours")?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(24)
            .max(1);
        Ok(hours_ago(now, window_hours))
    }

    pub(crate) fn builtin_curate_impl(&self, scope: &CurateScope) -> Result<CurateReport> {
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
            self.storage.conn_execute(
                "DELETE FROM chunk_success_traces",
                rusqlite::params![],
            )?;
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
            let governance_chunks = self
                .storage
                .query_chunks(
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

            // ── 3c. Archive: never_used — never entered context at all ──
            let never_used_cutoff = days_ago(&now_iso, self.never_used_age_days);
            let never_used = self.storage.query_chunks_params(
                "SELECT id FROM chunks
                 WHERE origin!='spark' AND protected=0 AND state IN ('active','pending')
                   AND used_count = 0 AND selected_count = 0
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
                    let eligible = !already_archived && self.storage.get_chunk(cid)?.map(|ch| {
                        ch.get("origin").and_then(Value::as_str) != Some("spark")
                            && ch.get("protected").and_then(Value::as_i64).unwrap_or(0) == 0
                            && matches!(
                                ch.get("state").and_then(Value::as_str),
                                Some("active") | Some("pending")
                            )
                    }).unwrap_or(false);
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
                    scope_origin, scope_origin, scope_skill, scope_skill
                ],
            )?;
            for c in &high_fail_chunks {
                if let Some(cid) = c.get("id").and_then(Value::as_str) {
                    if !report.archived.contains(&cid.to_string()) {
                        self.storage.update_chunk_state(
                            cid, "archived", Some("sustained_task_failure"), &now_iso,
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
                let last_used = c.get("last_used_at").and_then(Value::as_str).unwrap_or(&now_iso);
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
                   AND success_trace_ids_count >= 2
                   AND confidence >= ?
                   AND (? IS NULL OR origin=?)
                   AND (? IS NULL OR skill_name=?)",
                rusqlite::params![
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
        let purge_cutoff = days_ago(&now_iso, 30);
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

        Ok(report)
    }

    // ------------------------------------------------------------------
    // Public API 8: inspect
    // ------------------------------------------------------------------

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
        let confidence_buckets = self.storage.query_chunks(
            &format!("SELECT
               SUM(CASE WHEN confidence < 0.25 THEN 1 ELSE 0 END) AS low,
               SUM(CASE WHEN confidence >= 0.25 AND confidence < {0} THEN 1 ELSE 0 END) AS medium,
               SUM(CASE WHEN confidence >= {0} THEN 1 ELSE 0 END) AS high
             FROM chunks WHERE origin!='spark' AND state!='archived'",
                self.promote_confidence_min),
        )?;
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

    fn sanitize_content(&self, content: &str) -> (String, SanitizeAction) {
        self.sanitizer.sanitize(content)
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
        && chunk
            .get("embed_version")
            .and_then(Value::as_i64)
            .unwrap_or(1)
            >= embed_version
}

/// Normalize a query string before hashing into a context_key.
///
/// Goals: collapse whitespace variations and case differences so that
/// semantically equivalent queries (same words, different capitalisation or
/// spacing) accumulate statistics in the same context_stat bucket.
///
/// Deliberately conservative: no stemming, no stop-word removal. The canonical
/// query guidance in SKILL.md handles vocabulary consistency at the agent level.
fn normalize_query(query: &str) -> String {
    const STOP_WORDS: &[&str] = &[
        "a", "an", "and", "for", "in", "of", "on", "the", "to", "with",
    ];
    let cleaned: String = query
        .to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch.is_whitespace() {
                ch
            } else {
                ' '
            }
        })
        .collect();
    let mut tokens: Vec<&str> = cleaned
        .split_whitespace()
        .filter(|token| !STOP_WORDS.contains(token))
        .collect();
    tokens.sort_unstable();
    tokens.dedup();
    tokens.join(" ")
}

fn estimate_distill_prompt_tokens(log: &Value, related_logs: &[Value]) -> i64 {
    let primary: i64 = [
        "query",
        "recall_snapshot",
        "output",
        "output_summary",
        "nomination",
    ]
    .iter()
    .filter_map(|key| log.get(*key).and_then(Value::as_str))
    .map(|text| estimate_tokens(text) as i64)
    .sum();
    let log_id = log.get("id").and_then(Value::as_str).unwrap_or("");
    let context_key = log.get("context_key").and_then(Value::as_str);
    let related: i64 = related_logs
        .iter()
        .filter(|other| other.get("id").and_then(Value::as_str).unwrap_or("") != log_id)
        .filter(|other| {
            context_key.is_some()
                && other.get("context_key").and_then(Value::as_str) == context_key
        })
        .take(4)
        .flat_map(|other| {
            ["query", "output_summary", "outcome"]
                .into_iter()
                .filter_map(|key| other.get(key).and_then(Value::as_str))
        })
        .map(|text| estimate_tokens(text) as i64)
        .sum();
    primary + related
}

fn estimate_distilled_chunk_tokens(chunk: &DistilledChunk) -> i64 {
    estimate_tokens(&chunk.content) as i64
        + chunk
            .trigger_desc
            .as_deref()
            .map(estimate_tokens)
            .unwrap_or(0) as i64
        + chunk
            .anti_trigger_desc
            .as_deref()
            .map(estimate_tokens)
            .unwrap_or(0) as i64
}

fn anti_trigger_hit(query: &str, anti: &str) -> bool {
    let q_lower = query.to_lowercase();
    anti.to_lowercase().split(',').any(|part| {
        let p = part.trim();
        !p.is_empty() && q_lower.contains(p)
    })
}

fn block_cost(block: &[Value]) -> usize {
    block
        .iter()
        .map(|b| {
            b.get("token_count")
                .and_then(Value::as_u64)
                .map(|t| t as usize)
                .unwrap_or_else(|| {
                    estimate_tokens(b.get("content").and_then(Value::as_str).unwrap_or("")).max(100)
                })
        })
        .sum()
}

fn limit_knowledge(knowledge: Vec<Value>, top: Option<usize>) -> Vec<Value> {
    match top {
        None => knowledge,
        Some(0) => vec![],
        Some(n) => knowledge.into_iter().take(n).collect(),
    }
}

fn usage_state(used: Option<&[String]>) -> &'static str {
    match used {
        None => "unknown",
        Some([]) => "known_none",
        Some(_) => "known_some",
    }
}

fn ratio(numerator: i64, denominator: i64) -> f64 {
    if denominator <= 0 {
        0.0
    } else {
        ((numerator as f64 / denominator as f64) * 1000.0).round() / 1000.0
    }
}

fn validate_source(source: &str) -> Result<()> {
    if !matches!(
        source,
        "mcp" | "sdk" | "cli" | "hook" | "daemon" | "augmented"
    ) {
        return Err(InnateError::InvalidState(format!(
            "invalid event source: {source}"
        )));
    }
    Ok(())
}

fn count_query(storage: &Storage, sql: &str) -> Result<i64> {
    Ok(storage
        .query_chunks(sql)?
        .first()
        .and_then(|r| r.as_object())
        .and_then(|m| m.values().next())
        .and_then(Value::as_i64)
        .unwrap_or(0))
}

fn count_query_params<P: rusqlite::Params>(storage: &Storage, sql: &str, p: P) -> Result<i64> {
    Ok(storage
        .query_chunks_params(sql, p)?
        .first()
        .and_then(|r| r.as_object())
        .and_then(|m| m.values().next())
        .and_then(Value::as_i64)
        .unwrap_or(0))
}

fn days_ago(now_iso: &str, days: i64) -> String {
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

fn hours_ago(now_iso: &str, hours: i64) -> String {
    use chrono::{DateTime, Duration, Utc};
    if let Ok(t) = now_iso.parse::<DateTime<Utc>>() {
        let cutoff = t - Duration::hours(hours);
        return cutoff.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    }
    now_iso.to_string()
}

fn minutes_after(now_iso: &str, minutes: i64) -> String {
    use chrono::{DateTime, Duration, Utc};
    if let Ok(t) = now_iso.parse::<DateTime<Utc>>() {
        let cutoff = t + Duration::minutes(minutes);
        return cutoff.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    }
    now_iso.to_string()
}

fn hours_after(now_iso: &str, hours: i64) -> String {
    use chrono::{DateTime, Duration, Utc};
    if let Ok(t) = now_iso.parse::<DateTime<Utc>>() {
        let cutoff = t + Duration::hours(hours);
        return cutoff.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    }
    now_iso.to_string()
}

/// Return the number of whole days between two ISO timestamps (now - past; clamped ≥ 0).
fn iso_days_diff(now_iso: &str, past_iso: &str) -> i64 {
    use chrono::{DateTime, Utc};
    let parse = |s: &str| s.parse::<DateTime<Utc>>().ok();
    if let (Some(a), Some(b)) = (parse(now_iso), parse(past_iso)) {
        let diff = a - b;
        diff.num_days().max(0)
    } else {
        0
    }
}

/// DFS-based cycle detection on the hard-dep graph. Returns list of cycles (each is a Vec of ids).
fn detect_cycles(deps: &[Value]) -> Vec<Vec<String>> {
    use std::collections::HashMap;
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for d in deps {
        let src = d
            .get("src")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let dst = d
            .get("dst")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !src.is_empty() && !dst.is_empty() {
            adj.entry(src).or_default().push(dst);
        }
    }
    let nodes: Vec<String> = adj.keys().cloned().collect();
    let mut visited: HashSet<String> = HashSet::new();
    let mut on_stack: HashSet<String> = HashSet::new();
    let mut cycles: Vec<Vec<String>> = vec![];

    fn dfs(
        node: &str,
        adj: &HashMap<String, Vec<String>>,
        visited: &mut HashSet<String>,
        on_stack: &mut HashSet<String>,
        path: &mut Vec<String>,
        cycles: &mut Vec<Vec<String>>,
    ) {
        if on_stack.contains(node) {
            // Found cycle — extract loop segment.
            let start = path.iter().position(|n| n == node).unwrap_or(0);
            cycles.push(path[start..].to_vec());
            return;
        }
        if visited.contains(node) {
            return;
        }
        visited.insert(node.to_string());
        on_stack.insert(node.to_string());
        path.push(node.to_string());
        if let Some(children) = adj.get(node) {
            for child in children {
                dfs(child, adj, visited, on_stack, path, cycles);
            }
        }
        path.pop();
        on_stack.remove(node);
    }

    for node in nodes {
        let mut path = vec![];
        dfs(
            &node,
            &adj,
            &mut visited,
            &mut on_stack,
            &mut path,
            &mut cycles,
        );
    }
    cycles
}
