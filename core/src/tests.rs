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
fn mcp_is_a_valid_event_source() {
    let (kb, _f) = tmp_kb();
    let result = kb
        .recall(
            "mcp source",
            6000,
            true,
            false,
            None,
            "mcp",
            "false",
            false,
            "off",
        )
        .unwrap();
    kb.record(
        &result.trace_id,
        None,
        None,
        Some("closed through MCP"),
        Some("ok"),
        None,
        None,
        None,
        None,
        0,
        "mcp",
    )
    .unwrap();
}

#[test]
fn unknown_usage_does_not_penalize_selected_chunks() {
    let (kb, _f) = tmp_kb();
    let chunk_id = kb
        .add(
            "Use bounded retries",
            "note",
            Some("bounded retries"),
            None,
            "manual",
            None,
        )
        .unwrap();
    let first = kb
        .recall(
            "bounded retries",
            6000,
            true,
            false,
            None,
            "sdk",
            "false",
            false,
            "off",
        )
        .unwrap();
    let before = kb.storage.get_chunk(&chunk_id).unwrap().unwrap()["confidence"]
        .as_f64()
        .unwrap();
    kb.record(
        &first.trace_id,
        None,
        None,
        Some("completed without usage attribution"),
        Some("ok"),
        None,
        None,
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();
    let after_unknown = kb.storage.get_chunk(&chunk_id).unwrap().unwrap()["confidence"]
        .as_f64()
        .unwrap();
    assert_eq!(before, after_unknown);

    let second = kb
        .recall(
            "bounded retries",
            6000,
            true,
            false,
            None,
            "sdk",
            "false",
            false,
            "off",
        )
        .unwrap();
    let explicitly_unused: Vec<String> = vec![];
    kb.record(
        &second.trace_id,
        None,
        None,
        Some("completed and explicitly used no recalled chunks"),
        Some("ok"),
        Some(&explicitly_unused),
        None,
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();
    let after_known_none = kb.storage.get_chunk(&chunk_id).unwrap().unwrap()["confidence"]
        .as_f64()
        .unwrap();
    assert!(after_known_none < after_unknown);
}

#[test]
fn feedback_is_auditable_and_builds_contextual_governance_evidence() {
    let (kb, _f) = tmp_kb();
    let chunk_id = kb
        .add(
            "Always retry forever",
            "note",
            Some("retry policy"),
            None,
            "manual",
            None,
        )
        .unwrap();

    for _ in 0..2 {
        let recall = kb
            .recall(
                "retry policy",
                6000,
                true,
                false,
                None,
                "sdk",
                "false",
                false,
                "off",
            )
            .unwrap();
        let used = vec![chunk_id.clone()];
        kb.record_detailed(
            &recall.trace_id,
            None,
            None,
            Some("retry policy was unsuitable"),
            Some("fail"),
            Some(&used),
            "explicit",
            None,
            Some(&used),
            "user",
            Some("tester"),
            Some("unbounded retry is unsafe"),
            None,
            0,
            None,
            "sdk",
        )
        .unwrap();
    }

    let feedback_count = kb
        .storage
        .query_chunks_params(
            "SELECT COUNT(*) AS count FROM feedback_events WHERE chunk_id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap()[0]["count"]
        .as_i64();
    assert_eq!(feedback_count, Some(2));
    let proposals = kb
        .storage
        .query_chunks("SELECT * FROM governance_proposals WHERE state='pending'")
        .unwrap();
    assert_eq!(proposals.len(), 1);
    let context = kb
        .storage
        .query_chunks("SELECT * FROM chunk_context_stats")
        .unwrap();
    assert_eq!(context.len(), 1);
    assert_eq!(context[0]["failure_count"].as_i64(), Some(2));
    assert_eq!(context[0]["negative_feedback"].as_i64(), Some(2));
}

#[test]
fn record_requests_evolve_and_inspect_reports_feedback_metrics() {
    let file = NamedTempFile::new().unwrap();
    {
        let kb = KnowledgeBase::open(file.path()).unwrap();
        kb.storage
            .set_meta("evolve.threshold_new_count", "1")
            .unwrap();
    }
    let kb = KnowledgeBase::open(file.path()).unwrap();
    let trace_id = crate::utils::gen_uuid();
    kb.record(
        &trace_id,
        Some("queue evolve"),
        None,
        Some("reusable material"),
        Some("ok"),
        Some(&[]),
        None,
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();

    let requests = kb
        .storage
        .query_chunks("SELECT * FROM evolve_requests WHERE state='pending'")
        .unwrap();
    assert_eq!(requests.len(), 1);
    let inspect = kb.inspect().unwrap();
    assert_eq!(
        inspect["feedback_loop"]["trace_completion_rate"].as_f64(),
        Some(1.0)
    );
    assert_eq!(
        inspect["feedback_loop"]["usage_annotation_rate"].as_f64(),
        Some(1.0)
    );
    assert_eq!(
        inspect["feedback_loop"]["pending_evolve_requests"].as_i64(),
        Some(1)
    );
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

struct MultiChunkDistiller;

impl Distiller for MultiChunkDistiller {
    fn distill(&self, log_entries: &[Value]) -> Result<Vec<DistilledChunk>> {
        let source_log_id = log_entries[0]["id"].as_str().unwrap().to_string();
        Ok(vec![
            DistilledChunk {
                content: "first chunk".to_string(),
                trigger_desc: None,
                anti_trigger_desc: None,
                source_log_id: source_log_id.clone(),
                nomination: None,
            },
            DistilledChunk {
                content: "second chunk".to_string(),
                trigger_desc: None,
                anti_trigger_desc: None,
                source_log_id,
                nomination: None,
            },
        ])
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
fn multi_chunk_distiller_fails_without_leaving_log_screening() {
    let file = NamedTempFile::new().unwrap();
    let kb = KnowledgeBase::open_with(
        file.path(),
        None,
        None,
        Some(Arc::new(MultiChunkDistiller)),
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
        Some("distill_failed:expected_one_chunk_got_2")
    );
    assert_eq!(
        kb.storage
            .query_chunks("SELECT COUNT(*) AS cnt FROM chunks")
            .unwrap()[0]["cnt"]
            .as_i64(),
        Some(0)
    );
}

#[test]
fn distill_write_failure_marks_log_failed() {
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
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    let log_id = log["id"].as_str().unwrap();
    let existing_chunk = kb
        .add("existing chunk", "note", None, None, "manual", None)
        .unwrap();
    kb.storage
        .conn_execute(
            "UPDATE chunks SET distilled_from=? WHERE id=?",
            rusqlite::params![log_id, existing_chunk],
        )
        .unwrap();

    let result = kb.evolve("manual").unwrap();
    assert_eq!(result["distilled"].as_u64(), Some(0));
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(log["distill_state"].as_str(), Some("failed"));
    assert!(log["distill_note"]
        .as_str()
        .unwrap_or("")
        .starts_with("distill_write_failed:"));
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
fn distill_token_window_uses_actual_distill_time_not_log_creation_time() {
    let file = NamedTempFile::new().unwrap();
    let kb = KnowledgeBase::open(file.path()).unwrap();
    let first_trace = crate::utils::gen_uuid();
    kb.record(
        &first_trace,
        Some("old queued query"),
        None,
        Some("material distilled today"),
        Some("ok"),
        None,
        None,
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();
    let queued_at = (chrono::Utc::now() - chrono::Duration::hours(48))
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    kb.storage
        .conn_execute(
            "UPDATE episodic_log SET ts=? WHERE trace_id=?",
            rusqlite::params![queued_at, first_trace],
        )
        .unwrap();

    kb.evolve("manual").unwrap();
    let first_log = kb.storage.get_episodic_log(&first_trace).unwrap().unwrap();
    let used = first_log["distill_prompt_tokens"].as_i64().unwrap_or(0)
        + first_log["distill_completion_tokens"].as_i64().unwrap_or(0);
    assert!(used > 0);
    assert!(first_log["distill_accounted_at"].as_str().is_some());
    kb.storage
        .set_meta("max_distill_tokens_per_period", &used.to_string())
        .unwrap();
    kb.storage
        .set_meta("evolve.threshold_new_count", "1")
        .unwrap();
    drop(kb);

    let kb = KnowledgeBase::open(file.path()).unwrap();
    let second_trace = crate::utils::gen_uuid();
    kb.record(
        &second_trace,
        Some("new query"),
        None,
        Some("new material"),
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
fn migration_4_5_1_adds_distill_accounting_time() {
    let file = NamedTempFile::new().unwrap();
    let conn = rusqlite::Connection::open(file.path()).unwrap();
    conn.execute_batch(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO meta(key, value) VALUES ('schema_version', '4.5.1');
         CREATE TABLE episodic_log (
             id TEXT PRIMARY KEY,
             trace_id TEXT NOT NULL,
             lib_id TEXT NOT NULL,
             ts TEXT NOT NULL,
             query TEXT,
             recall_snapshot TEXT,
             output TEXT,
             output_summary TEXT,
             outcome TEXT,
             event_source TEXT NOT NULL DEFAULT 'sdk',
             nomination TEXT,
             priority INTEGER NOT NULL DEFAULT 0,
             distill_state TEXT NOT NULL,
             distill_note TEXT,
             distill_run_id TEXT,
             distill_locked_at TEXT,
             distill_prompt_tokens INTEGER,
             distill_completion_tokens INTEGER
         );
         CREATE TABLE usage_trace (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             trace_id TEXT NOT NULL,
             chunk_id TEXT,
             event TEXT NOT NULL,
             strength REAL,
             similarity REAL,
             tokens INTEGER,
             rank INTEGER,
             refine_mode TEXT,
             source TEXT NOT NULL DEFAULT 'sdk',
             ts TEXT NOT NULL
         );",
    )
    .unwrap();
    drop(conn);

    let applied = crate::migrate::run_migrations(file.path()).unwrap();
    assert_eq!(applied, vec!["4.5.1→4.5.2", "4.5.2→4.6"]);

    let conn = rusqlite::Connection::open(file.path()).unwrap();
    let has_column: bool = conn
        .prepare("PRAGMA table_info(episodic_log)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .filter_map(|row| row.ok())
        .any(|name| name == "distill_accounted_at");
    assert!(has_column);
    let version: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, "4.6");
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

#[test]
fn recall_refreshes_vector_cache_after_external_write() {
    let file = NamedTempFile::new().unwrap();
    let reader = KnowledgeBase::open(file.path()).unwrap();
    reader
        .add("cache warmup", "note", None, None, "manual", None)
        .unwrap();
    reader
        .recall(
            "cache warmup",
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

    let writer = KnowledgeBase::open(file.path()).unwrap();
    let external_id = writer
        .add(
            "knowledge written by another process",
            "note",
            Some("knowledge written by another process"),
            None,
            "manual",
            None,
        )
        .unwrap();

    let result = reader
        .recall(
            "knowledge written by another process",
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
    assert!(result
        .knowledge
        .iter()
        .any(|chunk| chunk["id"].as_str() == Some(external_id.as_str())));
}

#[test]
fn vector_search_with_zero_limit_returns_empty() {
    let (kb, _file) = tmp_kb();
    kb.add("zero limit", "note", None, None, "manual", None)
        .unwrap();

    let result = kb.storage.search_vec_content(&vec![0.0; 1024], 0).unwrap();
    assert!(result.is_empty());
}
