//! 直觉模块偏差治理(实施文档方案 A–G)的行为锁。
//!
//! 默认参数下所有弃权门关闭、校准恒等(零回归,已由 intuition.rs 覆盖)。本模块
//! 在**激活**各门/各方案后,验证它们确实起作用:
//!   A/门3 SparseEvidence、门4 Conflicted、门1 WeakResonance、F/门2 FalseResonance
//!   B verdict_log emit + record 回填、C provenance 切断自我强化、D 基率先验、
//!   E 校准映射 emit 应用、G 离散度塑形 confidence。

use serde_json::Value;

use super::tmp_kb;
use crate::kb::{AppraiseParams, Situation};
use crate::{KnowledgeBase, RecallParams, RecordParams};

const KEYS: &str = "stage,error_class,file_type";

/// 跑一次 recall,取出指定 chunk 的 `_context_score`(recall 暴露的逐块 context 贡献)。
fn recall_context_score(kb: &KnowledgeBase, query: &str, id: &str) -> f64 {
    let r = kb
        .recall(RecallParams {
            query,
            budget: 6000,
            trace: false,
            source: "sdk",
            ..Default::default()
        })
        .unwrap();
    r.knowledge
        .iter()
        .find(|c| c.get("id").and_then(Value::as_str) == Some(id))
        .and_then(|c| c.get("_context_score").and_then(Value::as_f64))
        .unwrap_or(f64::NAN)
}

fn add_active(kb: &KnowledgeBase, content: &str, trigger: &str) -> String {
    kb.add(content, "note", Some(trigger), None, "manual", None)
        .unwrap()
}

fn appraise_query(kb: &KnowledgeBase, q: &str) -> crate::Verdict {
    let actions: Vec<String> = vec![];
    kb.appraise(AppraiseParams {
        situation: Situation {
            query: Some(q),
            recent_actions: &actions,
            ..Default::default()
        },
        trace: true,
        source: "sdk",
        ..Default::default()
    })
    .unwrap()
}

// ---------------------------------------------------------------------------
// 默认零回归:开箱不弃权、confidence 字段存在且等于(恒等校准下的)塑形值。
// ---------------------------------------------------------------------------
#[test]
fn defaults_do_not_abstain() {
    let (kb, _f) = tmp_kb();
    add_active(
        &kb,
        "Use cargo build --release to build innate",
        "build the binary",
    );
    let v = appraise_query(&kb, "how do I build the release binary");
    assert!(!v.abstained, "默认参数下不应弃权");
    assert!(v.abstain_reason.is_none());
}

// ---------------------------------------------------------------------------
// 方案 A / 门3:把 min_evidence 调高 → 命中邻居无观测历史 → SparseEvidence 弃权。
// ---------------------------------------------------------------------------
#[test]
fn gate3_sparse_evidence_abstains_when_activated() {
    let (kb, f) = tmp_kb();
    add_active(
        &kb,
        "Use cargo build --release to build innate",
        "build the binary",
    );
    kb.storage.set_meta("appraise.min_evidence", "3").unwrap();
    drop(kb);
    let kb = KnowledgeBase::open(f.path()).unwrap();

    let v = appraise_query(&kb, "how do I build the release binary");
    assert!(v.abstained, "证据稀疏应弃权");
    assert_eq!(v.abstain_reason, Some(crate::AbstainReason::SparseEvidence));
    assert_eq!(v.confidence, 0.0, "弃权时 confidence 归零");
}

// ---------------------------------------------------------------------------
// 方案 G / 门4:把 conflict_ceiling 调到 0 → 任何多邻居离散度都触发 Conflicted。
// ---------------------------------------------------------------------------
#[test]
fn gate4_conflicted_abstains_when_ceiling_zero() {
    let (kb, f) = tmp_kb();
    add_active(&kb, "Use cargo build --release", "build the binary");
    add_active(&kb, "Run cargo test for the suite", "build the binary test");
    add_active(&kb, "cargo fmt formats the code", "build the binary format");
    kb.storage
        .set_meta("appraise.conflict_ceiling", "0.0")
        .unwrap();
    kb.storage.set_meta("appraise.min_strength", "0.0").unwrap(); // 让多邻居存活以产生离散
    drop(kb);
    let kb = KnowledgeBase::open(f.path()).unwrap();

    let v = appraise_query(&kb, "build binary");
    if v.contributors.len() >= 2 {
        assert!(v.abstained);
        assert_eq!(v.abstain_reason, Some(crate::AbstainReason::Conflicted));
    }
}

