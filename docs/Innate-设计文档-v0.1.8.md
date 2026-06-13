# Innate 自成长 Agent 程序性知识层设计文档 v0.1.8

**软件版本：0.1.8**  
**数据库 Schema：4.14**
**状态：当前实现基线**  
**校准日期：2026-06-12**  
**事实源：`core/src`、数据库迁移、SDK 与当前测试**

> 本文描述 Innate 0.1.8 的实际实现。软件发布版本使用 SemVer，数据库 Schema 使用独立迁移版本；两者不要求数值相同。旧版文档保留为历史设计记录，当旧文档与本文或代码冲突时，以当前代码和本文为准。

## 1. 系统定位

Innate 是面向 Agent 的本地程序性知识层。它把任务过程中的检索、使用、结果和显式反馈持久化，再通过蒸馏与治理把有效经验转化为可复用知识。

核心飞轮：

```text
Recall
  检索、排序、装包、生成 trace
    ↓
Agent 执行任务
    ↓
Record
  记录结果、知识使用、反馈和任务材料
    ↓
Evolve
  从可蒸馏日志中提炼候选知识
    ↓
Curate
  聚合、晋升、衰减、归档、去重和治理
    ↓
下一次 Recall
```

系统的目标不是保存完整对话，而是形成可审计、可调整、可淘汰的程序性知识。

## 2. 设计目标与边界

### 2.1 目标

- 在任务开始前返回与当前问题相关的程序性知识。
- 在任务结束后记录知识是否被使用、任务是否成功以及用户或评审反馈。
- 让高价值知识通过成功使用逐步晋升，让低价值知识通过失败、未使用、负反馈和时间衰减退出。
- 从任务材料中提炼新的通用原则，而不是复制原始输出。
- 所有关键决策都保留可查询的事实和状态。
- 单机、本地优先，核心能力不依赖常驻服务。

### 2.2 非目标

- 不作为聊天记录或文档全文存储系统。
- 不提供分布式多写者一致性协议。
- 不承诺语义级上下文聚类；当前上下文统计基于规范化查询哈希。
- 不自动证明蒸馏内容正确；候选知识仍需后续使用和治理。

## 3. 当前架构

```text
CLI ───────────────┐
MCP Server ────────┤
Python SDK ── CLI ─┤
TypeScript SDK ────┤── KnowledgeBase ── SQLite
Daemon ────── CLI ─┘         │
                             ├── EmbeddingProvider
                             ├── Distiller
                             ├── Sanitizer
                             └── Refiner
```

### 3.1 核心模块

- `KnowledgeBase`：Recall、Record、Evolve、Curate 和知识生命周期入口。
- `Storage`：SQLite 连接、事务、查询、向量缓存和余弦相似度搜索。
- `EmbeddingProvider`：内容向量与触发条件向量生成。
- `Distiller`：把 episodic log 提炼为候选知识。
- `Sanitizer`：在写入知识前执行拒绝或脱敏。
- `Refiner`：预算不足时裁剪，或在返回前适配知识。

### 3.2 存储与向量检索

- 数据库为本地 SQLite。
- 向量以 `f32` BLOB 存入 `vec_content` 和 `vec_trigger`。
- 检索由 Rust 执行余弦相似度计算，并使用进程内缓存。
- 当前没有 `sqlite-vec` 运行时依赖，也没有独立向量数据库。
- 数据库 schema 目标版本为 `4.14`。

### 3.3 模型配置

`~/.innate/settings.json` 可配置：

- LLM：OpenAI-compatible 或 Anthropic。
- Embedding：OpenAI-compatible，默认维度 `1536`。
- Daemon：Linux 目录监听与自动启动。

通过统一入口打开知识库时，会按设置注入远程模型；直接调用 `KnowledgeBase::open` 时使用本地 Dummy Embedding 和启发式 Distiller。

### 3.4 文件系统布局

所有本地状态位于 `~/.innate/`，按用途分为三个子目录，根目录只保留配置文件：

```text
~/.innate/
  settings.json            ← 用户配置（LLM / Embedding / Daemon / Backup）
  settings.schema.jsonc    ← settings.json 的 JSON Schema
  data/                    ← 数据库与运行时状态
    personal.db (+ -shm, -wal)   知识库主库
    daemon_state.sqlite          Daemon 私有状态（偏移量/inode/去重）
    daemon.pid                   Daemon 进程号
    backup_state.json            上次备份时间缓存
    tmp/                         备份 VACUUM 临时副本等临时文件
  logs/                    ← 运行日志
    daemon.log
    mcp.log
  sessions/                ← Agent 会话轨迹（Daemon 监听目录）
    session.log
```

