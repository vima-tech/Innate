//! Trace-semantics guard (Priority 1: clean observation data).
//!
//! `selected` must strictly mean "entered the model context". These tests lock
//! the three cases that previously polluted the feedback signal:
//!   1. a normal injected recall records `selected` events and opens a trace;
//!   2. a *session-only* recall (the daemon, which discards the knowledge) opens
//!      a trace for record-correlation but records **no** selection events;
//!   3. an *empty* recall is parked as a terminal `known_none`/`discarded` row
//!      (no-answer telemetry) and never enters the `open` pool or claims a
//!      `selected` chunk.

use super::*;

/// Count `usage_trace` rows of a given event kind for a trace.
fn event_count(kb: &KnowledgeBase, trace_id: &str, event: &str) -> i64 {
    kb.storage
        .query_chunks_params(
            "SELECT COUNT(*) AS cnt FROM usage_trace WHERE trace_id=? AND event=?",
            rusqlite::params![trace_id, event],
        )
        .unwrap()[0]["cnt"]
        .as_i64()
        .unwrap()
}

/// Read (distill_state, usage_state) for the episodic log of a trace.
fn log_states(kb: &KnowledgeBase, trace_id: &str) -> (String, String) {
    let rows = kb
        .storage
        .query_chunks_params(
            "SELECT distill_state, usage_state FROM episodic_log WHERE trace_id=?",
            rusqlite::params![trace_id],
        )
        .unwrap();
    assert_eq!(rows.len(), 1, "exactly one episodic log expected");
    (
        rows[0]["distill_state"].as_str().unwrap().to_string(),
        rows[0]["usage_state"].as_str().unwrap().to_string(),
    )
}

fn seed_chunk(kb: &KnowledgeBase) {
    // DummyEmbeddingProvider hashes the whole string, so an exact-text query
    // yields cosine 1.0 — enough to surface this chunk on a matching recall.
    kb.add(
        "alpha beta gamma",
        "note",
        Some("alpha beta gamma"),
        None,
        "manual",
        None,
    )
    .unwrap();
}

#[test]
fn injected_recall_records_selected_and_opens_trace() {
    let (kb, _f) = tmp_kb();
    seed_chunk(&kb);
    let res = kb
        .recall(RecallParams {
            query: "alpha beta gamma",
            budget: 4000,
            trace: true,
            source: "mcp",
            ..Default::default()
        })
        .unwrap();
    assert!(
        !res.knowledge.is_empty(),
        "matching query should surface the chunk"
    );
    assert!(
        event_count(&kb, &res.trace_id, "selected") >= 1,
        "injected recall records selected"
    );
    assert_eq!(
        log_states(&kb, &res.trace_id),
        ("open".into(), "unknown".into())
    );
}

#[test]
fn session_only_recall_opens_trace_without_selection() {
    let (kb, _f) = tmp_kb();
    seed_chunk(&kb);
    let res = kb
        .recall(RecallParams {
            query: "alpha beta gamma",
            budget: 4000,
            trace: true,
            source: "daemon",
            session_only: true,
            ..Default::default()
        })
        .unwrap();
    // The daemon still gets a trace_id to correlate a later record …
    assert!(!res.knowledge.is_empty());
    assert_eq!(
        log_states(&kb, &res.trace_id),
        ("open".into(), "unknown".into())
    );
    // … but claims nothing: no selected/retrieved events to inflate selected_count.
    assert_eq!(
        event_count(&kb, &res.trace_id, "selected"),
        0,
        "session trace must not select"
    );
    assert_eq!(
        event_count(&kb, &res.trace_id, "retrieved"),
        0,
        "session trace must not retrieve"
    );
}

#[test]
fn abandoned_session_only_trace_is_retired_immediately() {
    let (kb, _f) = tmp_kb();
    seed_chunk(&kb);
    let res = kb
        .recall(RecallParams {
            query: "alpha beta gamma",
            budget: 4000,
            trace: true,
            source: "daemon",
            session_only: true,
            ..Default::default()
        })
        .unwrap();

    kb.record(RecordParams {
        trace_id: &res.trace_id,
        task_state: Some("abandoned"),
        source: "daemon",
        ..Default::default()
    })
    .unwrap();

    let log = kb.storage.get_episodic_log(&res.trace_id).unwrap().unwrap();
    assert_eq!(log["task_state"].as_str(), Some("abandoned"));
    assert_eq!(log["distill_state"].as_str(), Some("discarded"));
    assert_eq!(log["distill_note"].as_str(), Some("abandoned"));
    assert_eq!(event_count(&kb, &res.trace_id, "selected"), 0);
}

/// A capture channel that cannot judge success still leaves a usable summary
/// behind. Distillation eligibility must follow the material, not the outcome.
#[test]
fn abandoned_session_with_material_is_distillable_without_an_outcome() {
    let (kb, _f) = tmp_kb();
    seed_chunk(&kb);
    let res = kb
        .recall(RecallParams {
            query: "alpha beta gamma",
            budget: 4000,
            trace: true,
            source: "hook",
            ..Default::default()
        })
        .unwrap();

    kb.record(RecordParams {
        trace_id: &res.trace_id,
        outcome: Some("unknown"),
        output_summary: Some("tried the flag, hit a dimension guard, worked around it"),
        task_state: Some("abandoned"),
        source: "hook",
        ..Default::default()
    })
    .unwrap();

    let log = kb.storage.get_episodic_log(&res.trace_id).unwrap().unwrap();
    assert_eq!(
        log["distill_state"].as_str(),
        Some("new"),
        "material from a channel that cannot judge success must still reach the distiller"
    );
}