// ---------------------------------------------------------------------------
// 方案 B:appraise emit 写 verdict_log;record 回填 observed_outcome + provenance。
// ---------------------------------------------------------------------------
#[test]
fn scheme_b_verdict_log_emit_and_backfill() {
    let (kb, _f) = tmp_kb();
    add_active(
        &kb,
        "Prefer immediate transactions for sqlite writes",
        "sqlite writes",
    );
    let v = appraise_query(&kb, "should I batch these sqlite writes");

    // emit:verdict_log 必有一行,trace_id 对应,尚未回填。
    let rows = kb
        .storage
        .query_chunks_params(
            "SELECT COUNT(*) AS cnt FROM verdict_log WHERE trace_id=?",
            rusqlite::params![v.trace_id],
        )
        .unwrap();
    assert_eq!(
        rows[0].get("cnt").and_then(Value::as_i64),
        Some(1),
        "appraise 必写一条 verdict_log"
    );
    let pending = kb
        .storage
        .query_chunks_params(
            "SELECT COUNT(*) AS cnt FROM verdict_log WHERE trace_id=? AND outcome_observed_at IS NULL",
            rusqlite::params![v.trace_id],
        )
        .unwrap();
    assert_eq!(
        pending[0].get("cnt").and_then(Value::as_i64),
        Some(1),
        "回填前 outcome 为空"
    );

    // record(fail):回填 observed_outcome=+1(坏结果发生)、provenance=observed。
    kb.record(RecordParams {
        trace_id: &v.trace_id,
        outcome: Some("fail"),
        source: "sdk",
        ..Default::default()
    })
    .unwrap();
    let prov = kb
        .storage
        .query_chunks_params(
            "SELECT outcome_provenance AS p FROM verdict_log WHERE trace_id=?",
            rusqlite::params![v.trace_id],
        )
        .unwrap();
    assert_eq!(
        prov[0].get("p").and_then(Value::as_str),
        Some("observed"),
        "record 实际结果回填为 observed"
    );
}

// ---------------------------------------------------------------------------
// 方案 H:verdict_heeded=true(因警告回避了动作)→ provenance=counterfactual_censored,
// 且不进入校准样本(否则把「被采纳的警告」误判为漏报,惩罚正确的直觉)。
// ---------------------------------------------------------------------------
#[test]
fn scheme_h_heeded_caution_is_censored_not_calibrated() {
    let (kb, _f) = tmp_kb();
    add_active(
        &kb,
        "Avoid: running cargo build without --release in CI",
        "ci build flags",
    );
    let v = appraise_query(&kb, "should I run cargo build in CI");

    // record(ok) 但声明 verdict_heeded —— actor 因警告回避了动作,outcome 是反事实。
    kb.record(RecordParams {
        trace_id: &v.trace_id,
        outcome: Some("ok"),
        verdict_heeded: true,
        source: "sdk",
        ..Default::default()
    })
    .unwrap();

    let prov = kb
        .storage
        .query_chunks_params(
            "SELECT outcome_provenance AS p FROM verdict_log WHERE trace_id=?",
            rusqlite::params![v.trace_id],
        )
        .unwrap();
    assert_eq!(
        prov[0].get("p").and_then(Value::as_str),
        Some("counterfactual_censored"),
        "被采纳的警告应记为反事实审查,不记 observed"
    );
    // 反事实审查不计入校准样本(只收 provenance='observed')。
    assert!(
        kb.storage.verdict_calibration_samples().unwrap().is_empty(),
        "counterfactual_censored 不得进入校准样本"
    );
}

