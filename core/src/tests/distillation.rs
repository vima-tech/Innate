use super::*;

pub(super) struct FailingDistiller;

impl Distiller for FailingDistiller {
    fn distill(&self, _log_entries: &[Value]) -> Result<Vec<DistilledChunk>> {
        Err(InnateError::Other("model offline".to_string()))
    }
}

pub(super) struct CountingFailingDistiller {
    pub(super) calls: Arc<AtomicUsize>,
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
    kb.record(RecordParams {
        trace_id: &trace_id,
        query: Some("query"),
        output: None,
        output_summary: Some("material"),
        outcome: Some("ok"),
        used: None,
        feedback_up: None,
        feedback_down: None,
        nomination: None,
        priority: 0,
        source: "sdk",
        ..Default::default()
    })
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
fn distillation_only_receives_same_context_related_logs() {
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
        kb.record(RecordParams {
            trace_id: &crate::utils::gen_uuid(),
            query: Some(query),
            output: None,
            output_summary: Some("reusable material"),
            outcome: Some("ok"),
            used: None,
            feedback_up: None,
            feedback_down: None,
            nomination: None,
            priority: 0,
            source: "sdk",
            ..Default::default()
        })
        .unwrap();
    }

    kb.evolve("manual").unwrap();

    assert_eq!(*related_counts.lock().unwrap(), vec![0, 0]);
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
    kb.record(RecordParams {
        trace_id: &trace_id,
        query: Some("query"),
        output: None,
        output_summary: Some("material"),
        outcome: Some("ok"),
        used: None,
        feedback_up: None,
        feedback_down: None,
        nomination: None,
        priority: 0,
        source: "sdk",
        ..Default::default()
    })
    .unwrap();

    let result = kb.evolve("manual").unwrap();
    assert_eq!(
        result["distilled"].as_u64(),
        Some(1),
        "log should count as 1 distilled"
    );
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(
        log["distill_state"].as_str(),
        Some("distilled"),
        "log must be 'distilled', not 'failed'"
    );
    let chunk_count = kb
        .storage
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
    kb.record(RecordParams {
        trace_id: &trace_id,
        query: Some("query"),
        output: None,
        output_summary: Some("material"),
        outcome: Some("ok"),
        used: None,
        feedback_up: None,
        feedback_down: None,
        nomination: None,
        priority: 0,
        source: "sdk",
        ..Default::default()
    })
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
    assert_eq!(
        result["distilled"].as_u64(),
        Some(1),
        "distillation must succeed even when a chunk with the same distilled_from already exists"
    );
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(log["distill_state"].as_str(), Some("distilled"));
    let count = kb
        .storage
        .query_chunks_params(
            "SELECT COUNT(*) AS cnt FROM chunks WHERE distilled_from=?",
            rusqlite::params![log_id],
        )
        .unwrap()[0]["cnt"]
        .as_i64()
        .unwrap();
    assert_eq!(count, 2, "both the pre-existing and new chunk must coexist");
}

#[test]
fn distill_records_prompt_and_completion_token_estimates() {
    let (kb, _file) = tmp_kb();
    let trace_id = crate::utils::gen_uuid();
    kb.record(RecordParams {
        trace_id: &trace_id,
        query: Some("How should retries be bounded?"),
        output: None,
        output_summary: Some("Use bounded exponential backoff with jitter."),
        outcome: Some("ok"),
        used: None,
        feedback_up: None,
        feedback_down: None,
        nomination: Some("Reusable retry guidance"),
        priority: 1,
        source: "sdk",
        ..Default::default()
    })
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
        kb.record(RecordParams {
            trace_id: &first_trace,
            query: Some("first query"),
            output: None,
            output_summary: Some("first reusable material"),
            outcome: Some("ok"),
            used: None,
            feedback_up: None,
            feedback_down: None,
            nomination: None,
            priority: 0,
            source: "sdk",
            ..Default::default()
        })
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
    kb.record(RecordParams {
        trace_id: &second_trace,
        query: Some("second query"),
        output: None,
        output_summary: Some("second reusable material"),
        outcome: Some("ok"),
        used: None,
        feedback_up: None,
        feedback_down: None,
        nomination: None,
        priority: 0,
        source: "sdk",
        ..Default::default()
    })
    .unwrap();

    let result = kb.evolve("threshold").unwrap();
    assert_eq!(result["distilled"].as_u64(), Some(0));
    assert_eq!(result["skipped"].as_str(), Some("distill_token_limit"));
    let second_log = kb.storage.get_episodic_log(&second_trace).unwrap().unwrap();
    assert_eq!(second_log["distill_state"].as_str(), Some("new"));
    let requests = kb
        .storage
        .query_chunks(
            "SELECT state, note, next_retry_at
             FROM evolve_requests
             WHERE state='pending' AND note='distill_token_limit'",
        )
        .unwrap();
    assert_eq!(requests.len(), 1, "budget-limited work must remain queued");
    assert!(requests[0]["next_retry_at"].as_str().is_some());
}

