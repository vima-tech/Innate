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
    DefaultSanitizer, DistilledChunk, Distiller, HeuristicDistiller, NoopReranker, NullRefiner,
    Refiner, Reranker, Sanitizer,
};
use crate::storage::{ChunkRow, EpisodicLogRow, Storage};
use crate::utils::{
    agent_source, content_hash, estimate_tokens, gen_uuid, pack_embedding, utc_now_iso,
    SanitizeAction,
};

mod appraise;
mod curate;
mod evolve;
mod inspection;
mod lifecycle;
mod recall;
mod record;
mod repair;
mod situation;

pub use appraise::{
    AbstainReason, AppraiseParams, Contributor, FlaggedPoint, Tier, Valence, Verdict,
    APPRAISE_ADVISORY,
};
pub use recall::RecallParams;
pub use record::RecordParams;
pub use repair::TraceRepairReport;
pub use situation::Situation;

// ---------------------------------------------------------------------------
// Tuning defaults
// ---------------------------------------------------------------------------

// Fused recall score weights. These intentionally sum to 1.05 (not 1.0): the
// score is a relative ranking signal, not a calibrated probability, so the extra
// 0.05 of headroom on content similarity is deliberate and the result is never
// re-normalised. Keep this in mind before "fixing" the sum.
const W_CONTENT: f64 = 0.55;
const W_TRIGGER: f64 = 0.25;
const W_CONFIDENCE: f64 = 0.10;
const W_CONTEXT: f64 = 0.15;
const W_ACTIVATION: f64 = 0.08;
// Hybrid 检索:lexical/BM25 channel weight. Modest by default so exact-term
// matches lift the right chunk without overpowering semantic similarity.
const W_LEXICAL: f64 = 0.25;
// ACT-R spreading-activation channel (SAG-inspired associative recall). Weight of
// the spread score in the fused sum. Defaults to 0.0 — OFF — so the no-LLM hot
// path is byte-for-byte unchanged until a multi-hop eval set justifies turning it
// on. When 0, recall skips the entity-expansion work entirely (zero added cost).
const W_SPREAD: f64 = 0.0;
// Entities linking more chunks than this are treated as non-discriminative and
// dropped from the spread (ACT-R fan effect taken to its limit). Keeps promiscuous
// tokens (`--release`, `rust`) from flooding the candidate set.
const SPREAD_FAN_CAP: i64 = 50;
// Number of top base-relevance candidates whose entities seed the 2-hop spread.
const SPREAD_SEED_N: usize = 5;
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
const METRICS_RETAIN_DAYS: i64 = 30;
const PROMOTE_USED_SUCCESS_MIN: i64 = 3;
const PROMOTE_CONFIDENCE_MIN: f64 = 0.60;
const DECAY_FLOOR: f64 = 0.20;
const EVOLVE_THRESHOLD: i64 = 5;
const DISTILL_BATCH_SIZE: usize = 20;
const PENDING_RECALL_PENALTY: f64 = 0.60;