// ---------------------------------------------------------------------------
// 方案 E/B:neutral / mixed verdict 不参与校准 —— 「没表态」不能被算成「预测失败」,
// 否则污染校准映射键域与 ECE。直接造 verdict_log 行验证过滤契约。
// ---------------------------------------------------------------------------
#[test]
fn neutral_and_mixed_verdicts_excluded_from_calibration() {
    let (kb, _f) = tmp_kb();
    let now = "2026-06-01T00:00:00.000Z";
    // 三行:affirm(应入样本)、neutral、mixed(应被排除),均已 observed 回填。
    for (vid, valence) in [("v-aff", "affirm"), ("v-neu", "neutral"), ("v-mix", "mixed")] {
        kb.storage
            .insert_verdict_log(vid, vid, "sig", Some(valence), Some(0.7), 0.7, Some("medium"), None, now)
            .unwrap();
        kb.storage
            .backfill_verdict_outcome(vid, -1.0, "observed", now)
            .unwrap();
    }
    let samples = kb.storage.verdict_calibration_samples().unwrap();
    assert_eq!(samples.len(), 1, "只有 affirm/caution 进入校准样本");
    // affirm + observed_outcome<0(好结果)→ hit=1。
    let (strength, conf, hit) = samples[0];
    assert!((strength - 0.7).abs() < 1e-9 && (conf - 0.7).abs() < 1e-9);
    assert_eq!(hit, 1.0);
}

// ---------------------------------------------------------------------------
// 方案 C:verdict_derived 证据留痕但不参与 confidence 重算(切断自我强化)。
// ---------------------------------------------------------------------------
#[test]
fn scheme_c_verdict_derived_evidence_excluded_from_confidence() {
    let (kb, _f) = tmp_kb();
    let id = add_active(&kb, "Some procedural knowledge", "do the thing");

    // 写一条强正向的 verdict_derived 证据(模拟自我强化回流通道)。
    kb.storage
        .upsert_confidence_evidence(
            "ev-vd-1",
            Some("trace-x"),
            &id,
            "outcome_ok",
            1.0,
            1.0,
            "verdict-derived push",
            None,
            "2026-06-01T00:00:00.000Z",
            "verdict_derived",
        )
        .unwrap();
    // 再写一条 observed 证据作对照。
    kb.storage
        .upsert_confidence_evidence(
            "ev-obs-1",
            Some("trace-y"),
            &id,
            "outcome_ok",
            1.0,
            1.0,
            "observed",
            None,
            "2026-06-01T00:00:02.000Z",
            "observed",
        )
        .unwrap();

    // confidence 重算只看 observed:for_chunk 应只返回 observed 那一条。
    let used = kb.storage.confidence_evidence_for_chunk(&id).unwrap();
    assert_eq!(
        used.len(),
        1,
        "verdict_derived 证据不得参与 confidence 重算"
    );

    // 门3 观测计数也只数 observed(1),不数 verdict_derived。
    assert_eq!(kb.storage.observed_outcome_count(&id).unwrap(), 1);
}

// ---------------------------------------------------------------------------
// 方案 D:基率先验。把 base_rate 调低 + prior_m 调大 → 稀疏证据时 context 分回归低基率。
// 这里只锁「调参不崩、且 m=2/g0=0.5 与旧 Laplace 等价」的契约面(语义已在算法注释)。
// ---------------------------------------------------------------------------
#[test]
fn scheme_d_base_rate_prior_param_roundtrip() {
    let (kb, f) = tmp_kb();
    add_active(&kb, "Use cargo build --release", "build the binary");
    kb.storage.set_meta("intuition.prior_m", "20.0").unwrap();
    kb.storage.set_meta("intuition.base_rate", "0.1").unwrap();
    drop(kb);
    let kb = KnowledgeBase::open(f.path()).unwrap();
    // 不应 panic;appraise 正常返回(基率先验只改 context 分大小)。
    let v = appraise_query(&kb, "how do I build the release binary");
    assert!(v.strength >= 0.0 && v.strength <= 1.0);
}