#[test]
fn distill_token_window_uses_actual_distill_time_not_log_creation_time() {
    let file = NamedTempFile::new().unwrap();
    let kb = KnowledgeBase::open(file.path()).unwrap();
    let first_trace = crate::utils::gen_uuid();
    kb.record(RecordParams {
        trace_id: &first_trace,
        query: Some("old queued query"),
        output: None,
        output_summary: Some("material distilled today"),
        outcome: Some("ok"),
        used: None,
        feedback_up: None,
        feedback_down: None,
        nomination: None,
        priority: 0,
        source: "sdk",
        ..Default::default()
    })
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
    kb.record(RecordParams {
        trace_id: &second_trace,
        query: Some("new query"),
        output: None,
        output_summary: Some("new material"),
        outcome: Some("ok"),
        used: None,
        feedback_up: None,
        feedback_down: None,
        nomination: None,
        priority: 0,
        source: "sdk",
        ..Default::default()
    })
    .unwrap();

    let result = kb.evolve("threshold").unwrap();
    assert_eq!(result["skipped"].as_str(), Some("distill_token_limit"));
    let second_log = kb.storage.get_episodic_log(&second_trace).unwrap().unwrap();
    assert_eq!(second_log["distill_state"].as_str(), Some("new"));
}

#[test]
fn scheduled_evolve_respects_distill_token_limit() {
    let file = NamedTempFile::new().unwrap();
    let first_trace = crate::utils::gen_uuid();
    {
        let kb = KnowledgeBase::open(file.path()).unwrap();
        kb.record(RecordParams {
            trace_id: &first_trace,
            query: Some("first scheduled budget query"),
            output: None,
            output_summary: Some("first scheduled budget material"),
            outcome: Some("ok"),
            used: None,
            feedback_up: None,
            feedback_down: None,
            nomination: None,
            priority: 0,
            source: "sdk",
            ..Default::default()
        })
        .unwrap();
        kb.evolve("manual").unwrap();
        let first_log = kb.storage.get_episodic_log(&first_trace).unwrap().unwrap();
        let used = first_log["distill_prompt_tokens"].as_i64().unwrap_or(0)
            + first_log["distill_completion_tokens"].as_i64().unwrap_or(0);
        kb.storage
            .set_meta("max_distill_tokens_per_period", &used.to_string())
            .unwrap();
        kb.storage
            .set_meta("evolve.threshold_new_count", "1")
            .unwrap();
    }

    let kb = KnowledgeBase::open(file.path()).unwrap();
    let second_trace = crate::utils::gen_uuid();
    kb.record(RecordParams {
        trace_id: &second_trace,
        query: Some("second scheduled budget query"),
        output: None,
        output_summary: Some("second scheduled budget material"),
        outcome: Some("ok"),
        used: None,
        feedback_up: None,
        feedback_down: None,
        nomination: None,
        priority: 0,
        source: "sdk",
        ..Default::default()
    })
    .unwrap();

    let result = kb.evolve("scheduled").unwrap();
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
    assert_eq!(
        applied,
        vec![
            "4.5.1→4.5.2",
            "4.5.2→4.6",
            "4.6→4.7",
            "4.7→4.8",
            "4.8→4.9",
            "4.9→4.10",
            "4.10→4.11",
            "4.11→4.12",
            "4.12→4.13",
            "4.13→4.14"
        ]
    );

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
    assert_eq!(version, "4.14");
}