- 路径由 `core/src/paths.rs` 统一定义，是唯一真相源；其他模块不得自行拼接 `~/.innate` 路径。
- `paths::ensure_layout()` 在 CLI 与 MCP 启动时执行：创建上述子目录，并把旧版扁平布局（`~/.innate/<file>`）中的文件迁移到对应子目录。迁移幂等、best-effort（单个移动失败不中断启动），且仅当目标不存在时才搬，数据库随其 `-shm`/`-wal` 一并迁移以保持 SQLite 一致性。
- 默认知识库路径为 `~/.innate/data/personal.db`；可用 `--db` 覆盖。
- `innate vacuum` 在 Curate 压缩与 trace 清理后回收磁盘空间（checkpoint WAL + VACUUM）。

## 4. 核心数据模型

### 4.1 知识块 `chunks`

主要来源：

| origin | 含义 | 初始状态 | 默认置信度 |
|---|---|---:|---:|
| `installed` | 安装的 Skill 知识 | `active` | `0.85` |
| `captured` | 手工或 Agent 捕获 | `active` 或 `pending` | `0.60` |
| `distilled` | 从任务日志蒸馏 | `pending` | `0.55` |
| `spark` | 未成熟想法 | 独立成熟度状态 | 单独管理 |

通用状态：

```text
pending ──满足晋升条件──> active
   │                       │
   └────治理/低价值───────┴──> archived
```

当前 Recall 排除 `archived` 和 `spark`。`pending` 允许进入候选集以完成冷启动，但最终融合分乘以 `0.60`，避免未经验证的知识与 active 知识等权竞争。

### 4.2 任务日志 `episodic_log`

每个 trace 对应一条任务日志，保存：

- 查询、输出、摘要和提名材料。
- 任务状态与结果。
- 使用的知识 ID 和归因方式。
- Recall 快照和上下文键。
- 蒸馏状态、锁和成本信息。

任务状态：

```text
recalled → running → completed
                   ↘ abandoned
                   ↘ timed_out
```

蒸馏状态：

```text
open → new → screening → distilled
                  └────→ failed

open ──无有效材料/过期──> discarded
```

### 4.3 反馈与治理事实

- `usage_trace`：retrieved、selected、refined、used、task_ok、task_fail。
- `feedback_events`：用户或 judge 的 up/down 事实。
- `chunk_context_stats`：知识在特定上下文中的成功、失败和反馈计数。
- `governance_proposals`：由持续负反馈形成的治理提案。
- `evolve_requests`：等待 Evolve 消费的持久请求。
- `chunk_success_traces`：Curate 聚合后的成功使用事实。

## 5. Recall：知识进入任务

### 5.1 检索流程

```text
query
  → 内容 embedding + trigger embedding
  → 两路向量候选合并
  → soft dependency 加分
  → 融合评分与 anti-trigger 惩罚
  → Top-K
  → 预算装包与 hard dependency 扩展
  → density refill
  → 可选 trim/adapt
  → 写入 trace 和 open episodic log
```

### 5.2 融合评分

当前默认参数：

```text
score =
    0.55 * content_similarity
  + 0.25 * trigger_similarity
  + 0.10 * confidence
  + 0.15 * context_score
```

注意：权重总和为 `1.05`，当前实现没有再次归一化。

如果查询命中 `anti_trigger_desc`，融合分乘以 `0.6`。

候选数默认最多 `20`。Soft dependency 对候选内容相似度增加 `0.05`；hard dependency 在装包阶段按 direct 或 closure 展开。

### 5.3 上下文分数

上下文键生成方式：

```text
lowercase(query)
  → 标点 → 空格
  → 分词，过滤停用词（a/an/and/for/in/of/on/the/to/with）
  → 词序排序 + 去重
  → 合并为空格分隔字符串
  → SHA-256
```

统计分数：

```text
wins   = success_count + 2 * positive_count
losses = failure_count + 2 * negative_count
posterior = (wins + 1) / (wins + losses + 2)
evidence_weight = min((wins + losses) / 5, 1)
context_score = (posterior - 0.5) * 2 * evidence_weight
```