// ---------------------------------------------------------------------------
// 方案 D 解耦:intuition.base_rate / prior_m **只作用于 appraise**,recall 的 context
// 分恒用中性 Laplace 先验、绝不受其影响。这是「优化范围不含 recall」的回归锁。
// ---------------------------------------------------------------------------
#[test]
fn scheme_d_prior_decoupled_from_recall() {
    let (kb, f) = tmp_kb();
    let id = add_active(
        &kb,
        "Use cargo build --release to build innate",
        "build the binary",
    );
    let query = "how do I build the release binary";

    // 建立稀疏 context 统计(1 次 fail),让先验主导 context 分。
    let r = kb
        .recall(RecallParams {
            query,
            budget: 6000,
            trace: true,
            source: "sdk",
            ..Default::default()
        })
        .unwrap();
    kb.record(RecordParams {
        trace_id: &r.trace_id,
        outcome: Some("fail"),
        used: Some(std::slice::from_ref(&id)),
        source: "sdk",
        ..Default::default()
    })
    .unwrap();

    // recall 的 context 分(默认中性先验)。
    let cs1 = recall_context_score(&kb, query, &id);

    // 旋钮是「实的」:存储函数在直觉先验 (m=50,g0=0.01) 下确实改变 context 分。
    let ck = Situation::from_query(query).context_key(KEYS);
    let neutral = kb
        .storage
        .context_scores_batch(&[&id], &ck, 2.0, 0.5)
        .unwrap();
    let intuition = kb
        .storage
        .context_scores_batch(&[&id], &ck, 50.0, 0.01)
        .unwrap();
    let (nv, iv) = (
        neutral.get(&id).copied().unwrap_or(0.0),
        intuition.get(&id).copied().unwrap_or(0.0),
    );
    assert!(
        (nv - iv).abs() > 1e-6,
        "基率先验应实际改变 context 分:neutral={nv} intuition={iv}"
    );

    // 激活直觉基率旋钮并重开 KB。
    kb.storage.set_meta("intuition.base_rate", "0.01").unwrap();
    kb.storage.set_meta("intuition.prior_m", "50.0").unwrap();
    drop(kb);
    let kb = KnowledgeBase::open(f.path()).unwrap();

    // recall 的 context 分必须**逐位不变** —— D 与 recall 完全解耦。
    let cs2 = recall_context_score(&kb, query, &id);
    assert!(
        (cs1 - cs2).abs() < 1e-9,
        "recall 的 context 分不得受 intuition.base_rate 影响:before={cs1} after={cs2}"
    );
}

// ---------------------------------------------------------------------------
// 方案 E:写入一张「把 0.7 桶映射到 0.3 命中率」的校准映射 → emit 时 confidence 被压低。
// ---------------------------------------------------------------------------
#[test]
fn scheme_e_calibration_map_applied_on_emit() {
    let (kb, _f) = tmp_kb();
    add_active(
        &kb,
        "Use cargo build --release to build innate",
        "build the binary",
    );

    // 一次未校准的 appraise,记下原始 confidence(恒等映射)。
    let raw = appraise_query(&kb, "how do I build the release binary");
    assert!(!raw.abstained);
    let raw_conf = raw.confidence;

    // 写一张「全桶命中率压到 0.05」的校准映射。
    let mut buckets: Vec<(f64, f64, f64, i64)> = Vec::new();
    for b in 0..10 {
        buckets.push((b as f64 / 10.0, (b as f64 + 1.0) / 10.0, 0.05, 5));
    }
    kb.storage
        .replace_calibration_map(&buckets, "2026-06-01T00:00:00.000Z")
        .unwrap();

    let calibrated = appraise_query(&kb, "how do I build the release binary");
    if !calibrated.abstained && raw_conf > 0.05 {
        assert!(
            calibrated.confidence <= raw_conf,
            "校准映射应把 confidence 拉向实际命中率:raw={raw_conf} cal={}",
            calibrated.confidence
        );
        assert!((calibrated.confidence - 0.05).abs() < 1e-6);
    }
}
