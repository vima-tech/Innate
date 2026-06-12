use super::*;

#[test]
fn governance_proposal_triggers_curate_archive() {
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add(
            "review-target content",
            "note",
            Some("review trigger"),
            None,
            "manual",
            None,
        )
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
        .add(
            "negative-feedback target",
            "note",
            Some("nf trigger"),
            None,
            "manual",
            None,
        )
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
        .add(
            "governance-evolve target",
            "note",
            Some("ge trigger"),
            None,
            "manual",
            None,
        )
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
    assert!(
        !requests.is_empty(),
        "governance evolve_request should be enqueued"
    );
}

#[test]
fn selected_unused_always_decreases_confidence() {
    let (kb, _file) = tmp_kb();
    // Start chunk at low confidence (below old target of 0.3).
    let chunk_id = kb
        .add(
            "low-conf chunk",
            "note",
            Some("lc trigger"),
            None,
            "manual",
            None,
        )
        .unwrap();
    kb.storage
        .update_chunk_confidence(
            &chunk_id,
            0.15,
            Some("test_setup"),
            &crate::utils::utc_now_iso(),
        )
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
    assert!(
        conf_after < 0.15,
        "confidence should decrease, got {conf_after}"
    );
}

// ── v4.8 feedback loop fix tests ──────────────────────────────────────────────

