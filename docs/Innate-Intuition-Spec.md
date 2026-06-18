# Innate · 直觉层 技术规格（Spec）—— 对齐真实 Rust 代码

> 基线：Cargo `0.1.11` / schema `4.14`。引擎不重写,只「拓宽触发 + 暴露 critic 契约 + 加诚实性度量」三处,全部骑在 `score_candidates` 上。
> 设计纪律：现有机制已覆盖的不新增表/列（沿用既有采纳哲学）。**本期零 schema 变更**（context_key 派生改的是计算逻辑,不动表结构）。

---

## 1. 改动清单（先看这张表）

| # | 改动 | 类型 | 复用什么 | 触及文件 |
|---|---|---|---|---|
| 1 | `Situation` 输入 + 双路拆分 | 核心 | 现有 embedder / `normalize_query` | `kb/recall.rs`、`kb/record/mod.rs`、新 `kb/situation.rs` |
| 2 | `appraise()` public 方法 | 新契约 | `score_candidates` / `write_recall_trace` | `kb/appraise.rs`、`kb/mod.rs` |
| 3 | `Verdict` 类型（无答案字段） | 新类型 | `_fused_score`、`anti_trigger_hit` | `kb/appraise.rs` |
| 4 | 校准曲线 / 单调性指标 | Observe 扩展 | `usage_trace` + `episodic_log.outcome` + `feedback_events` | `kb/inspection.rs` |
| 5 | `appraise.*` meta 阈值 | 配置 | 现有 meta 机制（`recall.w_*` 同款） | `kb/mod.rs` 默认值表 |
| 6 | override 回流 | **零新增** | 现有 `record(feedback)` + `confidence_evidence` | — |

> 唯一一处略动 schema 的可能：`chunk_context_stats` 的 `context_key` 现在=query hash,拓宽后语义变为「情景签名 hash」。**表结构不变,只是写入值的口径变**。需要一次性的兼容处理（见 §6）。

---

## 2. Situation 与双路拆分（核心改动）

### 2.1 类型（新增 `kb/situation.rs`）

```rust
#[derive(Debug, Clone, Default)]
pub struct Situation<'a> {
    pub query: Option<&'a str>,          // 显式提问，可空
    pub last_error: Option<&'a str>,     // 当前/上一个报错
    pub recent_actions: &'a [String],    // 近几步动作
    pub stage: Option<&'a str>,          // 任务阶段
    pub file_context: Option<&'a str>,   // 文件类型/路径摘要
}

impl Situation<'_> {
    /// Resonance 路：富情景拼束 → 交给 embedder（细粒度，连续相似度，不怕碎）
    pub fn embed_text(&self) -> String { /* [query]…[error]…[actions]…[stage]…[files]… */ }

    /// Calibration 路：粗化签名 → content_hash → context_key（必须稳定，否则炸桶）
    /// 只取 stage + error_class(last_error 归一化到类别) + file_type，不含原始报错文本。
    pub fn context_key(&self) -> String { content_hash(&self.coarse_signature()) }
    fn coarse_signature(&self) -> String { /* e.g. "stage=merge|err=TypeError|file=tsx" */ }
}
```

> **为什么必须拆**：`context_score_from_counts` 里 `evidence_weight = min(evidence/5, 1)`——一个 context_key 至少要累计 ~5 条证据才满权。若 context_key 用富情景 hash,每个情景几乎唯一,evidence 永远 <5,校准恒接近 0,白做。富情景只能进 embedding（连续相似度天然容纳碎片）。

### 2.2 回填一致性（读写两侧必须同源）
- `kb/recall.rs::score_candidates` 第 ~261 行：`let context_key = content_hash(&normalize_query(query));`
  → 改为 `let context_key = situation.context_key();`
- `kb/record/mod.rs` 第 ~149 行：`context_key: query.map(|q| content_hash(&normalize_query(q)))`
  → 改为 `context_key: situation.context_key()`（record 也需接收 situation，或从预写的 episodic_log 复用同一 context_key——后者更稳，见 §5）。

> 兼容老路径：`recall(RecallParams{query, ..})` 保留；内部把裸 query 包成 `Situation{query: Some(q), ..Default}`,行为与今天等价（粗签名退化为 query 归一化）。**旧调用零回归**。

---

## 3. appraise 契约（新增 public 方法）

### 3.1 签名（`kb/mod.rs` 上 `impl KnowledgeBase`）

```rust
pub struct AppraiseParams<'a> {
    pub situation: Situation<'a>,
    pub candidate: Option<&'a str>,  // 待评估的候选答案；折进 embed_text 锐化共振（仍纯数学）
    pub top: Option<usize>,          // 默认读 meta appraise.top
    pub min_strength: Option<f64>,   // 共振剪枝下限，默认读 meta
    pub trace: bool,                 // 是否写 recall trace（默认 true，供回流）
}

pub fn appraise(&self, params: AppraiseParams<'_>) -> Result<Verdict>;
```

### 3.2 返回类型（值域硬约束：无答案文本）

```rust
pub struct Verdict {
    pub valence: Valence,            // Affirm | Caution | Mixed | Neutral
    pub strength: f64,               // ∈[0,1]，聚合后；来源 = fused
    pub tier: Tier,                  // Weak | Medium | Strong（落 meta 阈值）
    pub flagged_points: Vec<FlaggedPoint>,
    pub contributors: Vec<Contributor>, // 可解释性
    pub trace_id: String,            // 贯穿 appraise→record 回流
}

pub struct FlaggedPoint {
    pub chunk_id: String,
    pub summary: String,             // 取自 chunk.trigger_desc，只提示「注意什么」
    pub resonance: f64,              // sim_content+sim_trigger 部分
    pub calibration: f64,            // confidence+context_score 部分
    pub strength: f64,               // 单条 fused
}
pub struct Contributor { pub chunk_id: String, pub valence: Valence, pub strength: f64 }
```

