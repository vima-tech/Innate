use super::*;

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
        .recall(RecallParams {
            query: "pending recall quality boundary",
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
        .recall(RecallParams {
            query: "pending recall quality boundary",
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
fn restore_starts_a_new_evidence_window() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add(
            "restored failure history",
            "note",
            Some("restore"),
            None,
            "manual",
            None,
        )
        .unwrap();
    kb.storage
        .conn_execute(
            "UPDATE chunks
             SET state='archived', state_reason='sustained_task_failure',
                 confidence=0.1, confidence_base=0.1,
                 selected_count=10, selected_count_base=0,
                 used_count=10, used_count_base=0,
                 used_success_count=0, used_success_count_base=0,
                 last_used_at='2020-01-01T00:00:00.000Z'
             WHERE id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap();
    for index in 0..10 {
        let trace_id = format!("old-restore-{index}");
        kb.storage
            .insert_usage_trace(
                &trace_id,
                Some(&chunk_id),
                "selected",
                1.0,
                None,
                None,
                None,
                Some(index + 1),
                None,
                "sdk",
                "2020-01-01T00:00:00.000Z",
            )
            .unwrap();
        kb.storage
            .insert_usage_trace(
                &trace_id,
                Some(&chunk_id),
                "used",
                0.3,
                None,
                None,
                None,
                None,
                Some("explicit"),
                "sdk",
                "2020-01-01T00:00:00.000Z",
            )
            .unwrap();
        kb.storage
            .insert_usage_trace(
                &trace_id,
                None,
                "task_fail",
                0.15,
                None,
                None,
                None,
                None,
                None,
                "sdk",
                "2020-01-01T00:00:00.000Z",
            )
            .unwrap();
    }

    kb.restore(&chunk_id).unwrap();
    kb.builtin_curate_impl(&CurateScope::default()).unwrap();

    let chunk = kb.storage.get_chunk(&chunk_id).unwrap().unwrap();
    assert_eq!(chunk["state"].as_str(), Some("active"));
    assert_eq!(chunk["selected_count"].as_i64(), Some(0));
    assert_eq!(chunk["used_count"].as_i64(), Some(0));
    assert_eq!(chunk["confidence"].as_f64(), Some(0.5));
    assert!(chunk["evidence_cutoff_at"].as_str().is_some());
}

#[test]
fn anonymous_feedback_does_not_create_governance_consensus() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add(
            "anonymous governance",
            "note",
            Some("anonymous"),
            None,
            "manual",
            None,
        )
        .unwrap();
    for _ in 0..5 {
        let trace_id = attributed_trace(&kb, &chunk_id);
        kb.record(RecordParams {
            trace_id: &trace_id,
            query: None,
            output: None,
            output_summary: None,
            outcome: None,
            used: None,
            used_attribution: "explicit",
            used_complete: Some(true),
            feedback_up: None,
            feedback_down: Some(std::slice::from_ref(&chunk_id)),
            feedback_kind: "user",
            feedback_actor: None,
            feedback_reason: None,
            nomination: None,
            priority: 0,
            task_state: None,
            source: "sdk",
            ..Default::default()
        })
        .unwrap();
    }

    let proposals = kb
        .storage
        .query_chunks_params(
            "SELECT COUNT(*) AS cnt FROM governance_proposals
             WHERE chunk_id=? AND state='pending'",
            rusqlite::params![chunk_id],
        )
        .unwrap();
    assert_eq!(proposals[0]["cnt"].as_i64(), Some(0));
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
        .add(
            "candidate only",
            "note",
            Some("candidate"),
            None,
            "manual",
            None,
        )
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
        .record(RecordParams {
            trace_id: &trace_id,
            query: None,
            output: None,
            output_summary: None,
            outcome: None,
            used: Some(&[chunk_id]),
            used_attribution: "explicit",
            used_complete: Some(true),
            feedback_up: None,
            feedback_down: None,
            feedback_kind: "user",
            feedback_actor: None,
            feedback_reason: None,
            nomination: None,
            priority: 0,
            task_state: None,
            source: "sdk",
            ..Default::default()
        })
        .unwrap_err();
    assert!(matches!(error, InnateError::InvalidState(_)));
}

#[test]
fn final_outcome_can_be_corrected_and_replayed() {
    let (kb, _file) = tmp_kb();
    let trace_id = crate::utils::gen_uuid();
    kb.record(RecordParams {
        trace_id: &trace_id,
        query: Some("outcome"),
        output: None,
        output_summary: Some("provisional"),
        outcome: Some("unknown"),
        used: None,
        feedback_up: None,
        feedback_down: None,
        nomination: None,
        priority: 0,
        source: "sdk",
        ..Default::default()
    })
    .unwrap();
    let provisional = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(provisional["task_state"].as_str(), Some("running"));
    assert_eq!(provisional["distill_state"].as_str(), Some("open"));
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
        source: "sdk",
        ..Default::default()
    })
    .unwrap();
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(log["outcome"].as_str(), Some("ok"));

    kb.record(RecordParams {
        trace_id: &trace_id,
        query: None,
        output: None,
        output_summary: None,
        outcome: Some("fail"),
        used: None,
        feedback_up: None,
        feedback_down: None,
        nomination: None,
        priority: 0,
        source: "sdk",
        ..Default::default()
    })
    .unwrap();
    let corrected = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(corrected["outcome"].as_str(), Some("fail"));
    let outcome_events = kb
        .storage
        .query_chunks_params(
            "SELECT event FROM usage_trace
             WHERE trace_id=? AND event IN ('task_ok','task_fail')",
            rusqlite::params![trace_id],
        )
        .unwrap();
    assert_eq!(outcome_events.len(), 1);
    assert_eq!(outcome_events[0]["event"].as_str(), Some("task_fail"));
}

#[test]
fn record_rejects_conflicting_feedback_for_same_chunk() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add(
            "feedback conflict",
            "note",
            Some("feedback"),
            None,
            "manual",
            None,
        )
        .unwrap();
    let trace_id = attributed_trace(&kb, &chunk_id);
    let error = kb
        .record(RecordParams {
            trace_id: &trace_id,
            query: None,
            output: None,
            output_summary: None,
            outcome: None,
            used: None,
            used_attribution: "explicit",
            used_complete: Some(true),
            feedback_up: Some(std::slice::from_ref(&chunk_id)),
            feedback_down: Some(std::slice::from_ref(&chunk_id)),
            feedback_kind: "user",
            feedback_actor: None,
            feedback_reason: None,
            nomination: None,
            priority: 0,
            task_state: None,
            source: "sdk",
            ..Default::default()
        })
        .unwrap_err();
    assert!(matches!(error, InnateError::InvalidState(_)));
}