这能合并大小写、标点、停用词和词序差异，但不能合并同义改写。

### 5.4 Trace

启用 trace 后，Recall 原子写入：

- 每个候选的 `retrieved`。
- 最终返回知识的 `selected`。
- 被裁剪知识的 `refined`。
- Spark 的 retrieval。
- 一条 `distill_state=open` 的 episodic log。

Trace 是后续使用归因、结果反馈和蒸馏的关联主键。

## 6. Record：反馈闭环入口

### 6.1 Record 输入

Record 可接收：

- `outcome`：`ok`、`fail`、`unknown`。
- `used`：实际使用的知识 ID。
- `used_attribution`：`explicit`、`cited`、`inferred`。
- `used_complete`：`used` 是否完整覆盖所有实际使用知识，默认 `true`。
- `feedback_up` / `feedback_down`。
- `feedback_kind`：`user` 或 `judge`。
- 输出、摘要、提名、任务状态和优先级。

`unknown` 是可解析的暂态结果，trace 保持 running/open，允许后续更新为 `ok` 或 `fail`。一旦已有最终结果，再写入不同最终结果会返回 `OutcomeConflict`。

### 6.2 使用归因

| 归因 | confidence signal strength |
|---|---:|
| `explicit` | `0.30` |
| `cited` | `0.25` |
| `inferred` | `0.15` |

成功任务中被使用的知识获得正向隐式信号。失败任务仍保留使用事实，但置信更新强度减半。

归因 ID 必须来自该 trace 实际 `selected` 的知识，只有 retrieved 但未进入上下文的候选不能声明为 used 或反馈对象。输入 ID 会先去重。

`used_complete=false` 表示增量事实：新 ID 与该 trace 已知 used 合并，未出现的知识不会受到惩罚。`used_complete=true` 表示完整快照，会整体替换旧声明。完整列表中未出现的 selected 知识即使 outcome 尚未到达，也会收到：

```text
target = 0.0
strength = 0.08
reason = selected_unused
```

Schema 4.8 前该路径以 `0.3` 为目标，低置信知识可能反而被抬高；当前实现已修复为始终向下调整。

### 6.3 显式反馈

| 来源 | strength |
|---|---:|
| 用户反馈 | `1.0` |
| Judge 反馈 | `0.6` |

每条反馈先尝试写入 `feedback_events`，再写入可重放的 `confidence_evidence`：

- 全局 confidence。
- 对应 `context_key` 的上下文统计。
- 负反馈治理证据。

置信度采用带强度的 EMA，基础 alpha 为 `0.2`。派生信号按 trace 唯一保存并从 `confidence_base` 重放，因此重复、乱序和 used 修正不会重复施加旧信号。显式反馈会依据距上次使用时间获得最高 `1.5` 的新近度系数。

同一次 Record 不允许同一知识同时出现在 up/down 中。相同 trace 后续提交反向信号时视为修正：删除旧信号并重放新信号，而不是同时累计正负证据。

### 6.4 日志是否进入蒸馏

任务完成且具备以下任一有效材料时，日志进入 `new`：

- `output_summary`。
- `nomination`。
- `output`。

只有 query、used 和 outcome 不足以生成可靠程序性知识；缺少上述材料时日志进入 `discarded`，避免无依据内容进入 LLM 和知识库。

Record 支持乱序补写。已完成但因 `insufficient_material`、`abandoned` 或 `timed_out` 丢弃的日志，在后续补入 output、output_summary 或 nomination 后重新进入 `new`；过滤、失效或已蒸馏终态不会被误重开。

### 6.5 Record 后的演化触发

事务提交后，系统判断是否写入 `evolve_requests`：

- 可蒸馏日志数达到 `5`：`threshold`。
- 已有日志且最老日志超过调度间隔：`scheduled`。
- pending 治理提案数达到 `3`：`governance`。
- 任一治理提案证据达到归档阈值 `3`：`governance_ready`。

`governance_ready` 是 Schema 4.8 的关键修复：单个知识的证据充分时，不再等待多个不同治理提案。

Record 只负责事务内写事实并提交演化请求，不在调用链内同步访问 LLM。请求由 daemon、CLI 或 MCP 的 Evolve 消费，避免反馈写入被网络延迟阻塞。

