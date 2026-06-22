use crate::errors::Result;
use crate::utils::{sanitize, SanitizeAction};
use serde_json::Value;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Sanitizer — injectable content sanitizer (§二·六)
// ---------------------------------------------------------------------------

/// Replaceable sanitizer. Inject via `KnowledgeBase::open_with`.
/// Default: `DefaultSanitizer` (wraps built-in heuristics).
pub trait Sanitizer: Send + Sync {
    fn sanitize(&self, content: &str) -> (String, SanitizeAction);
}

/// Built-in sanitizer — wraps `utils::sanitize()`.
pub struct DefaultSanitizer;

impl Sanitizer for DefaultSanitizer {
    fn sanitize(&self, content: &str) -> (String, SanitizeAction) {
        sanitize(content)
    }
}

/// No-op sanitizer — passes content through unchanged (use to disable sanitization).
pub struct NoopSanitizer;

impl Sanitizer for NoopSanitizer {
    fn sanitize(&self, content: &str) -> (String, SanitizeAction) {
        (content.to_string(), SanitizeAction::Allow)
    }
}

// ---------------------------------------------------------------------------
// Refiner — online trim / adapt
// ---------------------------------------------------------------------------

/// Online refiner — trims or adapts recalled chunks.
pub trait Refiner: Send + Sync {
    fn refine(&self, chunks: Vec<Value>, budget_tokens: Option<usize>) -> Result<Vec<Value>>;

    /// Trim a block to fit within `budget_tokens` given the active `query`.
    /// Returns `None` if trimming is not supported or the block cannot be trimmed while
    /// preserving hard-dep closure integrity.
    fn trim(&self, _block: &[Value], _query: &str, _budget_tokens: usize) -> Option<Vec<Value>> {
        None
    }
}

/// No-op refiner (default): returns chunks unchanged, trim is unsupported.
pub struct NullRefiner;

impl Refiner for NullRefiner {
    fn refine(&self, chunks: Vec<Value>, _budget: Option<usize>) -> Result<Vec<Value>> {
        Ok(chunks)
    }
}

/// Reranker — part (d): an OPT-IN, OFFLINE reasoning pass over the top vector/lexical
/// candidates. Returns the candidate ids in a new (more relevant) order. This is the
/// "PageIndex-style reasoning over relevance" benefit applied as a re-ranker on a small
/// shortlist, **never** in the latency-critical hook path (recall stays no-LLM unless a
/// caller explicitly sets `rerank=true`). Errors must be non-fatal at the call site:
/// recall falls back to the fused order so a flaky LLM never breaks retrieval.
pub trait Reranker: Send + Sync {
    /// Given the `query` and `candidates` (each a chunk JSON with at least `id`,
    /// `content`, `trigger_desc`), return the ids in preferred order. Implementations
    /// may return a subset/superset; the caller intersects with the known candidates.
    fn rerank(&self, query: &str, candidates: &[Value]) -> Result<Vec<String>>;
}

/// No-op reranker (default): preserves the incoming fused order. Used whenever no LLM
/// is configured or the caller did not opt in.
pub struct NoopReranker;

impl Reranker for NoopReranker {
    fn rerank(&self, _query: &str, candidates: &[Value]) -> Result<Vec<String>> {
        Ok(candidates
            .iter()
            .filter_map(|c| c.get("id").and_then(Value::as_str).map(str::to_string))
            .collect())
    }
}

/// Distiller — episodic logs → zero or more pending chunks per input log.
pub trait Distiller: Send + Sync {
    fn distill(&self, log_entries: &[Value]) -> Result<Vec<DistilledChunk>>;

    fn distill_with_context(
        &self,
        primary: &Value,
        _related_logs: &[Value],
    ) -> Result<Vec<DistilledChunk>> {
        self.distill(std::slice::from_ref(primary))
    }

    fn provenance(&self) -> DistillProvenance {
        DistillProvenance::default()
    }
}