// Intuition / appraise critic defaults (Spec §8). The appraise path reuses the
// same fused score as recall; these only govern how that score is tiered/flagged.
const APPRAISE_TIER_WEAK: f64 = 0.30;
const APPRAISE_TIER_STRONG: f64 = 0.65;
const APPRAISE_MIN_STRENGTH: f64 = 0.40;
const APPRAISE_TOP: usize = 8;
const APPRAISE_TRIGGER_HIT_MIN: f64 = 0.50;
const APPRAISE_CANDIDATE_IN_EMBED: bool = true;
// 弃权门(方案 A/F/G)。默认值保持现行行为(门2/门3/门4 关闭),由 meta 调参激活。
//   门2 signature_floor=0.0   → 关闭(任何一致度都放行)
//   门3 min_evidence=0        → 关闭(不要求观测历史)
//   门4 conflict_ceiling=1.0  → 关闭(离散度上界恒不触发)
// 门1 弱共振无需阈值:prune 后候选为空即弃权(WeakResonance),天然作动。
const APPRAISE_SIGNATURE_FLOOR: f64 = 0.0;
const APPRAISE_MIN_EVIDENCE: i64 = 0;
const APPRAISE_CONFLICT_CEILING: f64 = 1.0;
// 方案 D 基率锚定先验:prior = Beta(m·g0, m·(1-g0))。默认 m=2, g0=0.5 与旧 Laplace
// (wins+1)/(evidence+2) 完全等价 → 零行为变化,调大 m / 设真实基率即激活。
// **仅作用于 appraise(直觉/校准)路径**:实施文档明确范围不含 recall。
const INTUITION_PRIOR_M: f64 = 2.0;
const INTUITION_BASE_RATE: f64 = 0.5;
// recall(图书管理员)路径恒用中性 Laplace 先验(m=2, g0=0.5),与历史
// (wins+1)/(evidence+2) 逐位等价,绝不受 intuition.* 校准旋钮影响。方案 D 与 recall 解耦。
const RECALL_PRIOR_M: f64 = 2.0;
const RECALL_BASE_RATE: f64 = 0.5;
// 方案 E 校准映射桶数。
const CALIBRATION_BINS: i64 = 10;
const SITUATION_COARSE_KEYS: &str = "stage,error_class,file_type";
// Part (c) — query-embedding granularity. When true, recall folds the normalized
// situation signature (stage/error_class/file_type) into the embedded query text so
// the embedding anchors on the situation, not just raw words. Default OFF: opt-in
// and reversible (chunks are embedded from content/trigger, so enabling it shifts
// only the query side — measure with `innate recall-eval` before turning on).
const EMBED_SITUATION_SIGNATURE: bool = false;
const GOVERNANCE_ARCHIVE_THRESHOLD: i64 = 3;
const NEGATIVE_FEEDBACK_ARCHIVE_THRESHOLD: i64 = 5;
const GOVERNANCE_EVOLVE_THRESHOLD: i64 = 3;
const FAILURE_MIN_USES: i64 = 5;
const FAILURE_MAX_SUCCESS_RATE: f64 = 0.20;
const FAILURE_CONFIDENCE_MAX: f64 = 0.35;
const LOG_COMPACT_DAYS: i64 = 30;

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
    /// Opt-in offline reranker (part d). Defaults to `NoopReranker` (fused order
    /// preserved); set via `with_reranker` when an LLM is configured.
    reranker: Arc<dyn Reranker>,

    // Tuning params (loaded from meta at init)
    w_content: f64,
    w_trigger: f64,
    w_confidence: f64,
    w_context: f64,
    w_activation: f64,
    w_lexical: f64,
    w_spread: f64,
    spread_fan_cap: i64,
    spread_seed_n: usize,
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
    metrics_retain_days: i64,
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
    log_compact_days: i64,

    // Intuition / appraise critic params
    appraise_tier_weak: f64,
    appraise_tier_strong: f64,
    appraise_min_strength: f64,
    appraise_top: usize,
    appraise_trigger_hit_min: f64,
    appraise_candidate_in_embed: bool,
    appraise_signature_floor: f64,
    appraise_min_evidence: i64,
    appraise_conflict_ceiling: f64,
    intuition_prior_m: f64,
    intuition_base_rate: f64,
    calibration_bins: i64,
    situation_coarse_keys: String,
    embed_situation_signature: bool,
}

