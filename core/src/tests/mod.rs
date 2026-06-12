use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tempfile::NamedTempFile;

mod basics;
mod distill_failures;
mod distillation;
mod evolve_retries;
mod feedback;
mod governance;
mod reliability;
mod restoration;

use crate::embedding::{DummyEmbeddingProvider, EmbeddingProvider};
use crate::errors::{InnateError, Result};
use crate::kb::{CurateScope, KnowledgeBase};
use crate::refine::{DistilledChunk, Distiller, Refiner};

fn tmp_kb() -> (KnowledgeBase, NamedTempFile) {
    let f = NamedTempFile::new().unwrap();
    let kb = KnowledgeBase::open(f.path()).unwrap();
    (kb, f)
}

fn attributed_trace(kb: &KnowledgeBase, chunk_id: &str) -> String {
    attributed_trace_many(kb, &[chunk_id.to_string()])
}

fn attributed_trace_many(kb: &KnowledgeBase, chunk_ids: &[String]) -> String {
    let trace_id = crate::utils::gen_uuid();
    let now = crate::utils::utc_now_iso();
    let row = crate::storage::EpisodicLogRow {
        id: crate::utils::gen_uuid(),
        trace_id: trace_id.clone(),
        lib_id: kb.storage.lib_id().unwrap(),
        ts: now.clone(),
        query: Some("attributed query".to_string()),
        recall_snapshot: Some(
            serde_json::json!({"retrieved": chunk_ids, "selected": chunk_ids, "sparks": []})
                .to_string(),
        ),
        event_source: "sdk".to_string(),
        task_state: "recalled".to_string(),
        usage_state: "unknown".to_string(),
        context_key: Some(crate::utils::content_hash("attributed query")),
        distill_state: "open".to_string(),
        ..Default::default()
    };
    kb.storage.upsert_episodic_log(&row).unwrap();
    for (rank, chunk_id) in chunk_ids.iter().enumerate() {
        kb.storage
            .insert_usage_trace(
                &trace_id,
                Some(chunk_id),
                "selected",
                1.0,
                None,
                None,
                None,
                Some((rank + 1) as i64),
                None,
                "sdk",
                &now,
            )
            .unwrap();
    }
    trace_id
}

fn record_down_as(kb: &KnowledgeBase, chunk_id: &str, actor: &str) {
    let trace_id = attributed_trace(kb, chunk_id);
    kb.record_detailed(
        &trace_id,
        Some("q"),
        None,
        None,
        None,
        None,
        "explicit",
        true,
        None,
        Some(&[chunk_id.to_string()]),
        "user",
        Some(actor),
        None,
        None,
        0,
        None,
        "sdk",
    )
    .unwrap();
}
