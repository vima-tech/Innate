use super::*;

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
           event TEXT NOT NULL,
           ts TEXT NOT NULL
         );
         INSERT INTO usage_trace VALUES
           ('t1', 'c1', 'selected', '2026-01-01T00:00:00.000Z'),
           ('t2', 'c1', 'selected', '2026-01-01T00:00:00.000Z'),
           ('t1', 'c1', 'used', '2026-01-01T00:00:00.000Z'),
           ('t1', NULL, 'task_ok', '2026-01-01T00:00:00.000Z');
         CREATE TABLE episodic_log (
           id TEXT, trace_id TEXT, outcome TEXT,
           distill_state TEXT,
           distill_prompt_tokens INTEGER,
           distill_completion_tokens INTEGER,
           distill_accounted_at TEXT
         );
         INSERT INTO episodic_log VALUES
           ('l1', 't1', 'ok', 'distilled', NULL, NULL, NULL);
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
        vec!["4.11→4.12", "4.12→4.13", "4.13→4.14"]
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
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(values.0, 3);
    assert_eq!(values.1, 3);
    assert_eq!(values.2, 2);
    assert_eq!(values.3, None);
}

#[test]
fn migration_4_14_repairs_existing_4_13_baselines_and_cost_history() {
    let file = NamedTempFile::new().unwrap();
    {
        let kb = KnowledgeBase::open(file.path()).unwrap();
        let chunk_id = kb
            .add("migration repair", "note", None, None, "manual", None)
            .unwrap();
        let trace_id = attributed_trace(&kb, &chunk_id);
        kb.record(
            &trace_id,
            None,
            None,
            Some("repair material"),
            Some("ok"),
            Some(&[chunk_id.clone()]),
            None,
            None,
            None,
            0,
            "sdk",
        )
        .unwrap();
        kb.evolve("manual").unwrap();
        kb.storage
            .conn_execute(
                "UPDATE chunks
                 SET selected_count_base=selected_count,
                     used_count_base=used_count,
                     used_success_count_base=used_success_count,
                     last_used_base=last_used_at
                 WHERE id=?",
                rusqlite::params![chunk_id],
            )
            .unwrap();
        kb.storage
            .conn_execute("DROP TABLE distill_token_usage", [])
            .unwrap();
        kb.storage
            .conn_execute("ALTER TABLE chunks DROP COLUMN evidence_cutoff_at", [])
            .unwrap();
        kb.storage.set_meta("schema_version", "4.13").unwrap();
    }

    assert_eq!(
        crate::migrate::run_migrations(file.path()).unwrap(),
        vec!["4.13→4.14"]
    );
    let conn = rusqlite::Connection::open(file.path()).unwrap();
    let values = conn
        .query_row(
            "SELECT selected_count_base, used_count_base,
                    used_success_count_base, last_used_base
             FROM chunks WHERE content='migration repair'",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(values, (0, 0, 0, None));
    let cost_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM distill_token_usage", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(cost_rows > 0);
}

#[test]
fn decay_uses_last_decayed_at_for_incremental_delta() {
    // Issue 6 fix: two successive curate runs must not compound decay beyond the 90-day half-life.
    // If last_decayed_at is updated after first decay, the second run decays only the
    // incremental days since then, not the full idle period again.
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add("decay test", "note", Some("t"), None, "manual", None)
        .unwrap();
    // Simulate last_used_at 180 days ago (full 2× half-life idle).
    let old_ts = "2020-01-01T00:00:00.000Z";
    kb.storage
        .conn_execute(
            "UPDATE chunks SET last_used_at=? WHERE id=?",
            rusqlite::params![old_ts, chunk_id],
        )
        .unwrap();

    // First evolve: applies 180-day decay.
    kb.evolve("manual").unwrap();
    let conf1 = kb
        .storage
        .query_chunks_params(
            "SELECT confidence FROM chunks WHERE id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap()[0]["confidence"]
        .as_f64()
        .unwrap();

    // Second evolve (same day): last_decayed_at is today, delta_days ≈ 0 → no further decay.
    kb.evolve("manual").unwrap();
    let conf2 = kb
        .storage
        .query_chunks_params(
            "SELECT confidence FROM chunks WHERE id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap()[0]["confidence"]
        .as_f64()
        .unwrap();

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
    let chunk_id = kb
        .add("always fails", "note", Some("t"), None, "manual", None)
        .unwrap();
    // Force used_count=10, used_success_count=1 (10% success), confidence=0.22 (< 0.25 threshold).
    kb.storage
        .conn_execute(
            "UPDATE chunks SET used_count=10, used_count_base=10,
         used_success_count=1, used_success_count_base=1,
         confidence=0.22, confidence_base=0.22, \
         last_used_at=datetime('now') WHERE id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap();

    kb.evolve("manual").unwrap();

    let state = kb
        .storage
        .query_chunks_params(
            "SELECT state, state_reason FROM chunks WHERE id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap();
    assert_eq!(state[0]["state"].as_str(), Some("archived"));
    assert_eq!(
        state[0]["state_reason"].as_str(),
        Some("sustained_task_failure")
    );
}

#[test]
fn pending_chunk_with_sustained_failure_gets_archived() {
    // P1-A fix: curate 3f now covers state='pending' in addition to state='active'.
    // A pending chunk recalled frequently but never producing successes must be archived.
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add(
            "pending always fails",
            "note",
            Some("t"),
            None,
            "manual",
            None,
        )
        .unwrap();
    // Move to pending state (simulate distill output) and inject bad stats.
    kb.storage
        .conn_execute(
            "UPDATE chunks SET state='pending', used_count=10, used_count_base=10,
         used_success_count=0, used_success_count_base=0,
         confidence=0.22, confidence_base=0.22, \
         last_used_at=datetime('now') WHERE id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap();

    kb.evolve("manual").unwrap();

    let rows = kb
        .storage
        .query_chunks_params(
            "SELECT state, state_reason FROM chunks WHERE id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap();
    assert_eq!(
        rows[0]["state"].as_str(),
        Some("archived"),
        "pending chunk with sustained failure must be archived by curate 3f"
    );
    assert_eq!(
        rows[0]["state_reason"].as_str(),
        Some("sustained_task_failure")
    );
}