impl KnowledgeBase {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(db_path, None, None, None, None, None)
    }

    /// Persist a content embedding, rejecting any vector whose dimension differs
    /// from the configured provider. A mismatched vector is silently skipped at
    /// search time (cosine search only scores equal-dimension vectors), so an
    /// unchecked write becomes invisible recall loss with no error. Fail closed
    /// at the write boundary instead. All vector writers route through here.
    pub(crate) fn store_vec_content(&self, chunk_id: &str, cvec: &[f32]) -> Result<()> {
        let want = self.embedding.content_dim();
        if cvec.len() != want {
            return Err(InnateError::InvalidState(format!(
                "content embedding dim {} != configured {want} (chunk {chunk_id})",
                cvec.len()
            )));
        }
        self.storage
            .insert_vec_content(chunk_id, &pack_embedding(cvec))
    }

    /// Trigger-vector counterpart of [`store_vec_content`]; same fail-closed
    /// dimension guard against `trigger_dim()`.
    pub(crate) fn store_vec_trigger(&self, chunk_id: &str, tvec: &[f32]) -> Result<()> {
        let want = self.embedding.trigger_dim();
        if tvec.len() != want {
            return Err(InnateError::InvalidState(format!(
                "trigger embedding dim {} != configured {want} (chunk {chunk_id})",
                tvec.len()
            )));
        }
        self.storage
            .insert_vec_trigger(chunk_id, &pack_embedding(tvec))
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
        let reranker: Arc<dyn Reranker> = Arc::new(NoopReranker);

        let storage = Storage::open(db_path, embedding.content_dim(), embedding.trigger_dim())?;

        let mut kb = Self {
            storage,
            embedding,
            refiner,
            distiller,
            curator,
            sanitizer,
            reranker,
            w_lexical: W_LEXICAL,
            w_spread: W_SPREAD,
            spread_fan_cap: SPREAD_FAN_CAP,
            spread_seed_n: SPREAD_SEED_N,
            embed_situation_signature: EMBED_SITUATION_SIGNATURE,
            w_content: W_CONTENT,
            w_trigger: W_TRIGGER,
            w_confidence: W_CONFIDENCE,
            w_context: W_CONTEXT,
            w_activation: W_ACTIVATION,
            top_k_candidates: TOP_K_CANDIDATES,
            anti_trigger_penalty: ANTI_TRIGGER_PENALTY,
            density_refill: DENSITY_REFILL,
            low_conf_threshold: LOW_CONF_THRESHOLD,
            low_conf_idle_days: LOW_CONF_IDLE_DAYS,
            repeat_select_min: REPEAT_SELECT_MIN,
            repeat_select_conf_max: REPEAT_SELECT_CONF_MAX,
            never_used_age_days: NEVER_USED_AGE_DAYS,
            open_ttl_days: OPEN_TTL_DAYS,
            metrics_retain_days: METRICS_RETAIN_DAYS,
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
            log_compact_days: LOG_COMPACT_DAYS,
            appraise_tier_weak: APPRAISE_TIER_WEAK,
            appraise_tier_strong: APPRAISE_TIER_STRONG,
            appraise_min_strength: APPRAISE_MIN_STRENGTH,
            appraise_top: APPRAISE_TOP,
            appraise_trigger_hit_min: APPRAISE_TRIGGER_HIT_MIN,
            appraise_candidate_in_embed: APPRAISE_CANDIDATE_IN_EMBED,
            appraise_signature_floor: APPRAISE_SIGNATURE_FLOOR,
            appraise_min_evidence: APPRAISE_MIN_EVIDENCE,
            appraise_conflict_ceiling: APPRAISE_CONFLICT_CEILING,
            intuition_prior_m: INTUITION_PRIOR_M,
            intuition_base_rate: INTUITION_BASE_RATE,
            calibration_bins: CALIBRATION_BINS,
            situation_coarse_keys: SITUATION_COARSE_KEYS.to_string(),
        };
        kb.init_meta()?;
        kb.load_params()?;
        Ok(kb)
    }

    /// Install an opt-in offline reranker (part d). Used by `open_kb` when an LLM is
    /// configured; recall only invokes it when a caller passes `rerank=true`, so the
    /// default hook path stays no-LLM regardless.
    pub fn with_reranker(mut self, reranker: Arc<dyn Reranker>) -> Self {
        self.reranker = reranker;
        self
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
            ("recall.w_activation", "0.08"),
            ("recall.w_lexical", "0.25"),
            ("recall.w_spread", "0.0"),
            ("recall.spread_fan_cap", "50"),
            ("recall.spread_seed_n", "5"),
            ("recall.embed_situation_signature", "false"),
            ("recall.top_k_candidates", "20"),
            ("recall.anti_trigger_penalty", "0.6"),
            ("recall.density_refill", "true"),
            ("curate.low_conf_threshold", "0.25"),
            ("curate.low_conf_idle_days", "60"),
            ("curate.repeat_select_min", "10"),
            ("curate.repeat_select_conf_max", "0.5"),
            ("curate.never_used_age_days", "30"),
            ("metrics.retain_days", "30"),
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
            ("curate.log_compact_days", "30"),
            ("appraise.tier_weak", "0.30"),
            ("appraise.tier_strong", "0.65"),
            ("appraise.min_strength", "0.40"),
            ("appraise.top", "8"),
            ("appraise.trigger_hit_min", "0.50"),
            ("appraise.candidate_in_embed", "true"),
            ("appraise.signature_floor", "0.0"),
            ("appraise.min_evidence", "0"),
            ("appraise.conflict_ceiling", "1.0"),
            ("intuition.prior_m", "2.0"),
            ("intuition.base_rate", "0.5"),
            ("intuition.calibration_bins", "10"),
            ("situation.coarse_keys", "stage,error_class,file_type"),
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
        self.w_lexical = f("recall.w_lexical", W_LEXICAL);
        self.w_spread = f("recall.w_spread", W_SPREAD);
        self.spread_fan_cap = i("recall.spread_fan_cap", SPREAD_FAN_CAP).max(1);
        self.spread_seed_n = i("recall.spread_seed_n", SPREAD_SEED_N as i64).max(0) as usize;
        self.embed_situation_signature =
            b("recall.embed_situation_signature", EMBED_SITUATION_SIGNATURE);
        self.w_activation = f("recall.w_activation", W_ACTIVATION);
        self.top_k_candidates =
            i("recall.top_k_candidates", TOP_K_CANDIDATES as i64).max(1) as usize;
        self.anti_trigger_penalty = f("recall.anti_trigger_penalty", ANTI_TRIGGER_PENALTY);
        self.density_refill = b("recall.density_refill", DENSITY_REFILL);
        self.low_conf_threshold = f("curate.low_conf_threshold", LOW_CONF_THRESHOLD);
        self.low_conf_idle_days = i("curate.low_conf_idle_days", LOW_CONF_IDLE_DAYS);
        self.repeat_select_min = i("curate.repeat_select_min", REPEAT_SELECT_MIN);
        self.metrics_retain_days = i("metrics.retain_days", METRICS_RETAIN_DAYS);
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
        self.governance_archive_threshold = i(
            "curate.governance_archive_threshold",
            GOVERNANCE_ARCHIVE_THRESHOLD,
        )
        .max(1);
        self.negative_feedback_archive_threshold = i(
            "curate.negative_feedback_archive_threshold",
            NEGATIVE_FEEDBACK_ARCHIVE_THRESHOLD,
        )
        .max(1);
        self.governance_evolve_threshold = i(
            "evolve.governance_pending_threshold",
            GOVERNANCE_EVOLVE_THRESHOLD,
        )
        .max(1);
        self.governance_proposal_max_age_days =
            i("curate.governance_proposal_max_age_days", 30).max(1);
        self.failure_min_uses = i("curate.failure_min_uses", FAILURE_MIN_USES).max(1);
        self.failure_max_success_rate =
            f("curate.failure_max_success_rate", FAILURE_MAX_SUCCESS_RATE).clamp(0.0, 1.0);
        self.failure_confidence_max =
            f("curate.failure_confidence_max", FAILURE_CONFIDENCE_MAX).clamp(0.0, 1.0);
        self.log_compact_days = i("curate.log_compact_days", LOG_COMPACT_DAYS).max(1);
        let s = |k: &str, d: &str| -> String {
            self.storage
                .get_meta(k)
                .ok()
                .flatten()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| d.to_string())
        };
        self.appraise_tier_weak = f("appraise.tier_weak", APPRAISE_TIER_WEAK).clamp(0.0, 1.0);
        self.appraise_tier_strong = f("appraise.tier_strong", APPRAISE_TIER_STRONG).clamp(0.0, 1.0);
        self.appraise_min_strength =
            f("appraise.min_strength", APPRAISE_MIN_STRENGTH).clamp(0.0, 1.0);
        self.appraise_top = i("appraise.top", APPRAISE_TOP as i64).max(1) as usize;
        self.appraise_trigger_hit_min =
            f("appraise.trigger_hit_min", APPRAISE_TRIGGER_HIT_MIN).clamp(0.0, 1.0);
        self.appraise_candidate_in_embed =
            b("appraise.candidate_in_embed", APPRAISE_CANDIDATE_IN_EMBED);
        self.appraise_signature_floor =
            f("appraise.signature_floor", APPRAISE_SIGNATURE_FLOOR).clamp(0.0, 1.0);
        self.appraise_min_evidence = i("appraise.min_evidence", APPRAISE_MIN_EVIDENCE).max(0);
        self.appraise_conflict_ceiling =
            f("appraise.conflict_ceiling", APPRAISE_CONFLICT_CEILING).clamp(0.0, 1.0);
        self.intuition_prior_m = f("intuition.prior_m", INTUITION_PRIOR_M).max(0.0);
        self.intuition_base_rate = f("intuition.base_rate", INTUITION_BASE_RATE).clamp(0.0, 1.0);
        self.calibration_bins = i("intuition.calibration_bins", CALIBRATION_BINS).clamp(2, 100);
        self.situation_coarse_keys = s("situation.coarse_keys", SITUATION_COARSE_KEYS);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct CandidateInfo {
    chunk: Value,
    sim_content: f32,
    sim_trigger: f32,
    /// Lexical/BM25 channel score ∈ [0,1] (hybrid 检索). Zero when the chunk was
    /// found only by vector search; positive when an exact-term match recovered it.
    sim_lexical: f32,
    /// ACT-R spreading-activation score ∈ [0,1]. Positive when the chunk was
    /// reached via a shared entity (with the query or a high-relevance seed),
    /// even if no similarity/lexical channel surfaced it. Zero on the default
    /// (w_spread = 0) path.
    sim_spread: f32,
}

