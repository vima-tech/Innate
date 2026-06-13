use super::*;

#[test]
fn stale_governance_proposal_expires() {
    // P2-B fix: proposals with insufficient evidence older than governance_proposal_max_age_days
    // must be rejected so they stop triggering repeated evolve cycles.
    use crate::utils::gen_uuid;

    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add(
            "controversial chunk",
            "note",
            Some("t"),
            None,
            "manual",
            None,
        )
        .unwrap();
    let proposal_id = gen_uuid();
    // Insert a pending proposal with only 1 evidence (< governance_archive_threshold=3),
    // created 60 days ago (> governance_proposal_max_age_days=30).
    kb.storage.conn_execute(
        "INSERT INTO governance_proposals(id, chunk_id, proposal_type, reason, evidence_count, state, created_at, updated_at)
         VALUES (?, ?, 'archive', 'test', 1, 'pending', '2020-01-01T00:00:00.000Z', '2020-01-01T00:00:00.000Z')",
        rusqlite::params![proposal_id, chunk_id],
    ).unwrap();

    kb.evolve("manual").unwrap();

    let rows = kb
        .storage
        .query_chunks_params(
            "SELECT state FROM governance_proposals WHERE id=?",
            rusqlite::params![proposal_id],
        )
        .unwrap();
    assert_eq!(
        rows[0]["state"].as_str(),
        Some("rejected"),
        "stale low-evidence proposal must be expired (rejected)"
    );
    // The chunk itself must NOT be archived since the proposal lacked evidence.
    let chunk_rows = kb
        .storage
        .query_chunks_params(
            "SELECT state FROM chunks WHERE id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap();
    assert_ne!(
        chunk_rows[0]["state"].as_str(),
        Some("archived"),
        "chunk must not be archived when proposal expires without sufficient evidence"
    );
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
    assert_eq!(
        count, 0,
        "old completed evolve_requests must be pruned by curate Step 9"
    );
}

#[test]
fn inspect_includes_pending_oldest_ts() {
    // P3-A fix: inspect must include pending_oldest_ts so stale pending debt is visible.
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add("old pending", "note", Some("t"), None, "manual", None)
        .unwrap();
    kb.storage
        .conn_execute(
            "UPDATE chunks SET state='pending', created_at='2020-01-01T00:00:00.000Z' WHERE id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap();

    let info = kb.inspect().unwrap();
    let oldest = &info["chunks"]["pending_oldest_ts"];
    assert!(
        !oldest.is_null(),
        "pending_oldest_ts must be present when pending chunks exist"
    );
    assert_eq!(
        oldest.as_str(),
        Some("2020-01-01T00:00:00.000Z"),
        "pending_oldest_ts must reflect the oldest pending chunk's created_at"
    );
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
    assert_eq!(
        initial, after_outcome,
        "unknown usage must not imply unused"
    );

    kb.record(
        &trace_id,
        None,
        None,
        None,
        None,
        Some(std::slice::from_ref(&chunk_id)),
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
        Some(std::slice::from_ref(&first)),
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
        Some(std::slice::from_ref(&second)),
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
        .claim_evolve_request_with_reason(&now, "1970-01-01T00:00:00.000Z")
        .unwrap()
        .unwrap()
        .id;
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
        .claim_evolve_request_with_reason("9999-01-01T00:00:00.000Z", "9999-01-01T00:00:00.000Z")
        .unwrap()
        .unwrap()
        .id;
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
            Some(std::slice::from_ref(&chunk_id)),
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
        .add(
            "stable aggregation",
            "note",
            Some("stable"),
            None,
            "manual",
            None,
        )
        .unwrap();
    let trace_id = attributed_trace(&kb, &chunk_id);

    kb.record_detailed(
        &trace_id,
        None,
        None,
        None,
        None,
        Some(std::slice::from_ref(&chunk_id)),
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
        Some(std::slice::from_ref(&first)),
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
        Some(std::slice::from_ref(&second)),
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
        .add(
            "partial second",
            "note",
            Some("second"),
            None,
            "manual",
            None,
        )
        .unwrap();
    let trace_id = attributed_trace_many(&kb, &[first.clone(), second.clone()]);

    for chunk_id in [&first, &second] {
        kb.record_detailed(
            &trace_id,
            None,
            None,
            None,
            None,
            Some(std::slice::from_ref(chunk_id)),
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
    assert_eq!(
        used,
        std::collections::HashSet::from([first.clone(), second.clone()])
    );

    kb.record_detailed(
        &trace_id,
        None,
        None,
        None,
        None,
        Some(std::slice::from_ref(&second)),
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
        Some(std::slice::from_ref(&first)),
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
        Some(std::slice::from_ref(&second)),
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