## 7. Evolve：从任务材料生成候选知识

### 7.1 触发模式

- `manual`：显式执行，不要求已有请求。
- `scheduled`：主动恢复到期失败日志、唤醒超过调度间隔的 `new` 日志；没有可领取请求时仍执行 Curate。
- `threshold`：根据可蒸馏日志数量触发。

Evolve 会领取一条请求并设置 lease；过期的 running 请求可恢复，避免进程异常后永久卡住。低于阈值或达到自动蒸馏 token 上限时，蒸馏类请求保持 `pending` 并设置 `next_retry_at`，不会被错误标记完成；治理类请求在 Curate 完成后直接结束。

### 7.2 蒸馏事务

```text
claim new logs → screening
  → 调用 Distiller
  → sanitize
  → 生成双向量
  → 原子写入 chunk、vectors 和日志终态
  → builtin Curate
```

Distiller 按日志隔离执行，每条日志可以产出零到多个候选知识。每次调用只接收同批、同 `context_key` 的最多四条相关交互作为重复模式和冲突判断上下文，输出仍必须归属于当前主日志。一条日志调用或解析失败只把该日志标记为 `failed`，同批其他日志继续处理。

失败日志在 5 分钟冷却后由 scheduled 或其他 Evolve 自动恢复并写入 `distill_retry` 请求，单日志最多尝试三次；`distill_attempts` 和 `distill_last_failed_at` 持久化保存，因此达到上限后不会形成无限成本循环，成功重试也不会抹除历史失败指标。每次尝试写入 `distill_token_usage`，预算和 Inspect 使用累计账本而非日志最后一次调用值。模型调用设置 30 秒超时和 800 token 输出上限。

`nomination` 是蒸馏输入和来源标记，不被默认视为可信人工内容。配置 LLM 时 nomination 仍需经过泛化与格式校验；启发式降级路径产出的知识保持 `pending` 并受 Recall 降权约束。

普通蒸馏知识初始：

```text
origin = distilled
state = pending
confidence = 0.55
```

发生脱敏时初始置信度为 `0.40`。

## 8. Curate：让知识变好或退出

### 8.1 聚合与清理

Curate 以迁移基线加保留的 selected、used 和 task outcome 事实重算计数。用于归因和修正的紧凑事实不再删除，也不会在 Evolve 成功后折入新基线；仅清理可重建的 retrieved/refined 明细。Spark retrieval 会保留用于重复出现统计。

Restore 会写入 `evidence_cutoff_at`，将 confidence、使用计数和上下文统计重置到新的人工观察窗口。历史事实仍保留用于审计，但 Curate、上下文统计和治理只重放 cutoff 之后的事实，避免恢复后被旧失败立即再次归档。

同时处理：

- 超时的 `screening` 日志恢复为 `failed`。
- 超过 `14` 天的 open 日志进入 `discarded` / `timed_out`。
- 超过保留期的终态 episodic log 清除 query/output 等大字段，但保留紧凑 trace 身份和归因事实，用于审计与修正。

### 8.2 晋升

Pending 知识满足以下条件后晋升 active：

```text
used_success_count >= 3
distinct successful traces >= 2
confidence >= 0.60
```

Schema 4.8 将普通蒸馏初始置信度提高到 `0.55`，并把晋升阈值降至 `0.60`。因此蒸馏知识可以只依靠后续成功使用跨过阈值，闭合：

```text
distilled pending
  → Recall 命中
  → Record used + outcome=ok
  → confidence 上升
  → Curate promote
  → active
```

### 8.3 衰减与归档

非 protected active/pending 知识按 90 天半衰期向 floor 衰减：

```text
new_confidence =
  floor + (confidence - floor) * 0.5 ^ (idle_days / 90)
```

Schema 4.8 的 floor 为 `0.20`，低于归档阈值 `0.25`。这修复了旧 floor `0.30` 导致知识永远无法仅靠衰减进入低置信归档的问题。

主要归档路径：

| 路径 | 默认条件 | state_reason |
|---|---|---|
| 低置信闲置 | confidence `< 0.25` 且闲置 `60` 天 | `low_confidence` |
| 重复入选未用 | selected 至少 `10` 次且 confidence `< 0.5` | `repeated_selected_unused` |
| 长期未使用 | 创建后 `30` 天仍未使用 | `never_used` |
| 治理提案 | pending proposal evidence `>= 3` | `governance_proposal` |
| 持续任务失败 | 使用至少 `5` 次、成功率 `< 20%`、confidence `< 0.35` | `sustained_task_failure` |
| 内容重复 | 相同 content hash，保留 protected 或更高置信项 | `duplicate` |

