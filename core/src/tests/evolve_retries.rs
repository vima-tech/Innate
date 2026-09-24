use super::distillation::{CountingFailingDistiller, FailingDistiller};
use super::*;

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
    kb.storage
        .request_evolve(
            &crate::utils::gen_uuid(),
            "governance_ready",
            &crate::utils::utc_now_iso(),
        )
        .unwrap();
    let report = kb.evolve("threshold").unwrap();
    assert!(
        report.get("curate").map(|v| !v.is_null()).unwrap_or(false),
        "curate must run for threshold evolve even when below distill threshold"
    );
    let pending = kb
        .storage
        .query_chunks(
            "SELECT COUNT(*) AS cnt FROM evolve_requests
             WHERE state='pending' AND reason='governance_ready'",
        )
        .unwrap();
    assert_eq!(pending[0]["cnt"].as_i64(), Some(0));
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

    let state = kb
        .storage
        .query_chunks_params(
            "SELECT distill_state FROM episodic_log WHERE trace_id=?",
            rusqlite::params![trace_id],
        )
        .unwrap();
    assert_eq!(
        state[0]["distill_state"].as_str(),
        Some("discarded"),
        "old failed log must be retried in the same evolve cycle"
    );
}

#[test]
fn scheduled_evolve_recovers_retryable_failed_log_without_existing_request() {
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
    kb.record(RecordParams {
        trace_id: &trace_id,
        query: Some("automatic retry"),
        output: None,
        output_summary: Some("retryable material"),
        outcome: Some("ok"),
        used: None,
        feedback_up: None,
        feedback_down: None,
        nomination: Some("worth keeping: distillation fixture"),
        priority: 0,
        source: "sdk",
        ..Default::default()
    })
    .unwrap();
    kb.evolve("manual").unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    kb.storage
        .conn_execute(
            "UPDATE episodic_log
             SET distill_last_failed_at='2020-01-01T00:00:00.000Z'
             WHERE trace_id=?",
            rusqlite::params![trace_id],
        )
        .unwrap();
    kb.storage
        .conn_execute("DELETE FROM evolve_requests", [])
        .unwrap();

    kb.evolve("scheduled").unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(log["distill_attempts"].as_i64(), Some(2));
}

#[test]
fn distill_retry_cost_is_cumulative() {
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
        query: Some("cost retry"),
        output: None,
        output_summary: Some("cost material"),
        outcome: Some("ok"),
        used: None,
        feedback_up: None,
        feedback_down: None,
        nomination: Some("worth keeping: distillation fixture"),
        priority: 0,
        source: "sdk",
        ..Default::default()
    })
    .unwrap();
    kb.evolve("manual").unwrap();
    kb.storage
        .conn_execute(
            "UPDATE episodic_log
             SET distill_last_failed_at='2020-01-01T00:00:00.000Z'
             WHERE trace_id=?",
            rusqlite::params![trace_id],
        )
        .unwrap();
    kb.evolve("manual").unwrap();

    let usage = kb
        .storage
        .query_chunks_params(
            "SELECT COUNT(*) AS attempts,
                    SUM(prompt_tokens + completion_tokens) AS total
             FROM distill_token_usage
             WHERE log_id=(SELECT id FROM episodic_log WHERE trace_id=?)",
            rusqlite::params![trace_id],
        )
        .unwrap();
    assert_eq!(usage[0]["attempts"].as_i64(), Some(2));
    let latest = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    let latest_total = latest["distill_prompt_tokens"].as_i64().unwrap_or(0)
        + latest["distill_completion_tokens"].as_i64().unwrap_or(0);
    assert!(usage[0]["total"].as_i64().unwrap_or(0) > latest_total);
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
    kb.record(RecordParams {
        trace_id: &trace_id,
        query: Some("persistent failure"),
        output: None,
        output_summary: Some("reusable material"),
        outcome: Some("ok"),
        used: None,
        feedback_up: None,
        feedback_down: None,
        nomination: Some("worth keeping: distillation fixture"),
        priority: 0,
        source: "sdk",
        ..Default::default()
    })
    .unwrap();

    for attempt in 0..3 {
        // evolve() succeeds even on distillation failure; failure details stay in DB.
        kb.evolve("manual").unwrap();
        if attempt < 2 {
            kb.storage
                .conn_execute(
                    "UPDATE episodic_log
                     SET distill_accounted_at='2020-01-01T00:00:00.000Z',
                         distill_last_failed_at='2020-01-01T00:00:00.000Z'
                     WHERE trace_id=?",
                    rusqlite::params![trace_id],
                )
                .unwrap();
        }
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

// ── Requeue must not collide with idx_evolve_pending_reason ──
//
// Live incident 2026-09-23: an evolve killed while holding a `distill_retry` lease
// left a stale `running` row; the next evolve queued a new pending `distill_retry`.
// Lease recovery then reset the stale row to pending, hit the one-pending-per-reason
// unique index, and every later evolve failed on its first statement.

/// `day` sets `requested_at` to 2001-01-<day>, so a lower day is an older request.
fn insert_request(kb: &KnowledgeBase, id: &str, reason: &str, state: &str, day: u32) {
    let requested_at = format!("2001-01-{day:02}T00:00:00.000Z");
    kb.storage
        .conn_execute_count(
            "INSERT INTO evolve_requests(id, reason, state, requested_at, leased_at, completed_at,
                                         attempts, next_retry_at)
             VALUES (?1, ?2, ?3, ?4,
                     CASE WHEN ?3='running' THEN '2000-01-01T00:00:00.000Z' END,
                     CASE WHEN ?3='failed' THEN '2000-01-01T00:00:00.000Z' END,
                     1, NULL)",
            rusqlite::params![id, reason, state, requested_at],
        )
        .unwrap();
}

