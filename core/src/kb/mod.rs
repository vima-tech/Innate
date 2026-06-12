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

mod curate;
mod evolve;
mod inspection;
mod lifecycle;
mod recall;
mod record;

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
