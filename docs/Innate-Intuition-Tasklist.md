# Innate · 直觉层 Tasklist（Claude Code 执行用）—— 对齐真实代码

> 基线：Cargo `0.1.11` / schema `4.14`,Rust core。配套 `Innate-Intuition-PRD.md` / `Innate-Intuition-Spec.md`。
> 三条真实改动：**① 拓宽触发（context_key 双路拆分） → ② 暴露 appraise critic 契约 → ③ 加诚实性度量**。
> 全程纪律：`recall()` 零回归；本期零 schema 表结构变更；同步路径无 LLM；`Verdict` 无答案文本。

执行方式建议：每个任务一次 `claude -p`,产出「实现 + 单测 + cargo test 自验」。每个里程碑跑一次回归。

---

## M0 · 护栏（先做，防止破坏现有引擎）

### T0.1 recall 零回归基线测试
- **交付物**：`core/src/tests/` 增 `intuition_guard.rs`,锁定现有 `recall()` 在固定夹具上的输出快照（knowledge id 序、fused 序）。
- **验收**：当前代码全绿；后续任何改动若动到 recall 输出即报警。
- **依赖**：无。

### T0.2 三不变量断言
- **交付物**：断言 ① 同步 appraise 路径无 LLM 调用（mock `llm.rs`,断言未触发）；② `Verdict` 无答案字段（类型层面 + 序列化快照）；③ archived chunk 物理仍在。
- **依赖**：无（②③ 待 M2 类型就位后补全）。

---

## M1 · 拓宽触发（核心，承重）

### T1.1 新增 `Situation` 类型与双路方法
- **交付物**：`core/src/kb/situation.rs`,含 `Situation` 结构 + `embed_text()`（富情景拼束）+ `context_key()`（粗签名 hash）+ `coarse_signature()`（取 `meta.situation.coarse_keys`）。
- **验收**：① `embed_text` 含所有非空字段；② `context_key` 对「同 stage+error_class+file_type、但报错文本不同」的两个情景**产出相同 key**(证明不炸桶);③ 纯 query 情景的 context_key 与旧 `normalize_query` 行为可退化等价。
- **依赖**：T0.1。
- **关键**：`error_class` 需把 `last_error` 归一化到类别(正则/前缀),不能带原始文本——这是不炸桶的命门。

### T1.2 recall 接入 situation.context_key（读侧）
- **交付物**：`kb/recall.rs::score_candidates` 第 ~261 行 `context_key` 改用 `situation.context_key()`；`RecallParams` 内部把裸 `query` 包成 `Situation{query:Some(q),..}` 走同一路径。
- **验收**：T0.1 快照在「纯 query」夹具下**不变**(零回归)；新增「富情景」夹具能命中 query 无词匹配但情景相似的 chunk。
- **依赖**：T1.1。

### T1.3 record 接入同源 context_key（写侧）
- **交付物**：`kb/record/mod.rs` 第 ~149 行 context_key 改为**复用预写 `episodic_log` 行里的 context_key**(而非重新 `normalize_query`),保证读写同桶。
- **验收**：一次 appraise→record 闭环后,`chunk_context_stats` 落在与 appraise 相同的 context_key 桶。
- **依赖**：T1.2、T2.3（episodic_log 预写需带新 context_key）。

---

## M2 · appraise critic 契约

### T2.1 `Verdict` / `FlaggedPoint` / `Contributor` 类型
- **交付物**：`core/src/kb/appraise.rs` 定义类型(Spec §3.2),`Valence`/`Tier` 枚举。
- **验收**：编译期保证无 `answer`/`fix`/`corrected_*` 字段；serde 序列化快照入 T0.2。
- **依赖**：无。

### T2.2 valence 派生 + strength 聚合
- **交付物**：`valence_of(chunk)`(anti_hit / fail-origin / context_score<0 → Caution；trigger_hit & calibration>0 → Affirm)；`aggregate()` 取 max + 极性裁决 + 落 meta 阈值分档。
- **验收**：① 高共振+低校准(老误报)样例 strength 被压低；② caution≥affirm → Caution；③ 分档边界正确。
- **依赖**：T2.1。

### T2.3 `appraise()` 主流程 + 写 trace
- **交付物**：`KnowledgeBase::appraise`(Spec §4)：sanitize → embed → 复用 score_candidates(situation context_key) → 聚合 → 写 `usage_trace` + 预写 `episodic_log(context_key=situation.context_key())` → 返回 Verdict。
- **验收**：① 同步全程无 LLM(T0.2 断言)；② trace_id 可被 record UPDATE 同行；③ flagged_points 仅 caution 且过 min_strength。
- **依赖**：T2.2、T1.1。