#[test]
fn distilled_chunk_can_auto_promote_via_implicit_signals() {
    // Verifies that a distilled chunk (conf=0.55, pending) can cross the 0.60
    // promote threshold using only implicit outcome=ok signals (no feedback_up).
    use crate::refine::{DistilledChunk, Distiller};
    struct ImmediateDistiller;
    impl Distiller for ImmediateDistiller {
        fn distill(
            &self,
            logs: &[serde_json::Value],
        ) -> crate::errors::Result<Vec<DistilledChunk>> {
            Ok(logs
                .iter()
                .map(|l| DistilledChunk {
                    content: "test principle from distillation".to_string(),
                    trigger_desc: Some("test trigger".to_string()),
                    anti_trigger_desc: None,
                    source_log_id: l["id"].as_str().unwrap_or("").to_string(),
                    nomination: None,
                })
                .collect())
        }
    }

    let f = NamedTempFile::new().unwrap();
    let kb = KnowledgeBase::open_with(
        f.path(),
        None,
        None,
        Some(std::sync::Arc::new(ImmediateDistiller)),
        None,
        None,
    )
    .unwrap();

    // Produce a distilled chunk: record → evolve → chunk in pending state.
    let trace_id = crate::utils::gen_uuid();
    kb.record(
        &trace_id,
        Some("test query"),
        None,
        Some("test summary"),
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

    let chunks = kb
        .storage
        .query_chunks_params(
            "SELECT id, confidence, state FROM chunks WHERE origin='distilled'",
            rusqlite::params![],
        )
        .unwrap();
    assert_eq!(chunks.len(), 1, "should have one distilled chunk");
    let chunk_id = chunks[0]["id"].as_str().unwrap().to_string();
    let initial_conf = chunks[0]["confidence"].as_f64().unwrap();
    assert!(
        (initial_conf - 0.55).abs() < 0.01,
        "initial conf should be 0.55, got {initial_conf}"
    );
    assert_eq!(chunks[0]["state"].as_str(), Some("pending"));

    // Simulate 5 successful uses across distinct traces to push confidence over 0.60.
    for i in 0..5 {
        let t = attributed_trace(&kb, &chunk_id);
        // Record usage: used=[chunk_id], outcome=ok — triggers implicit confidence nudge.
        kb.record(
            &t,
            Some(&format!("query variant {i}")),
            None,
            None,
            Some("ok"),
            Some(std::slice::from_ref(&chunk_id)),
            None,
            None,
            None,
            0,
            "sdk",
        )
        .unwrap();
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
    let chunk_id = kb
        .add(
            "idle chunk",
            "note",
            Some("idle trigger"),
            None,
            "manual",
            None,
        )
        .unwrap();

    // Manually set confidence below the new decay floor but above the old floor.
    // conf = 0.22 → below archive threshold (0.25) → 3a should archive it.
    let now = utc_now_iso();
    kb.storage
        .update_chunk_confidence(&chunk_id, 0.22, Some("test"), &now)
        .unwrap();
    // Set last_used_at to simulate 90+ days idle (cutoff = 60 days).
    kb.storage
        .query_chunks_params(
            "UPDATE chunks SET last_used_at=?, last_used_base=? WHERE id=?",
            rusqlite::params![
                "2020-01-01T00:00:00.000Z",
                "2020-01-01T00:00:00.000Z",
                chunk_id
            ],
        )
        .unwrap();

    let report = kb
        .builtin_curate_impl(&crate::kb::CurateScope::default())
        .unwrap();
    assert!(
        report.archived.contains(&chunk_id),
        "conf=0.22 idle chunk should be archived via 3a; archived={:?}",
        report.archived
    );

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
    let chunk_id = kb
        .add(
            "governance-ready target",
            "note",
            Some("gr trigger"),
            None,
            "manual",
            None,
        )
        .unwrap();

    // 3 feedback_down events: 2nd creates proposal, 3rd updates evidence_count to 3.
    for actor in ["reviewer-1", "reviewer-2", "reviewer-3"] {
        record_down_as(&kb, &chunk_id, actor);
    }

    // Should have exactly one evolve_request with reason='governance_ready'.
    let requests = kb.storage.query_chunks_params(
        "SELECT reason FROM evolve_requests WHERE state='pending' AND reason='governance_ready'",
        rusqlite::params![],
    ).unwrap();
    assert!(
        !requests.is_empty(),
        "governance_ready evolve_request should be enqueued after 3 downs on one chunk"
    );
}

// ── v4.9 feedback-loop fixes ─────────────────────────────────────────────────

#[test]
fn feedback_events_are_idempotent() {
    // Issue 1 fix: same (trace_id, chunk_id, signal) must not insert duplicate rows.
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add("idempotent target", "note", Some("t"), None, "manual", None)
        .unwrap();
    let trace_id = attributed_trace(&kb, &chunk_id);

    // Two record() calls with the same trace_id, same chunk, same signal.
    kb.record(
        &trace_id,
        Some("q"),
        None,
        None,
        None,
        None,
        None,
        Some(std::slice::from_ref(&chunk_id)),
        None,
        0,
        "sdk",
    )
    .unwrap();
    kb.record(
        &trace_id,
        Some("q"),
        None,
        None,
        None,
        None,
        None,
        Some(std::slice::from_ref(&chunk_id)),
        None,
        0,
        "sdk",
    )
    .unwrap();

    let count = kb
        .storage
        .query_chunks_params(
            "SELECT COUNT(*) AS cnt FROM feedback_events
         WHERE trace_id=? AND chunk_id=? AND signal='down'",
            rusqlite::params![trace_id, chunk_id],
        )
        .unwrap();
    assert_eq!(
        count[0]["cnt"].as_i64(),
        Some(1),
        "duplicate (trace_id, chunk_id, signal) must produce only one row"
    );
}

#[test]
fn positive_feedback_offsets_governance_proposal() {
    // Issue 2 fix: net_negative = downs - ups < 2 → no governance proposal.
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add(
            "net feedback target",
            "note",
            Some("t"),
            None,
            "manual",
            None,
        )
        .unwrap();

    // Two downs.
    for _ in 0..2 {
        let t = attributed_trace(&kb, &chunk_id);
        kb.record(
            &t,
            Some("q"),
            None,
            None,
            None,
            None,
            None,
            Some(std::slice::from_ref(&chunk_id)),
            None,
            0,
            "sdk",
        )
        .unwrap();
    }
    // One up — offsets one down, net = 1 < 2.
    let t = attributed_trace(&kb, &chunk_id);
    kb.record(
        &t,
        Some("q"),
        None,
        None,
        None,
        None,
        Some(std::slice::from_ref(&chunk_id)),
        None,
        None,
        0,
        "sdk",
    )
    .unwrap();

    let proposals = kb
        .storage
        .query_chunks_params(
            "SELECT COUNT(*) AS cnt FROM governance_proposals WHERE chunk_id=? AND state='pending'",
            rusqlite::params![chunk_id],
        )
        .unwrap();
    assert_eq!(
        proposals[0]["cnt"].as_i64(),
        Some(0),
        "positive feedback should cancel the pending governance proposal (net_negative=1 < 2)"
    );
}

#[test]
fn governance_proposal_rejected_for_protected_chunk() {
    // Issue 6 fix: protected chunks must have their proposal state='rejected' after curate,
    // not 'accepted', since they are never actually archived.
    let (kb, _file) = tmp_kb();
    // kind="skill" → protected=1.
    let chunk_id = kb
        .add(
            "protected knowledge",
            "skill",
            Some("t"),
            None,
            "manual",
            None,
        )
        .unwrap();

    // 3 downs → upsert proposal with evidence_count=3 (= governance_archive_threshold).
    for actor in ["reviewer-1", "reviewer-2", "reviewer-3"] {
        record_down_as(&kb, &chunk_id, actor);
    }

    kb.evolve("manual").unwrap();

    let chunk_rows = kb
        .storage
        .query_chunks_params(
            "SELECT state FROM chunks WHERE id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap();
    assert_eq!(
        chunk_rows[0]["state"].as_str(),
        Some("active"),
        "protected chunk must not be archived by governance"
    );

    let proposal_rows = kb
        .storage
        .query_chunks_params(
            "SELECT state FROM governance_proposals WHERE chunk_id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap();
    assert_eq!(
        proposal_rows[0]["state"].as_str(),
        Some("rejected"),
        "proposal for protected chunk must be rejected, not accepted"
    );
}

// ── v4.10 flywheel reliability fixes ─────────────────────────────────────────

#[test]
fn feedback_derived_updates_skipped_on_duplicate() {
    // Issue 1 fix: derived confidence/context updates must not run when INSERT OR IGNORE skips
    // the row (same trace_id + chunk_id + signal already exists).
    let (kb, _file) = tmp_kb();
    let chunk_id = kb
        .add(
            "dup feedback target",
            "note",
            Some("t"),
            None,
            "manual",
            None,
        )
        .unwrap();
    let initial_conf = kb
        .storage
        .query_chunks_params(
            "SELECT confidence FROM chunks WHERE id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap()[0]["confidence"]
        .as_f64()
        .unwrap();

    let trace_id = attributed_trace(&kb, &chunk_id);
    // Two down calls with the same trace_id — only first should update confidence.
    kb.record(
        &trace_id,
        Some("q"),
        None,
        None,
        None,
        None,
        None,
        Some(std::slice::from_ref(&chunk_id)),
        None,
        0,
        "sdk",
    )
    .unwrap();
    let conf_after_first = kb
        .storage
        .query_chunks_params(
            "SELECT confidence FROM chunks WHERE id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap()[0]["confidence"]
        .as_f64()
        .unwrap();
    assert!(
        conf_after_first < initial_conf,
        "first down must lower confidence"
    );

    kb.record(
        &trace_id,
        Some("q"),
        None,
        None,
        None,
        None,
        None,
        Some(std::slice::from_ref(&chunk_id)),
        None,
        0,
        "sdk",
    )
    .unwrap();
    let conf_after_second = kb
        .storage
        .query_chunks_params(
            "SELECT confidence FROM chunks WHERE id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap()[0]["confidence"]
        .as_f64()
        .unwrap();
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
    let chunk_id = kb
        .add("two-call target", "note", Some("t"), None, "manual", None)
        .unwrap();
    let trace_id = attributed_trace(&kb, &chunk_id);

    // Step 1: report used, no outcome.
    kb.record(
        &trace_id,
        Some("q"),
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
    let conf_before = kb
        .storage
        .query_chunks_params(
            "SELECT confidence FROM chunks WHERE id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap()[0]["confidence"]
        .as_f64()
        .unwrap();

    // Step 2: report outcome, no used — should still update confidence for the saved chunk.
    kb.record(
        &trace_id,
        Some("q"),
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
    let conf_after = kb
        .storage
        .query_chunks_params(
            "SELECT confidence FROM chunks WHERE id=?",
            rusqlite::params![chunk_id],
        )
        .unwrap()[0]["confidence"]
        .as_f64()
        .unwrap();
    assert!(
        conf_after > conf_before,
        "ok outcome must raise confidence even when used is omitted from the outcome call"
    );
}