/// The other half of the decoupling, and the invariant it must not break: an
/// unknown outcome may unlock distillation, but it must never move a score —
/// even when the caller also claims the chunk was used.
#[test]
fn unknown_outcome_unlocks_distillation_without_moving_confidence() {
    let (kb, _f) = tmp_kb();
    let chunk_id = kb
        .add(
            "alpha beta gamma",
            "note",
            Some("alpha beta gamma"),
            None,
            "manual",
            None,
        )
        .unwrap();
    let before = kb.storage.get_chunk(&chunk_id).unwrap().unwrap()["confidence"]
        .as_f64()
        .unwrap();

    let res = kb
        .recall(RecallParams {
            query: "alpha beta gamma",
            budget: 4000,
            trace: true,
            source: "hook",
            ..Default::default()
        })
        .unwrap();

    let used = vec![chunk_id.clone()];
    kb.record(RecordParams {
        trace_id: &res.trace_id,
        outcome: Some("unknown"),
        output_summary: Some("did some work, cannot tell whether it succeeded"),
        used: Some(&used),
        task_state: Some("abandoned"),
        source: "hook",
        ..Default::default()
    })
    .unwrap();

    let log = kb.storage.get_episodic_log(&res.trace_id).unwrap().unwrap();
    assert_eq!(log["distill_state"].as_str(), Some("new"));

    let after = kb.storage.get_chunk(&chunk_id).unwrap().unwrap()["confidence"]
        .as_f64()
        .unwrap();
    assert_eq!(
        after, before,
        "an unknown outcome must never move confidence, even when it unlocks distillation"
    );
}

#[test]
fn empty_recall_is_parked_as_known_none_not_open() {
    let (kb, _f) = tmp_kb();
    seed_chunk(&kb);
    // A non-matching query gated above the candidate's confidence-only score
    // yields zero surfaced knowledge.
    let res = kb
        .recall(RecallParams {
            query: "totally unrelated query text",
            budget: 4000,
            trace: true,
            source: "hook",
            min_score: Some(0.9),
            ..Default::default()
        })
        .unwrap();
    assert!(res.knowledge.is_empty(), "gated non-match should be empty");
    // Terminal, out of the `open` pool, no selection claimed.
    assert_eq!(
        log_states(&kb, &res.trace_id),
        ("discarded".into(), "known_none".into())
    );
    assert_eq!(event_count(&kb, &res.trace_id, "selected"), 0);
}

#[test]
fn repair_traces_strips_daemon_pollution_and_recomputes_selected() {
    let (kb, _f) = tmp_kb();
    let cid = kb
        .add(
            "repair target",
            "note",
            Some("repair target"),
            None,
            "manual",
            None,
        )
        .unwrap();
    let now = crate::utils::utc_now_iso();

    // Simulate pre-fix pollution: a daemon session trace that wrote a false
    // `selected` event + an `open` episodic log, plus one legitimate hook select.
    let daemon_trace = crate::utils::gen_uuid();
    let ins = |trace: &str, src: &str| {
        kb.storage
            .insert_usage_trace(
                trace,
                Some(&cid),
                "selected",
                1.0,
                None,
                None,
                None,
                Some(1),
                None,
                src,
                &now,
            )
            .unwrap();
    };
    ins(&daemon_trace, "daemon");
    ins(&crate::utils::gen_uuid(), "hook");
    kb.storage
        .upsert_episodic_log(&crate::storage::EpisodicLogRow {
            id: crate::utils::gen_uuid(),
            trace_id: daemon_trace.clone(),
            lib_id: kb.storage.lib_id().unwrap(),
            ts: now.clone(),
            query: Some("daemon session".into()),
            recall_snapshot: Some(r#"{"retrieved":["x"],"selected":["x"],"sparks":[]}"#.into()),
            event_source: "daemon".into(),
            task_state: "recalled".into(),
            usage_state: "unknown".into(),
            distill_state: "open".into(),
            ..Default::default()
        })
        .unwrap();
    kb.storage
        .conn_execute(
            "UPDATE chunks SET selected_count=2 WHERE id=?",
            rusqlite::params![cid],
        )
        .unwrap();

    // Dry-run reports but does not mutate.
    let dry = kb.repair_traces(true).unwrap();
    assert_eq!(dry.daemon_events_deleted, 1);
    assert_eq!(dry.selected_before, 2);
    assert_eq!(dry.selected_after, 1);
    assert_eq!(
        event_count(&kb, &daemon_trace, "selected"),
        1,
        "dry-run must not delete"
    );

    // Real run: daemon select gone, count recomputed to the lone hook select,
    // daemon open log retired, and a second run is a no-op (idempotent).
    let r = kb.repair_traces(false).unwrap();
    assert_eq!(r.daemon_events_deleted, 1);
    assert_eq!(r.open_logs_retired, 1);
    assert_eq!(event_count(&kb, &daemon_trace, "selected"), 0);
    assert_eq!(
        log_states(&kb, &daemon_trace),
        ("discarded".into(), "known_none".into())
    );
    let chunk = kb.storage.get_chunk(&cid).unwrap().unwrap();
    assert_eq!(chunk["selected_count"].as_i64(), Some(1));
    assert_eq!(
        kb.repair_traces(false).unwrap().daemon_events_deleted,
        0,
        "idempotent"
    );
}
