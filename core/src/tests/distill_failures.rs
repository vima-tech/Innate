use super::*;

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
        kb.record(RecordParams {
            trace_id: &crate::utils::gen_uuid(),
            query: Some(query),
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
    kb.record(RecordParams {
        trace_id: &trace_id,
        query: Some("query only"),
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