#[derive(Debug, Default, Clone)]
pub struct DistillProvenance {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub prompt_version: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DistilledChunk {
    pub content: String,
    /// Short human-readable skill label (1-3 words) shown in the web UI's
    /// `row-skill` slot. `None` falls back to `trigger_desc` at insert time.
    pub skill_name: Option<String>,
    pub trigger_desc: Option<String>,
    pub anti_trigger_desc: Option<String>,
    pub source_log_id: String,
    pub nomination: Option<String>,
    /// Per-chunk override for `ChunkRow.distill_provider`. Set by
    /// [`ResilientDistiller`] to `"heuristic_fallback"` so operators can tell
    /// which chunks were produced by the deterministic fallback rather than the
    /// primary (LLM) distiller. `None` ⇒ use the batch-level `provenance()`.
    pub provider_override: Option<String>,
}

/// Heuristic distiller: extracts chunks from log output / nomination fields.
pub struct HeuristicDistiller;

impl Distiller for HeuristicDistiller {
    fn distill(&self, log_entries: &[Value]) -> Result<Vec<DistilledChunk>> {
        let mut out = Vec::new();
        for entry in log_entries {
            let id = entry["id"].as_str().unwrap_or("").to_string();
            let nomination = entry["nomination"].as_str();
            let text = nomination.or_else(|| entry["output_summary"].as_str());
            if let Some(t) = text {
                let t = t.trim();
                if !t.is_empty() {
                    let query = entry["query"].as_str().map(str::trim).unwrap_or("");
                    let outcome = entry["outcome"].as_str().unwrap_or("");

                    // Use query as trigger_desc for embedding — it caused this log and
                    // gives the chunk a useful retrieval signal without baking the query
                    // into the content (which creates retrieval-overfit chunks).
                    let trigger_desc = entry["query"]
                        .as_str()
                        .map(|q| q.trim().chars().take(80).collect::<String>())
                        .filter(|q| !q.is_empty())
                        .or_else(|| {
                            t.lines()
                                .map(str::trim)
                                .find(|l| l.len() > 10)
                                .map(|l| l.chars().take(80).collect())
                        });

                    // Keep content query-agnostic so the chunk is reusable across
                    // similar but not identical queries. Nominations are preserved as-is.
                    let content = if nomination.is_some() {
                        t.to_string()
                    } else if outcome == "fail" {
                        format!("Avoid: {t}")
                    } else {
                        t.to_string()
                    };

                    // For failed tasks, discourage re-triggering in the same query context.
                    let anti_trigger_desc = if outcome == "fail" && !query.is_empty() {
                        Some(query.chars().take(60).collect::<String>())
                    } else {
                        None
                    };

                    // Short skill label: first few words of the trigger phrase.
                    let skill_name = trigger_desc
                        .as_deref()
                        .map(|t| t.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
                        .filter(|s| !s.is_empty());

                    out.push(DistilledChunk {
                        content,
                        skill_name,
                        trigger_desc,
                        anti_trigger_desc,
                        source_log_id: id,
                        nomination: entry["nomination"].as_str().map(str::to_string),
                        provider_override: None,
                    });
                }
            }
        }
        Ok(out)
    }

    fn provenance(&self) -> DistillProvenance {
        DistillProvenance {
            provider: Some("heuristic".to_string()),
            model: None,
            prompt_version: Some("3".to_string()),
        }
    }
}

/// Resilient distiller — wraps a `primary` distiller (e.g. the LLM `HttpDistiller`)
/// with a deterministic `fallback` (e.g. `HeuristicDistiller`) so knowledge
/// creation never depends on the primary staying available.
///
/// Policy (see `docs/Innate-设计-确定性兜底蒸馏-v1.md`): the primary gets the
/// first `llm_attempt_budget` attempts at a log — measured by the log's
/// `distill_attempts` counter, which only increments on a `failed` terminal
/// state. While attempts remain, a primary error is **propagated** so the
/// existing `failed → recover → retry` machinery gives the primary another
/// chance (preserving quality). Once `distill_attempts >= llm_attempt_budget`,
/// a primary error triggers the deterministic fallback instead of failing the
/// log (guaranteeing eventual capture). A primary *success* — including a valid
/// empty result (nothing worth distilling) — is always used as-is and never
/// falls back.
///
/// This is **not** a second LLM: the fallback is a deterministic, local pass, so
/// it stays within the single-LLM fault-tolerance contract.
pub struct ResilientDistiller {
    primary: Arc<dyn Distiller>,
    fallback: Arc<dyn Distiller>,
    llm_attempt_budget: i64,
}

impl ResilientDistiller {
    pub fn new(
        primary: Arc<dyn Distiller>,
        fallback: Arc<dyn Distiller>,
        llm_attempt_budget: i64,
    ) -> Self {
        Self {
            primary,
            fallback,
            llm_attempt_budget,
        }
    }

    fn budget_exhausted(&self, log: &Value) -> bool {
        log.get("distill_attempts")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            >= self.llm_attempt_budget
    }

    fn tag_fallback(mut chunks: Vec<DistilledChunk>) -> Vec<DistilledChunk> {
        for c in &mut chunks {
            c.provider_override = Some("heuristic_fallback".to_string());
        }
        chunks
    }
}

impl Distiller for ResilientDistiller {
    fn distill(&self, log_entries: &[Value]) -> Result<Vec<DistilledChunk>> {
        match self.primary.distill(log_entries) {
            Ok(chunks) => Ok(chunks),
            Err(e) => {
                let exhausted = log_entries
                    .first()
                    .map(|l| self.budget_exhausted(l))
                    .unwrap_or(false);
                if exhausted {
                    Ok(Self::tag_fallback(self.fallback.distill(log_entries)?))
                } else {
                    Err(e)
                }
            }
        }
    }

    fn distill_with_context(
        &self,
        primary: &Value,
        related_logs: &[Value],
    ) -> Result<Vec<DistilledChunk>> {
        match self.primary.distill_with_context(primary, related_logs) {
            Ok(chunks) => Ok(chunks),
            Err(e) => {
                if self.budget_exhausted(primary) {
                    Ok(Self::tag_fallback(
                        self.fallback.distill_with_context(primary, related_logs)?,
                    ))
                } else {
                    Err(e)
                }
            }
        }
    }

    fn provenance(&self) -> DistillProvenance {
        self.primary.provenance()
    }
}
