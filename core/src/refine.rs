use serde_json::Value;
use crate::errors::Result;

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

/// Distiller — episodic log → new pending chunks.
pub trait Distiller: Send + Sync {
    fn distill(&self, log_entries: &[Value]) -> Result<Vec<DistilledChunk>>;
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
            // Use nomination text if present, else output_summary, else skip.
            let text = entry["nomination"]
                .as_str()
                .or_else(|| entry["output_summary"].as_str());
            if let Some(t) = text {
                let t = t.trim();
                if !t.is_empty() {
                    out.push(DistilledChunk {
                        content: t.to_string(),
                        trigger_desc: None,
                        anti_trigger_desc: None,
                        source_log_id: id,
                        nomination: entry["nomination"].as_str().map(str::to_string),
                    });
                }
            }
        }
        Ok(out)
    }
}