fn request_row(kb: &KnowledgeBase, id: &str) -> (String, String) {
    let rows = kb
        .storage
        .query_chunks_params(
            "SELECT state, COALESCE(note,'') AS note FROM evolve_requests WHERE id=?",
            rusqlite::params![id],
        )
        .unwrap();
    (
        rows[0]["state"].as_str().unwrap().to_string(),
        rows[0]["note"].as_str().unwrap().to_string(),
    )
}

#[test]
fn stale_lease_with_same_reason_pending_does_not_wedge_claim() {
    let (kb, _file) = tmp_kb();
    insert_request(&kb, "stale", "distill_retry", "running", 1);
    insert_request(&kb, "fresh", "distill_retry", "pending", 2);

    let now = crate::utils::utc_now_iso();
    let claim = kb
        .storage
        .claim_evolve_request_with_reason(&now, &now)
        .expect("claim must not fail on the one-pending-per-reason index")
        .expect("the pending request is claimable");
    assert_eq!(claim.id, "fresh");
    assert_eq!(
        request_row(&kb, "stale"),
        ("completed".into(), "lease_recovered_merged".into())
    );
}

#[test]
fn several_stale_leases_with_one_reason_requeue_only_the_oldest() {
    let (kb, _file) = tmp_kb();
    insert_request(&kb, "older", "distill_retry", "running", 1);
    insert_request(&kb, "newer", "distill_retry", "running", 2);

    let now = crate::utils::utc_now_iso();
    let claim = kb
        .storage
        .claim_evolve_request_with_reason(&now, &now)
        .unwrap()
        .unwrap();
    assert_eq!(claim.id, "older");
    assert_eq!(
        request_row(&kb, "newer"),
        ("completed".into(), "lease_recovered_merged".into())
    );
}

#[test]
fn retryable_failure_with_same_reason_pending_does_not_wedge_claim() {
    let (kb, _file) = tmp_kb();
    insert_request(&kb, "failed", "threshold", "failed", 1);
    insert_request(&kb, "queued", "threshold", "pending", 2);

    let now = crate::utils::utc_now_iso();
    let claim = kb
        .storage
        .claim_evolve_request_with_reason(&now, &now)
        .unwrap()
        .unwrap();
    assert_eq!(claim.id, "queued");
    assert_eq!(
        request_row(&kb, "failed"),
        ("completed".into(), "retry_failed_merged".into())
    );
}

#[test]
fn scheduled_evolve_recovers_from_the_wedged_queue_state() {
    let (kb, _file) = tmp_kb();
    insert_request(&kb, "stale", "distill_retry", "running", 1);
    insert_request(&kb, "fresh", "distill_retry", "pending", 2);
    kb.evolve("scheduled")
        .expect("evolve must survive a stale lease that shares a reason with a pending request");
    let pending_or_running = kb
        .storage
        .query_chunks(
            "SELECT COUNT(*) AS cnt FROM evolve_requests WHERE state IN ('pending','running')",
        )
        .unwrap();
    assert_eq!(pending_or_running[0]["cnt"].as_i64(), Some(0));
}
