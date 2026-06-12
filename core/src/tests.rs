use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

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

    for i in 0..2 {
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
        let actor = format!("tester-{i}");
        kb.record_detailed(
            &recall.trace_id,
            None,
            None,
            Some("retry policy was unsuitable"),
            Some("fail"),
            Some(&used),
            "explicit",
            true,
            None,
            Some(&used),
            "user",
            Some(&actor),
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

struct CountingFailingDistiller {
    calls: Arc<AtomicUsize>,
}

impl Distiller for CountingFailingDistiller {
    fn distill(&self, _log_entries: &[Value]) -> Result<Vec<DistilledChunk>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(InnateError::Other("persistent model failure".to_string()))
    }
}

struct ContextAwareDistiller {
    related_counts: Arc<Mutex<Vec<usize>>>,
}

impl Distiller for ContextAwareDistiller {
    fn distill(&self, log_entries: &[Value]) -> Result<Vec<DistilledChunk>> {
        Ok(log_entries
            .iter()
            .filter_map(|log| {
                log["id"].as_str().map(|id| DistilledChunk {
                    content: "fallback".to_string(),
                    trigger_desc: None,
                    anti_trigger_desc: None,
                    source_log_id: id.to_string(),
                    nomination: None,
                })
            })
            .collect())
    }

    fn distill_with_context(
        &self,
        primary: &Value,
        related_logs: &[Value],
    ) -> Result<Vec<DistilledChunk>> {
        let primary_id = primary["id"].as_str().unwrap();
        let related_count = related_logs
            .iter()
            .filter(|log| log["id"].as_str() != Some(primary_id))
            .count();
        self.related_counts.lock().unwrap().push(related_count);
        Ok(vec![DistilledChunk {
            content: format!("context for {primary_id}"),
            trigger_desc: None,
            anti_trigger_desc: None,
            source_log_id: primary_id.to_string(),
            nomination: None,
        }])
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

    // evolve() now returns Ok even when distillation fails; failures are
    // recorded in episodic_log and reported to stderr for bounded retry.
    kb.evolve("manual").unwrap();
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(log["distill_state"].as_str(), Some("failed"));
    assert_eq!(
        log["distill_note"].as_str(),
        Some("distill_failed:model offline")
    );
}

#[test]
fn distillation_receives_related_logs_without_losing_failure_isolation() {
    let file = NamedTempFile::new().unwrap();
    let related_counts = Arc::new(Mutex::new(Vec::new()));
    let kb = KnowledgeBase::open_with(
        file.path(),
        None,
        None,
        Some(Arc::new(ContextAwareDistiller {
            related_counts: Arc::clone(&related_counts),
        })),
        None,
        None,
    )
    .unwrap();
    for query in ["first pattern", "second pattern"] {
        kb.record(
            &crate::utils::gen_uuid(),
            Some(query),
            None,
            Some("reusable material"),
            Some("ok"),
            None,
            None,
            None,
            None,
            0,
            "sdk",
        )
        .unwrap();
    }

    kb.evolve("manual").unwrap();

    assert_eq!(*related_counts.lock().unwrap(), vec![1, 1]);
}

#[test]
fn multi_chunk_distiller_produces_multiple_chunks() {
    // P2-D fix: distill_batch supports N >= 1 chunks per log.
    // A distiller returning 2 chunks from one log should insert both as pending
    // and mark the log as 'distilled', not 'failed'.
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
    assert_eq!(result["distilled"].as_u64(), Some(1), "log should count as 1 distilled");
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(log["distill_state"].as_str(), Some("distilled"),
        "log must be 'distilled', not 'failed'");
    let chunk_count = kb.storage
        .query_chunks("SELECT COUNT(*) AS cnt FROM chunks WHERE origin='distilled'")
        .unwrap()[0]["cnt"]
        .as_i64()
        .unwrap();
    assert_eq!(chunk_count, 2, "both distilled chunks must be inserted");
}

#[test]
fn multiple_chunks_can_share_same_distilled_from() {
    // P2-D: the unique constraint on distilled_from was dropped in v4.11 to allow
    // multi-chunk distillation. Verify multiple chunks can coexist with the same source log.
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
    let log_id = log["id"].as_str().unwrap().to_string();
    let existing_chunk = kb
        .add("existing chunk", "note", None, None, "manual", None)
        .unwrap();
    // Both the existing chunk and any new chunk can share the same distilled_from.
    kb.storage
        .conn_execute(
            "UPDATE chunks SET distilled_from=? WHERE id=?",
            rusqlite::params![log_id, existing_chunk],
        )
        .unwrap();

    // The unique constraint is gone — distillation should succeed and produce a second chunk.
    let result = kb.evolve("manual").unwrap();
    assert_eq!(result["distilled"].as_u64(), Some(1),
        "distillation must succeed even when a chunk with the same distilled_from already exists");
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(log["distill_state"].as_str(), Some("distilled"));
    let count = kb.storage.query_chunks_params(
        "SELECT COUNT(*) AS cnt FROM chunks WHERE distilled_from=?",
        rusqlite::params![log_id],
    ).unwrap()[0]["cnt"].as_i64().unwrap();
    assert_eq!(count, 2, "both the pre-existing and new chunk must coexist");
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
         );
         CREATE TABLE chunks (
             id TEXT PRIMARY KEY,
             content TEXT NOT NULL DEFAULT '',
             content_hash TEXT NOT NULL DEFAULT '',
             origin TEXT NOT NULL DEFAULT 'captured',
             state TEXT NOT NULL DEFAULT 'active',
             confidence REAL NOT NULL DEFAULT 0.5,
             protected INTEGER NOT NULL DEFAULT 0,
             used_count INTEGER NOT NULL DEFAULT 0,
             used_success_count INTEGER NOT NULL DEFAULT 0,
             selected_count INTEGER NOT NULL DEFAULT 0,
             created_at TEXT NOT NULL DEFAULT '1970-01-01T00:00:00.000Z',
             updated_at TEXT NOT NULL DEFAULT '1970-01-01T00:00:00.000Z',
             distilled_from TEXT
         );
         CREATE UNIQUE INDEX IF NOT EXISTS idx_chunks_distilled_from ON chunks(distilled_from) WHERE distilled_from IS NOT NULL;",
    )
    .unwrap();
    drop(conn);

    let applied = crate::migrate::run_migrations(file.path()).unwrap();
    assert_eq!(applied, vec!["4.5.1→4.5.2", "4.5.2→4.6", "4.6→4.7", "4.7→4.8",
        "4.8→4.9", "4.9→4.10", "4.10→4.11", "4.11→4.12", "4.12→4.13"]);

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
    assert_eq!(version, "4.13");
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

#[test]
fn governance_proposal_triggers_curate_archive() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add("review-target content", "note", Some("review trigger"), None, "manual", None)
        .unwrap();

    // 3 traces with negative feedback: evidence_count reaches governance_archive_threshold (3).
    // 1st down: negative_count=1 → no proposal yet.
    // 2nd down: negative_count=2 → proposal created with evidence_count=2.
    // 3rd down: negative_count=3 → proposal upserted with evidence_count=3.
    for actor in ["reviewer-1", "reviewer-2", "reviewer-3"] {
        record_down_as(&kb, &chunk_id, actor);
    }

    let proposals = kb
        .storage
        .query_chunks_params(
            "SELECT evidence_count, state FROM governance_proposals WHERE chunk_id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap();
    assert_eq!(proposals[0]["evidence_count"].as_i64(), Some(3));
    assert_eq!(proposals[0]["state"].as_str(), Some("pending"));

    let report = kb.builtin_curate_impl(&CurateScope::default()).unwrap();
    assert!(report.archived.contains(&chunk_id));

    let chunk = kb.storage.get_chunk(&chunk_id).unwrap().unwrap();
    assert_eq!(chunk["state"].as_str(), Some("archived"));
    assert_eq!(chunk["state_reason"].as_str(), Some("governance_proposal"));

    let proposals_after = kb
        .storage
        .query_chunks_params(
            "SELECT state FROM governance_proposals WHERE chunk_id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap();
    assert_eq!(proposals_after[0]["state"].as_str(), Some("accepted"));
}

#[test]
fn sustained_negative_feedback_triggers_curate_archive() {
    use crate::utils::{gen_uuid, utc_now_iso};

    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add("negative-feedback target", "note", Some("nf trigger"), None, "manual", None)
        .unwrap();

    // Insert 5 negative feedback events directly to isolate step 3e from step 3d.
    let now = utc_now_iso();
    for index in 0..5 {
        let actor = format!("reviewer-{index}");
        kb.storage
            .insert_feedback_event(
                &gen_uuid(),
                &gen_uuid(),
                &chunk_id,
                "down",
                1.0,
                "sdk",
                Some(&actor),
                None,
                None,
                &now,
            )
            .unwrap();
    }
    kb.storage
        .upsert_governance_proposal(
            &gen_uuid(),
            &chunk_id,
            "review_applicability",
            "weighted negative feedback",
            5,
            5.0,
            5,
            &now,
        )
        .unwrap();

    let report = kb.builtin_curate_impl(&CurateScope::default()).unwrap();
    assert!(report.archived.contains(&chunk_id));

    let chunk = kb.storage.get_chunk(&chunk_id).unwrap().unwrap();
    assert_eq!(chunk["state"].as_str(), Some("archived"));
    assert_eq!(chunk["state_reason"].as_str(), Some("governance_proposal"));
}

#[test]
fn governance_pending_enqueues_evolve_request() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let kb = KnowledgeBase::open(file.path()).unwrap();
    // Lower the threshold so a single pending proposal triggers evolve.
    kb.storage
        .set_meta("evolve.governance_pending_threshold", "1")
        .unwrap();
    drop(kb);

    let kb = KnowledgeBase::open(file.path()).unwrap();
    let chunk_id = kb
        .add("governance-evolve target", "note", Some("ge trigger"), None, "manual", None)
        .unwrap();

    // 2 downs create a governance proposal → enqueue_evolve_if_needed fires on 2nd record().
    for actor in ["reviewer-1", "reviewer-2"] {
        record_down_as(&kb, &chunk_id, actor);
    }

    let requests = kb
        .storage
        .query_chunks(
            "SELECT reason FROM evolve_requests WHERE state='pending' AND reason='governance'",
        )
        .unwrap();
    assert!(!requests.is_empty(), "governance evolve_request should be enqueued");
}

#[test]
fn selected_unused_always_decreases_confidence() {
    let (kb, _file) = tmp_kb();
    // Start chunk at low confidence (below old target of 0.3).
    let chunk_id = kb
        .add("low-conf chunk", "note", Some("lc trigger"), None, "manual", None)
        .unwrap();
    kb.storage
        .update_chunk_confidence(&chunk_id, 0.15, Some("test_setup"), &crate::utils::utc_now_iso())
        .unwrap();

    // record() with a used declaration that excludes chunk_id proves it was selected but unused.
    let trace = attributed_trace(&kb, &chunk_id);
    // An explicit complete empty declaration proves the selected chunk was unused.
    kb.record(
        &trace,
        Some("query"),
        None,
        None,
        Some("ok"),
        Some(&[]),
        None,
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();

    let chunk = kb.storage.get_chunk(&chunk_id).unwrap().unwrap();
    let conf_after = chunk["confidence"].as_f64().unwrap();
    assert!(conf_after < 0.15, "confidence should decrease, got {conf_after}");
}

// ── v4.8 feedback loop fix tests ──────────────────────────────────────────────

#[test]
fn distilled_chunk_can_auto_promote_via_implicit_signals() {
    // Verifies that a distilled chunk (conf=0.55, pending) can cross the 0.60
    // promote threshold using only implicit outcome=ok signals (no feedback_up).
    use crate::refine::{DistilledChunk, Distiller};
    struct ImmediateDistiller;
    impl Distiller for ImmediateDistiller {
        fn distill(&self, logs: &[serde_json::Value]) -> crate::errors::Result<Vec<DistilledChunk>> {
            Ok(logs.iter().map(|l| DistilledChunk {
                content: "test principle from distillation".to_string(),
                trigger_desc: Some("test trigger".to_string()),
                anti_trigger_desc: None,
                source_log_id: l["id"].as_str().unwrap_or("").to_string(),
                nomination: None,
            }).collect())
        }
    }

    let f = NamedTempFile::new().unwrap();
    let kb = KnowledgeBase::open_with(
        f.path(),
        None, None,
        Some(std::sync::Arc::new(ImmediateDistiller)),
        None, None,
    ).unwrap();

    // Produce a distilled chunk: record → evolve → chunk in pending state.
    let trace_id = crate::utils::gen_uuid();
    kb.record(&trace_id, Some("test query"), None, Some("test summary"),
              Some("ok"), None, None, None, None, 0, "sdk").unwrap();
    kb.evolve("manual").unwrap();

    let chunks = kb.storage.query_chunks_params(
        "SELECT id, confidence, state FROM chunks WHERE origin='distilled'",
        rusqlite::params![],
    ).unwrap();
    assert_eq!(chunks.len(), 1, "should have one distilled chunk");
    let chunk_id = chunks[0]["id"].as_str().unwrap().to_string();
    let initial_conf = chunks[0]["confidence"].as_f64().unwrap();
    assert!((initial_conf - 0.55).abs() < 0.01, "initial conf should be 0.55, got {initial_conf}");
    assert_eq!(chunks[0]["state"].as_str(), Some("pending"));

    // Simulate 5 successful uses across distinct traces to push confidence over 0.60.
    for i in 0..5 {
        let t = attributed_trace(&kb, &chunk_id);
        // Record usage: used=[chunk_id], outcome=ok — triggers implicit confidence nudge.
        kb.record(&t, Some(&format!("query variant {i}")),
                  None, None, Some("ok"), Some(&[chunk_id.clone()]),
                  None, None, None, 0, "sdk").unwrap();
    }

    let after = kb.storage.get_chunk(&chunk_id).unwrap().unwrap();
    let conf_after = after["confidence"].as_f64().unwrap();
    assert!(conf_after > 0.60,
        "confidence should exceed promote threshold 0.60 after 5 implicit ok signals, got {conf_after}");
}

#[test]
fn decay_floor_enables_archive_by_confidence() {
    // Verifies that the lower decay floor (0.20 < archive threshold 0.25) allows
    // curate 3a to actually archive idle low-confidence chunks.
    use crate::utils::utc_now_iso;

    let (kb, _file) = tmp_kb();
    let chunk_id = kb.add("idle chunk", "note", Some("idle trigger"), None, "manual", None).unwrap();

    // Manually set confidence below the new decay floor but above the old floor.
    // conf = 0.22 → below archive threshold (0.25) → 3a should archive it.
    let now = utc_now_iso();
    kb.storage.update_chunk_confidence(&chunk_id, 0.22, Some("test"), &now).unwrap();
    // Set last_used_at to simulate 90+ days idle (cutoff = 60 days).
    kb.storage.query_chunks_params(
        "UPDATE chunks SET last_used_at=?, last_used_base=? WHERE id=?",
        rusqlite::params![
            "2020-01-01T00:00:00.000Z",
            "2020-01-01T00:00:00.000Z",
            chunk_id
        ],
    ).unwrap();

    let report = kb.builtin_curate_impl(&crate::kb::CurateScope::default()).unwrap();
    assert!(report.archived.contains(&chunk_id),
        "conf=0.22 idle chunk should be archived via 3a; archived={:?}", report.archived);

    let chunk = kb.storage.get_chunk(&chunk_id).unwrap().unwrap();
    assert_eq!(chunk["state"].as_str(), Some("archived"));
    assert_eq!(chunk["state_reason"].as_str(), Some("low_confidence"));
}

#[test]
fn governance_ready_triggers_immediate_evolve() {
    // Verifies that 3 downs on a single chunk (evidence_count reaches threshold)
    // causes enqueue_evolve_if_needed to enqueue "governance_ready" immediately,
    // even without 3 different pending proposals.
    let (kb, _file) = tmp_kb();
    let chunk_id = kb.add("governance-ready target", "note", Some("gr trigger"), None, "manual", None).unwrap();

    // 3 feedback_down events: 2nd creates proposal, 3rd updates evidence_count to 3.
    for actor in ["reviewer-1", "reviewer-2", "reviewer-3"] {
        record_down_as(&kb, &chunk_id, actor);
    }

    // Should have exactly one evolve_request with reason='governance_ready'.
    let requests = kb.storage.query_chunks_params(
        "SELECT reason FROM evolve_requests WHERE state='pending' AND reason='governance_ready'",
        rusqlite::params![],
    ).unwrap();
    assert!(!requests.is_empty(),
        "governance_ready evolve_request should be enqueued after 3 downs on one chunk");
}

// ── v4.9 feedback-loop fixes ─────────────────────────────────────────────────

#[test]
fn feedback_events_are_idempotent() {
    // Issue 1 fix: same (trace_id, chunk_id, signal) must not insert duplicate rows.
    let (kb, _file) = tmp_kb();
    let chunk_id = kb.add("idempotent target", "note", Some("t"), None, "manual", None).unwrap();
    let trace_id = attributed_trace(&kb, &chunk_id);

    // Two record() calls with the same trace_id, same chunk, same signal.
    kb.record(&trace_id, Some("q"), None, None, None,
              None, None, Some(&[chunk_id.clone()]), None, 0, "sdk").unwrap();
    kb.record(&trace_id, Some("q"), None, None, None,
              None, None, Some(&[chunk_id.clone()]), None, 0, "sdk").unwrap();

    let count = kb.storage.query_chunks_params(
        "SELECT COUNT(*) AS cnt FROM feedback_events
         WHERE trace_id=? AND chunk_id=? AND signal='down'",
        rusqlite::params![trace_id, chunk_id],
    ).unwrap();
    assert_eq!(count[0]["cnt"].as_i64(), Some(1),
        "duplicate (trace_id, chunk_id, signal) must produce only one row");
}

#[test]
fn positive_feedback_offsets_governance_proposal() {
    // Issue 2 fix: net_negative = downs - ups < 2 → no governance proposal.
    let (kb, _file) = tmp_kb();
    let chunk_id = kb.add("net feedback target", "note", Some("t"), None, "manual", None).unwrap();

    // Two downs.
    for _ in 0..2 {
        let t = attributed_trace(&kb, &chunk_id);
        kb.record(&t, Some("q"), None, None, None,
                  None, None, Some(&[chunk_id.clone()]), None, 0, "sdk").unwrap();
    }
    // One up — offsets one down, net = 1 < 2.
    let t = attributed_trace(&kb, &chunk_id);
    kb.record(&t, Some("q"), None, None, None,
              None, Some(&[chunk_id.clone()]), None, None, 0, "sdk").unwrap();

    let proposals = kb.storage.query_chunks_params(
        "SELECT COUNT(*) AS cnt FROM governance_proposals WHERE chunk_id=? AND state='pending'",
        rusqlite::params![chunk_id],
    ).unwrap();
    assert_eq!(proposals[0]["cnt"].as_i64(), Some(0),
        "positive feedback should cancel the pending governance proposal (net_negative=1 < 2)");
}

#[test]
fn governance_proposal_rejected_for_protected_chunk() {
    // Issue 6 fix: protected chunks must have their proposal state='rejected' after curate,
    // not 'accepted', since they are never actually archived.
    let (kb, _file) = tmp_kb();
    // kind="skill" → protected=1.
    let chunk_id = kb.add("protected knowledge", "skill", Some("t"), None, "manual", None).unwrap();

    // 3 downs → upsert proposal with evidence_count=3 (= governance_archive_threshold).
    for actor in ["reviewer-1", "reviewer-2", "reviewer-3"] {
        record_down_as(&kb, &chunk_id, actor);
    }

    kb.evolve("manual").unwrap();

    let chunk_rows = kb.storage.query_chunks_params(
        "SELECT state FROM chunks WHERE id=?",
        rusqlite::params![chunk_id],
    ).unwrap();
    assert_eq!(chunk_rows[0]["state"].as_str(), Some("active"),
        "protected chunk must not be archived by governance");

    let proposal_rows = kb.storage.query_chunks_params(
        "SELECT state FROM governance_proposals WHERE chunk_id=?",
        rusqlite::params![chunk_id],
    ).unwrap();
    assert_eq!(proposal_rows[0]["state"].as_str(), Some("rejected"),
        "proposal for protected chunk must be rejected, not accepted");
}

// ── v4.10 flywheel reliability fixes ─────────────────────────────────────────

#[test]
fn feedback_derived_updates_skipped_on_duplicate() {
    // Issue 1 fix: derived confidence/context updates must not run when INSERT OR IGNORE skips
    // the row (same trace_id + chunk_id + signal already exists).
    let (kb, _file) = tmp_kb();
    let chunk_id = kb.add("dup feedback target", "note", Some("t"), None, "manual", None).unwrap();
    let initial_conf = kb.storage.query_chunks_params(
        "SELECT confidence FROM chunks WHERE id=?", rusqlite::params![chunk_id],
    ).unwrap()[0]["confidence"].as_f64().unwrap();

    let trace_id = attributed_trace(&kb, &chunk_id);
    // Two down calls with the same trace_id — only first should update confidence.
    kb.record(&trace_id, Some("q"), None, None, None,
              None, None, Some(&[chunk_id.clone()]), None, 0, "sdk").unwrap();
    let conf_after_first = kb.storage.query_chunks_params(
        "SELECT confidence FROM chunks WHERE id=?", rusqlite::params![chunk_id],
    ).unwrap()[0]["confidence"].as_f64().unwrap();
    assert!(conf_after_first < initial_conf, "first down must lower confidence");

    kb.record(&trace_id, Some("q"), None, None, None,
              None, None, Some(&[chunk_id.clone()]), None, 0, "sdk").unwrap();
    let conf_after_second = kb.storage.query_chunks_params(
        "SELECT confidence FROM chunks WHERE id=?", rusqlite::params![chunk_id],
    ).unwrap()[0]["confidence"].as_f64().unwrap();
    assert_eq!(
        (conf_after_first * 1000.0).round(),
        (conf_after_second * 1000.0).round(),
        "duplicate feedback must not further change confidence"
    );
}

#[test]
fn outcome_without_used_falls_back_to_saved_used_ids() {
    // Issue 2 fix: if outcome is reported separately from used, confidence must still update
    // for the chunks saved in the first call's used_ids.
    let (kb, _file) = tmp_kb();
    let chunk_id = kb.add("two-call target", "note", Some("t"), None, "manual", None).unwrap();
    let trace_id = attributed_trace(&kb, &chunk_id);

    // Step 1: report used, no outcome.
    kb.record(&trace_id, Some("q"), None, None, None,
              Some(&[chunk_id.clone()]), None, None, None, 0, "sdk").unwrap();
    let conf_before = kb.storage.query_chunks_params(
        "SELECT confidence FROM chunks WHERE id=?", rusqlite::params![chunk_id],
    ).unwrap()[0]["confidence"].as_f64().unwrap();

    // Step 2: report outcome, no used — should still update confidence for the saved chunk.
    kb.record(&trace_id, Some("q"), None, None, Some("ok"),
              None, None, None, None, 0, "sdk").unwrap();
    let conf_after = kb.storage.query_chunks_params(
        "SELECT confidence FROM chunks WHERE id=?", rusqlite::params![chunk_id],
    ).unwrap()[0]["confidence"].as_f64().unwrap();
    assert!(conf_after > conf_before,
        "ok outcome must raise confidence even when used is omitted from the outcome call");
}

#[test]
fn scheduled_evolve_runs_curate_without_pending_request() {
    // Issue 7 fix: scheduled evolve must run curate (decay/archive/promote) even with no
    // pending evolve_request — quiet periods must not freeze time-based maintenance.
    let (kb, _file) = tmp_kb();
    // No record() calls → no evolve_request queued.
    let report = kb.evolve("scheduled").unwrap();
    // curate must be present (not null) even with skipped distillation.
    assert!(
        report.get("curate").map(|v| !v.is_null()).unwrap_or(false),
        "curate must run for scheduled evolve even without a pending request"
    );
    assert_eq!(report["skipped"].as_str(), Some("no_evolve_request"));
}

#[test]
fn threshold_evolve_runs_curate_when_below_distill_threshold() {
    // Issue 8 fix: threshold evolve below distill threshold must still run curate
    // so that governance and time-decay are not suppressed by a low-volume period.
    let (kb, _file) = tmp_kb();
    // Ensure evolve_requests has a pending entry so threshold evolve can claim it.
    kb.storage.request_evolve(&crate::utils::gen_uuid(), "governance_ready", &crate::utils::utc_now_iso()).unwrap();
    let report = kb.evolve("threshold").unwrap();
    assert!(
        report.get("curate").map(|v| !v.is_null()).unwrap_or(false),
        "curate must run for threshold evolve even when below distill threshold"
    );
}

#[test]
fn failed_distill_logs_are_retried_after_cooloff() {
    // Retryable failed logs must be consumed in this evolve cycle, not merely reset
    // after the distillation phase has already passed.
    let (kb, _file) = tmp_kb();
    let old_ts = "2020-01-01T00:00:00.000Z";
    let trace_id = crate::utils::gen_uuid();
    let row = crate::storage::EpisodicLogRow {
        id: crate::utils::gen_uuid(),
        trace_id: trace_id.clone(),
        lib_id: kb.storage.lib_id().unwrap(),
        ts: old_ts.to_string(),
        event_source: "sdk".to_string(),
        task_state: "completed".to_string(),
        usage_state: "unknown".to_string(),
        distill_state: "failed".to_string(),
        distill_note: Some("distill_failed:timeout".to_string()),
        ..Default::default()
    };
    kb.storage.upsert_episodic_log(&row).unwrap();

    kb.evolve("manual").unwrap();

    let state = kb.storage.query_chunks_params(
        "SELECT distill_state FROM episodic_log WHERE trace_id=?",
        rusqlite::params![trace_id],
    ).unwrap();
    assert_eq!(
        state[0]["distill_state"].as_str(),
        Some("discarded"),
        "old failed log must be retried in the same evolve cycle"
    );
}

#[test]
fn distill_retries_are_bounded_and_failures_remain_observable() {
    let file = NamedTempFile::new().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let kb = KnowledgeBase::open_with(
        file.path(),
        None,
        None,
        Some(Arc::new(CountingFailingDistiller {
            calls: Arc::clone(&calls),
        })),
        None,
        None,
    )
    .unwrap();
    let trace_id = crate::utils::gen_uuid();
    kb.record(
        &trace_id,
        Some("persistent failure"),
        None,
        Some("reusable material"),
        Some("ok"),
        None,
        None,
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();

    for _ in 0..3 {
        // evolve() succeeds even on distillation failure; failure details stay in DB.
        kb.evolve("manual").unwrap();
        kb.storage
            .conn_execute(
                "UPDATE episodic_log
                 SET distill_accounted_at='2020-01-01T00:00:00.000Z'
                 WHERE trace_id=?",
                rusqlite::params![trace_id],
            )
            .unwrap();
    }
    kb.evolve("manual").unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(log["distill_state"].as_str(), Some("failed"));
    assert_eq!(log["distill_attempts"].as_i64(), Some(3));
    assert!(log["distill_last_failed_at"].as_str().is_some());
    let inspect = kb.inspect().unwrap();
    assert_eq!(
        inspect["feedback_loop"]["failed_distill_logs_30d"].as_i64(),
        Some(1)
    );
}

#[test]
fn evolve_preserves_replayable_usage_facts_and_counts() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add(
            "evolve count conservation",
            "note",
            Some("count"),
            None,
            "manual",
            None,
        )
        .unwrap();
    let trace_id = attributed_trace(&kb, &chunk_id);
    kb.record(
        &trace_id,
        None,
        None,
        Some("completed"),
        Some("ok"),
        Some(&[chunk_id.clone()]),
        None,
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();
    kb.storage
        .conn_execute(
            "UPDATE episodic_log SET distill_state='discarded' WHERE trace_id=?",
            rusqlite::params![trace_id],
        )
        .unwrap();

    kb.evolve("manual").unwrap();
    kb.builtin_curate_impl(&CurateScope::default()).unwrap();

    let chunk = kb.storage.get_chunk(&chunk_id).unwrap().unwrap();
    assert_eq!(chunk["selected_count"].as_i64(), Some(1));
    assert_eq!(chunk["used_count"].as_i64(), Some(1));
    assert_eq!(chunk["used_success_count"].as_i64(), Some(1));
    let retained = kb
        .storage
        .query_chunks_params(
            "SELECT COUNT(*) AS cnt FROM usage_trace
             WHERE trace_id=? AND event IN ('selected','used','task_ok')",
            rusqlite::params![trace_id],
        )
        .unwrap();
    assert_eq!(retained[0]["cnt"].as_i64(), Some(3));
}

#[test]
fn feedback_can_be_corrected_after_successful_evolve() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add(
            "post evolve correction",
            "note",
            Some("correction"),
            None,
            "manual",
            None,
        )
        .unwrap();
    let initial = kb.storage.get_chunk(&chunk_id).unwrap().unwrap()["confidence"]
        .as_f64()
        .unwrap();
    let trace_id = attributed_trace(&kb, &chunk_id);
    kb.record(
        &trace_id,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(&[chunk_id.clone()]),
        None,
        0,
        "sdk",
    )
    .unwrap();
    kb.storage
        .conn_execute(
            "UPDATE episodic_log SET distill_state='discarded' WHERE trace_id=?",
            rusqlite::params![trace_id],
        )
        .unwrap();

    kb.evolve("manual").unwrap();
    kb.record(
        &trace_id,
        None,
        None,
        None,
        None,
        None,
        Some(&[chunk_id.clone()]),
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();

    let chunk = kb.storage.get_chunk(&chunk_id).unwrap().unwrap();
    assert_eq!(chunk["confidence_base"].as_f64(), Some(initial));
    assert!(chunk["confidence"].as_f64().unwrap() > initial);
    let evidence = kb
        .storage
        .query_chunks_params(
            "SELECT kind FROM confidence_evidence WHERE trace_id=? AND chunk_id=?",
            rusqlite::params![trace_id, chunk_id],
        )
        .unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0]["kind"].as_str(), Some("feedback_up"));
}

#[test]
fn completed_trace_without_outcome_reaches_a_terminal_distill_state() {
    let (kb, _file) = tmp_kb();
    let trace_id = crate::utils::gen_uuid();
    kb.record_detailed(
        &trace_id,
        Some("completed without outcome"),
        None,
        Some("reusable material"),
        None,
        None,
        "explicit",
        true,
        None,
        None,
        "user",
        None,
        None,
        None,
        0,
        Some("completed"),
        "sdk",
    )
    .unwrap();

    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(log["task_state"].as_str(), Some("completed"));
    assert_eq!(log["distill_state"].as_str(), Some("new"));
    assert!(log["completed_at"].as_str().is_some());
}

#[test]
fn usage_annotation_rate_excludes_non_completed_traces() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add("metric attribution", "note", None, None, "manual", None)
        .unwrap();
    let running_trace = attributed_trace(&kb, &chunk_id);
    kb.record_detailed(
        &running_trace,
        None,
        None,
        None,
        None,
        Some(&[chunk_id]),
        "explicit",
        true,
        None,
        None,
        "user",
        None,
        None,
        None,
        0,
        Some("running"),
        "sdk",
    )
    .unwrap();
    kb.record(
        &crate::utils::gen_uuid(),
        Some("completed"),
        None,
        None,
        Some("ok"),
        None,
        None,
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();

    let inspect = kb.inspect().unwrap();
    assert_eq!(
        inspect["feedback_loop"]["usage_annotation_rate"].as_f64(),
        Some(0.0)
    );
}

#[test]
fn migration_4_12_baselines_exclude_retained_usage_facts() {
    let file = NamedTempFile::new().unwrap();
    let conn = rusqlite::Connection::open(file.path()).unwrap();
    conn.execute_batch(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO meta VALUES ('schema_version', '4.11');
         CREATE TABLE chunks (
           id TEXT PRIMARY KEY,
           confidence REAL NOT NULL,
           selected_count INTEGER NOT NULL,
           used_count INTEGER NOT NULL,
           used_success_count INTEGER NOT NULL,
           last_used_at TEXT
         );
         INSERT INTO chunks VALUES
           ('c1', 0.7, 5, 4, 3, '2026-01-01T00:00:00.000Z');
         CREATE TABLE usage_trace (
           trace_id TEXT NOT NULL,
           chunk_id TEXT,
           event TEXT NOT NULL
         );
         INSERT INTO usage_trace VALUES
           ('t1', 'c1', 'selected'),
           ('t2', 'c1', 'selected'),
           ('t1', 'c1', 'used'),
           ('t1', NULL, 'task_ok');
         CREATE TABLE episodic_log (trace_id TEXT, outcome TEXT);
         INSERT INTO episodic_log VALUES ('t1', 'ok');
         CREATE TABLE feedback_events (
           chunk_id TEXT, context_key TEXT, signal TEXT
         );
         CREATE TABLE chunk_context_stats (
           chunk_id TEXT, context_key TEXT,
           success_count INTEGER, failure_count INTEGER,
           positive_feedback INTEGER, negative_feedback INTEGER
         );
         CREATE TABLE governance_proposals (id TEXT);
         CREATE TABLE evolve_requests (
           id TEXT, reason TEXT, state TEXT, requested_at TEXT
         );",
    )
    .unwrap();
    drop(conn);

    assert_eq!(
        crate::migrate::run_migrations(file.path()).unwrap(),
        vec!["4.11→4.12", "4.12→4.13"]
    );
    let conn = rusqlite::Connection::open(file.path()).unwrap();
    let values = conn
        .query_row(
            "SELECT selected_count_base, used_count_base,
                    used_success_count_base, last_used_base
             FROM chunks WHERE id='c1'",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(values.0, 3);
    assert_eq!(values.1, 3);
    assert_eq!(values.2, 2);
    assert_eq!(values.3, "2026-01-01T00:00:00.000Z");
}

#[test]
fn decay_uses_last_decayed_at_for_incremental_delta() {
    // Issue 6 fix: two successive curate runs must not compound decay beyond the 90-day half-life.
    // If last_decayed_at is updated after first decay, the second run decays only the
    // incremental days since then, not the full idle period again.
    let (kb, _file) = tmp_kb();
    let chunk_id = kb.add("decay test", "note", Some("t"), None, "manual", None).unwrap();
    // Simulate last_used_at 180 days ago (full 2× half-life idle).
    let old_ts = "2020-01-01T00:00:00.000Z";
    kb.storage.conn_execute(
        "UPDATE chunks SET last_used_at=? WHERE id=?",
        rusqlite::params![old_ts, chunk_id],
    ).unwrap();

    // First evolve: applies 180-day decay.
    kb.evolve("manual").unwrap();
    let conf1 = kb.storage.query_chunks_params(
        "SELECT confidence FROM chunks WHERE id=?", rusqlite::params![chunk_id],
    ).unwrap()[0]["confidence"].as_f64().unwrap();

    // Second evolve (same day): last_decayed_at is today, delta_days ≈ 0 → no further decay.
    kb.evolve("manual").unwrap();
    let conf2 = kb.storage.query_chunks_params(
        "SELECT confidence FROM chunks WHERE id=?", rusqlite::params![chunk_id],
    ).unwrap()[0]["confidence"].as_f64().unwrap();

    assert!(
        (conf1 - conf2).abs() < 0.01,
        "second curate on same day must not compound decay: conf1={conf1:.3} conf2={conf2:.3}"
    );
}

#[test]
fn sustaining_failing_chunk_gets_archived() {
    // Issue 5 fix: chunks frequently used but rarely succeeding (and low confidence) must be
    // archived via 3f rather than surviving indefinitely via last_used_at refresh.
    let (kb, _file) = tmp_kb();
    let chunk_id = kb.add("always fails", "note", Some("t"), None, "manual", None).unwrap();
    // Force used_count=10, used_success_count=1 (10% success), confidence=0.22 (< 0.25 threshold).
    kb.storage.conn_execute(
        "UPDATE chunks SET used_count=10, used_count_base=10,
         used_success_count=1, used_success_count_base=1,
         confidence=0.22, confidence_base=0.22, \
         last_used_at=datetime('now') WHERE id=?",
        rusqlite::params![chunk_id],
    ).unwrap();

    kb.evolve("manual").unwrap();

    let state = kb.storage.query_chunks_params(
        "SELECT state, state_reason FROM chunks WHERE id=?",
        rusqlite::params![chunk_id],
    ).unwrap();
    assert_eq!(state[0]["state"].as_str(), Some("archived"));
    assert_eq!(state[0]["state_reason"].as_str(), Some("sustained_task_failure"));
}

#[test]
fn pending_chunk_with_sustained_failure_gets_archived() {
    // P1-A fix: curate 3f now covers state='pending' in addition to state='active'.
    // A pending chunk recalled frequently but never producing successes must be archived.
    let (kb, _file) = tmp_kb();
    let chunk_id = kb.add("pending always fails", "note", Some("t"), None, "manual", None).unwrap();
    // Move to pending state (simulate distill output) and inject bad stats.
    kb.storage.conn_execute(
        "UPDATE chunks SET state='pending', used_count=10, used_count_base=10,
         used_success_count=0, used_success_count_base=0,
         confidence=0.22, confidence_base=0.22, \
         last_used_at=datetime('now') WHERE id=?",
        rusqlite::params![chunk_id],
    ).unwrap();

    kb.evolve("manual").unwrap();

    let rows = kb.storage.query_chunks_params(
        "SELECT state, state_reason FROM chunks WHERE id=?",
        rusqlite::params![chunk_id],
    ).unwrap();
    assert_eq!(rows[0]["state"].as_str(), Some("archived"),
        "pending chunk with sustained failure must be archived by curate 3f");
    assert_eq!(rows[0]["state_reason"].as_str(), Some("sustained_task_failure"));
}

#[test]
fn stale_governance_proposal_expires() {
    // P2-B fix: proposals with insufficient evidence older than governance_proposal_max_age_days
    // must be rejected so they stop triggering repeated evolve cycles.
    use crate::utils::gen_uuid;

    let (kb, _file) = tmp_kb();
    let chunk_id = kb.add("controversial chunk", "note", Some("t"), None, "manual", None).unwrap();
    let proposal_id = gen_uuid();
    // Insert a pending proposal with only 1 evidence (< governance_archive_threshold=3),
    // created 60 days ago (> governance_proposal_max_age_days=30).
    kb.storage.conn_execute(
        "INSERT INTO governance_proposals(id, chunk_id, proposal_type, reason, evidence_count, state, created_at, updated_at)
         VALUES (?, ?, 'archive', 'test', 1, 'pending', '2020-01-01T00:00:00.000Z', '2020-01-01T00:00:00.000Z')",
        rusqlite::params![proposal_id, chunk_id],
    ).unwrap();

    kb.evolve("manual").unwrap();

    let rows = kb.storage.query_chunks_params(
        "SELECT state FROM governance_proposals WHERE id=?",
        rusqlite::params![proposal_id],
    ).unwrap();
    assert_eq!(rows[0]["state"].as_str(), Some("rejected"),
        "stale low-evidence proposal must be expired (rejected)");
    // The chunk itself must NOT be archived since the proposal lacked evidence.
    let chunk_rows = kb.storage.query_chunks_params(
        "SELECT state FROM chunks WHERE id=?", rusqlite::params![chunk_id],
    ).unwrap();
    assert_ne!(chunk_rows[0]["state"].as_str(), Some("archived"),
        "chunk must not be archived when proposal expires without sufficient evidence");
}

#[test]
fn old_evolve_requests_are_pruned() {
    // P2-C fix: completed evolve_requests older than 30 days must be deleted by curate
    // to prevent the table from growing unboundedly and slowing COUNT(*) queries.
    use crate::utils::gen_uuid;

    let (kb, _file) = tmp_kb();
    // Insert a 'completed' request from 60 days ago.
    kb.storage.conn_execute(
        "INSERT INTO evolve_requests(id, reason, state, requested_at, completed_at)
         VALUES (?, 'threshold', 'completed', '2020-01-01T00:00:00.000Z', '2020-01-01T00:00:00.000Z')",
        rusqlite::params![gen_uuid()],
    ).unwrap();

    kb.evolve("manual").unwrap();

    let count = kb.storage.query_chunks(
        "SELECT COUNT(*) AS cnt FROM evolve_requests WHERE state='completed' AND requested_at < '2021-01-01T00:00:00.000Z'"
    ).unwrap()[0]["cnt"].as_i64().unwrap();
    assert_eq!(count, 0, "old completed evolve_requests must be pruned by curate Step 9");
}

#[test]
fn inspect_includes_pending_oldest_ts() {
    // P3-A fix: inspect must include pending_oldest_ts so stale pending debt is visible.
    let (kb, _file) = tmp_kb();
    let chunk_id = kb.add("old pending", "note", Some("t"), None, "manual", None).unwrap();
    kb.storage.conn_execute(
        "UPDATE chunks SET state='pending', created_at='2020-01-01T00:00:00.000Z' WHERE id=?",
        rusqlite::params![chunk_id],
    ).unwrap();

    let info = kb.inspect().unwrap();
    let oldest = &info["chunks"]["pending_oldest_ts"];
    assert!(!oldest.is_null(), "pending_oldest_ts must be present when pending chunks exist");
    assert_eq!(oldest.as_str(), Some("2020-01-01T00:00:00.000Z"),
        "pending_oldest_ts must reflect the oldest pending chunk's created_at");
}

#[test]
fn record_rejects_chunk_not_attributed_to_trace() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add("unattributed", "note", Some("u"), None, "manual", None)
        .unwrap();
    let error = kb
        .record(
            &crate::utils::gen_uuid(),
            Some("q"),
            None,
            None,
            Some("ok"),
            Some(&[chunk_id]),
            None,
            None,
            None,
            0,
            "sdk",
        )
        .unwrap_err();
    assert!(matches!(error, InnateError::InvalidState(_)));
}

#[test]
fn outcome_before_used_closes_the_same_feedback_loop() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add("late usage", "note", Some("late"), None, "manual", None)
        .unwrap();
    let trace_id = attributed_trace(&kb, &chunk_id);
    let initial = kb.storage.get_chunk(&chunk_id).unwrap().unwrap()["confidence"]
        .as_f64()
        .unwrap();

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
        "sdk",
    )
    .unwrap();
    let after_outcome = kb.storage.get_chunk(&chunk_id).unwrap().unwrap()["confidence"]
        .as_f64()
        .unwrap();
    assert_eq!(initial, after_outcome, "unknown usage must not imply unused");

    kb.record(
        &trace_id,
        None,
        None,
        None,
        None,
        Some(&[chunk_id.clone()]),
        None,
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();
    let after_used = kb.storage.get_chunk(&chunk_id).unwrap().unwrap()["confidence"]
        .as_f64()
        .unwrap();
    assert!(after_used > initial);
}

