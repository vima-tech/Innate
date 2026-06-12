pub mod backup;
pub mod cli;
pub mod daemon;
pub mod embedding;
pub mod errors;
pub mod hook;
pub mod install;
pub mod kb;
pub mod llm;
pub mod mcp;
pub mod migrate;
pub mod refine;
pub mod settings;
pub mod storage;
pub mod upgrade;
pub mod utils;

#[cfg(test)]
mod tests;

pub use errors::{InnateError, Result};
pub use kb::{CurateReport, KnowledgeBase, RecallResult};

/// Open a KnowledgeBase at `db_path`, injecting LLM providers from `~/.innate/settings.json`
/// if configured. Falls back to DummyEmbeddingProvider + HeuristicDistiller when no LLM
/// settings are present.
pub fn open_kb(db_path: impl AsRef<std::path::Path>) -> Result<KnowledgeBase> {
    use std::sync::Arc;
    let s = settings::load();

    let embedding: Option<Arc<dyn embedding::EmbeddingProvider>> = s.embedding.as_ref().map(|c| {
        Arc::new(llm::LlmEmbeddingProvider::new(c.clone())) as Arc<dyn embedding::EmbeddingProvider>
    });

    let distiller: Option<Arc<dyn refine::Distiller>> = s
        .llm
        .as_ref()
        .map(|c| llm::build_distiller(c) as Arc<dyn refine::Distiller>);

    KnowledgeBase::open_with(db_path, embedding, None, distiller, None, None)
}
