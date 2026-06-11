pub mod cli;
pub mod daemon;
pub mod embedding;
pub mod errors;
pub mod kb;
pub mod mcp;
pub mod migrate;
pub mod refine;
pub mod storage;
pub mod utils;

mod tests;

pub use errors::{InnateError, Result};
pub use kb::{CurateReport, KnowledgeBase, RecallResult};
