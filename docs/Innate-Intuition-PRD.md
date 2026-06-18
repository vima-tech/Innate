# Innate · 直觉层（Intuition Critic）PRD —— 基于真实代码版

> 代码基线：仓库 `vima-tech/Innate`，Cargo `0.1.11`，DB schema `4.14`，Rust 实现。
> 本文**取代**早先基于 v4.5.1 设计稿的草案（那版假设的 polarity 列、裸 EMA 校准均与现状不符）。
> 一句话定位：**recall 是 actor 侧的「行动前装配知识」；appraise 是 critic 侧的「对候选答案有没有底」。两者共用同一个打分引擎,叠加而非替换。**

---

## 0. 核心认知：直觉 critic 的底座已经建好

读真实代码后的最重要结论——上轮以为要新建的东西,现在的引擎里大部分已存在,只是没被命名为「直觉」、也没暴露成 critic 契约。

`kb/recall.rs::score_candidates` 的融合打分**本质上已经是 resonance × calibration**：

```
fused = w_content·sim_content + w_trigger·sim_trigger      ← resonance（情景语义匹配）
      + w_confidence·confidence + w_context·context_score  ← calibration（可靠度）
      × PENDING_RECALL_PENALTY（pending 抑制）
      × anti_trigger_penalty（命中 anti_trigger_desc 时抑制）  ← caution 的一半
```
默认权重（`meta`，可配）：`w_content=0.55 / w_trigger=0.25 / w_confidence=0.10 / w_context=0.15 / anti_trigger_penalty=0.6`。

各要素与真实实现的对应：

| 直觉要素 | 现状 | 真实实现位置 |
|---|---|---|
| resonance（共振） | ✅ 已有 | `sim_content + sim_trigger`（对 `vec_content`/`vec_trigger` 做 Rust cosine） |
| calibration（校准） | ✅ 已有,且是贝叶斯的 | `context_score_from_counts`：证据加权后验,按 `context_key` 分桶 |
| confidence 漂移 | ✅ 已有,证据账本 | `confidence_evidence` 账本 → `recompute_chunk_confidence`（非裸 EMA） |
| 「变淡」/ fade | ✅ **已实现** | curate decay：`alpha=1−0.5^(days/90)`,90 天半衰期拉向 `decay_floor` |
| caution / 抑制 | ✅ 已有 | `anti_trigger_penalty` + `anti_trigger_desc` |
| override 回流 | ✅ 已有,更强 | `feedback_events` → `confidence_evidence(kind=feedback)` |
| 「老是错」检测 | ✅ 已有 | `governance_proposals`（重复负反馈→提案→归档） |
| polarity 列（上轮提议） | ❌ **不需要** | 极性可由 trigger-hit / anti_trigger-hit / 来源派生,不新增列 |

---

## 1. 真正缺的，只有三件

1. **触发面是窄的（核心 gap）。** `context_key = content_hash(normalize_query(query))`——`recall.rs` 与 `record/mod.rs` 两侧都只从 query 派生 context。情景里的报错、近几步动作、任务阶段、文件特征都没进来。共振和校准分桶**都被锁死在「显式问的那句话」上**。
2. **没有 critic 契约。** `recall() → RecallResult{ knowledge: Vec<Value> }` 返回「该加载哪些知识去用」（actor 侧）。`_fused_score` 算出来了但只写进 trace,没有 `appraise(situation, candidate) → Verdict` 这种「拿候选答案、回判断」的入口。actor↔critic 的翻转还没落到代码。
3. **没有诚实性度量。** `inspect()` 的 `feedback_loop` 很丰富（trace_completion_rate / task_success_rate / feedback_coverage / knowledge_debt_ratio）,但没有**校准曲线**——没人验证「高 fused 是否真的预测 task_ok」,也没有 strong/weak 命中率差。

---

## 2. 目标与非目标

### 2.1 目标（In Scope）
1. **拓宽触发**：引入 `Situation` 输入,同时驱动 ① embedding（resonance）与 ② `context_key` 派生（calibration 分桶）。
2. **暴露 critic 契约**：新增 `appraise(situation, candidate) → Verdict`,复用 `score_candidates`,surfaced `strength` + `valence` + `flagged_points`。
3. **加诚实性度量**：`inspect()` 增校准曲线 / 单调性 / 误报率 / 沉默率。
4. **与 AutoForge 审核节点合流**：in-system preview 的高风险点标记 = `appraise` 输出的 `flagged_points`。

### 2.2 非目标（Out of Scope）
- ❌ `appraise` **不产出答案**。`Verdict` 值域里不得有答案文本字段——只有对候选的判断（lethal trifecta 防线）。
- ❌ 不改 `recall()` 现有返回与行为（appraise 是新增 public 方法,旧调用零影响）。
- ❌ 同步 appraise 路径不引入 LLM（保持 recall 纯 Rust 数学；LLM 只在 `evolve`/`refine` 异步路径,现状如此）。
- ❌ **不新增 polarity 列**（极性派生,见 §4）；不为多 embedder 做过早抽象。

