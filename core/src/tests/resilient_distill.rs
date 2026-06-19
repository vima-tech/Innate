//! Tests for #3 Phase 1 — deterministic fallback distillation (`ResilientDistiller`),
//! plus an empirical assessment of whether Phase 2 (in-batch recurrence-aware
//! confidence) is worth building.
//!
//! Design: `docs/Innate-设计-确定性兜底蒸馏-v1.md`.

use super::*;

use crate::refine::{HeuristicDistiller, ResilientDistiller};
use serde_json::json;

/// Primary distiller that always errors — stands in for an unavailable LLM.
struct AlwaysFailDistiller;
impl Distiller for AlwaysFailDistiller {
    fn distill(&self, _logs: &[Value]) -> Result<Vec<DistilledChunk>> {
        Err(InnateError::Other("primary offline".to_string()))
    }
}

/// Primary distiller that always succeeds — to prove success never falls back.
struct OkDistiller;
impl Distiller for OkDistiller {
    fn distill(&self, logs: &[Value]) -> Result<Vec<DistilledChunk>> {
        Ok(vec![DistilledChunk {
            content: "primary".to_string(),
            source_log_id: logs[0]["id"].as_str().unwrap_or("").to_string(),
            ..Default::default()
        }])
    }
}

// ── Phase 1: decision logic (unit) ─────────────────────────────────────────

#[test]
fn resilient_falls_back_only_after_budget_exhausted() {
    let r = ResilientDistiller::new(
        Arc::new(AlwaysFailDistiller),
        Arc::new(HeuristicDistiller),
        2,
    );

    // attempts(0) < budget(2): primary error propagates so the retry machinery
    // gets to give the LLM another chance — no premature fallback.
    let fresh = json!({"id": "L1", "query": "q", "output_summary": "do X then Y", "distill_attempts": 0});
    assert!(r.distill_with_context(&fresh, std::slice::from_ref(&fresh)).is_err());

    // attempts(2) >= budget(2): primary error triggers deterministic fallback,
    // tagged so the chunk's provenance is honest.
    let exhausted = json!({"id": "L1", "query": "q", "output_summary": "do X then Y", "distill_attempts": 2});
    let chunks = r
        .distill_with_context(&exhausted, std::slice::from_ref(&exhausted))
        .unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(
        chunks[0].provider_override.as_deref(),
        Some("heuristic_fallback")
    );

    // A primary *success* is always used as-is, even past budget — never falls back.
    let r_ok = ResilientDistiller::new(Arc::new(OkDistiller), Arc::new(HeuristicDistiller), 0);
    let chunks = r_ok
        .distill_with_context(&exhausted, std::slice::from_ref(&exhausted))
        .unwrap();
    assert_eq!(chunks[0].content, "primary");
    assert_eq!(chunks[0].provider_override, None);
}

// ── Phase 1: end-to-end through evolve ──────────────────────────────────────

#[test]
fn evolve_creates_chunk_via_fallback_when_llm_unavailable() {
    // budget=0 ⇒ fall back on the very first failure: knowledge is still created.
    let file = NamedTempFile::new().unwrap();
    let distiller = Arc::new(ResilientDistiller::new(
        Arc::new(AlwaysFailDistiller),
        Arc::new(HeuristicDistiller),
        0,
    ));
    let kb =
        KnowledgeBase::open_with(file.path(), None, None, Some(distiller), None, None).unwrap();

    let trace_id = crate::utils::gen_uuid();
    kb.record(RecordParams {
        trace_id: &trace_id,
        query: Some("how to deploy"),
        output_summary: Some("run migrations then restart service"),
        outcome: Some("ok"),
        source: "sdk",
        ..Default::default()
    })
    .unwrap();
    kb.evolve("manual").unwrap();

    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(
        log["distill_state"].as_str(),
        Some("distilled"),
        "fallback must complete distillation rather than leaving the log failed"
    );

    let chunks = kb
        .storage
        .query_chunks("SELECT distill_provider, state FROM chunks WHERE origin='distilled'")
        .unwrap();
    assert_eq!(chunks.len(), 1, "fallback should create exactly one chunk");
    assert_eq!(
        chunks[0]["distill_provider"].as_str(),
        Some("heuristic_fallback")
    );
    assert_eq!(
        chunks[0]["state"].as_str(),
        Some("pending"),
        "fallback chunk stays gated (pending), not auto-trusted"
    );
}