#[test]
fn corrected_used_declaration_replays_confidence_and_counts() {
    let (kb, _file) = tmp_kb();
    let first = kb
        .add("first", "note", Some("first"), None, "manual", None)
        .unwrap();
    let second = kb
        .add("second", "note", Some("second"), None, "manual", None)
        .unwrap();
    let first_initial = kb.storage.get_chunk(&first).unwrap().unwrap()["confidence"]
        .as_f64()
        .unwrap();
    let second_initial = kb.storage.get_chunk(&second).unwrap().unwrap()["confidence"]
        .as_f64()
        .unwrap();
    let trace_id = attributed_trace_many(&kb, &[first.clone(), second.clone()]);

    kb.record(
        &trace_id,
        None,
        None,
        None,
        Some("ok"),
        Some(&[first.clone()]),
        None,
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();
    kb.record(
        &trace_id,
        None,
        None,
        None,
        None,
        Some(&[second.clone()]),
        None,
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();
    kb.builtin_curate_impl(&CurateScope::default()).unwrap();

    let first_row = kb.storage.get_chunk(&first).unwrap().unwrap();
    let second_row = kb.storage.get_chunk(&second).unwrap().unwrap();
    assert_eq!(first_row["used_count"].as_i64(), Some(0));
    assert_eq!(second_row["used_count"].as_i64(), Some(1));
    assert!(first_row["confidence"].as_f64().unwrap() < first_initial);
    assert!(second_row["confidence"].as_f64().unwrap() > second_initial);
}

#[test]
fn partial_used_declaration_does_not_penalize_omitted_chunks() {
    let (kb, _file) = tmp_kb();
    let used = kb
        .add("used", "note", Some("used"), None, "manual", None)
        .unwrap();
    let omitted = kb
        .add("omitted", "note", Some("omitted"), None, "manual", None)
        .unwrap();
    let omitted_initial = kb.storage.get_chunk(&omitted).unwrap().unwrap()["confidence"]
        .as_f64()
        .unwrap();
    let trace_id = attributed_trace_many(&kb, &[used.clone(), omitted.clone()]);

    kb.record_detailed(
        &trace_id,
        None,
        None,
        None,
        Some("ok"),
        Some(&[used]),
        "explicit",
        false,
        None,
        None,
        "user",
        None,
        None,
        None,
        0,
        None,
        "sdk",
    )
    .unwrap();

    let row = kb.storage.get_chunk(&omitted).unwrap().unwrap();
    assert_eq!(row["confidence"].as_f64(), Some(omitted_initial));
    let evidence = kb
        .storage
        .query_chunks_params(
            "SELECT COUNT(*) AS cnt FROM confidence_evidence
             WHERE trace_id=? AND chunk_id=? AND kind='selected_unused'",
            rusqlite::params![trace_id, omitted],
        )
        .unwrap();
    assert_eq!(evidence[0]["cnt"].as_i64(), Some(0));
}

#[test]
fn evolve_requests_preserve_reasons_and_retry_failures() {
    let (kb, _file) = tmp_kb();
    let now = crate::utils::utc_now_iso();
    kb.storage
        .request_evolve(&crate::utils::gen_uuid(), "threshold", &now)
        .unwrap();
    kb.storage
        .request_evolve(&crate::utils::gen_uuid(), "governance_ready", &now)
        .unwrap();
    let pending = kb
        .storage
        .query_chunks("SELECT reason FROM evolve_requests WHERE state='pending'")
        .unwrap();
    assert_eq!(pending.len(), 2);

    let id = kb
        .storage
        .claim_evolve_request(&now, "1970-01-01T00:00:00.000Z")
        .unwrap()
        .unwrap();
    let reason = kb
        .storage
        .query_chunks_params(
            "SELECT reason FROM evolve_requests WHERE id=?",
            rusqlite::params![id],
        )
        .unwrap();
    assert_eq!(reason[0]["reason"].as_str(), Some("governance_ready"));
    kb.storage
        .finish_evolve_request(&id, "failed", Some("transient"), &now)
        .unwrap();
    let retried = kb
        .storage
        .claim_evolve_request(
            "9999-01-01T00:00:00.000Z",
            "9999-01-01T00:00:00.000Z",
        )
        .unwrap()
        .unwrap();
    assert_eq!(retried, id);
}

#[test]
fn repeated_feedback_from_one_actor_is_capped_for_governance() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add("actor cap", "note", Some("actor"), None, "manual", None)
        .unwrap();
    for _ in 0..5 {
        let trace_id = attributed_trace(&kb, &chunk_id);
        kb.record_detailed(
            &trace_id,
            None,
            None,
            None,
            None,
            None,
            "explicit",
            true,
            None,
            Some(&[chunk_id.clone()]),
            "user",
            Some("same-user"),
            None,
            None,
            0,
            None,
            "sdk",
        )
        .unwrap();
    }
    let pending = kb
        .storage
        .query_chunks_params(
            "SELECT COUNT(*) AS cnt FROM governance_proposals
             WHERE chunk_id=? AND state='pending'",
            rusqlite::params![chunk_id],
        )
        .unwrap();
    assert_eq!(pending[0]["cnt"].as_i64(), Some(0));
}

#[test]
fn repeated_curate_does_not_double_count_open_trace_facts() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add("stable aggregation", "note", Some("stable"), None, "manual", None)
        .unwrap();
    let trace_id = attributed_trace(&kb, &chunk_id);

    kb.record_detailed(
        &trace_id,
        None,
        None,
        None,
        None,
        Some(&[chunk_id.clone()]),
        "explicit",
        false,
        None,
        None,
        "user",
        Some("actor"),
        None,
        None,
        0,
        Some("running"),
        "sdk",
    )
    .unwrap();

    kb.builtin_curate_impl(&CurateScope::default()).unwrap();
    kb.builtin_curate_impl(&CurateScope::default()).unwrap();

    let row = kb.storage.get_chunk(&chunk_id).unwrap().unwrap();
    assert_eq!(row["selected_count"].as_i64(), Some(1));
    assert_eq!(row["used_count"].as_i64(), Some(1));
}