> **不得出现**任何 `answer` / `suggested_fix` / `corrected_output` 字段。`summary` 来自既有 `trigger_desc`,是「这类情景要当心 X」,不是「答案应为 Y」。

---

## 4. 核心算法（同步路径，纯 Rust，无 LLM）

```rust
pub fn appraise(&self, p: AppraiseParams) -> Result<Verdict> {
    // 1. sanitize（复用现有 hook）
    // 2. embed_text = situation.embed_text() (+candidate 折入)
    // 3. 复用 score_candidates：注意把内部 context_key 改为 situation.context_key()
    //    每个候选得到 fused(=resonance×calibration 全因子) + sim_* + conf + context_score + anti_hit
    let scored = self.score_candidates_for_situation(&p.situation, p.candidate)?;

    // 4. 逐条 strength = fused（已含全部因子，不再二次相乘）
    //    valence_of(chunk): anti_hit || outcome_fail_origin || context_score<0 → Caution；
    //                       trigger_hit && calibration>0 → Affirm；else Neutral
    // 5. 聚合：strength = max over contributors；极性按 max(caution) vs max(affirm) 裁决
    // 6. tier = 落 meta(appraise.tier_weak / tier_strong)
    // 7. flagged_points = caution 类，按 strength 降序，过 min_strength
    // 8. if p.trace { write_recall_trace(...) + 预写 episodic_log(context_key=situation.context_key()) }
    //    —— 与 recall 同款时序，使后续 record(trace_id, feedback) 能 UPDATE 同行回流
}
```

聚合默认 `strength = max(s_affirm, s_caution)`；裁决：

```
caution_strength >= affirm_strength && >0 → Caution
affirm_strength  >  caution_strength && >0 → Affirm
both >0 → Mixed ; else Neutral
```

> **resonance / calibration 拆分仅用于可解释性输出**（FlaggedPoint 的两个字段）,聚合用的是融合后的 `fused`,与现有 recall 完全一致,不引入新打分路径。

---

## 5. override 回流（零新增代码）

主判断推翻 strong 信号后,调既有 `record`：

```rust
kb.record(RecordParams { trace_id, feedback: Some("down"), reason: Some("…"), ..Default::default() })?;
// → 写 feedback_events(signal='down') → upsert_confidence_evidence(kind=feedback, alpha=0.2*strength*recency)
// → recompute_chunk_confidence：confidence += alpha*(target-confidence)
// → 该 chunk 下次 appraise 的 calibration 下降 = critic 判别力被校准
```

> 关键：appraise 预写 `episodic_log` 时落的 `context_key = situation.context_key()`；record 从该行**复用同一 context_key** 更新 `chunk_context_stats`——保证「这次直觉在哪个情景桶里被印证/证伪」记账一致。这是双路拆分能闭环的前提。

---

## 6. schema / 兼容处理

- **无表结构变更**。`chunk_context_stats.context_key` 列复用,语义从「query hash」转为「情景签名 hash」。
- 历史桶（旧 query-hash key）与新桶（情景签名 key）**键空间不冲突但不互通**。处理策略：不迁移,让旧桶随 `log_compact` / 自然衰减淡出；新证据写新桶。`evidence_weight` 冷启期偏低属预期,随使用回升。
- 回滚：不调 `appraise` + context_key 派生还原为 `normalize_query` 即退回今天行为。

---

## 7. Observe 诚实性指标（`kb/inspection.rs` 扩展）

在 `inspect()` 现有 `feedback_loop` 段旁新增 `intuition_calibration`：

```rust
// 数据源：usage_trace(retrieved 带 _fused_score/similarity, trace_id)
//        ⨝ episodic_log(trace_id → outcome) ⨝ feedback_events
// 按 strength 分桶（weak/medium/strong），每桶统计实际 task_ok 率：
{
  "monotonicity_gap": strong_hit_rate - weak_hit_rate,   // 应显著 >0
  "ece": Σ bucket_weight·|avg_strength - actual_hit_rate|,
  "false_alarm_rate": caution&strong 但 outcome=ok 占比,
  "silence_rate": neutral|weak 占比,
  "buckets": [ {tier, avg_strength, actual_hit_rate, n}, … ]
}
```
并接入既有 `suggestions`：若 `monotonicity_gap` 不显著 → 提示「strength 可能是噪声,检查权重/情景签名粒度」。

---

## 8. meta 默认值（`kb/mod.rs` 默认表，与 `recall.w_*` 同处）

```
appraise.tier_weak     = 0.30
appraise.tier_strong   = 0.65
appraise.min_strength  = 0.40
appraise.top           = 8
appraise.candidate_in_embed = true   # candidate 是否折进共振 embedding（安全姿态开关，见 PRD §5）
situation.coarse_keys  = "stage,error_class,file_type"   # 粗签名取哪些维度
```

---

## 9. 不做的事（防止滑回 generator）
1. ❌ `Verdict` 不含答案文本字段（值域硬约束 + 单测断言）。
2. ❌ 同步 appraise 不调 LLM（深层语义矛盾核验留未来异步 critic,独立路径）。
3. ❌ 不新增 polarity 列（valence 派生）。
4. ❌ 不为多 embedder / 多引擎做过早抽象。
