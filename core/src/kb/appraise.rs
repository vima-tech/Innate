//! appraise — the critic contract.
//!
//! `recall()` is the **actor** side: "which knowledge should I load to act?". `appraise()` is
//! the **critic** side: "do I have any footing on this candidate answer?". Both ride the *same*
//! fused score (`w_content·sim_content + w_trigger·sim_trigger + w_confidence·conf +
//! w_context·context_score + w_activation·activation`, with the pending/anti penalties); appraise
//! does not introduce a
//! second scoring path. It only *re-reads* that score as strength + valence and surfaces what to
//! be careful about.
//!
//! Hard value-domain constraint (PRD §2.2 / §5, the lethal-trifecta defence): a [`Verdict`]
//! carries **no answer text** — no `answer`, `fix`, `corrected_*`. `flagged_points` say "watch
//! out for X", never "the answer is Y". The synchronous path is pure Rust math — **no LLM**.

use serde::Serialize;
use serde_json::{json, Value};

use super::actr_activation;
use crate::errors::Result;
use crate::storage::EpisodicLogRow;
use crate::utils::{gen_uuid, utc_now_iso, SanitizeAction};

use super::{anti_trigger_hit, validate_source, KnowledgeBase, Situation, PENDING_RECALL_PENALTY};

// ---------------------------------------------------------------------------
// Public types — note the absence of any answer-bearing field (enforced by T0.2).
// ---------------------------------------------------------------------------

/// 返给 agent 的固定声明:直觉只是参考信号,不是精准答案。在 MCP / CLI 的 appraise
/// 响应里随每个 verdict 一起返回,提醒 actor「权衡、勿盲从、勿让直觉覆盖你自己对正确
/// 答案的判断」。这是值域护栏(PRD §2.2/§5「直觉永不产出答案」)在交付层的显式表态。
pub const APPRAISE_ADVISORY: &str = "Reference signal only — this is intuition (footing/caution), \
not a precise or verified answer. Weigh it as one input; do not defer to it and never let it \
override your own analysis of the correct answer. flagged_points are things to watch for, never \
prescribed solutions. When abstained=true the critic has no footing — that is correct, not a failure.";

/// Polarity of an intuition. Derived, never stored as a column (PRD §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Valence {
    /// Trigger-hit and positive calibration — "you have footing here".
    Affirm,
    /// Anti-trigger hit, failure-origin, or negative context history — "be careful here".
    Caution,
    /// Both affirm and caution signals fired.
    Mixed,
    /// Nothing resonated meaningfully — stay quiet.
    Neutral,
}

/// Strength band, from the fused score against `meta.appraise.tier_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Weak,
    Medium,
    Strong,
}

/// 方案 A —— 弃权原因(四道门)。弃权是一等输出,不是失败:critic 的第一能力是
/// 「说不知道」。短路顺序求值,记录第一道触发的门。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AbstainReason {
    /// 门1:prune 后没有任何候选越过共振地板 —— 根本没共振到东西。
    WeakResonance,
    /// 门2:rich 嵌入近但 signature 远 —— 疑似假共振(方案 F)。
    FalseResonance,
    /// 门3:命中邻居缺乏实际观测结果历史 —— 没证据就不装懂(方案 C/门3)。
    SparseEvidence,
    /// 门4:top-k 邻居 fused 离散度过大 —— 本情境真实模糊(方案 G)。
    Conflicted,
}

/// A single thing to be careful about. Comes from a caution-class chunk's `trigger_desc` —
/// "this kind of situation tends to bite", never a prescribed answer.
#[derive(Debug, Clone, Serialize)]
pub struct FlaggedPoint {
    pub chunk_id: String,
    /// What to watch for. Sourced from the chunk's existing `trigger_desc`. No answer text.
    pub summary: String,
    /// Resonance component (sim_content + sim_trigger, weighted).
    pub resonance: f64,
    /// Calibration component (confidence + context_score, weighted).
    pub calibration: f64,
    /// Single-chunk fused strength ∈ [0,1].
    pub strength: f64,
}

/// One contributing chunk, for explainability.
#[derive(Debug, Clone, Serialize)]
pub struct Contributor {
    pub chunk_id: String,
    pub valence: Valence,
    pub strength: f64,
}

