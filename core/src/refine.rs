use crate::errors::Result;
use crate::utils::{sanitize, SanitizeAction};
use serde_json::Value;

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

#[derive(Debug, Clone)]
pub struct DistilledChunk {
    pub content: String,
    pub trigger_desc: Option<String>,
    pub anti_trigger_desc: Option<String>,
    pub source_log_id: String,
    pub nomination: Option<String>,
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

                    out.push(DistilledChunk {
                        content,
                        trigger_desc,
                        anti_trigger_desc,
                        source_log_id: id,
                        nomination: entry["nomination"].as_str().map(str::to_string),
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
            prompt_version: Some("2".to_string()),
        }
    }
}