---

## 3. 核心概念（对齐真实代码）

### 3.1 Situation（情景）—— 替代裸 query
触发共振的信号束：query（可空）+ last_error + recent_actions + stage + file_context。直觉关联的是 situation,不是 question。

### 3.2 双路拆分（本期最关键的设计细节）
当前 `context_key` 和 embedding **共用** `normalize_query(query)`。拓宽后必须**拆成两路**,否则会炸桶：

- **Resonance 路（连续相似度）**：用**完整富情景** embed → 比 `vec_trigger`/`vec_content`。细粒度无所谓,它是连续相似度。
- **Calibration 路（离散分桶）**：必须把情景**粗化成稳定签名**再 hash 成 `context_key`（如 `stage + error_class + file_type`,**不含**原始报错文本）。否则每个略有差异的情景都成新桶,`chunk_context_stats` 永远攒不够证据,`evidence_weight=min(evidence/5,1)` 永远接近 0,校准失效。

> 一句话：**富情景喂共振,粗签名喂校准。** 现状把两者混成一个 `normalize_query`,这是本期要解开的结。

### 3.3 Strength（强度）
连续值,来源 = `score_candidates` 的 `fused`（已含 resonance×calibration 全部因子）。按 `meta` 阈值分档：weak / medium / strong。**不做任何拔高**,诚实性由 §5 指标监督。

### 3.4 Valence（极性）—— 派生,不建列
- **affirm**：trigger-hit 且 calibration（confidence + context_score）为正。
- **caution**：`anti_trigger_hit` 命中,或来源 trace `outcome='fail'` 蒸馏的 chunk,或 `context_score < 0`（这个情景下历史负多）。
- **flagged_points** = caution 类命中块的 `trigger_desc` 摘要（如发票红冲）。

### 3.5 分档响应（消费侧协议）
| 档 | 主判断动作 |
|---|---|
| weak | 留痕,不打断 |
| medium | 提示,看一眼 flagged_points |
| strong | **强制显式回应**：采纳,或给出「为何压过它」的理由 → 经 `record(feedback='down')` 回流降 confidence |

> 强直觉不是有否决权,是有「强制被回应」权——提高被忽略的成本,不夺取控制权。

---

## 4. 成功指标（KPI）

> 核心 KPI 不是召回率,是「该响时响、不该响时静」的判别质量。全部数据已存在于 `usage_trace`（similarity/strength per retrieved）+ `episodic_log.outcome` + `feedback_events`。

| 指标 | 定义 | 阈值 |
|---|---|---|
| **强度单调性（首要）** | strong 桶 task_ok 率 − weak 桶 task_ok 率 | 显著 > 0,否则 strength=噪声 |
| **误报率（杀手指标）** | valence=caution 且 strong,但事后 outcome=ok 的占比 | 越低越好,比漏报更致命 |
| **校准 ECE** | 各桶 \|平均 strength − 实际命中率\| 加权和 | →0 |
| **沉默率（健康）** | appraise 返回 neutral/weak 占比 | 平淡情景应高 |

---

## 5. 安全边界

宽情景触发 = 隐式宽输入注入面（你 MCP 文档里的 lethal trifecta）。靠**「critic 只评估不生成」**砍掉一整档危险,但仍钉死：
1. `Verdict` 值域无答案文本（`flagged_points` 只提示「注意什么」,不给「答案该是什么」）。
2. 信号可被压过且回流（精准回答主路即天生 override；推翻 → `record(feedback='down')` → `confidence_evidence` 降 confidence）。
3. situation 组装前过现有 sanitize hook（`refine.rs`/`hook.rs` 路径）。

---

## 6. 与 AutoForge 合流

同一个 actor–critic 在两尺度复用：**AutoForge = actor**（Claude Code 写代码）,**Innate = critic**（审核节点旁路盖戳、标高风险点）。AutoForge v0.8「in-system preview 自动标出发票红冲类高风险点」的标记器,就是 `appraise` 的 `flagged_points`。

---

## 7. 验收口径（DoD）
- [ ] `appraise(situation, candidate)` 返回 `{valence, strength, tier, flagged_points}`,**同步路径无 LLM**,值域无答案文本。
- [ ] Situation 双路拆分生效：富情景进 embedding,粗签名进 context_key；读写两侧 context_key 一致。
- [ ] strength 落 `meta` 阈值分档；strong/weak 命中率显著拉开。
- [ ] override 回流闭环（复用 `record` + `feedback_events`）。
- [ ] `inspect()` 输出校准曲线 / 误报率 / 单调性 / 沉默率。
- [ ] AutoForge 一个 caution→flagged_points 样例端到端跑通。
- [ ] `recall()` 行为零回归（旧调用方不受影响）。
