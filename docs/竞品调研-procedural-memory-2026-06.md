# Procedural-Memory 同类产品调研报告

> 调研日期：2026-06-19 · 来源：`github.com/topics/procedural-memory` 及相关研究
> 视角：为 Innate（Rust 程序性知识层）寻找差异化与改进空间

---

## 0. 摘要

`procedural-memory` topic 下与 Innate 真正同构（"让 agent 把成功经验沉淀为可复用程序"）的项目约 8 个。
各项目都收敛到**三类记忆（episodic / semantic / procedural）**的生物学框架，但在四个维度上分化明显：

1. **抽取方式**：LLM 蒸馏 vs 确定性算法（序列比对 / Hebbian）
2. **打分与遗忘**：相似度为主 vs 引入 ACT-R 激活 / Ebbinghaus 衰减 / 信念传播
3. **生命周期**：删除/归档 vs supersede 取代链 + 版本谱系
4. **可信度**：几乎全员**无评测**，仅 strata 拿出统计检验

Innate 的差异化定位：**唯一 Rust、本地优先、设计文档即编码基线、治理闭环 + 直觉层 critic**。
最大短板与全行业一致——**缺评测**；这恰是低成本高回报的差异化机会。

---

## 1. 竞品全景

| 项目 | 语言 / 星 | 一句话 | 最值得借鉴的独有机制 |
|---|---|---|---|
| **mengram** | Python / 178 | 类人三记忆 | 程序按失败反馈**自动版本演进**；`ask()` 带引用综合答案；多框架适配 |
| **kyros-ai** | Python / 92 | Memory OS + 遗忘曲线 | **类别化 Ebbinghaus 衰减**；**信念传播**冲突消解；Merkle 完整性 |
| **brainbox** | TS / 12 | Hebbian 文件共访 | 共访连边 + **2 跳扩散激活**；**反召回升级衰减**；bug 关联 2× 学习 |
| **myelin** | Python / 1 | 重复行为→程序 | **确定性程序抽取**（ACT-R + 多序列比对，零 LLM）；**跨 agent 迁移** |
| **strata** | Python / 5 | 跨会话记忆库 | **supersede 取代链**；**有真实评测**（McNemar）；MCP 只读防注入 |
| **gradata** | Python / 0 | 纠正→规则 | 纠正复利为行为规则（文档信息有限） |
| **WebSculpt** | TS / 2 | 浏览器流程→CLI | 把成功 workflow 固化为可复用命令 |
| **mnemonic / strata 系** | Py/HTML | Claude Code 文件记忆 | YAML/markdown 双时态持久化 |

研究前沿（非 topic 内但同向）：**A-Mem**（NeurIPS 2025 Agentic Memory）、**Mem0 / MemOS / Letta(MemGPT)** —— 代表"记忆即操作系统"与动态记忆操作的方向。

---

## 2. 逐项深度分析

### 2.1 mengram（178★，最成熟）
- **架构**：三记忆 + Cohere 多语 embedding（23 语）+ `search_all()` 统一检索。
- **杀手锏**：
  - **程序按失败演进**：用户报告失败 → 程序自动生成修正版本（v1→v2→v3），保留 failure context 与版本历史。这是"程序性记忆"最贴题的实现。
  - **`ask()` 综合答案 + 引用**：返回合成回答而非原始事实列表。
  - **生态**：MCP + LangChain + CrewAI(5 工具) + OpenClaw(12 工具) + REST/CLI；Claude Code hooks 自动存取；Cognitive Profile；多用户隔离；视觉文件上传。
- **弱点**：未公开衰减/遗忘细节；无评测。
- **Innate 可借**：程序**版本谱系**（演进而非覆盖）、综合答案模式、框架适配器。

### 2.2 kyros-ai（92★，机制最丰富）
- **架构**：App → SDK(Py/TS) → FastAPI → PostgreSQL + Redis + pgvector，`<20ms` 召回。
- **杀手锏**：
  - **类别化 Ebbinghaus 衰减**：不同类别不同半衰期（行情 1.4 天 / 用户身份 693 天），防膨胀。
  - **信念传播**：事实冲突时置信度沿语义图涟漪传播，自动冲突消解 + 关系跟踪。
  - **Merkle 完整性**：SHA-256 + Merkle 证明，防篡改审计。