#[test]
fn stale_screening_is_reported_as_recovered() {
    let (kb, _file) = tmp_kb();
    let trace_id = crate::utils::gen_uuid();
    kb.record(RecordParams {
        trace_id: &trace_id,
        query: Some("query"),
        output: None,
        output_summary: Some("material"),
        outcome: Some("ok"),
        used: None,
        feedback_up: None,
        feedback_down: None,
        nomination: None,
        priority: 0,
        source: "sdk",
        ..Default::default()
    })
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
fn add_dependency_wires_public_dep_edges() {
    let (kb, _file) = tmp_kb();
    let a = kb
        .add("depends", "note", Some("a"), None, "manual", None)
        .unwrap();
    let b = kb
        .add("dependency", "note", Some("b"), None, "manual", None)
        .unwrap();

    // A valid hard dependency declared via the public API is persisted and
    // visible to the recall-time read path.
    kb.add_dependency(&a, &b, "hard").unwrap();
    let deps = kb.storage.get_deps(&a).unwrap();
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].0, b);
    assert_eq!(deps[0].1, "hard");

    // Idempotent: re-declaring the same edge is a no-op.
    kb.add_dependency(&a, &b, "hard").unwrap();
    assert_eq!(kb.storage.get_deps(&a).unwrap().len(), 1);

    // Invalid kind is rejected.
    assert!(matches!(
        kb.add_dependency(&a, &b, "weak").unwrap_err(),
        InnateError::InvalidState(_)
    ));

    // Missing source or target is rejected so no dangling edge is written.
    assert!(matches!(
        kb.add_dependency(&a, "nope", "hard").unwrap_err(),
        InnateError::ChunkNotFound(_)
    ));
    assert!(matches!(
        kb.add_dependency("nope", &b, "hard").unwrap_err(),
        InnateError::ChunkNotFound(_)
    ));
}

#[test]
fn add_with_deps_is_atomic_on_missing_dependency() {
    let (kb, _file) = tmp_kb();
    // Creating a chunk that depends on a non-existent chunk must fail AND leave
    // no partial chunk behind — the chunk + vectors + deps are one transaction.
    let err = kb
        .add_with_deps(
            "orphan-maker",
            "note",
            Some("t"),
            None,
            "manual",
            None,
            &[("does-not-exist".to_string(), "hard".to_string())],
        )
        .unwrap_err();
    assert!(matches!(err, InnateError::ChunkNotFound(_)));
    let rows = kb
        .storage
        .query_chunks_params(
            "SELECT id FROM chunks WHERE content=?",
            rusqlite::params!["orphan-maker"],
        )
        .unwrap();
    assert!(
        rows.is_empty(),
        "chunk must not persist when a declared dependency is invalid"
    );
}

#[test]
fn idempotent_add_merges_new_dependencies() {
    let (kb, _file) = tmp_kb();
    let target = kb
        .add("dep target", "note", Some("t"), None, "manual", None)
        .unwrap();
    // First add of the content, no deps.
    let id1 = kb
        .add("duplicate body", "note", Some("d"), None, "manual", None)
        .unwrap();
    assert_eq!(kb.storage.get_deps(&id1).unwrap().len(), 0);

    // Re-adding identical content with a NEW dependency must return the same
    // chunk AND merge the edge, rather than silently dropping it.
    let id2 = kb
        .add_with_deps(
            "duplicate body",
            "note",
            Some("d"),
            None,
            "manual",
            None,
            &[(target.clone(), "hard".to_string())],
        )
        .unwrap();
    assert_eq!(id1, id2, "duplicate content returns the existing chunk id");
    let deps = kb.storage.get_deps(&id1).unwrap();
    assert_eq!(deps.len(), 1, "the new dependency must be merged in");
    assert_eq!(deps[0].0, target);

    // Merging is idempotent: a second identical merge does not duplicate edges.
    kb.add_with_deps(
        "duplicate body",
        "note",
        Some("d"),
        None,
        "manual",
        None,
        &[(target.clone(), "hard".to_string())],
    )
    .unwrap();
    assert_eq!(kb.storage.get_deps(&id1).unwrap().len(), 1);

    // A bad dependency target on the idempotent path is still rejected.
    let err = kb
        .add_with_deps(
            "duplicate body",
            "note",
            Some("d"),
            None,
            "manual",
            None,
            &[("nope".to_string(), "hard".to_string())],
        )
        .unwrap_err();
    assert!(matches!(err, InnateError::ChunkNotFound(_)));
}