#[test]
fn evolve_retries_llm_before_falling_back() {
    // budget=2 ⇒ while LLM attempts remain, a failure must NOT fall back; the log
    // goes 'failed' for retry. Only once attempts reach the budget does the
    // deterministic fallback rescue it.
    let file = NamedTempFile::new().unwrap();
    let distiller = Arc::new(ResilientDistiller::new(
        Arc::new(AlwaysFailDistiller),
        Arc::new(HeuristicDistiller),
        2,
    ));
    let kb =
        KnowledgeBase::open_with(file.path(), None, None, Some(distiller), None, None).unwrap();

    let trace_id = crate::utils::gen_uuid();
    kb.record(RecordParams {
        trace_id: &trace_id,
        query: Some("q"),
        output_summary: Some("material to keep"),
        outcome: Some("ok"),
        source: "sdk",
        ..Default::default()
    })
    .unwrap();

    // First attempt: attempts(0) < budget(2) → error propagates → failed, no chunk.
    kb.evolve("manual").unwrap();
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(log["distill_state"].as_str(), Some("failed"));
    assert_eq!(log["distill_attempts"].as_i64(), Some(1));
    assert_eq!(
        kb.storage
            .query_chunks("SELECT id FROM chunks WHERE origin='distilled'")
            .unwrap()
            .len(),
        0,
        "must not fall back while LLM budget remains"
    );

    // Simulate LLM retries exhausted (attempts reached budget, log back to 'new').
    kb.storage
        .conn_execute(
            "UPDATE episodic_log SET distill_state='new', distill_attempts=2 WHERE trace_id=?",
            rusqlite::params![trace_id],
        )
        .unwrap();
    kb.evolve("manual").unwrap();
    let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
    assert_eq!(log["distill_state"].as_str(), Some("distilled"));
    let chunks = kb
        .storage
        .query_chunks("SELECT distill_provider FROM chunks WHERE origin='distilled'")
        .unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(
        chunks[0]["distill_provider"].as_str(),
        Some("heuristic_fallback")
    );
}

// ── Phase 2 assessment: is in-batch recurrence-aware confidence worth it? ────

/// Records the same-context cluster size (`related_logs.len()`) the distiller
/// sees per call — the exact signal Phase 2 would key on.
struct ClusterSizeProbe {
    sizes: Arc<Mutex<Vec<usize>>>,
}
impl Distiller for ClusterSizeProbe {
    fn distill(&self, logs: &[Value]) -> Result<Vec<DistilledChunk>> {
        Ok(logs
            .iter()
            .filter_map(|l| {
                l["id"].as_str().map(|id| DistilledChunk {
                    content: format!("c-{id}"),
                    source_log_id: id.to_string(),
                    ..Default::default()
                })
            })
            .collect())
    }
    fn distill_with_context(&self, primary: &Value, related: &[Value]) -> Result<Vec<DistilledChunk>> {
        self.sizes.lock().unwrap().push(related.len());
        let id = primary["id"].as_str().unwrap_or("").to_string();
        Ok(vec![DistilledChunk {
            content: format!("c-{id}"),
            source_log_id: id,
            ..Default::default()
        }])
    }
}

fn max_cluster_size(per_session: bool) -> usize {
    let file = NamedTempFile::new().unwrap();
    let sizes = Arc::new(Mutex::new(Vec::new()));
    let kb = KnowledgeBase::open_with(
        file.path(),
        None,
        None,
        Some(Arc::new(ClusterSizeProbe {
            sizes: Arc::clone(&sizes),
        })),
        None,
        None,
    )
    .unwrap();
    // 3 topics × 3 recurrences. Same query ⇒ same context_key.
    for _ in 0..3 {
        for q in ["topic alpha", "topic beta", "topic gamma"] {
            kb.record(RecordParams {
                trace_id: &crate::utils::gen_uuid(),
                query: Some(q),
                output_summary: Some("reusable material"),
                outcome: Some("ok"),
                source: "sdk",
                ..Default::default()
            })
            .unwrap();
            if per_session {
                kb.evolve("manual").unwrap(); // realistic Stop-hook cadence: evolve each session
            }
        }
    }
    if !per_session {
        kb.evolve("manual").unwrap(); // deferred: one batch over everything
    }
    let v = sizes.lock().unwrap().clone();
    v.into_iter().max().unwrap_or(0)
}

#[test]
fn phase2_inbatch_recurrence_is_a_no_op_under_realistic_cadence() {
    // Phase 2's only unique offering is boosting initial confidence for patterns
    // that RECUR within one distill batch. This measures whether that even
    // happens. Under per-session evolve (the Stop-hook default), each batch drains
    // a single new log → cluster size 1 → zero recurrence signal → Phase 2 is a
    // no-op. Recurrence only appears when batching is deliberately deferred — and
    // even there, cross-session recurrence is ALREADY handled globally by the
    // usage_trace → curate → confidence → promote path, which is cadence-
    // independent. Verdict: Phase 2 not worth building.
    let per_session = max_cluster_size(true);
    let deferred = max_cluster_size(false);
    eprintln!(
        "[phase2] max in-batch same-context cluster: per_session={per_session} deferred={deferred}"
    );
    assert_eq!(
        per_session, 1,
        "per-session evolve ⇒ no in-batch recurrence ⇒ Phase 2 adds nothing"
    );
    assert!(
        deferred >= 2,
        "in-batch recurrence only exists when batching is deferred (the non-default regime)"
    );
}
