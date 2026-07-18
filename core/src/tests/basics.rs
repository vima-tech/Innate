use super::*;

#[test]
fn min_score_gate_drops_subthreshold_candidates() {
    // The relevance gate (used by always-on hooks) must drop candidates below the
    // threshold before packing/trace, so an impossibly high gate yields empty knowledge
    // while no gate returns the same chunk.
    let (kb, _f) = tmp_kb();
    let id = kb
        .add(
            "Prefer composition over inheritance",
            "note",
            Some("design principle"),
            None,
            "manual",
            None,
        )
        .unwrap();

    let base = RecallParams {
        query: "design principle",
        budget: 6000,
        trace: false,
        include_sparks: false,
        top: None,
        source: "sdk",
        expand_deps: "false",
        allow_trim: false,
        refine_mode: "off",
        min_score: None,
        session_only: false,
        ..Default::default()
    };

    // No gate: the chunk is retrievable.
    let ungated = kb.recall(base.clone()).unwrap();
    assert!(ungated
        .knowledge
        .iter()
        .any(|c| c["id"].as_str() == Some(id.as_str())));

    // Fused scores span at most ~1.05; a gate of 2.0 must drop everything.
    let gated = kb
        .recall(RecallParams {
            min_score: Some(2.0),
            session_only: false,
            ..base
        })
        .unwrap();
    assert!(gated.knowledge.is_empty());
    assert!(gated.empty);
}

#[test]
fn pending_relevance_gate_uses_score_before_lifecycle_penalty() {
    let (kb, _f) = tmp_kb();
    let id = kb
        .add(
            "Prefer composition over inheritance",
            "note",
            Some("design principle"),
            None,
            "manual",
            None,
        )
        .unwrap();
    kb.storage
        .conn_execute(
            "UPDATE chunks SET state='pending' WHERE id=?",
            rusqlite::params![id],
        )
        .unwrap();

    let base = RecallParams {
        query: "design principle",
        budget: 6000,
        trace: false,
        include_sparks: false,
        top: None,
        source: "hook",
        expand_deps: "false",
        allow_trim: false,
        refine_mode: "off",
        min_score: None,
        session_only: false,
        ..Default::default()
    };
    let ungated = kb.recall(base.clone()).unwrap();
    let penalized_score = ungated
        .knowledge
        .iter()
        .find(|chunk| chunk["id"].as_str() == Some(id.as_str()))
        .and_then(|chunk| chunk["_fused_score"].as_f64())
        .expect("pending chunk should be retrievable without a gate");
    let pre_penalty_score = penalized_score / 0.60;
    let gate = (penalized_score + pre_penalty_score) / 2.0;

    let gated = kb
        .recall(RecallParams {
            min_score: Some(gate),
            ..base
        })
        .unwrap();
    assert!(
        gated
            .knowledge
            .iter()
            .any(|chunk| chunk["id"].as_str() == Some(id.as_str())),
        "a relevant pending chunk must pass the gate before its ranking penalty is applied"
    );
}

