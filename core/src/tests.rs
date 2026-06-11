use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tempfile::NamedTempFile;

use crate::embedding::{DummyEmbeddingProvider, EmbeddingProvider};
use crate::errors::{InnateError, Result};
use crate::kb::{CurateScope, KnowledgeBase};
use crate::refine::{DistilledChunk, Distiller, Refiner};

fn tmp_kb() -> (KnowledgeBase, NamedTempFile) {
    let f = NamedTempFile::new().unwrap();
    let kb = KnowledgeBase::open(f.path()).unwrap();
    (kb, f)
}

#[test]
fn add_and_recall() {
    let (kb, _f) = tmp_kb();
    let id = kb
        .add(
            "Always validate user input at system boundaries",
            "note",
            Some("input validation"),
            None,
            "manual",
            None,
        )
        .unwrap();
    assert!(!id.is_empty());

    let result = kb
        .recall(
            "validate input",
            6000,
            false,
            false,
            None,
            "sdk",
            "false",
            false,
            "off",
        )
        .unwrap();
    assert!(!result.trace_id.is_empty());
}

#[test]
fn spark_and_promote() {
    let (kb, _f) = tmp_kb();
    let sid = kb
        .spark("Use HNSW index for recall scalability", None, None)
        .unwrap();
    assert!(!sid.is_empty());

    let nid = kb.promote_spark(&sid, "note").unwrap();
    assert!(!nid.is_empty());

    let chunk = kb.storage.get_chunk(&nid).unwrap().unwrap();
    assert_eq!(chunk["origin"].as_str().unwrap(), "captured");
    assert_eq!(chunk["state"].as_str().unwrap(), "active");
}

#[test]
fn record_state_machine() {
    let (kb, _f) = tmp_kb();
    let trace_id = crate::utils::gen_uuid();
    kb.record(
        &trace_id,
        Some("test query"),
        None,
        Some("summary"),
        Some("ok"),
        None,
        None,
        None,
        None,
        0,
        "cli",
    )
    .unwrap();
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(log["distill_state"].as_str().unwrap(), "new");

    kb.record(
        &trace_id,
        None,
        None,
        None,
        Some("ok"),
        None,
        None,
        None,
        None,
        0,
        "cli",
    )
    .unwrap();
    let log2 = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(log2["distill_state"].as_str().unwrap(), "new");
}

#[test]
fn invalidate_cascade() {
    let (kb, _f) = tmp_kb();
    let id = kb
        .add("sensitive content", "note", None, None, "manual", None)
        .unwrap();
    kb.invalidate(&id, "test").unwrap();
    let chunk = kb.storage.get_chunk(&id).unwrap().unwrap();
    assert_eq!(chunk["state"].as_str().unwrap(), "archived");
    assert_eq!(chunk["confidence"].as_f64().unwrap(), 0.0);
    let h = chunk["content_hash"].as_str().unwrap();
    assert!(kb.storage.is_hash_invalidated(h).unwrap());
}

#[test]
fn inspect_returns_counts() {
    let (kb, _f) = tmp_kb();
    kb.add("test chunk", "note", None, None, "manual", None)
        .unwrap();
    let info = kb.inspect().unwrap();
    let active = info["chunks"]["active"].as_i64().unwrap_or(0);
    assert!(active >= 1);
}

#[test]
fn evolve_smoke() {
    let (kb, _f) = tmp_kb();
    let result = kb.evolve("manual").unwrap();
    assert!(result["distilled"].is_number());
}

struct CountingRefiner {
    calls: Arc<AtomicUsize>,
}

impl Refiner for CountingRefiner {
    fn refine(&self, chunks: Vec<Value>, _budget: Option<usize>) -> Result<Vec<Value>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(chunks)
    }
}

