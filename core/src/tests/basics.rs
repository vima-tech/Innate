use super::*;

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
        .recall(RecallParams {
            query: "validate input",
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
    assert!(!result.trace_id.is_empty());
}

#[test]
fn warm_cache_reflects_writes_made_after_first_recall() {
    // Locks the incremental vector-cache invariant: a recall warms the in-memory
    // cache, and a chunk added afterwards must still be retrievable by the next
    // recall on the same long-lived KnowledgeBase (no stale-cache miss).
    let (kb, _f) = tmp_kb();

    let id_a = kb
        .add(
            "First fact about caching",
            "note",
            Some("caching"),
            None,
            "manual",
            None,
        )
        .unwrap();

    // Warm the cache.
    let first = kb
        .recall(RecallParams {
            query: "caching",
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
    assert!(first
        .knowledge
        .iter()
        .any(|c| c["id"].as_str() == Some(id_a.as_str())));

    // Write after the cache is warm — must land in the in-memory cache.
    let id_b = kb
        .add(
            "Second fact added later",
            "note",
            Some("later"),
            None,
            "manual",
            None,
        )
        .unwrap();

    let second = kb
        .recall(RecallParams {
            query: "later",
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
    assert!(
        second
            .knowledge
            .iter()
            .any(|c| c["id"].as_str() == Some(id_b.as_str())),
        "chunk added after cache warm-up was not retrievable"
    );
}

#[test]
fn curate_compacts_old_terminal_logs_and_keeps_recent() {
    // Step 8 of curate compacts terminal episodic_log rows older than the
    // configurable window, NULLing heavy payload while preserving the row.
    let (kb, _f) = tmp_kb();
    let lib = kb.storage.lib_id().unwrap();

    let mk = |trace: &str, ts: &str| crate::storage::EpisodicLogRow {
        id: crate::utils::gen_uuid(),
        trace_id: trace.to_string(),
        lib_id: lib.clone(),
        ts: ts.to_string(),
        output_summary: Some("heavy payload to be compacted".to_string()),
        event_source: "sdk".to_string(),
        task_state: "completed".to_string(),
        usage_state: "unknown".to_string(),
        distill_state: "distilled".to_string(),
        ..Default::default()
    };

    // One ancient terminal log (well before the 30-day window) and one fresh one.
    kb.storage
        .upsert_episodic_log(&mk("old-trace", "2020-01-01T00:00:00.000Z"))
        .unwrap();
    kb.storage
        .upsert_episodic_log(&mk("new-trace", &crate::utils::utc_now_iso()))
        .unwrap();

    kb.evolve("manual").unwrap();

    let old = kb.storage.get_episodic_log("old-trace").unwrap().unwrap();
    let new = kb.storage.get_episodic_log("new-trace").unwrap().unwrap();
    assert!(
        old["output_summary"].is_null(),
        "old terminal log should have been compacted"
    );
    assert_eq!(
        new["output_summary"].as_str(),
        Some("heavy payload to be compacted"),
        "recent terminal log must be preserved"
    );
}

#[test]
fn vacuum_runs_and_reports_size() {
    let (kb, _f) = tmp_kb();
    let (before, after) = kb.storage.vacuum().unwrap();
    assert!(before > 0 && after > 0);
    assert!(after <= before, "vacuum must not grow the file");
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
    kb.record(RecordParams {
        trace_id: &trace_id,
        query: Some("test query"),
        output: None,
        output_summary: Some("summary"),
        outcome: Some("ok"),
        used: None,
        feedback_up: None,
        feedback_down: None,
        nomination: None,
        priority: 0,
        source: "cli",
        ..Default::default()
    })
    .unwrap();
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(log["distill_state"].as_str().unwrap(), "new");

    kb.record(RecordParams {
        trace_id: &trace_id,
        query: None,
        output: None,
        output_summary: None,
        outcome: Some("ok"),
        used: None,
        feedback_up: None,
        feedback_down: None,
        nomination: None,
        priority: 0,
        source: "cli",
        ..Default::default()
    })
    .unwrap();
    let log2 = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(log2["distill_state"].as_str().unwrap(), "new");
}

#[test]
fn late_material_reopens_insufficient_material_log() {
    let (kb, _f) = tmp_kb();
    let trace_id = crate::utils::gen_uuid();
    kb.record(RecordParams {
        trace_id: &trace_id,
        query: Some("late material"),
        output: None,
        output_summary: None,
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
    let discarded = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(discarded["distill_state"].as_str(), Some("discarded"));
    assert_eq!(
        discarded["distill_note"].as_str(),
        Some("insufficient_material")
    );

    kb.record(RecordParams {
        trace_id: &trace_id,
        query: None,
        output: None,
        output_summary: Some("material arrived after completion"),
        outcome: None,
        used: None,
        feedback_up: None,
        feedback_down: None,
        nomination: None,
        priority: 0,
        source: "sdk",
        ..Default::default()
    })
    .unwrap();

    let reopened = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(reopened["distill_state"].as_str(), Some("new"));
    assert!(reopened["distill_note"].is_null());
}

#[test]
fn mcp_is_a_valid_event_source() {
    let (kb, _f) = tmp_kb();
    let result = kb
        .recall(RecallParams {
            query: "mcp source",
            budget: 6000,
            trace: true,
            include_sparks: false,
            top: None,
            source: "mcp",
            expand_deps: "false",
            allow_trim: false,
            refine_mode: "off",
        })
        .unwrap();
    kb.record(RecordParams {
        trace_id: &result.trace_id,
        query: None,
        output: None,
        output_summary: Some("closed through MCP"),
        outcome: Some("ok"),
        used: None,
        feedback_up: None,
        feedback_down: None,
        nomination: None,
        priority: 0,
        source: "mcp",
        ..Default::default()
    })
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
        .recall(RecallParams {
            query: "bounded retries",
            budget: 6000,
            trace: true,
            include_sparks: false,
            top: None,
            source: "sdk",
            expand_deps: "false",
            allow_trim: false,
            refine_mode: "off",
        })
        .unwrap();
    let before = kb.storage.get_chunk(&chunk_id).unwrap().unwrap()["confidence"]
        .as_f64()
        .unwrap();
    kb.record(RecordParams {
        trace_id: &first.trace_id,
        query: None,
        output: None,
        output_summary: Some("completed without usage attribution"),
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
    let after_unknown = kb.storage.get_chunk(&chunk_id).unwrap().unwrap()["confidence"]
        .as_f64()
        .unwrap();
    assert_eq!(before, after_unknown);

    let second = kb
        .recall(RecallParams {
            query: "bounded retries",
            budget: 6000,
            trace: true,
            include_sparks: false,
            top: None,
            source: "sdk",
            expand_deps: "false",
            allow_trim: false,
            refine_mode: "off",
        })
        .unwrap();
    let explicitly_unused: Vec<String> = vec![];
    kb.record(RecordParams {
        trace_id: &second.trace_id,
        query: None,
        output: None,
        output_summary: Some("completed and explicitly used no recalled chunks"),
        outcome: Some("ok"),
        used: Some(&explicitly_unused),
        feedback_up: None,
        feedback_down: None,
        nomination: None,
        priority: 0,
        source: "sdk",
        ..Default::default()
    })
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
            .recall(RecallParams {
                query: "retry policy",
                budget: 6000,
                trace: true,
                include_sparks: false,
                top: None,
                source: "sdk",
                expand_deps: "false",
                allow_trim: false,
                refine_mode: "off",
            })
            .unwrap();
        let used = vec![chunk_id.clone()];
        let actor = format!("tester-{i}");
        kb.record(RecordParams {
            trace_id: &recall.trace_id,
            query: None,
            output: None,
            output_summary: Some("retry policy was unsuitable"),
            outcome: Some("fail"),
            used: Some(&used),
            used_attribution: "explicit",
            used_complete: Some(true),
            feedback_up: None,
            feedback_down: Some(&used),
            feedback_kind: "user",
            feedback_actor: Some(&actor),
            feedback_reason: Some("unbounded retry is unsafe"),
            nomination: None,
            priority: 0,
            task_state: None,
            source: "sdk",
        })
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
    kb.record(RecordParams {
        trace_id: &trace_id,
        query: Some("queue evolve"),
        output: None,
        output_summary: Some("reusable material"),
        outcome: Some("ok"),
        used: Some(&[]),
        feedback_up: None,
        feedback_down: None,
        nomination: None,
        priority: 0,
        source: "sdk",
        ..Default::default()
    })
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

    kb.recall(RecallParams {
        query: "Refiner mode test",
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
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    kb.recall(RecallParams {
        query: "Refiner mode test",
        budget: 6000,
        trace: false,
        include_sparks: false,
        top: None,
        source: "sdk",
        expand_deps: "false",
        allow_trim: false,
        refine_mode: "adapt",
    })
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