Protected 和 Spark 不走普通自动归档路径。

### 8.4 负反馈治理

治理证据按反馈 strength 加权，并采用 `90` 天半衰期衰减。单个 actor 的净贡献限制在 `[-1, 1]`；无 actor 时按 `anonymous:<source>` 聚合，不会把每个 trace 伪装成独立用户。Record 在新反馈到达时同步刷新对应 chunk，Curate 只重算仍为 pending 的治理提案，不再扫描全部历史反馈：

```text
净负分 < 2 或有效 actor < 2：不创建 proposal
净负分 >= 2 且有效 actor >= 2：创建 review_applicability proposal
净负分 >= 3 且有效 actor >= 2：写入 governance_ready 请求
Curate：归档知识并接受 proposal
```

直接的 `5` 次负反馈归档是第二条保险路径。默认阈值下，治理提案路径通常会先发生。

## 9. 反馈闭环评估

### 9.1 已闭合路径

1. **成功使用闭环**

```text
Recall selected
→ Record used + ok
→ confidence/context success
→ Curate promotion
→ 后续 Recall 排名提升
```

2. **未使用闭环**

```text
Recall selected
→ Record 提供 used 列表但未包含该知识
→ selected_unused 向下更新
→ 重复发生后归档
```

3. **显式正反馈闭环**

```text
feedback_up
→ 持久反馈事实
→ confidence 与 context score 提升
→ 同类查询排名提升
```

4. **显式负反馈闭环**

```text
feedback_down
→ 持久反馈事实
→ confidence 与 context score 下降
→ governance proposal
→ evolve request
→ Curate 归档
```

5. **新知识成长闭环**

```text
任务材料
→ Distill pending@0.55
→ 成功使用
→ confidence >= 0.60
→ Curate active
```

6. **自然淘汰闭环**

```text
长期闲置
→ confidence 向 0.20 衰减
→ 跌破 0.25
→ Curate 归档
```

### 9.2 Schema 4.14 后的剩余边界

1. Pending 仍允许 Recall，用于通过真实任务完成冷启动，但融合分按 `0.60` 降权；Curate 会对长期未使用、持续失败和低置信 pending 自动归档。
2. 上下文键已做大小写、标点、停用词、词序归一化，但真正的同义表达仍需要后续语义聚类。
3. 未提交 `used` 时系统不会猜测 selected-unused；调用方必须通过 `used_complete` 明确完整或部分标注。
4. 任务 outcome 默认在多个 used chunk 间均分证据强度；需要更精确因果归因时应提交 chunk 级显式反馈。
5. 治理共识只统计非空 actor，匿名反馈仍影响 confidence 但不形成多人治理；actor 身份由集成方提供，Core 尚不负责外部身份认证。
6. selected、used、outcome 和 feedback 紧凑事实为支持任意时间修正仍长期保留，数据库体积需要后续引入有界修正窗口和可校验 checkpoint。

## 10. 可观测性

`inspect` 当前提供：

- chunk、log、rebuild queue 和债务比例。
- stale screening、蒸馏成本与 recurring sparks。
- trace completion rate。
- usage annotation rate。
- trace use rate 和 selected-to-used rate。
- task success rate。
- feedback coverage 和 feedback event 数。
- timed-out traces。
- pending evolve requests 和 governance proposals。
- confidence distribution。

闭环健康至少应同时观察：

```text
Record 完成率
used 标注率
selected → used 转化率
显式反馈覆盖率
pending → active 晋升率
active → archived 淘汰率
evolve request 等待时间
```

## 11. 并发与失败处理

- 关键写操作使用 `BEGIN IMMEDIATE`。
- Recall trace 与 open log 原子写入。
- Record 的结果、使用、反馈、上下文和治理更新在同一事务中完成。
- Evolve 先 claim 日志，再进行模型调用和最终写入。
- 蒸馏写入 chunk、双向量和日志终态时保持原子性。
- Evolve request 使用 running lease，可恢复异常中断。
- Outcome 冲突显式报错，不做最后写入覆盖。
- Sanitizer 在知识持久化前执行。

