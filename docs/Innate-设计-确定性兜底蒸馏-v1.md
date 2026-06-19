# 设计：确定性兜底蒸馏（Resilient Distillation）—— #3 落地形态 v1

> 目标：把 evolve 固化关键路径上的 **LLM 依赖从"决定能否创建知识"降级为"决定知识质量"**，
> 实现"用户无感、稳定创建"。对标 myelin"LLM-free 核心学习"原则，但适配 Innate 数据模型
> （无 action 序列，故不做 MSA）。详见 [[feedback_passive_capture_principle]] / [[project_competitive_roadmap]] #3。

## 1. 问题陈述

- `open_kb` 仅在 `settings.llm` 配置时注入 `HttpDistiller`（LLM）；否则用确定性的 `HeuristicDistiller`。
- 一旦配了远程 LLM，**所有蒸馏都走 LLM**。LLM 不可用（超时/限流/网络/额度）时：
  - `distill_batch` 对该 log `finish_distill_log(failed)`；
  - evolve 把 `attempts<3` 的 failed log 恢复成 `new` 重试（retry_cutoff 5 分钟）；
  - **attempts 达 3 后永久停在 `failed`，该 log 承载的知识彻底不被创建。**
- 这正是用户关注的"不稳定"：捕获(online，无感) 成功了，但固化(offline) 因 LLM 把知识丢了。

**约束（已核实代码）：**
- `Distiller` 是纯函数：`distill_with_context(&Value, &[Value])`，**看不到 storage/历史**，故跨批次复现计数不能在此做（那是 curate 的 usage→confidence→promote 职责，已存在）。
- `claim_distill_batch` 用 `SELECT *`，返回的 log Value **含 `distill_attempts`** → 兜底逻辑可在 distiller 内按 attempts 决策，无需改 `distill_batch`。
- 每个 log 的 chunk 必须 `source_log_id == log_id`（distill_batch 强校验）；`HeuristicDistiller` 用 `entry["id"]`，天然满足。

## 2. 目标 / 非目标

**目标**
- 配了 LLM 时：LLM 先试（保质量），失败到预算后**确定性兜底**，保证知识最终一定被创建（保稳定）。
- 不引入第二个 LLM（遵守 [[feedback_single_llm]]：容错=同一 LLM 请求级重试 + 确定性兜底，确定性通道不是"备用 LLM"）。
- 兜底产物仍走 pending + Sanitizer + 治理闸门（[[feedback_passive_capture_principle]]：无感的是"记录"，不是"采信"）。

**非目标（本期不做）**
- 不做 myelin 式多序列比对（Innate 无 action 序列）。
- 不做跨批次复现计数→置信（已由 usage→promotion 覆盖；见 §6 Phase 2 评估后再定）。
- 不改 online 捕获路径（hook/daemon/record 已是"哑捕获"，本就稳定）。

## 3. 设计：`ResilientDistiller` 包装器

### 3.1 结构（`refine.rs`）

```rust
/// Wraps a primary distiller (e.g. LLM) with a deterministic fallback.
/// LLM gets the first `llm_attempt_budget` attempts (quality); once a log has
/// failed that many times, the fallback distiller guarantees capture (stability).
pub struct ResilientDistiller {
    primary: Arc<dyn Distiller>,
    fallback: Arc<dyn Distiller>,
    llm_attempt_budget: i64, // default 2; test injects 0 for immediate fallback
}

impl Distiller for ResilientDistiller {
    fn distill_with_context(&self, primary_log: &Value, related: &[Value])
        -> Result<Vec<DistilledChunk>>
    {
        match self.primary.distill_with_context(primary_log, related) {
            Ok(chunks) => Ok(chunks),               // 成功(含合法空结果)→ 不兜底
            Err(e) => {
                let attempts = primary_log.get("distill_attempts")
                    .and_then(Value::as_i64).unwrap_or(0);
                if attempts >= self.llm_attempt_budget {
                    // 预算耗尽：确定性兜底，保证捕获
                    let mut chunks = self.fallback.distill_with_context(primary_log, related)?;
                    for c in &mut chunks { c.provider_override = Some("heuristic_fallback".into()); }
                    Ok(chunks)
                } else {
                    Err(e)   // 仍有预算：抛错 → 沿用现有 failed→retry 机制下次再试 LLM
                }
            }
        }
    }
    // distill() 同理委托；provenance() 返回 primary 的（逐 chunk 用 override 修正，见 3.2）
}
```

**语义**：
- 成功（包括"无可蒸馏"返回空）→ 直接采用，**不**兜底（合法 discard 不应被掩盖）。
- 失败且 `attempts < budget` → **抛错**，让现有 `failed→恢复new→重试` 机制给 LLM 第二次机会（保质量）。
- 失败且 `attempts >= budget` → **确定性兜底**产出 chunk（保稳定），打上 `heuristic_fallback` 溯源。
- 时间线：attempts 0,1 试 LLM（各隔 ~5min retry_cutoff）；attempt 2（最后一次，因恢复要求 attempts<3）触发兜底 → 最坏 ~10min 延迟后确定性捕获，而非永久丢失。