/// The critic's judgement. **No answer-bearing field may ever be added here.**
#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub valence: Valence,
    /// Aggregate strength ∈ [0,1]; the max fused over contributors.
    pub strength: f64,
    pub tier: Tier,
    pub flagged_points: Vec<FlaggedPoint>,
    pub contributors: Vec<Contributor>,
    /// Threads appraise → record so an override can flow back via `record(feedback='down')`.
    pub trace_id: String,
    /// 方案 A:弃权是一等输出。`true` 时 valence=Neutral、flagged 为空、confidence=0,
    /// 但 strength 仍可见(弃权率/弃权精度的健康信号)。
    pub abstained: bool,
    /// 弃权原因;表态则 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abstain_reason: Option<AbstainReason>,
    /// 方案 E/G:经校准映射 + 邻居离散度塑形后的置信度 ∈ [0,1]。与 strength 区分:
    /// strength 是原始共振强度,confidence 是「直觉对自己几斤几两的诚实估计」。
    pub confidence: f64,
    /// 方案 G:top-k 邻居 fused 离散度(max-min)。透明化,供下游判读模糊性。
    pub dispersion: f64,
}

/// Parameters for [`KnowledgeBase::appraise`].
#[derive(Debug, Clone, Default)]
pub struct AppraiseParams<'a> {
    pub situation: Situation<'a>,
    /// The candidate answer under judgement. Folded into the resonance embedding to sharpen the
    /// match (still pure math) when `meta.appraise.candidate_in_embed` is true; always sanitized
    /// first. Never echoed back in the Verdict.
    pub candidate: Option<&'a str>,
    /// Resonance prune floor; default `meta.appraise.min_strength`.
    pub min_strength: Option<f64>,
    /// Candidate cap; default `meta.appraise.top`.
    pub top: Option<usize>,
    /// Write a recall/episodic trace so a later `record` can flow back. Default true.
    pub trace: bool,
    /// Event source written to traces (mcp | sdk | cli | hook | daemon | augmented).
    pub source: &'a str,
}

/// Per-candidate scored result with the resonance/calibration decomposition exposed
/// for explainability. The aggregate uses `fused` — the same number recall ranks on.
struct ScoredCandidate {
    chunk_id: String,
    trigger_desc: String,
    fused: f64,
    resonance: f64,
    calibration: f64,
    valence: Valence,
}

