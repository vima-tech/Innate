//! Intuition layer guards (PRD/Spec/Tasklist M0–M4).
//!
//! These lock the three invariants: `recall()` zero-regression, `Verdict` carries no
//! answer text, and the synchronous appraise path is pure math (no LLM). Plus the
//! double-path context_key, caution flagging, override reflow, and honesty metrics.

use serde_json::Value;

use super::tmp_kb;
use crate::kb::{AppraiseParams, Situation};
use crate::{RecordParams, Tier, Valence};

const KEYS: &str = "stage,error_class,file_type";

fn add_active(kb: &crate::KnowledgeBase, content: &str, trigger: &str, anti: Option<&str>) -> String {
    kb.add(content, "note", Some(trigger), anti, "manual", None)
        .unwrap()
}

// ---------------------------------------------------------------------------
// T1.2 — recall stays zero-regression: a bare query's context_key still equals
// the legacy content_hash(normalize_query(query)), which Situation::from_query
// reproduces.
// ---------------------------------------------------------------------------
#[test]
fn recall_context_key_zero_regression() {
    let (kb, _f) = tmp_kb();
    add_active(&kb, "Use cargo build --release", "build the binary", None);
    let query = "How to build the Release binary?";
    let result = kb
        .recall(crate::RecallParams {
            query,
            budget: 6000,
            trace: true,
            source: "sdk",
            ..Default::default()
        })
        .unwrap();
    let log = kb.storage.get_episodic_log(&result.trace_id).unwrap().unwrap();
    let stored = log.get("context_key").and_then(Value::as_str).unwrap();
    let expected = Situation::from_query(query).context_key(KEYS);
    assert_eq!(stored, expected, "recall context_key must match legacy query hash");
}

// ---------------------------------------------------------------------------
// T0.2 — Verdict has no answer-bearing field, at the serialization layer.
// ---------------------------------------------------------------------------
#[test]
fn verdict_has_no_answer_field() {
    let (kb, _f) = tmp_kb();
    add_active(&kb, "Prefer immediate transactions", "sqlite writes", None);
    let actions: Vec<String> = vec![];
    let verdict = kb
        .appraise(AppraiseParams {
            situation: Situation {
                query: Some("should I batch these writes"),
                recent_actions: &actions,
                ..Default::default()
            },
            candidate: Some("yes, wrap in a single transaction"),
            trace: true,
            source: "sdk",
            ..Default::default()
        })
        .unwrap();
    let json = serde_json::to_value(&verdict).unwrap();
    let serialized = serde_json::to_string(&json).unwrap().to_lowercase();
    for banned in ["answer", "suggested_fix", "corrected", "\"fix\""] {
        assert!(
            !serialized.contains(banned),
            "Verdict must not carry answer text; found `{banned}`"
        );
    }
    assert!((0.0..=1.0).contains(&verdict.strength));
    assert!(!verdict.trace_id.is_empty());
}

// ---------------------------------------------------------------------------
// T2.2 / T2.3 — a chunk whose anti_trigger matches the situation flags Caution.
// ---------------------------------------------------------------------------
#[test]
fn caution_chunk_is_flagged() {
    let (kb, _f) = tmp_kb();
    // Failure-distilled style chunk: anti_trigger fires on "red reversal".
    add_active(
        &kb,
        "Avoid: blindly red-reversing invoices without checking the period lock",
        "invoice posting",
        Some("red reversal"),
    );
    let actions: Vec<String> = vec![];
    let verdict = kb
        .appraise(AppraiseParams {
            situation: Situation {
                query: Some("about to do a red reversal on this invoice"),
                stage: Some("implement"),
                recent_actions: &actions,
                ..Default::default()
            },
            candidate: None,
            min_strength: Some(0.0),
            trace: true,
            source: "sdk",
            ..Default::default()
        })
        .unwrap();
    assert!(
        matches!(verdict.valence, Valence::Caution | Valence::Mixed),
        "anti-trigger hit should yield caution, got {:?}",
        verdict.valence
    );
    assert!(
        !verdict.flagged_points.is_empty(),
        "caution should surface flagged_points"
    );
}