#[test]
fn corrupt_vector_blob_fails_recall_closed() {
    let (kb, _file) = tmp_kb();
    let id = kb
        .add(
            "vectored knowledge",
            "note",
            Some("trigger"),
            None,
            "manual",
            None,
        )
        .unwrap();
    // Corrupt the stored content vector to 3 bytes (not a multiple of 4).
    kb.storage
        .conn_execute(
            "UPDATE vec_content SET embedding=? WHERE chunk_id=?",
            rusqlite::params![vec![1u8, 2, 3], id],
        )
        .unwrap();
    kb.storage.invalidate_vector_caches();
    // Recall must fail-closed rather than silently using a truncated vector.
    let result = kb.recall(RecallParams {
        query: "vectored knowledge",
        budget: 6000,
        source: "sdk",
        ..Default::default()
    });
    assert!(
        result.is_err(),
        "recall must fail on a structurally corrupt persisted embedding"
    );
}

#[test]
fn recall_refreshes_vector_cache_after_external_write() {
    let file = NamedTempFile::new().unwrap();
    let reader = KnowledgeBase::open(file.path()).unwrap();
    reader
        .add("cache warmup", "note", None, None, "manual", None)
        .unwrap();
    reader
        .recall(RecallParams {
            query: "cache warmup",
            budget: 6000,
            trace: false,
            include_sparks: false,
            top: None,
            source: "sdk",
            expand_deps: "false",
            allow_trim: false,
            refine_mode: "off",
        })
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
        .recall(RecallParams {
            query: "knowledge written by another process",
            budget: 6000,
            trace: false,
            include_sparks: false,
            top: None,
            source: "sdk",
            expand_deps: "false",
            allow_trim: false,
            refine_mode: "off",
        })
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

/// Embedding provider that lies about its output: it declares a dimension but
/// returns a vector of a different length. Used to prove the write-side guard
/// rejects dimension-mismatched vectors instead of silently storing them.
struct LyingEmbeddingProvider {
    declared: usize,
    actual: usize,
}

impl EmbeddingProvider for LyingEmbeddingProvider {
    fn content_dim(&self) -> usize {
        self.declared
    }
    fn trigger_dim(&self) -> usize {
        self.declared
    }
    fn embed_content(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![0.1; self.actual])
    }
    fn embed_trigger(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![0.1; self.actual])
    }
}

#[test]
fn add_rejects_dimension_mismatched_vector() {
    let file = NamedTempFile::new().unwrap();
    let embedding: Arc<dyn EmbeddingProvider> =
        Arc::new(LyingEmbeddingProvider { declared: 4, actual: 2 });
    let kb =
        KnowledgeBase::open_with(file.path(), Some(embedding), None, None, None, None).unwrap();

    // A vector whose real length disagrees with the configured dimension would
    // be silently dropped at search time, so the write must fail closed.
    let err = kb
        .add("mismatched", "note", Some("t"), None, "manual", None)
        .unwrap_err();
    assert!(matches!(err, InnateError::InvalidState(_)), "got: {err:?}");

    // And nothing partial is left behind — the transaction rolled back.
    let rows = kb
        .storage
        .query_chunks_params(
            "SELECT id FROM chunks WHERE content=?",
            rusqlite::params!["mismatched"],
        )
        .unwrap();
    assert!(rows.is_empty(), "no chunk should persist on a bad-dim write");
}