impl KnowledgeBase {
    pub fn appraise(&self, params: AppraiseParams<'_>) -> Result<Verdict> {
        let AppraiseParams {
            situation,
            candidate,
            min_strength,
            top,
            trace,
            source,
        } = params;
        let source = if source.is_empty() { "sdk" } else { source };
        validate_source(source)?;
        let min_strength = min_strength.unwrap_or(self.appraise_min_strength);
        let top = top.unwrap_or(self.appraise_top);

        let trace_id = gen_uuid();
        let now = utc_now_iso();

        // 1. Sanitize the resonance inputs before they touch the embedder (PRD §5). A Discard
        //    verdict on either neutralizes that input rather than embedding hostile text.
        let raw_embed = situation.embed_text();
        let (embed_clean, embed_action) = self.sanitize_content(&raw_embed);
        let mut embed_text = if matches!(embed_action, SanitizeAction::Discard) {
            String::new()
        } else {
            embed_clean
        };
        // Lowercased text used for anti-trigger matching (situation + candidate).
        let mut anti_match = embed_text.to_lowercase();
        if self.appraise_candidate_in_embed {
            if let Some(cand) = candidate.map(str::trim).filter(|c| !c.is_empty()) {
                let (cand_clean, cand_action) = self.sanitize_content(cand);
                if !matches!(cand_action, SanitizeAction::Discard) {
                    embed_text.push_str("\n[candidate] ");
                    embed_text.push_str(&cand_clean);
                    anti_match.push('\n');
                    anti_match.push_str(&cand_clean.to_lowercase());
                }
            }
        }

        // 2. Resonance embedding + candidate gathering (reuses the recall ANN path).
        let (q_content, q_trigger) = self
            .embedding
            .embed_both(&embed_text)
            .map_err(|e| crate::errors::InnateError::EmbeddingUnavailable(e.to_string()))?;
        let mut candidates = self.ann_candidates(&q_content, &q_trigger)?;
        self.apply_soft_dep_bonus(&mut candidates)?;

        // 3. Calibration path: one context_key for read + the pre-written episodic_log (Spec §5).
        let context_key = situation.context_key(&self.situation_coarse_keys);
        let cand_ids: Vec<String> = candidates
            .values()
            .filter_map(|info| {
                info.chunk
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        let cand_refs: Vec<&str> = cand_ids.iter().map(String::as_str).collect();
        let ctx_scores = self.storage.context_scores_batch(
            &cand_refs,
            &context_key,
            self.intuition_prior_m,
            self.intuition_base_rate,
        )?;
        // 方案 F 门2:哪些邻居在 signature 通道(coarse 情境桶)也有校准历史。
        let sig_present = self
            .storage
            .context_stat_present_batch(&cand_refs, &context_key)?;

        // 4. Score every candidate with the *same* fused math as recall, but keep the
        //    resonance / calibration split for explainability, and derive a valence.
        let mut scored: Vec<ScoredCandidate> = Vec::with_capacity(candidates.len());
        for info in candidates.into_values() {
            let chunk = &info.chunk;
            let chunk_id = chunk.get("id").and_then(Value::as_str).unwrap_or("");
            let conf = chunk
                .get("confidence")
                .and_then(Value::as_f64)
                .unwrap_or(0.5);
            let context_score = ctx_scores.get(chunk_id).copied().unwrap_or(0.0);

            let resonance =
                self.w_content * info.sim_content as f64 + self.w_trigger * info.sim_trigger as f64;
            // ACT-R activation (recency × frequency) — same usage-history signal recall fuses;
            // grouped with calibration since it reflects accumulated use, not query resonance.
            let used_count = chunk.get("used_count").and_then(Value::as_i64).unwrap_or(0);
            let last_used_at = chunk.get("last_used_at").and_then(Value::as_str);
            let activation = actr_activation(used_count, last_used_at, &now);
            let calibration = self.w_confidence * conf
                + self.w_context * context_score
                + self.w_activation * activation;
            let mut fused = resonance + calibration;
            if chunk.get("state").and_then(Value::as_str) == Some("pending") {
                fused *= PENDING_RECALL_PENALTY;
            }
            let anti = chunk
                .get("anti_trigger_desc")
                .and_then(Value::as_str)
                .unwrap_or("");
            let anti_hit = !anti.is_empty() && anti_trigger_hit(&anti_match, anti);
            if anti_hit {
                fused *= self.anti_trigger_penalty;
            }

            // Failure-origin proxy: the heuristic distiller writes "Avoid: …" content and an
            // anti_trigger_desc for fail-outcome traces; either marks a caution chunk.
            let content = chunk.get("content").and_then(Value::as_str).unwrap_or("");
            let fail_origin = content.trim_start().starts_with("Avoid:") || !anti.is_empty();
            let trigger_hit = info.sim_trigger as f64 >= self.appraise_trigger_hit_min;

            let valence = if anti_hit || fail_origin || context_score < 0.0 {
                Valence::Caution
            } else if trigger_hit && calibration > 0.0 {
                Valence::Affirm
            } else {
                Valence::Neutral
            };

            let trigger_desc = chunk
                .get("trigger_desc")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    content
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(120)
                        .collect()
                });

            scored.push(ScoredCandidate {
                chunk_id: chunk_id.to_string(),
                trigger_desc,
                fused: fused.clamp(0.0, 1.0),
                resonance,
                calibration,
                valence,
            });
        }
        scored.sort_by(|a, b| {
            b.fused
                .partial_cmp(&a.fused)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        // Resonance prune (Spec §3.1: min_strength is the resonance lower bound). Sub-threshold
        // contributors are noise — they must not set strength/tier/valence, otherwise an
        // unrelated situation reads as weak-caution and silence_rate becomes dishonest. The floor
        // is the single gate for strength, tier, valence, contributors *and* flagged_points.
        scored.retain(|s| s.fused >= min_strength);
        scored.truncate(top);

        // strength = max fused over survivors; dispersion (方案 G) = fused 极差。
        let strength = scored.iter().map(|s| s.fused).fold(0.0_f64, f64::max);
        let dispersion = if scored.len() >= 2 {
            let hi = scored.iter().map(|s| s.fused).fold(f64::MIN, f64::max);
            let lo = scored.iter().map(|s| s.fused).fold(f64::MAX, f64::min);
            (hi - lo).clamp(0.0, 1.0)
        } else {
            0.0
        };

        // ---- 方案 A:四道弃权门(短路顺序,记录第一道触发的门)----
        let mut abstain: Option<AbstainReason> = None;
        // 门1 弱共振:prune 后无候选越过共振地板 —— 根本没共振到。
        if scored.is_empty() {
            abstain = Some(AbstainReason::WeakResonance);
        }
        // 门2 假共振(方案 F):signature 通道一致度低于地板。默认 floor=0 → 关闭。
        if abstain.is_none() && self.appraise_signature_floor > 0.0 {
            let agree = scored
                .iter()
                .filter(|s| sig_present.contains(&s.chunk_id))
                .count() as f64
                / scored.len() as f64;
            if agree < self.appraise_signature_floor {
                abstain = Some(AbstainReason::FalseResonance);
            }
        }
        // 门3 证据稀疏(方案 C/门3):有实际观测历史的邻居不足。默认 min_evidence=0 → 关闭。
        if abstain.is_none() && self.appraise_min_evidence > 0 {
            let mut observed = 0_i64;
            for s in &scored {
                if self.storage.observed_outcome_count(&s.chunk_id)? >= 1 {
                    observed += 1;
                }
            }
            if observed < self.appraise_min_evidence {
                abstain = Some(AbstainReason::SparseEvidence);
            }
        }
        // 门4 真实模糊(方案 G):邻居离散度超过上界。默认 ceiling=1.0 → 关闭。
        if abstain.is_none() && dispersion > self.appraise_conflict_ceiling {
            abstain = Some(AbstainReason::Conflicted);
        }

        // 5. Aggregate: strength = max fused over surviving contributors; valence by max-affirm
        //    vs max-caution. flagged_points = the caution survivors.
        let max_for = |v: Valence| -> f64 {
            scored
                .iter()
                .filter(|s| s.valence == v)
                .map(|s| s.fused)
                .fold(0.0_f64, f64::max)
        };
        let s_affirm = max_for(Valence::Affirm);
        let s_caution = max_for(Valence::Caution);

        let directional_valence = match (s_affirm > 0.0, s_caution > 0.0) {
            (true, true) => Valence::Mixed,
            (false, true) => Valence::Caution,
            (true, false) => Valence::Affirm,
            (false, false) => Valence::Neutral,
        };
        let directional_tier = if strength >= self.appraise_tier_strong {
            Tier::Strong
        } else if strength >= self.appraise_tier_weak {
            Tier::Medium
        } else {
            Tier::Weak
        };

        // 方案 E:校准映射(分桶查表;空 map = 恒等)。方案 G:再按离散度折损。
        let calibrated = self.calibrate_confidence(strength);
        let shaped_conf = (calibrated * (1.0 - dispersion)).clamp(0.0, 1.0);

        // 弃权时 valence=Neutral、tier=Weak、flagged 为空、confidence=0;strength 仍可见。
        let (valence, tier, confidence) = if abstain.is_some() {
            (Valence::Neutral, Tier::Weak, 0.0)
        } else {
            (directional_valence, directional_tier, shaped_conf)
        };

        let flagged_points: Vec<FlaggedPoint> = if abstain.is_some() {
            Vec::new()
        } else {
            scored
                .iter()
                .filter(|s| s.valence == Valence::Caution && s.fused >= min_strength)
                .map(|s| FlaggedPoint {
                    chunk_id: s.chunk_id.clone(),
                    summary: s.trigger_desc.clone(),
                    resonance: s.resonance,
                    calibration: s.calibration,
                    strength: s.fused,
                })
                .collect()
        };
        // contributors 始终保留(可解释性;弃权样本也留痕,符合零数据丢失)。
        let contributors: Vec<Contributor> = scored
            .iter()
            .map(|s| Contributor {
                chunk_id: s.chunk_id.clone(),
                valence: s.valence,
                strength: s.fused,
            })
            .collect();

        let verdict = Verdict {
            valence,
            strength,
            tier,
            flagged_points,
            contributors,
            trace_id: trace_id.clone(),
            abstained: abstain.is_some(),
            abstain_reason: abstain,
            confidence,
            dispersion,
        };

        // 6. Trace — same shape/timing as recall so a later record(trace_id, …) UPDATEs the
        //    same episodic_log row and flows the override back through confidence_evidence.
        if trace {
            self.write_appraise_trace(
                &trace_id,
                &context_key,
                &raw_embed,
                &scored,
                &verdict,
                source,
                &now,
            )?;
        }

        Ok(verdict)
    }

    #[allow(clippy::too_many_arguments)]
    fn write_appraise_trace(
        &self,
        trace_id: &str,
        context_key: &str,
        situation_text: &str,
        scored: &[ScoredCandidate],
        verdict: &Verdict,
        source: &str,
        now: &str,
    ) -> Result<()> {
        let lib_id = self.storage.lib_id()?;
        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            for (rank, s) in scored.iter().enumerate() {
                let sim = Some(s.fused);
                self.storage.insert_usage_trace(
                    trace_id,
                    Some(&s.chunk_id),
                    "retrieved",
                    1.0,
                    sim,
                    Some("appraise"),
                    None,
                    Some((rank + 1) as i64),
                    None,
                    source,
                    now,
                )?;
                // Mark contributors 'selected' too: the critic leaned on them, so they must be
                // attributable for `record(feedback=…)` to flow an override back (Spec §5).
                self.storage.insert_usage_trace(
                    trace_id,
                    Some(&s.chunk_id),
                    "selected",
                    1.0,
                    sim,
                    Some("appraise"),
                    None,
                    Some((rank + 1) as i64),
                    None,
                    source,
                    now,
                )?;
            }
            // The verdict is persisted in recall_snapshot (free-form TEXT, no schema change) so the
            // honesty metrics in inspect() can bucket by tier/valence and join the later outcome.
            let contributor_ids: Vec<&String> = scored.iter().map(|s| &s.chunk_id).collect();
            let snapshot = json!({
                "appraise": {
                    "valence": verdict.valence,
                    "tier": verdict.tier,
                    "strength": verdict.strength,
                    "confidence": verdict.confidence,
                    "dispersion": verdict.dispersion,
                    "abstained": verdict.abstained,
                    "abstain_reason": verdict.abstain_reason,
                    "flagged": verdict.flagged_points.iter().map(|f| &f.chunk_id).collect::<Vec<_>>(),
                },
                "retrieved": contributor_ids,
                "selected": contributor_ids,
            });
            let log = EpisodicLogRow {
                id: gen_uuid(),
                trace_id: trace_id.to_string(),
                lib_id,
                ts: now.to_string(),
                query: Some(situation_text.chars().take(500).collect()),
                recall_snapshot: Some(snapshot.to_string()),
                event_source: source.to_string(),
                task_state: "recalled".to_string(),
                usage_state: "unknown".to_string(),
                context_key: Some(context_key.to_string()),
                distill_state: "open".to_string(),
                ..Default::default()
            };
            self.storage.upsert_episodic_log(&log)?;
            // 方案 B:写 verdict_log —— 直觉模块可证伪的唯一数据源。弃权也入表
            // (abstain_reason 非空、valence/conf 为空),弃权率本身是健康度信号。
            let abstain_reason = verdict.abstain_reason.as_ref().map(|r| {
                serde_json::to_value(r)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default()
            });
            let tier_str = serde_json::to_value(verdict.tier)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string));
            let valence_str = serde_json::to_value(verdict.valence)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string));
            self.storage.insert_verdict_log(
                &gen_uuid(),
                trace_id,
                context_key,
                if verdict.abstained {
                    None
                } else {
                    valence_str.as_deref()
                },
                if verdict.abstained {
                    None
                } else {
                    Some(verdict.confidence)
                },
                verdict.strength,
                if verdict.abstained {
                    None
                } else {
                    tier_str.as_deref()
                },
                abstain_reason.as_deref(),
                now,
            )?;
            self.storage.commit()
        })();
        if result.is_err() {
            let _ = self.storage.rollback();
        }
        result
    }

    /// 方案 E:把原始强度经学习到的校准映射(分桶查表)转成校准置信度。
    /// 空 map(冷启动 / 数据不足)= 恒等,不引入偏差。命中桶则返回该桶的实际命中率。
    fn calibrate_confidence(&self, raw: f64) -> f64 {
        let map = match self.storage.load_calibration_map() {
            Ok(m) if !m.is_empty() => m,
            _ => return raw.clamp(0.0, 1.0),
        };
        for (lo, hi, rate) in &map {
            if raw >= *lo && raw < *hi {
                return rate.clamp(0.0, 1.0);
            }
        }
        // 落在最后一桶上界(raw==1.0)→ 用最高桶。
        map.last().map(|(_, _, r)| r.clamp(0.0, 1.0)).unwrap_or(raw)
    }
}
