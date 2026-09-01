//! Regressions for the 2026-09 tuning pass.
//!
//! Each test pins one defect that was found by reading the live library rather
//! than the code, and that no existing test could have caught.

use super::*;
use crate::kb::DISTILLED_SEED_CONFIDENCE;

/// A never-used chunk that keeps being selected must eventually be archived --
/// the end-to-end version of the invariant above.
#[test]
fn repeatedly_selected_never_used_chunk_is_archived() {
    let (kb, _f) = tmp_kb();
    let chunk_id = kb
        .add("noisy status report", "note", Some("noise"), None, "manual", None)
        .unwrap();
    // Seed it the way distillation would, and give it the selection history of a
    // chunk that wins every recall and helps with nothing.
    kb.storage
        .conn_execute_count(
            // `selected_count` is re-derived from usage_trace by curate's
            // aggregate step, so the durable `_base` counters are what a
            // pre-seeded history has to go through.
            "UPDATE chunks SET state='pending', confidence=?,
                 selected_count=50, selected_count_base=50,
                 used_count=0, used_count_base=0
             WHERE id=?",
            rusqlite::params![DISTILLED_SEED_CONFIDENCE, chunk_id],
        )
        .unwrap();

    kb.builtin_curate_impl(&CurateScope::default()).unwrap();

    let state = kb.storage.get_chunk(&chunk_id).unwrap().unwrap()["state"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(state, "archived");
}

/// A time-based curate pass must not run on every scheduler tick. Unthrottled it
/// ran 39,383 times in 30 days -- a full aggregate/archive/promote/decay
/// transaction each time -- to distil 414 chunks.
#[test]
fn scheduled_curate_is_throttled_between_runs() {
    let (kb, _f) = tmp_kb();

    let first = kb.evolve("scheduled").unwrap();
    assert_ne!(
        first.get("skipped").and_then(Value::as_str),
        Some("curate_throttled"),
        "the first pass on a fresh library has no last_agg_ts and must run"
    );

    let second = kb.evolve("scheduled").unwrap();
    assert_eq!(
        second.get("skipped").and_then(Value::as_str),
        Some("curate_throttled")
    );
    assert!(second.get("curate_next_due_at").is_some());

    // A session-end (`manual`) evolve with nothing to distil is throttled the
    // same way. Session ends are frequent — leaving this path unguarded kept
    // curate running dozens of times a day for no state change.
    let manual = kb.evolve("manual").unwrap();
    assert_eq!(manual.get("distilled").and_then(Value::as_i64), Some(0));
    assert_eq!(
        manual.get("curate_skipped").and_then(Value::as_str),
        Some("throttled")
    );
}

/// A record naming one unattributable chunk must not discard the whole call.
/// Rejecting outright killed 16% of MCP records in a system whose scarcest
/// resource is feedback.
#[test]
fn record_keeps_attributable_ids_and_reports_the_rest() {
    let (kb, _f) = tmp_kb();
    let good = kb
        .add("real knowledge", "note", Some("real"), None, "manual", None)
        .unwrap();
    let stale = kb
        .add("never recalled", "note", Some("other"), None, "manual", None)
        .unwrap();
    let trace_id = attributed_trace(&kb, &good);

    let report = kb
        .record(RecordParams {
            trace_id: &trace_id,
            outcome: Some("ok"),
            used: Some(&[good.clone(), stale.clone()]),
            source: "mcp",
            ..Default::default()
        })
        .unwrap();

    assert_eq!(report.unattributed.len(), 1);
    assert_eq!(report.unattributed[0].chunk_id, stale);

    // The attributable half landed in full.
    let used_rows = kb
        .storage
        .query_chunks_params(
            "SELECT chunk_id FROM usage_trace WHERE trace_id=? AND event='used'",
            rusqlite::params![trace_id],
        )
        .unwrap();
    let used_ids: Vec<&str> = used_rows
        .iter()
        .filter_map(|r| r.get("chunk_id").and_then(Value::as_str))
        .collect();
    assert_eq!(used_ids, vec![good.as_str()]);
}

/// Long chunks match everything and convert worse; the fused score must reflect
/// that. Two chunks with the same head text, one padded, must not tie.
#[test]
fn length_penalty_demotes_a_padded_chunk() {
    let (kb, _f) = tmp_kb();
    let short = kb
        .add("alpha beta gamma", "note", Some("alpha"), None, "manual", None)
        .unwrap();
    let padded_content = format!("alpha beta gamma{}", " padding".repeat(400));
    let long = kb
        .add(&padded_content, "note", Some("alpha"), None, "manual", None)
        .unwrap();
    for id in [&short, &long] {
        kb.approve(id).unwrap();
    }

    let result = kb
        .recall(RecallParams {
            query: "alpha beta gamma",
            budget: 40_000,
            top: Some(10),
            source: "cli",
            ..Default::default()
        })
        .unwrap();

    let score_of = |id: &str| -> f64 {
        result
            .knowledge
            .iter()
            .find(|c| c.get("id").and_then(Value::as_str) == Some(id))
            .and_then(|c| c.get("_fused_score").and_then(Value::as_f64))
            .unwrap_or(f64::NAN)
    };
    assert!(
        score_of(&short) > score_of(&long),
        "the padded chunk must score lower: short={} long={}",
        score_of(&short),
        score_of(&long)
    );
}

/// The heuristic distiller copies a summary verbatim, so a summary announcing
/// that nothing was produced became a permanent, highly-recalled chunk.
#[test]
fn heuristic_distiller_skips_session_status_reports() {
    use crate::refine::HeuristicDistiller;

    let report = serde_json::json!({
        "id": "log-1",
        "query": "assess the rebuild",
        "output_summary": "Parked the rebuild pending review. \u{672a}\u{505a}\u{4efb}\u{4f55}\u{4ee3}\u{7801}\u{6539}\u{52a8}.",
    });
    let knowledge = serde_json::json!({
        "id": "log-2",
        "query": "fix pagination flicker",
        "output_summary": "Root cause: the :active transform forces reflow; animate opacity instead.",
    });

    let out = HeuristicDistiller.distill(&[report, knowledge]).unwrap();
    assert_eq!(out.len(), 1, "only the actionable summary should survive");
    assert!(out[0].content.contains("Root cause"));

    // An explicit nomination is a deliberate "keep this" and is never filtered.
    let nominated = serde_json::json!({
        "id": "log-3",
        "query": "q",
        "nomination": "\u{672a}\u{505a}\u{4efb}\u{4f55}\u{4ee3}\u{7801}\u{6539}\u{52a8} but a human asked to keep this",
    });
    assert_eq!(HeuristicDistiller.distill(&[nominated]).unwrap().len(), 1);
}

/// Chunk ids offered by the recall hook live inside a JSON-encoded message
/// body, where a newline is the two characters `\\n`. Scanning the raw file for a
/// marker that spans a newline matches nothing, so the "did the agent quote this
/// chunk" check found zero citations on every real transcript while passing
/// happily against a hand-written fixture with literal newlines.
#[test]
fn offered_chunk_ids_are_read_from_decoded_message_text() {
    let cid = "38af2cde-67b2-4a9f-a816-856096c31a1c";
    // Built through serde, exactly as a real transcript is written.
    let user = serde_json::json!({"message": {"role": "user", "content":
        format!("<innate-recall>\nrecalled 1 chunk\n\n- [{cid}] (confidence 0.5) x\n")}});
    let assistant = serde_json::json!({"message": {"role": "assistant", "content":
        [{"type": "text", "text": format!("Applied [{cid}] from the recalled set.")}]}});
    let transcript = format!("{user}\n{assistant}\n");

    // The raw file does not contain a real newline before "- [".
    assert!(!transcript.contains("\n- ["));

    let offered = crate::hook::uuids_after(&crate::hook::role_text(&transcript, None), "- [");
    assert_eq!(offered, vec![cid.to_string()]);

    let said = crate::hook::role_text(&transcript, Some("assistant"));
    assert!(said.contains(cid), "assistant text must carry the citation");
}

/// The Stop hook recovers this session's traces from the transcript, which is
/// the only place they are recorded.
#[test]
fn stop_hook_recovers_trace_ids_from_transcript() {
    let transcript = "some text\ntrace_id: 11111111-2222-3333-4444-555555555555\n\
                      noise\ntrace_id: aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\n\
                      trace_id: 11111111-2222-3333-4444-555555555555\n\
                      trace_id: not-a-uuid-at-all\n";
    let ids = crate::hook::uuids_after(transcript, "trace_id: ");
    assert_eq!(
        ids,
        vec![
            "11111111-2222-3333-4444-555555555555".to_string(),
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string(),
        ]
    );
}