#[test]
fn refine_runs_only_in_adapt_mode() {
    let file = NamedTempFile::new().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let refiner = Arc::new(CountingRefiner {
        calls: Arc::clone(&calls),
    });
    let kb = KnowledgeBase::open_with(file.path(), None, Some(refiner), None, None, None).unwrap();
    kb.add("Refiner mode test", "note", None, None, "manual", None)
        .unwrap();

    kb.recall(
        "Refiner mode test",
        6000,
        false,
        false,
        None,
        "sdk",
        "false",
        false,
        "off",
    )
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    kb.recall(
        "Refiner mode test",
        6000,
        false,
        false,
        None,
        "sdk",
        "false",
        false,
        "adapt",
    )
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

struct FailingDistiller;

impl Distiller for FailingDistiller {
    fn distill(&self, _log_entries: &[Value]) -> Result<Vec<DistilledChunk>> {
        Err(InnateError::Other("model offline".to_string()))
    }
}

#[test]
fn distiller_error_marks_log_failed() {
    let file = NamedTempFile::new().unwrap();
    let kb = KnowledgeBase::open_with(
        file.path(),
        None,
        None,
        Some(Arc::new(FailingDistiller)),
        None,
        None,
    )
    .unwrap();
    let trace_id = crate::utils::gen_uuid();
    kb.record(
        &trace_id,
        Some("query"),
        None,
        Some("material"),
        Some("ok"),
        None,
        None,
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();

    let result = kb.evolve("manual").unwrap();
    assert_eq!(result["distilled"].as_u64(), Some(0));
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(log["distill_state"].as_str(), Some("failed"));
    assert_eq!(
        log["distill_note"].as_str(),
        Some("distill_failed:model offline")
    );
}

#[test]
fn distill_records_prompt_and_completion_token_estimates() {
    let (kb, _file) = tmp_kb();
    let trace_id = crate::utils::gen_uuid();
    kb.record(
        &trace_id,
        Some("How should retries be bounded?"),
        None,
        Some("Use bounded exponential backoff with jitter."),
        Some("ok"),
        None,
        None,
        None,
        Some("Reusable retry guidance"),
        1,
        "sdk",
    )
    .unwrap();

    kb.evolve("manual").unwrap();
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert!(log["distill_prompt_tokens"].as_i64().unwrap_or(0) > 0);
    assert!(log["distill_completion_tokens"].as_i64().unwrap_or(0) > 0);
}

#[test]
fn threshold_evolve_respects_distill_token_limit() {
    let file = NamedTempFile::new().unwrap();
    let first_trace = crate::utils::gen_uuid();
    {
        let kb = KnowledgeBase::open(file.path()).unwrap();
        kb.record(
            &first_trace,
            Some("first query"),
            None,
            Some("first reusable material"),
            Some("ok"),
            None,
            None,
            None,
            None,
            0,
            "sdk",
        )
        .unwrap();
        kb.evolve("manual").unwrap();
        let first_log = kb.storage.get_episodic_log(&first_trace).unwrap().unwrap();
        let used = first_log["distill_prompt_tokens"].as_i64().unwrap_or(0)
            + first_log["distill_completion_tokens"].as_i64().unwrap_or(0);
        assert!(used > 0);
        kb.storage
            .set_meta("max_distill_tokens_per_period", &used.to_string())
            .unwrap();
        kb.storage
            .set_meta("evolve.threshold_new_count", "1")
            .unwrap();
    }

    let kb = KnowledgeBase::open(file.path()).unwrap();
    let second_trace = crate::utils::gen_uuid();
    kb.record(
        &second_trace,
        Some("second query"),
        None,
        Some("second reusable material"),
        Some("ok"),
        None,
        None,
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();

    let result = kb.evolve("threshold").unwrap();
    assert_eq!(result["distilled"].as_u64(), Some(0));
    assert_eq!(result["skipped"].as_str(), Some("distill_token_limit"));
    let second_log = kb.storage.get_episodic_log(&second_trace).unwrap().unwrap();
    assert_eq!(second_log["distill_state"].as_str(), Some("new"));
}

#[test]
fn opening_with_mismatched_embedding_dimensions_fails() {
    let file = NamedTempFile::new().unwrap();
    drop(KnowledgeBase::open(file.path()).unwrap());

    let embedding: Arc<dyn EmbeddingProvider> = Arc::new(DummyEmbeddingProvider::new(8, 4));
    let result = KnowledgeBase::open_with(file.path(), Some(embedding), None, None, None, None);
    let error = result.err().expect("dimension mismatch should fail");
    assert!(error.to_string().contains("content_dim"));
}

#[test]
fn stale_screening_is_reported_as_recovered() {
    let (kb, _file) = tmp_kb();
    let trace_id = crate::utils::gen_uuid();
    kb.record(
        &trace_id,
        Some("query"),
        None,
        Some("material"),
        Some("ok"),
        None,
        None,
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();
    kb.storage
        .conn_execute(
            "UPDATE episodic_log
             SET distill_state='screening', distill_run_id='test-run',
                 distill_locked_at='2000-01-01T00:00:00.000Z'
             WHERE trace_id=?",
            rusqlite::params![trace_id],
        )
        .unwrap();

    let report = kb.builtin_curate_impl(&CurateScope::default()).unwrap();
    assert_eq!(report.recovered.len(), 1);
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(
        log["distill_note"].as_str(),
        Some("screening_timeout:test-run")
    );
}

#[test]
fn dedupe_respects_scope_and_records_canonical_parent() {
    let (kb, _file) = tmp_kb();
    let canonical = kb
        .add(
            "canonical scoped chunk",
            "note",
            None,
            None,
            "manual",
            Some("scope-a"),
        )
        .unwrap();
    let duplicate = kb
        .add(
            "duplicate scoped chunk",
            "note",
            None,
            None,
            "manual",
            Some("scope-a"),
        )
        .unwrap();
    let outside = kb
        .add(
            "outside scoped chunk",
            "note",
            None,
            None,
            "manual",
            Some("scope-b"),
        )
        .unwrap();
    kb.storage
        .conn_execute(
            "UPDATE chunks
             SET content_hash='forced-duplicate',
                 confidence=CASE id WHEN ? THEN 0.9 WHEN ? THEN 0.5 ELSE 0.1 END
             WHERE id IN (?,?,?)",
            rusqlite::params![canonical, duplicate, canonical, duplicate, outside],
        )
        .unwrap();

    let report = kb
        .builtin_curate_impl(&CurateScope {
            skill_name: Some("scope-a".to_string()),
            ..CurateScope::default()
        })
        .unwrap();
    assert_eq!(report.deduped, vec![duplicate.clone()]);

    let duplicate_chunk = kb.storage.get_chunk(&duplicate).unwrap().unwrap();
    assert_eq!(duplicate_chunk["state"].as_str(), Some("archived"));
    assert_eq!(
        duplicate_chunk["parent_id"].as_str(),
        Some(canonical.as_str())
    );

    let outside_chunk = kb.storage.get_chunk(&outside).unwrap().unwrap();
    assert_eq!(outside_chunk["state"].as_str(), Some("active"));
    assert!(outside_chunk["parent_id"].is_null());
}

#[test]
fn curate_reports_missing_hard_dependency_as_orphan() {
    let (kb, _file) = tmp_kb();
    let source = kb
        .add("source chunk", "note", None, None, "manual", None)
        .unwrap();
    kb.storage
        .insert_dep(&source, "missing-hard-dependency", "hard", None)
        .unwrap();

    let report = kb.builtin_curate_impl(&CurateScope::default()).unwrap();
    assert_eq!(report.orphans, vec!["missing-hard-dependency"]);
}