#[test]
fn agent_source_dimension_is_captured_and_orthogonal_to_channel() {
    // The agent product identity (INNATE_AGENT) lands on both the chunk and the
    // recall episodic_log, independently of the access channel (event_source).
    let (kb, _f) = tmp_kb();
    std::env::set_var("INNATE_AGENT", "claude-code");

    let id = kb
        .add(
            "Use cargo build --release to compile innate",
            "note",
            Some("build the binary"),
            None,
            "manual",
            None,
        )
        .unwrap();
    let chunk = kb.storage.get_chunk(&id).unwrap().unwrap();
    assert_eq!(chunk["agent"].as_str(), Some("claude-code"));

    let res = kb
        .recall(RecallParams {
            query: "build the binary",
            budget: 6000,
            trace: true,
            source: "cli",
            expand_deps: "false",
            refine_mode: "off",
            ..Default::default()
        })
        .unwrap();
    let log = kb
        .storage
        .get_episodic_log(&res.trace_id)
        .unwrap()
        .unwrap();
    assert_eq!(log["agent"].as_str(), Some("claude-code"));
    // Channel and agent are distinct dimensions.
    assert_eq!(log["event_source"].as_str(), Some("cli"));

    std::env::remove_var("INNATE_AGENT");
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
            min_score: None,
            session_only: false,
            ..Default::default()
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
            min_score: None,
            session_only: false,
            ..Default::default()
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
            min_score: None,
            session_only: false,
            ..Default::default()
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
            min_score: None,
            session_only: false,
            ..Default::default()
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
            min_score: None,
            session_only: false,
            ..Default::default()
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
            min_score: None,
            session_only: false,
            ..Default::default()
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
                min_score: None,
                session_only: false,
                ..Default::default()
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
            ..Default::default()
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

// 混合检索:词法/BM25 通道应召回「精确 token 命中、但语义嵌入(此处用哑向量)
// 命中不到」的 chunk —— 这正是向量检索对错误码/参数/符号名的盲区。
#[test]
fn lexical_channel_recovers_exact_token_match() {
    let (kb, _f) = tmp_kb();
    // 一堆干扰 chunk + 一个含罕见精确 token(错误码 E0599)的目标 chunk。
    for i in 0..5 {
        kb.add(
            &format!("general advice number {i} about unrelated topics"),
            "note",
            Some("misc guidance"),
            None,
            "manual",
            None,
        )
        .unwrap();
    }
    let target = kb
        .add(
            "Fix E0599 no-method-found by importing the relevant trait into scope",
            "note",
            Some("rust trait method resolution"),
            None,
            "manual",
            None,
        )
        .unwrap();

    // 用纯精确 token 查询 —— 哑向量不会把它排上来,词法通道必须命中。
    let r = kb
        .recall(RecallParams {
            query: "E0599",
            budget: 8000,
            trace: false,
            source: "sdk",
            ..Default::default()
        })
        .unwrap();
    let hit = r
        .knowledge
        .iter()
        .any(|c| c.get("id").and_then(|v| v.as_str()) == Some(target.as_str()));
    assert!(hit, "词法通道应召回含精确 token E0599 的 chunk");
}

// 部分 d:opt-in 重排器应在 rerank=true 时重排候选(此处用无网络的桩重排器,把指定
// id 顶到最前),且默认 rerank=false 时不触发(热路径保持无 LLM)。
struct PinReranker {
    first: String,
}
impl crate::refine::Reranker for PinReranker {
    fn rerank(&self, _q: &str, candidates: &[Value]) -> crate::errors::Result<Vec<String>> {
        let mut ids: Vec<String> = candidates
            .iter()
            .filter_map(|c| c["id"].as_str().map(str::to_string))
            .collect();
        ids.sort_by_key(|id| usize::from(*id != self.first));
        Ok(ids)
    }
}

#[test]
fn opt_in_reranker_reorders_shortlist() {
    let file = NamedTempFile::new().unwrap();
    let kb = KnowledgeBase::open_with(file.path(), None, None, None, None, None).unwrap();
    // Three chunks sharing a trigger word so all surface as candidates.
    let mut ids = Vec::new();
    for tag in ["alpha", "beta", "gamma"] {
        ids.push(
            kb.add(
                &format!("workflow step {tag} for the pipeline"),
                "note",
                Some("pipeline workflow step"),
                None,
                "manual",
                None,
            )
            .unwrap(),
        );
    }
    let pinned = ids[2].clone();
    let kb = kb.with_reranker(std::sync::Arc::new(PinReranker {
        first: pinned.clone(),
    }));

    let reranked = kb
        .recall(RecallParams {
            query: "pipeline workflow step",
            budget: 100_000,
            trace: false,
            source: "sdk",
            rerank: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        reranked.knowledge.first().and_then(|c| c["id"].as_str()),
        Some(pinned.as_str()),
        "rerank=true should pin the reranker's first id to the front"
    );
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
        min_score: None,
        session_only: false,
        ..Default::default()
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
        min_score: None,
        session_only: false,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn actr_activation_zero_for_unused_chunks() {
    use crate::kb::actr_activation;
    // Never-used knowledge contributes nothing → recall stays zero-regression
    // for freshly-added chunks (used_count == 0 or no last_used_at timestamp).
    let now = "2026-06-19T00:00:00.000Z";
    assert_eq!(
        actr_activation(0, Some("2026-06-18T00:00:00.000Z"), now),
        0.0
    );
    assert_eq!(actr_activation(5, None, now), 0.0);
}

#[test]
fn actr_activation_recency_and_frequency_monotonic() {
    use crate::kb::actr_activation;
    let now = "2026-06-19T00:00:00.000Z";
    let recent = actr_activation(3, Some("2026-06-18T00:00:00.000Z"), now); // 1 day ago
    let stale = actr_activation(3, Some("2026-03-19T00:00:00.000Z"), now); // ~3 months ago
                                                                           // More recent use ⇒ higher activation at equal frequency.
    assert!(
        recent > stale,
        "recent {recent} should exceed stale {stale}"
    );

    let used_once = actr_activation(1, Some("2026-06-18T00:00:00.000Z"), now);
    let used_many = actr_activation(20, Some("2026-06-18T00:00:00.000Z"), now);
    // More uses ⇒ higher activation at equal recency.
    assert!(
        used_many > used_once,
        "many {used_many} should exceed once {used_once}"
    );
}

#[test]
fn actr_activation_is_bounded_unit_interval() {
    use crate::kb::actr_activation;
    let now = "2026-06-19T00:00:00.000Z";
    // Extreme frequency, just used: still strictly below 1.0.
    let hot = actr_activation(100_000, Some("2026-06-19T00:00:00.000Z"), now);
    assert!(hot > 0.0 && hot < 1.0, "activation {hot} must be in (0,1)");
    // Single ancient use: still strictly above 0.0.
    let cold = actr_activation(1, Some("2000-01-01T00:00:00.000Z"), now);
    assert!(
        cold > 0.0 && cold < 1.0,
        "activation {cold} must be in (0,1)"
    );
}