#[test]
fn terminal_trace_usage_can_be_corrected_after_curate() {
    let (kb, _file) = tmp_kb();
    let first = kb
        .add("old attribution", "note", Some("old"), None, "manual", None)
        .unwrap();
    let second = kb
        .add("new attribution", "note", Some("new"), None, "manual", None)
        .unwrap();
    let trace_id = attributed_trace_many(&kb, &[first.clone(), second.clone()]);

    kb.record(
        &trace_id,
        None,
        None,
        Some("completed"),
        Some("ok"),
        Some(&[first.clone()]),
        None,
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();
    kb.storage
        .conn_execute(
            "UPDATE episodic_log SET distill_state='discarded' WHERE trace_id=?",
            rusqlite::params![trace_id],
        )
        .unwrap();
    kb.builtin_curate_impl(&CurateScope::default()).unwrap();

    kb.record(
        &trace_id,
        None,
        None,
        None,
        None,
        Some(&[second.clone()]),
        None,
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();
    kb.builtin_curate_impl(&CurateScope::default()).unwrap();

    assert_eq!(
        kb.storage.get_chunk(&first).unwrap().unwrap()["used_count"].as_i64(),
        Some(0)
    );
    assert_eq!(
        kb.storage.get_chunk(&second).unwrap().unwrap()["used_count"].as_i64(),
        Some(1)
    );
}

#[test]
fn partial_used_reports_merge_until_a_complete_report_replaces_them() {
    let (kb, _file) = tmp_kb();
    let first = kb
        .add("partial first", "note", Some("first"), None, "manual", None)
        .unwrap();
    let second = kb
        .add("partial second", "note", Some("second"), None, "manual", None)
        .unwrap();
    let trace_id = attributed_trace_many(&kb, &[first.clone(), second.clone()]);

    for chunk_id in [&first, &second] {
        kb.record_detailed(
            &trace_id,
            None,
            None,
            None,
            None,
            Some(&[chunk_id.clone()]),
            "explicit",
            false,
            None,
            None,
            "user",
            None,
            None,
            None,
            0,
            Some("running"),
            "sdk",
        )
        .unwrap();
    }

    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    let used: std::collections::HashSet<String> =
        serde_json::from_str(log["used_ids"].as_str().unwrap()).unwrap();
    assert_eq!(used, std::collections::HashSet::from([first.clone(), second.clone()]));

    kb.record_detailed(
        &trace_id,
        None,
        None,
        None,
        None,
        Some(&[second.clone()]),
        "explicit",
        true,
        None,
        None,
        "user",
        None,
        None,
        None,
        0,
        Some("running"),
        "sdk",
    )
    .unwrap();
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    let used: Vec<String> = serde_json::from_str(log["used_ids"].as_str().unwrap()).unwrap();
    assert_eq!(used, vec![second]);
}

#[test]
fn partial_used_reports_preserve_per_chunk_attribution() {
    let (kb, _file) = tmp_kb();
    let first = kb
        .add("explicit use", "note", Some("first"), None, "manual", None)
        .unwrap();
    let second = kb
        .add("inferred use", "note", Some("second"), None, "manual", None)
        .unwrap();
    let trace_id = attributed_trace_many(&kb, &[first.clone(), second.clone()]);

    kb.record_detailed(
        &trace_id,
        None,
        None,
        None,
        Some("ok"),
        Some(&[first.clone()]),
        "explicit",
        false,
        None,
        None,
        "user",
        None,
        None,
        None,
        0,
        Some("completed"),
        "sdk",
    )
    .unwrap();
    kb.record_detailed(
        &trace_id,
        None,
        None,
        None,
        None,
        Some(&[second.clone()]),
        "inferred",
        false,
        None,
        None,
        "user",
        None,
        None,
        None,
        0,
        None,
        "sdk",
    )
    .unwrap();

    let rows = kb
        .storage
        .query_chunks_params(
            "SELECT chunk_id, attribution, strength FROM usage_trace
             WHERE trace_id=? AND event='used' ORDER BY chunk_id",
            rusqlite::params![trace_id],
        )
        .unwrap();
    let first_row = rows
        .iter()
        .find(|row| row["chunk_id"].as_str() == Some(first.as_str()))
        .unwrap();
    let second_row = rows
        .iter()
        .find(|row| row["chunk_id"].as_str() == Some(second.as_str()))
        .unwrap();
    assert_eq!(first_row["attribution"].as_str(), Some("explicit"));
    assert_eq!(first_row["strength"].as_f64(), Some(0.3));
    assert_eq!(second_row["attribution"].as_str(), Some("inferred"));
    assert_eq!(second_row["strength"].as_f64(), Some(0.15));

    let evidence = kb
        .storage
        .query_chunks_params(
            "SELECT chunk_id, alpha FROM confidence_evidence
             WHERE trace_id=? AND kind='outcome_ok'",
            rusqlite::params![trace_id],
        )
        .unwrap();
    let first_alpha = evidence
        .iter()
        .find(|row| row["chunk_id"].as_str() == Some(first.as_str()))
        .and_then(|row| row["alpha"].as_f64())
        .unwrap();
    let second_alpha = evidence
        .iter()
        .find(|row| row["chunk_id"].as_str() == Some(second.as_str()))
        .and_then(|row| row["alpha"].as_f64())
        .unwrap();
    assert!(first_alpha > second_alpha);
}

#[test]
fn pending_chunks_receive_a_recall_penalty() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add(
            "pending recall quality boundary",
            "note",
            Some("quality boundary"),
            None,
            "manual",
            None,
        )
        .unwrap();
    let active = kb
        .recall(
            "pending recall quality boundary",
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
    let active_score = active
        .knowledge
        .iter()
        .find(|chunk| chunk["id"].as_str() == Some(chunk_id.as_str()))
        .and_then(|chunk| chunk["_fused_score"].as_f64())
        .unwrap();
    kb.storage
        .conn_execute(
            "UPDATE chunks SET state='pending' WHERE id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap();
    let pending = kb
        .recall(
            "pending recall quality boundary",
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
    let pending_score = pending
        .knowledge
        .iter()
        .find(|chunk| chunk["id"].as_str() == Some(chunk_id.as_str()))
        .and_then(|chunk| chunk["_fused_score"].as_f64())
        .unwrap();
    assert!(pending_score < active_score);
}

#[test]
fn restore_supersedes_historical_governance_feedback() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add(
            "restorable governance",
            "note",
            Some("restore"),
            None,
            "manual",
            None,
        )
        .unwrap();
    for actor in ["reviewer-a", "reviewer-b", "reviewer-c"] {
        record_down_as(&kb, &chunk_id, actor);
    }
    kb.builtin_curate_impl(&CurateScope::default()).unwrap();
    assert_eq!(
        kb.storage.get_chunk(&chunk_id).unwrap().unwrap()["state"].as_str(),
        Some("archived")
    );
    kb.storage
        .conn_execute(
            "UPDATE feedback_events SET ts='2020-01-01T00:00:00.000Z' WHERE chunk_id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap();

    kb.restore(&chunk_id).unwrap();
    kb.builtin_curate_impl(&CurateScope::default()).unwrap();

    let chunk = kb.storage.get_chunk(&chunk_id).unwrap().unwrap();
    assert_eq!(chunk["state"].as_str(), Some("active"));
    let proposals = kb
        .storage
        .query_chunks_params(
            "SELECT state FROM governance_proposals WHERE chunk_id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap();
    assert!(proposals
        .iter()
        .all(|proposal| proposal["state"].as_str() == Some("rejected")));
}

#[test]
fn restoring_invalidated_chunk_resets_zero_confidence_baseline() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add("invalidated restore", "note", None, None, "manual", None)
        .unwrap();
    kb.invalidate(&chunk_id, "incorrect invalidation").unwrap();
    kb.restore(&chunk_id).unwrap();

    let chunk = kb.storage.get_chunk(&chunk_id).unwrap().unwrap();
    assert_eq!(chunk["state"].as_str(), Some("active"));
    assert_eq!(chunk["confidence_base"].as_f64(), Some(0.5));
    assert_eq!(chunk["confidence"].as_f64(), Some(0.5));
}

#[test]
fn record_rejects_retrieved_but_unselected_attribution() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add("candidate only", "note", Some("candidate"), None, "manual", None)
        .unwrap();
    let trace_id = crate::utils::gen_uuid();
    let now = crate::utils::utc_now_iso();
    kb.storage
        .upsert_episodic_log(&crate::storage::EpisodicLogRow {
            id: crate::utils::gen_uuid(),
            trace_id: trace_id.clone(),
            lib_id: kb.storage.lib_id().unwrap(),
            ts: now.clone(),
            query: Some("candidate".to_string()),
            recall_snapshot: Some(
                serde_json::json!({"retrieved": [chunk_id], "selected": [], "sparks": []})
                    .to_string(),
            ),
            event_source: "sdk".to_string(),
            task_state: "recalled".to_string(),
            usage_state: "unknown".to_string(),
            distill_state: "open".to_string(),
            ..Default::default()
        })
        .unwrap();

    let error = kb
        .record_detailed(
            &trace_id,
            None,
            None,
            None,
            None,
            Some(&[chunk_id]),
            "explicit",
            true,
            None,
            None,
            "user",
            None,
            None,
            None,
            0,
            None,
            "sdk",
        )
        .unwrap_err();
    assert!(matches!(error, InnateError::InvalidState(_)));
}

#[test]
fn unknown_outcome_can_be_resolved_but_final_outcome_is_immutable() {
    let (kb, _file) = tmp_kb();
    let trace_id = crate::utils::gen_uuid();
    kb.record(
        &trace_id,
        Some("outcome"),
        None,
        Some("provisional"),
        Some("unknown"),
        None,
        None,
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();
    let provisional = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(provisional["task_state"].as_str(), Some("running"));
    assert_eq!(provisional["distill_state"].as_str(), Some("open"));
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
        "sdk",
    )
    .unwrap();
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(log["outcome"].as_str(), Some("ok"));

    let error = kb
        .record(
            &trace_id,
            None,
            None,
            None,
            Some("fail"),
            None,
            None,
            None,
            None,
            0,
            "sdk",
        )
        .unwrap_err();
    assert!(matches!(error, InnateError::OutcomeConflict { .. }));
}

#[test]
fn record_rejects_conflicting_feedback_for_same_chunk() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add("feedback conflict", "note", Some("feedback"), None, "manual", None)
        .unwrap();
    let trace_id = attributed_trace(&kb, &chunk_id);
    let error = kb
        .record_detailed(
            &trace_id,
            None,
            None,
            None,
            None,
            None,
            "explicit",
            true,
            Some(&[chunk_id.clone()]),
            Some(&[chunk_id]),
            "user",
            None,
            None,
            None,
            0,
            None,
            "sdk",
        )
        .unwrap_err();
    assert!(matches!(error, InnateError::InvalidState(_)));
}

#[test]
fn curate_keeps_confidence_base_stable_for_replayable_evidence() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add("confidence replay", "note", Some("confidence"), None, "manual", None)
        .unwrap();
    let initial_base = kb.storage.get_chunk(&chunk_id).unwrap().unwrap()["confidence_base"]
        .as_f64()
        .unwrap();
    record_down_as(&kb, &chunk_id, "reviewer");

    kb.builtin_curate_impl(&CurateScope::default()).unwrap();
    let row = kb.storage.get_chunk(&chunk_id).unwrap().unwrap();
    assert_eq!(row["confidence_base"].as_f64(), Some(initial_base));
    let evidence = kb
        .storage
        .query_chunks_params(
            "SELECT COUNT(*) AS cnt FROM confidence_evidence WHERE chunk_id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap();
    assert_eq!(evidence[0]["cnt"].as_i64(), Some(1));
}

#[test]
fn complete_usage_without_outcome_records_selected_unused() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add("unused evidence", "note", Some("unused"), None, "manual", None)
        .unwrap();
    let trace_id = attributed_trace(&kb, &chunk_id);

    kb.record_detailed(
        &trace_id,
        None,
        None,
        None,
        None,
        Some(&[]),
        "explicit",
        true,
        None,
        None,
        "user",
        None,
        None,
        None,
        0,
        Some("running"),
        "sdk",
    )
    .unwrap();

    let evidence = kb
        .storage
        .query_chunks_params(
            "SELECT COUNT(*) AS cnt FROM confidence_evidence
             WHERE trace_id=? AND chunk_id=? AND kind='selected_unused'",
            rusqlite::params![trace_id, chunk_id],
        )
        .unwrap();
    assert_eq!(evidence[0]["cnt"].as_i64(), Some(1));
}

#[test]
fn opposite_feedback_replaces_previous_signal_for_same_trace() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add("correctable feedback", "note", Some("feedback"), None, "manual", None)
        .unwrap();
    let trace_id = attributed_trace(&kb, &chunk_id);

    kb.record(
        &trace_id,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(&[chunk_id.clone()]),
        None,
        0,
        "sdk",
    )
    .unwrap();
    kb.record(
        &trace_id,
        None,
        None,
        None,
        None,
        None,
        Some(&[chunk_id.clone()]),
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();

    let rows = kb
        .storage
        .query_chunks_params(
            "SELECT signal FROM feedback_events WHERE trace_id=? AND chunk_id=?",
            rusqlite::params![trace_id, chunk_id],
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["signal"].as_str(), Some("up"));
}

struct SelectiveFailingDistiller;

impl Distiller for SelectiveFailingDistiller {
    fn distill(&self, logs: &[Value]) -> Result<Vec<DistilledChunk>> {
        let log = &logs[0];
        if log["query"].as_str() == Some("bad") {
            return Err(InnateError::Other("bad log".to_string()));
        }
        Ok(vec![DistilledChunk {
            content: "isolated success".to_string(),
            trigger_desc: Some("success".to_string()),
            anti_trigger_desc: None,
            source_log_id: log["id"].as_str().unwrap().to_string(),
            nomination: None,
        }])
    }
}

#[test]
fn distill_failure_is_isolated_to_the_failing_log() {
    let file = NamedTempFile::new().unwrap();
    let kb = KnowledgeBase::open_with(
        file.path(),
        None,
        None,
        Some(Arc::new(SelectiveFailingDistiller)),
        None,
        None,
    )
    .unwrap();
    for query in ["good", "bad"] {
        kb.record(
            &crate::utils::gen_uuid(),
            Some(query),
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
    }

    // evolve() succeeds even when one log fails; failure is isolated to that log.
    kb.evolve("manual").unwrap();
    let states = kb
        .storage
        .query_chunks("SELECT query, distill_state FROM episodic_log ORDER BY query")
        .unwrap();
    assert_eq!(states[0]["query"].as_str(), Some("bad"));
    assert_eq!(states[0]["distill_state"].as_str(), Some("failed"));
    assert_eq!(states[1]["query"].as_str(), Some("good"));
    assert_eq!(states[1]["distill_state"].as_str(), Some("distilled"));
}

#[test]
fn outcome_without_reusable_material_is_not_queued_for_distillation() {
    let (kb, _file) = tmp_kb();
    let trace_id = crate::utils::gen_uuid();
    kb.record(
        &trace_id,
        Some("query only"),
        None,
        None,
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
    assert_eq!(log["distill_state"].as_str(), Some("discarded"));
    assert_eq!(log["distill_note"].as_str(), Some("insufficient_material"));
}

#[test]
fn stale_pending_chunk_without_usage_is_archived() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add("stale pending", "note", Some("stale"), None, "manual", None)
        .unwrap();
    kb.storage
        .conn_execute(
            "UPDATE chunks SET state='pending', created_at='2020-01-01T00:00:00.000Z'
             WHERE id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap();
    kb.builtin_curate_impl(&CurateScope::default()).unwrap();
    let row = kb.storage.get_chunk(&chunk_id).unwrap().unwrap();
    assert_eq!(row["state"].as_str(), Some("archived"));
    assert_eq!(row["state_reason"].as_str(), Some("never_used"));
}