- **弱点**：程序性记忆实现细节单薄；无评测；重基础设施（PG+Redis）不适合本地优先。
- **Innate 可借**：**类别化半衰期**（挂 `meta.curate.*`）、**冲突消解**（超越 dedupe）。完整性/审计对本地单用户偏重，可缓做。

### 2.3 brainbox（12★，神经科学派）
- **架构**：文件/工具/错误 = 神经元，序列共访（25 项窗口）建突触，位置衰减加权。
- **杀手锏**：
  - **Hebbian 学习**：SNAP sigmoid 可塑性（中点 0.5 防爆炸）；高频路径 **myelination**（BCM 滑动阈值，封顶 0.95）形成"超高速通道"。
  - **2 跳扩散激活**：召回一个文件激活相关文件（fan-out 上限 + 度归一化）。
  - **反召回升级衰减**：忽略建议则惩罚递增（10%→19%→27%）。
  - **错误增强学习**：bug-fix 关联 2× 学习率。
- **弱点**：偏"文件共访预测"，非真正的程序步骤；无评测。
- **Innate 可借**：**从真实用法学共访图**（现有 dep 是人工/蒸馏的）、反召回升级（被忽略的知识加速衰减）、失败关联加权。

### 2.4 myelin（1★，算法最硬核 —— 与 Innate 最互补）
- **架构**：监控 episodes → ACT-R 激活打分 `B(i)=ln(Σtⱼ⁻ᵈ)` + 聚类 → **ClustalW 式多序列比对**抽共识工作流 → SQLite 存储。
- **杀手锏**：
  - **确定性程序抽取**：多序列比对从重复 action 序列抽程序，**不依赖 LLM**，可复现可解释零 API 成本。
  - **贝叶斯置信**：成功 `c+=（1−c)*0.15`，失败 `c*=0.85`，trust 等级 candidate/validated/trusted。
  - **5 信号融合检索**：文本 25% + 向量 25% + 实体 20% + 时序 15% + 激活 15%。
  - **跨 agent 能力感知迁移**：按目标工具集改写程序步骤。
- **弱点**：单人项目、星少、成熟度低；无评测。
- **Innate 可借**：**确定性序列抽取**（作为 LLM distill 的互补通道，不违反单 LLM 原则）、**ACT-R 激活**（✅ 已采纳，见 §4）、**跨 agent 迁移**（直接服务 AutoForge）。

### 2.5 strata（5★，工程最克制 —— 唯一有评测）
- **架构**：三记忆；SQLite FTS5 + 可选 on-device fastembed 混合检索；路径路由（问程序返回 runbook）。
- **杀手锏**：
  - **supersede 取代链**：不自动衰减，而是显式生命周期——程序变更时旧条目"被取代"并挂前向链接到新版，保留历史推理又不展示过时内容。
  - **真实评测**：`stale-suppression off 7/19 → on 19/19`，配对 McNemar `12–0, p≈0.0005`。**全 topic 唯一拿统计检验说话的**。
  - **MCP 只读防注入**：刻意不开放 MCP 写，写入需用户显式确认，消除静默 prompt injection。
- **弱点**：无自动衰减（靠人工 supersede）；规模/生态有限。
- **Innate 可借**：**supersede 取代链 + 陈旧抑制**（优于当前 archive/invalidate 终态删除）、**评测方法学（McNemar 配对检验）**、**MCP 写入注入面审视**。

### 2.6 其余
- **gradata**：纠正→行为规则复利，理念好但公开信息不足。
- **WebSculpt**：浏览器成功流程→可复用 CLI 命令，是"程序"的具体落地形态，可启发 Innate 对"工具调用序列"类程序的建模。
- **mnemonic / 双时态 YAML 系**：面向 Claude Code 的文件式记忆，双时态（bi-temporal）值得注意——Innate 目前只有单时间轴。

---

## 3. 横向能力矩阵