## 12. 接口边界

### 12.1 CLI / MCP

主要命令或工具：

- `recall`
- `record`
- `add`
- `spark`
- `evolve`
- `inspect`
- `approve`
- `archive`
- `invalidate`
- `restore`
- Spark 成熟、晋升和丢弃操作

MCP 直接调用 Rust Core。配置 daemon 自动启动且存在 watch dirs 时，MCP 会尝试启动 daemon，但失败不会阻止 MCP 主流程。

### 12.2 SDK

- Python SDK 通过 `innate` CLI 子进程访问核心。
- TypeScript SDK 同时提供 CLI 子进程和 MCP client 入口。
- Python `augmented` 装饰器可自动执行 recall、running record、成功或失败 record。

SDK 不拥有独立业务规则；状态和治理规则以 Rust Core 为准。

## 13. 0.1.8 版本与 Schema 演进

Innate 的对外软件版本与数据库 Schema 独立演进。本设计文档对应的软件发布线为 `0.1.8`，当前数据库 Schema 为 `4.14`。

### 13.1 Schema 4.8 反馈闭环变化

| 变化 | Schema 4.7 | Schema 4.8 | 闭环影响 |
|---|---:|---:|---|
| 蒸馏初始 confidence | `0.45` | `0.55` | 候选知识可通过成功使用接近晋升线 |
| 晋升 confidence 下限 | `0.65` | `0.60` | 隐式成功信号可使候选知识自动晋升 |
| confidence decay floor | `0.30` | `0.20` | 闲置知识可跌破 `0.25` 归档线 |
| selected-unused target | `0.30` | `0.00` | 低置信知识不再被“未使用”信号抬高 |
| governance evolve | 按 pending proposal 总数 | 增加单 proposal ready 判断 | 证据充分时立即入队 |
| context key | 原始 query hash | 小写并合并空白后 hash | 基础上下文反馈更稳定 |

数据库迁移 `4.7 → 4.8` 更新：

```text
curate.promote_confidence_min = 0.60
curate.decay_floor = 0.20
schema_version = 4.8
```

### 13.2 Schema 4.9 治理一致性变化

- 为 `feedback_events(trace_id, chunk_id, signal)` 增加唯一索引。
- 反馈事实写入使用 `INSERT OR IGNORE`，避免事实表重复累计。
- 治理证据改为净负反馈 `down - up`。
- 只有实际完成治理归档的 proposal 才进入 `accepted`；不符合条件的 proposal 进入 `rejected`。
- migration target、初始化 schema 和运行时期望版本统一为 `4.9`。

### 13.3 Schema 4.12 反馈飞轮变化

- trace 只能反馈或声明使用其 Recall 快照中的知识。
- outcome 与 used 可乱序到达；派生置信度证据按 trace 重放。
- `unknown` outcome 可解析为最终结果；已提交的 ok/fail 也可被后续纠正，旧 task outcome 事实会被替换并重放派生证据。
- 完整 used 声明整体修正，部分 used 声明增量合并。
- 归因范围收紧到实际 selected 知识；retrieved-only 候选不可反馈或声明使用。
- selected/used/success 计数从迁移基线与保留事实重算。
- Curate 不再把可重放事实反复折入基线，也不提前清除终态归因和反馈证据。
- Pending 纳入衰减、长期未使用和持续失败退出路径。
- 治理证据按 strength、actor、90 天半衰期和单 actor 上限计算。
- Evolve 不再丢弃不同原因的请求，失败请求最多自动重试三次。
- Inspect 统一使用 30 天窗口，并公开失败请求和失败蒸馏数量。
- LLM 输入调用前脱敏，格式错误重试，设置请求超时，多 chunk 与生成来源可审计。
- Distiller 逐日志隔离失败；没有 output、summary 或 nomination 的任务不会进入 LLM 蒸馏。

### 13.4 Schema 4.13 闭环可靠性变化