> ⚠ 实现时需核对 `distill_attempts` 的自增时点（在 `finish_distill_log(failed)` 内自增？），
> 据此把 `budget` 精确定为 2（使"第 3 次=最后一次"走兜底）。设计意图固定，常数实现时校准。

### 3.2 溯源精度（`DistilledChunk` 增字段）

`provenance()` 是 per-batch 单次调用，无法区分某条 log 是否走了兜底。故给 `DistilledChunk` 加：

```rust
pub provider_override: Option<String>,  // 非 None 时覆盖 batch provenance.provider
```

`distill_batch` 写 `ChunkRow.distill_provider` 时：`dc.provider_override.clone().or(provenance.provider.clone())`。
→ web/inspect 能看出哪些 chunk 是兜底产物（运维可见性 + 后续可选择性重蒸馏策略）。

### 3.3 注入（`lib.rs::open_kb`）

```rust
let distiller = s.llm.as_ref().map(|c| {
    let primary = llm::build_distiller(c);
    Arc::new(ResilientDistiller::new(primary, Arc::new(HeuristicDistiller), 2))
        as Arc<dyn Distiller>
});
```

- 未配 LLM → 仍是裸 `HeuristicDistiller`（无 LLM、无不稳定，兜底无意义）。
- 配了 LLM → 自动获得兜底，无需用户配置。

## 4. 可靠性 / 幂等性分析

- 复用现有终态机（`distilled/discarded/failed/screening`）与 `BEGIN IMMEDIATE` 写事务，**不新增状态**。
- 兜底产物与正常产物走同一 prepared→embed→atomic-write 路径：content_hash 去重、`is_hash_invalidated` 过滤、embedding 失败仍 defer。
- 幂等：claim/commit 不变；兜底不改变"一个 log 终态一次"的语义。

## 5. 威胁模型对齐

- 兜底 chunk 仍：`state=pending` + Sanitizer（Discard/Redact）+ content-hash 去重 + 治理 approve 才促活。
- 不新增网络/写入面。`heuristic_fallback` 溯源使注入式可疑内容更易审计。

## 6. Phase 2（可选，先评估再定）：复现感知置信

- 设想：用 batch 内 `related_logs`（同 context_key）数量给初始置信加权（myelin frequency→confidence 的确定性版）。
- **现状已有替代**：跨会话复现由 usage_trace→curate aggregate→confidence EMA→promote 覆盖；且同 context_key log 同批共现概率低 → 价值有限。
- **决定**：本期**不做**，避免与 curate 职责重叠、控制风险。留待评测（§7）显示"pending 噪声过多"时再启。

## 7. 评测计划（扩 `core/src/tests/eval.rs` 或 distillation 测试）

1. `distill_falls_back_to_deterministic_on_llm_failure`：注入"primary 永远 Err"的测试 distiller + `HeuristicDistiller` 兜底 + `budget=0` → 一次 evolve 后断言：log 终态 `distilled`、生成 1 个 pending chunk、`distill_provider="heuristic_fallback"`。
2. `distill_retries_llm_before_falling_back`：`budget=2` + primary 头两次 Err → 断言前两次该 log 落 `failed`（attempts 累加），不产出 chunk（留给重试），不误兜底。
3. 回归：现有 distillation 测试全绿；裸 HeuristicDistiller（无 LLM）行为不变。
4. 质量不回退：`eval.rs` 既有 4 项指标不受影响（蒸馏侧改动，不碰 recall 融合）。

## 8. 影响面 / 风险

| 改动 | 文件 | 风险 |
|---|---|---|
| 新增 `ResilientDistiller` + `provider_override` 字段 | `refine.rs` | 低（纯新增 + 一处字段） |
| distill_batch 写 provider 用 override | `kb/evolve.rs` | 低（一行 or 短逻辑） |
| open_kb 包装注入 | `lib.rs` | 低 |
| 新测试 | `tests/` | 无 |

- 主要风险：`distill_attempts` 自增时点导致 budget off-by-one → 用测试 2 锁死语义。
- 行为变化：LLM 持续不可用时，知识以"确定性低质版"被创建（pending、可治理），而非丢失——**这是期望的权衡**。

## 9. 实施顺序

1. `refine.rs`：`provider_override` 字段 + `ResilientDistiller`（含单测桩 distiller）。
2. `kb/evolve.rs`：写 `distill_provider` 处用 `provider_override`。
3. `lib.rs`：`open_kb` 包装。
4. 测试 1/2/3 + 全量回归 + clippy。
5. 同步 AGENTS.md 架构表（refine.rs 角色）+ 设计文档 v0.1.9。
```