| 能力 | mengram | kyros | brainbox | myelin | strata | **Innate** |
|---|:--:|:--:|:--:|:--:|:--:|:--:|
| 语言/部署 | Py/云 | Py/云重 | TS | Py | Py/本地 | **Rust/本地** |
| 向量检索 | ✅ | ✅ | — | ✅ | 可选 | ✅ 纯 Rust |
| 多信号融合 | 部分 | 部分 | ✅ | ✅(5) | 部分 | ✅(**5，含 ACT-R**) |
| ACT-R 激活 | — | — | 近似 | ✅ | — | ✅(新增) |
| 衰减/遗忘 | ? | ✅ 类别化 | ✅ | ✅ | 人工 supersede | ✅ 全局指数 |
| 冲突消解 | — | ✅ 信念传播 | — | — | supersede | dedupe |
| 程序版本谱系 | ✅ | — | — | ✅ trust | ✅ supersede | 部分 |
| 确定性抽取 | — | — | ✅ | ✅ | — | — (LLM distill) |
| 跨 agent 迁移 | — | — | — | ✅ | — | — |
| 治理/审批闭环 | — | — | — | — | 写需确认 | ✅ 完整 |
| 直觉层 critic | — | — | — | — | — | ✅ appraise |
| 评测/基准 | — | — | — | — | ✅ McNemar | ❌ |
| MCP | ✅ | ✅ | ✅ | ✅(21) | ✅ 只读 | ✅(14) |

**读法**：Innate 在**治理闭环、直觉层 critic、Rust 本地一体化**上独一份；在**确定性抽取、跨 agent 迁移、评测**上落后；衰减比 kyros 粗、生命周期比 strata 简单。

---

## 4. 已落地改进 #2 —— ACT-R 激活项

本次调研同步实现了 myelin 同源的 ACT-R 基础水平激活，补齐 Innate 检索缺失的"近因×频次"信号：

- **公式**：`activation = σ( ln(1+used_count) − 0.5·ln(1+recency_days) )`，`σ` 为 logistic，输出 `(0,1)`。
- **融入**：`kb/recall.rs::score_candidates` 第 5 融合信号；`kb/appraise.rs` 计入 calibration 分量（保持二者同一融合公式）。
- **参数**：新增 `recall.w_activation`（默认 `0.08`，meta 可调）；`inspect` 已打印。
- **零回归保证**：`used_count==0` 或无 `last_used_at` → 激活恒为 `0`，新加知识/测试不受影响。
- **验证**：3 个单测（零值/单调性/有界）+ 全量 147 测试通过；设计文档 v0.1.9 §5.2 已同步。

---

## 5. 后续改进路线（按杠杆排序）

| # | 改进 | 借鉴自 | 杠杆 | 状态 |
|---|---|---|---|---|
| 1 | **评测基准套件**（precision@k / stale-suppression / 衰减漂移 + 配对检验） | strata | ★★★ 最大空白，全行业唯一差异化点 | 待做（建议下一步） |
| 2 | ACT-R 激活项 | myelin | ★★★ | ✅ 已完成 |
| 3 | **确定性序列抽取**（多序列比对，作 LLM distill 互补通道） | myelin | ★★ 降 LLM 依赖、可解释 | 待做 |
| 4 | **跨 agent 能力感知迁移** | myelin | ★★ 直接服务 AutoForge | 待做 |
| 5 | **supersede 取代链 + 陈旧抑制** | strata/mengram | ★★ 治理增强 | 待做 |
| 6 | **类别化衰减半衰期** | kyros | ★ | 待做 |
| 7 | **冲突消解（信念传播）** | kyros | ★ | 待做 |

**建议**：立刻做 #1 评测套件——它是后续所有调参/改动的安全网，且是 Innate 相对全行业最容易建立的可信度护城河。#3/#4 是与 AutoForge 强相关的中期结构性投资。

---

## 6. 结论

- 行业已就"三记忆 + 多信号检索 + 遗忘"形成共识，差异在**抽取确定性、生命周期精细度、可信度证明**。
- Innate 的**治理闭环 + 直觉层 critic + Rust 本地一体化**是差异化资产；本次补上 ACT-R 激活后，检索信号已与最硬核的 myelin 持平。
- 真正能拉开身位的是 **#1 评测**——把"自成长有效"从口号变成可复现的统计结论，这是同类项目集体缺位的高地。