### T2.4 暴露到 CLI / MCP / SDK
- **交付物**：`innate appraise`(CLI,`cli.rs`)、MCP tool(`mcp.rs`)、Py/TS SDK 薄封装(`sdks/`)。输出 JSON = Verdict。
- **验收**：`innate appraise --situation … --candidate …` 返回结构化 Verdict；MCP tool schema 不含答案字段。
- **依赖**：T2.3。

---

## M3 · override 回流（几乎零新增）

### T3.1 回流闭环集成测试
- **交付物**：`tests/` 验证 `record(trace_id, feedback='down', reason=…)` → `feedback_events` → `confidence_evidence(kind=feedback)` → `recompute_chunk_confidence` 使该 chunk confidence 下降 → 下次 appraise 的 calibration 随之降。
- **验收**：强信号被「有理由推翻」后,同情景下该块 strength 下降。**实现端零新增**(仅验证既有 record 语义在 appraise trace 上成立)。
- **依赖**：T2.3、T1.3。

---

## M4 · 诚实性度量（系统健康命门）

### T4.1 校准曲线 / 单调性 / 误报率 / 沉默率
- **交付物**：`kb/inspection.rs` 增 `intuition_calibration`(Spec §7)：`usage_trace ⨝ episodic_log.outcome ⨝ feedback_events`,按 strength 分桶算实际 task_ok 率。
- **验收**：输出 `monotonicity_gap` / `ece` / `false_alarm_rate` / `silence_rate` / 各桶明细。
- **依赖**：T2.3、T3.1。

### T4.2 接入 inspect suggestions
- **交付物**：`monotonicity_gap` 不显著 → 推送「strength 可能是噪声,检查权重 / 情景签名粒度」；误报率高 → 推送相应提示。
- **验收**：`innate inspect` 可见直觉健康段 + 可执行建议。
- **依赖**：T4.1。

---

## M5 · AutoForge 合流（端到端）

### T5.1 审核节点 critic 适配器
- **交付物**：把 AutoForge「当前实现情景」映射成 `Situation`、调 `appraise`、把 `flagged_points` 渲染成 in-system preview 高风险标记的适配层（经 MCP tool）。
- **验收**：含「发票红冲」类 caution chunk 的样例上,preview 自动标出该风险点(actor=AutoForge,critic=Innate)。
- **依赖**：T2.4。

---

## 依赖图与并行

```
M0 ─┬─> M1(T1.1→T1.2) ─┐
    └─> M2(T2.1→T2.2→T2.3) ─┬─> T1.3(读写同源, 需 T2.3) ─> M3 ─> M4
                            ├─> T2.4 ─> M5(AutoForge)
```
- **必须串行**：T1.1 → T1.2；T2.1 → T2.2 → T2.3。
- **可并行**：M1 与 M2 早期任务可并行,在 T1.3 处汇合(读写同源 context_key)。
- **首个可演示**：到 T2.3 即可 demo「情景 → 带强度极性的判断」；到 T5.1 可讲 actor–critic 合流。

## 全局验收（对齐 PRD §7 DoD）
- [ ] `recall()` 零回归(T0.1 快照不变)。
- [ ] Situation 双路拆分生效,读写 context_key 同源(T1.2/T1.3)。
- [ ] `appraise` 返回 `{valence,strength,tier,flagged_points}`,无 LLM、无答案字段(T2.3/T0.2)。
- [ ] override 回流闭环(T3.1)。
- [ ] `inspect` 可见校准曲线/单调性/误报率(T4.2)。
- [ ] AutoForge caution→flagged_points 端到端(T5.1)。

---

## 附：建议先确认的两个判断（动工前）
1. **粗签名维度**：`situation.coarse_keys` 默认 `stage,error_class,file_type` 是否够稳又够分？太粗→所有情景挤一个桶,校准失去情景区分力；太细→炸桶。这是双路拆分的唯一调参点,建议先用真实 AutoForge trace 跑一版分布再定。
2. **candidate 是否折进共振 embedding**(`appraise.candidate_in_embed`)：折入→共振更准,但 AutoForge 场景 candidate=AI 生成代码,等于把待审代码喂进触发面,须确认先过 sanitize。这个开关直接决定 M5 合流时的安全姿态。
