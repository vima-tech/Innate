pub mod backup;
pub mod cli;
pub mod daemon;
pub mod embedding;
pub mod errors;
pub mod hook;
pub mod install;
pub mod kb;
pub mod llm;
pub mod llm_trace;
pub mod mcp;
pub mod migrate;
pub mod paths;
pub mod refine;
pub mod settings;
pub mod storage;
pub mod upgrade;
pub mod utils;
pub mod web;

#[cfg(test)]
mod tests;

pub use errors::{InnateError, Result};
pub use kb::{
    AppraiseParams, Contributor, CurateReport, FlaggedPoint, KnowledgeBase, RecallParams,
    RecallResult, RecordParams, Situation, Tier, Valence, Verdict,
};

/// Open a KnowledgeBase at `db_path`, injecting LLM providers from `~/.innate/settings.json`
/// if configured. Falls back to DummyEmbeddingProvider + HeuristicDistiller when no LLM
/// settings are present.
pub fn open_kb(db_path: impl AsRef<std::path::Path>) -> Result<KnowledgeBase> {
    use std::sync::Arc;
    let s = settings::load()?;

    let embedding: Option<Arc<dyn embedding::EmbeddingProvider>> = s.embedding.as_ref().map(|c| {
        Arc::new(llm::LlmEmbeddingProvider::new(c.clone())) as Arc<dyn embedding::EmbeddingProvider>
    });

    // When a remote LLM distiller is configured, wrap it with a deterministic
    // fallback so knowledge creation never depends on the LLM staying available:
    // the LLM gets the first 2 attempts per log (quality); after that the
    // deterministic HeuristicDistiller guarantees capture (stability).
    let distiller: Option<Arc<dyn refine::Distiller>> = s.llm.as_ref().map(|c| {
        let primary = llm::build_distiller(c) as Arc<dyn refine::Distiller>;
        Arc::new(refine::ResilientDistiller::new(
            primary,
            Arc::new(refine::HeuristicDistiller),
            2,
        )) as Arc<dyn refine::Distiller>
    });

    KnowledgeBase::open_with(db_path, embedding, None, distiller, None, None)
}