// ---------------------------------------------------------------------------
// T3.1 — override reflow: record(feedback='down') on the appraise trace lowers
// the chunk's confidence (the critic's calibration is corrected). Pure reuse of
// existing record semantics — implementation side is zero-new-code.
// ---------------------------------------------------------------------------
#[test]
fn override_reflow_lowers_confidence() {
    let (kb, _f) = tmp_kb();
    let cid = add_active(
        &kb,
        "Avoid: force-pushing to shared branches",
        "git push",
        Some("force push"),
    );
    let conf_before = kb
        .storage
        .get_chunk(&cid)
        .unwrap()
        .unwrap()
        .get("confidence")
        .and_then(Value::as_f64)
        .unwrap();

    let actions: Vec<String> = vec![];
    let verdict = kb
        .appraise(AppraiseParams {
            situation: Situation {
                query: Some("planning a force push to main"),
                recent_actions: &actions,
                ..Default::default()
            },
            candidate: None,
            min_strength: Some(0.0),
            trace: true,
            source: "sdk",
            ..Default::default()
        })
        .unwrap();
    assert!(verdict.contributors.iter().any(|c| c.chunk_id == cid));

    // Operator overrides the caution with a reason → flows back through record.
    kb.record(RecordParams {
        trace_id: &verdict.trace_id,
        feedback_down: Some(std::slice::from_ref(&cid)),
        feedback_reason: Some("safe in this isolated worktree"),
        source: "sdk",
        ..Default::default()
    })
    .unwrap();

    let conf_after = kb
        .storage
        .get_chunk(&cid)
        .unwrap()
        .unwrap()
        .get("confidence")
        .and_then(Value::as_f64)
        .unwrap();
    assert!(
        conf_after < conf_before,
        "override should lower confidence: {conf_before} -> {conf_after}"
    );
}

// ---------------------------------------------------------------------------
// T4.1 — inspect() surfaces intuition calibration once appraisals exist.
// ---------------------------------------------------------------------------
#[test]
fn inspect_reports_intuition_calibration() {
    let (kb, _f) = tmp_kb();
    add_active(&kb, "Pin the toolchain version", "ci config", None);
    let actions: Vec<String> = vec![];
    for i in 0..3 {
        let v = kb
            .appraise(AppraiseParams {
                situation: Situation {
                    query: Some("ci config drift"),
                    stage: Some("ci"),
                    recent_actions: &actions,
                    ..Default::default()
                },
                trace: true,
                source: "sdk",
                ..Default::default()
            })
            .unwrap();
        let outcome = if i == 0 { "fail" } else { "ok" };
        kb.record(RecordParams {
            trace_id: &v.trace_id,
            outcome: Some(outcome),
            source: "sdk",
            ..Default::default()
        })
        .unwrap();
    }
    let report = kb.inspect().unwrap();
    let cal = report.get("intuition_calibration").unwrap();
    assert_eq!(cal.get("appraisals").and_then(Value::as_i64).unwrap(), 3);
    assert!(cal.get("monotonicity_gap").is_some());
    assert!(cal.get("ece").is_some());
    assert!(cal.get("silence_rate").is_some());
    assert!(cal.get("buckets").and_then(Value::as_array).is_some());
}

// ---------------------------------------------------------------------------
// Tier banding sanity: strength below tier_weak → Weak.
// ---------------------------------------------------------------------------
#[test]
fn neutral_quiet_situation_is_weak() {
    let (kb, _f) = tmp_kb();
    // Empty KB → no contributors → strength 0 → Neutral / Weak (high silence).
    let actions: Vec<String> = vec![];
    let verdict = kb
        .appraise(AppraiseParams {
            situation: Situation {
                query: Some("totally unrelated ambient question"),
                recent_actions: &actions,
                ..Default::default()
            },
            trace: false,
            source: "sdk",
            ..Default::default()
        })
        .unwrap();
    assert_eq!(verdict.tier, Tier::Weak);
    assert_eq!(verdict.valence, Valence::Neutral);
}