- 移除成功 Evolve 后的证据折叠和 attributed event 清理，终态 trace 仍可修正 used、outcome 和反馈。
- `4.11 → 4.12` 迁移基线扣除仍保留的 usage facts，避免首次 Curate 双计数。
- Record 仅提交事实和 evolve request，不同步执行 Evolve 或访问 LLM。
- `task_state=completed` 即进入蒸馏终态判断，不再因缺少 outcome 留在 open 后被误判超时。
- 部分 used 上报保留每个 chunk 自己的 attribution 和 strength；完整上报仍可整体替换。
- Restore 形成新的人工治理分界点：历史反馈保留审计，但不会立即再次触发归档；新反馈仍可重新治理。
- Pending Recall 融合分乘以 `0.60`。
- Distiller 获得同批 related-log 上下文，同时保持逐主日志失败隔离。
- nomination 不再绕过 LLM 泛化。
- 新增 `distill_attempts`、`distill_last_failed_at` 和 evolve `last_failed_at`，重试上限与失败历史可审计。
- usage annotation 指标仅以 completed trace 为分子范围，比例不会因运行中 trace 超过 `1.0`。

### 13.5 Schema 4.14 飞轮可达性变化

- scheduled Evolve 自动恢复到期 failed 日志，并为 aged new 日志建立持久请求。
- 低阈值和 token 预算门改为延期蒸馏请求，不再把未消费日志对应请求标记完成。
- 自动触发统一执行 token 预算；manual 保留显式运维覆盖能力。
- 每次蒸馏尝试写入 `distill_token_usage`，重试成本可累计审计。
- Record 支持任务完成后补入材料并重开 `insufficient_material` 日志。
- ok/fail outcome 支持纠正，task outcome 与 confidence evidence 同步替换。
- Restore 使用 `evidence_cutoff_at` 开启新观察窗口，清除旧派生状态但保留历史事实。
- 匿名反馈不计入多人治理共识，避免同一来源被误当成独立 actor。
- related-log 仅限相同 `context_key`，降低跨任务污染。
- Inspect 的 task success、selected-to-used 和蒸馏成本使用一致 cohort 与累计账本。
- `4.13 → 4.14` 迁移修复早期 4.12/4.13 聚合基线，并回填已有蒸馏成本。

## 14. 0.1.8 验收基线

当前测试覆盖的关键闭环包括：

- 显式反馈可审计并积累上下文治理证据。
- selected-unused 始终降低 confidence。
- 治理提案达到阈值后 Curate 归档。
- 持续负反馈可直接归档。
- pending governance 可写入 evolve request。
- 蒸馏知识可仅靠隐式成功信号越过 `0.60`。
- `0.20` decay floor 允许低置信闲置归档。
- 单一治理提案 ready 时立即写入 `governance_ready` 请求。
- 连续 Curate 不重复累计开放 trace，终态 trace 在 Curate 后仍可修正归因。
- 部分 used 增量合并，完整 used 整体替换。
- 相反反馈修正旧信号，单日志蒸馏失败不拖垮同批成功日志。
- 完整 Evolve 后再次 Curate 仍保持 selected/used/success 计数守恒，且反馈可以反向修正。
- 部分 used 上报保持每个 chunk 的 attribution，不被后续增量报告覆盖。
- 失败蒸馏最多尝试三次，历史失败在成功或停止重试后仍可观测。
- scheduled 无现有请求时仍可自动重试失败日志，预算受限请求保持 pending。
- 完成后补入材料可重新进入蒸馏，ok/fail outcome 可纠正并重放。
- Restore 后旧失败事实不会触发立即再次归档，匿名反馈不能形成治理共识。
- related-log 仅在相同上下文内传入 Distiller，nomination 经过泛化路径。
- migration chain 最终到达 schema `4.14`，并覆盖已存在 4.13 数据修复。
- 所有对外软件包版本均为 `0.1.8`。

## 15. 后续优先级

按反馈闭环风险排序：

1. 将上下文统计从规范化词法指纹升级为可控的语义聚类。
2. 增加 chunk 级任务贡献权重，进一步提高多知识任务的因果归因精度。
3. 增加带 checkpoint 的长期事实压缩策略，在可重放、修正窗口与数据库体积之间取得平衡。
4. 将治理 actor 绑定到宿主认证身份，避免调用方伪造多个 actor。

---

**文档结论：** 当前实现使用数据库 Schema 4.14。反馈事实、置信度证据、上下文统计和使用计数已经形成可审计、可重放、支持乱序和修正的闭环；自动 Evolve 对失败、预算延期和低流量日志保持可达，Restore 从新证据窗口重新成长。剩余重点是语义上下文聚类、长期事实 checkpoint、可信 actor 身份和更细粒度因果贡献建模。