/// True when a coarse signature carries at least one real value (not empty /
/// `none` / `unknown`) — used to decide whether folding it into the embed query
/// adds signal or just noise.
fn signature_has_signal(sig: &str) -> bool {
    sig.split('|').any(|p| {
        p.split_once('=')
            .map(|(_, v)| !v.is_empty() && v != "none" && v != "unknown")
            .unwrap_or(false)
    })
}

/// Fresh candidate from a chunk with all channel sims zeroed (callers set the
/// channel(s) that surfaced it). Centralised so adding a channel touches one place.
fn new_candidate(chunk: &Value) -> CandidateInfo {
    CandidateInfo {
        chunk: chunk.clone(),
        sim_content: 0.0,
        sim_trigger: 0.0,
        sim_lexical: 0.0,
        sim_spread: 0.0,
    }
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
            context_key.is_some() && other.get("context_key").and_then(Value::as_str) == context_key
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

fn hours_ago(now_iso: &str, hours: i64) -> String {
    use chrono::{DateTime, Duration, Utc};
    if let Ok(t) = now_iso.parse::<DateTime<Utc>>() {
        let cutoff = t - Duration::hours(hours);
        return cutoff.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    }
    now_iso.to_string()
}

impl KnowledgeBase {
    /// Time a fallible operation, persist a one-row `operation_runs` summary (P3a,
    /// design doc §5.3), and return its result unchanged. Instrumentation is
    /// **non-fatal**: a failed metrics insert never affects the wrapped operation.
    /// Raw LLM detail still lives in `llm_trace.log`; this holds only the aggregatable
    /// summary (status / duration / error_kind). `error_kind` comes from the closed
    /// vocabulary in `storage::metrics::classify_error` so the top-list groups cleanly.
    pub(crate) fn measure<T>(
        &self,
        op: &str,
        source: Option<&str>,
        trace_id: Option<&str>,
        f: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let started = std::time::Instant::now();
        let started_at = utc_now_iso();
        let result = f();
        let duration_ms = started.elapsed().as_millis() as i64;
        let (status, error_kind) = match &result {
            Ok(_) => ("ok", None),
            Err(e) => (
                "error",
                Some(crate::storage::metrics::classify_error(e).to_string()),
            ),
        };
        let run = crate::storage::metrics::OperationRun {
            id: gen_uuid(),
            trace_id: trace_id.map(|s| s.to_string()),
            op: op.to_string(),
            source: source.map(|s| s.to_string()),
            agent: crate::utils::agent_source(),
            status: status.to_string(),
            error_kind,
            started_at,
            duration_ms,
            counts_json: None,
            params_json: None,
        };
        let _ = self.storage.insert_operation_run(&run);
        result
    }

    /// Embed content+trigger as one timed `embed` operation_run (full-path coverage:
    /// add / spark / promote / distill / rebuild_embeddings). Returns the two Results
    /// **unchanged** so each caller keeps its own embedding-failure fallback. The op row
    /// is best-effort/non-fatal; when called inside an open transaction it simply joins
    /// it (same connection, no lock conflict). `error_kind` uses the closed vocabulary.
    pub(crate) fn embed_pair(
        &self,
        content: &str,
        trigger: &str,
        source: &str,
    ) -> (Result<Vec<f32>>, Result<Vec<f32>>) {
        let started = std::time::Instant::now();
        let started_at = utc_now_iso();
        let c = self.embedding.embed_content(content);
        let t = self.embedding.embed_trigger(trigger);
        let duration_ms = started.elapsed().as_millis() as i64;
        let (status, error_kind) = match c.as_ref().err().or(t.as_ref().err()) {
            None => ("ok", None),
            Some(e) => (
                "error",
                Some(crate::storage::metrics::classify_error(e).to_string()),
            ),
        };
        let run = crate::storage::metrics::OperationRun {
            id: gen_uuid(),
            trace_id: None,
            op: "embed".to_string(),
            source: Some(source.to_string()),
            agent: crate::utils::agent_source(),
            status: status.to_string(),
            error_kind,
            started_at,
            duration_ms,
            counts_json: None,
            params_json: None,
        };
        let _ = self.storage.insert_operation_run(&run);
        (c, t)
    }
}

fn minutes_ago(now_iso: &str, minutes: i64) -> String {
    use chrono::{DateTime, Duration, Utc};
    if let Ok(t) = now_iso.parse::<DateTime<Utc>>() {
        let cutoff = t - Duration::minutes(minutes);
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

/// Fractional days between two ISO timestamps (≥ 0). Finer than `iso_days_diff`
/// so the activation recency term keeps sub-day resolution.
fn iso_fractional_days(now_iso: &str, past_iso: &str) -> f64 {
    use chrono::{DateTime, Utc};
    let parse = |s: &str| s.parse::<DateTime<Utc>>().ok();
    if let (Some(a), Some(b)) = (parse(now_iso), parse(past_iso)) {
        ((a - b).num_seconds().max(0)) as f64 / 86_400.0
    } else {
        0.0
    }
}

/// ACT-R decay exponent for the base-level activation recency term.
const ACTR_DECAY: f64 = 0.5;

/// ACT-R-inspired base-level activation, bounded to `(0, 1)`.
///
/// Fuses **frequency** (how often a chunk has been used) and **recency** (time
/// since last use) into one re-ranking signal, following the standard ACT-R
/// approximation `B = ln(n) − d·ln(t)` (Petrov 2006), here using
/// `B = ln(1 + used_count) − d·ln(1 + recency_days)` and squashed with a
/// logistic so it stays on the same `[0, 1]` scale as the other fused-score
/// terms (content/trigger sim, confidence, context).
///
/// Returns `0.0` for never-used chunks (no usage history → no boost), which
/// keeps recall **zero-regression** for freshly-added knowledge: a chunk with
/// `used_count == 0` contributes nothing to the fused score.
pub(super) fn actr_activation(used_count: i64, last_used_at: Option<&str>, now_iso: &str) -> f64 {
    if used_count <= 0 {
        return 0.0;
    }
    let Some(last) = last_used_at else {
        return 0.0;
    };
    let recency_days = iso_fractional_days(now_iso, last);
    let b = (1.0 + used_count as f64).ln() - ACTR_DECAY * (1.0 + recency_days).ln();
    1.0 / (1.0 + (-b).exp())
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