#[test]
fn curate_keeps_confidence_base_stable_for_replayable_evidence() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add(
            "confidence replay",
            "note",
            Some("confidence"),
            None,
            "manual",
            None,
        )
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
        .add(
            "unused evidence",
            "note",
            Some("unused"),
            None,
            "manual",
            None,
        )
        .unwrap();
    let trace_id = attributed_trace(&kb, &chunk_id);

    kb.record(RecordParams {
        trace_id: &trace_id,
        query: None,
        output: None,
        output_summary: None,
        outcome: None,
        used: Some(&[]),
        used_attribution: "explicit",
        used_complete: Some(true),
        feedback_up: None,
        feedback_down: None,
        feedback_kind: "user",
        feedback_actor: None,
        feedback_reason: None,
        nomination: None,
        priority: 0,
        task_state: Some("running"),
        source: "sdk",
        ..Default::default()
    })
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
        .add(
            "correctable feedback",
            "note",
            Some("feedback"),
            None,
            "manual",
            None,
        )
        .unwrap();
    let trace_id = attributed_trace(&kb, &chunk_id);

    kb.record(RecordParams {
        trace_id: &trace_id,
        query: None,
        output: None,
        output_summary: None,
        outcome: None,
        used: None,
        feedback_up: None,
        feedback_down: Some(std::slice::from_ref(&chunk_id)),
        nomination: None,
        priority: 0,
        source: "sdk",
        ..Default::default()
    })
    .unwrap();
    kb.record(RecordParams {
        trace_id: &trace_id,
        query: None,
        output: None,
        output_summary: None,
        outcome: None,
        used: None,
        feedback_up: Some(std::slice::from_ref(&chunk_id)),
        feedback_down: None,
        nomination: None,
        priority: 0,
        source: "sdk",
        ..Default::default()
    })
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
