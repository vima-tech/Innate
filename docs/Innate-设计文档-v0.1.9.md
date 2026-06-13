# Innate 自成长 Agent 程序性知识层设计文档 v0.1.9

**文档版本：0.1.9（模块化代码基线校准）**
**软件版本：0.1.9（`core/Cargo.toml`）**
**数据库 Schema：4.14**
**状态：当前实现基线（编码基线）**
**校准日期：2026-06-13**
**事实源：`core/src`、`core/src/schema.sql`、迁移链、SDK 与当前测试（104 项全过）**

> 版本说明：Innate 的「软件发布版本（SemVer）」「数据库 Schema 版本」「设计文档版本」三者独立演进，数值不要求一致。
> - 软件版本 `0.1.9`：见 `Cargo.toml`，对外发布线。
> - Schema 版本 `4.14`：见 `schema.sql` 与迁移链。
> - 文档版本 `0.1.9`：本文在 v0.1.8 文档基础上，按 **模块化重构（提交 `a6274a8`，将单体源文件拆分为聚焦模块）**、**install 写入加固（`a6d6fd6`）** 与 **死代码清理（`claim_evolve_request` 删除、`HttpDistiller` 合并）** 重新校准。核心反馈飞轮行为与 Schema 相对 v0.1.8 未变，本文新增对安装、Daemon、备份、自更新、迁移、Hook 等此前欠覆盖模块的完整描述。
>
> 当旧文档（`Innate-设计文档-v0.1.8.md` 及更早版本）与本文或代码冲突时，以当前代码和本文为准。

---

## 1. 系统定位

Innate 是面向 Agent 的本地程序性知识层。它把任务过程中的检索、使用、结果和显式反馈持久化，再通过蒸馏与治理把有效经验转化为可复用知识。

核心飞轮：

```text
Recall   检索、排序、装包、生成 trace
   ↓
Agent    执行任务
   ↓
Record   记录结果、知识使用、反馈和任务材料
   ↓
Evolve   从可蒸馏日志中提炼候选知识
   ↓
Curate   聚合、晋升、衰减、归档、去重和治理
   ↓
下一次 Recall
```

系统目标不是保存完整对话，而是形成可审计、可调整、可淘汰的程序性知识。

## 2. 设计目标与边界

### 2.1 目标

- 任务开始前返回与当前问题相关的程序性知识。
- 任务结束后记录知识是否被使用、任务是否成功、用户或评审反馈。
- 让高价值知识通过成功使用逐步晋升；让低价值知识通过失败、未使用、负反馈和时间衰减退出。
- 从任务材料中提炼通用原则，而不是复制原始输出。
- 所有关键决策保留可查询的事实和状态。
- 单机、本地优先，核心能力不依赖常驻服务。

### 2.2 非目标

- 不作为聊天记录或文档全文存储系统。
- 不提供分布式多写者一致性协议。
- 不承诺语义级上下文聚类；当前上下文统计基于规范化查询哈希。
- 不自动证明蒸馏内容正确；候选知识仍需后续使用和治理。

## 3. 当前架构

### 3.1 访问层与依赖方向

```text
MCP    (innate mcp)          ← JSON-RPC 2.0 over stdio；14 工具；直接进程内调用 Core
CLI    (innate <cmd>)        ← clap 薄包装；直接进程内调用 Core
SDKs   (Python / TypeScript) ← 子进程包装 CLI 二进制
Daemon (innate daemon start) ← 后台日志/Hook 监听器；调用 CLI 子进程
                                       │
                                       ▼
                           KnowledgeBase（lib）── 8 大 Public API
                           SQLite + 纯 Rust 向量检索
```

- **MCP 与 CLI 进程内直接调用 `KnowledgeBase`**。
- **SDK 与 Daemon 从不直接打开数据库**，一律 shell out 到 `innate` 二进制——保证业务规则与治理逻辑单一来源于 Rust Core。

### 3.2 模块化 crate 布局（`core/src/`）

重构后源码按职责拆分为目录模块（旧版单文件 `kb.rs`/`storage.rs`/`daemon.rs` 已不存在）：

| 路径 | 职责 |
|---|---|
| `lib.rs` | crate 根；`open_kb()` 按 `settings.json` 注入远程模型，否则降级 Dummy/Heuristic |
| `main.rs` | 二进制入口，转发 `cli::run` |
| `kb/mod.rs` | `KnowledgeBase` 结构、参数加载、`open_with` 注入点、依赖环检测 |
| `kb/recall.rs` | `recall`：向量候选、融合评分、装包、依赖扩展、trace 写入 |
| `kb/record/mod.rs` | `record` / `record_detailed`：归因、反馈、演化触发 |
| `kb/record/evidence.rs` | confidence EMA 重放、上下文统计、治理证据写入 |
| `kb/evolve.rs` | `evolve`：领取请求、lease、蒸馏事务编排 |
| `kb/curate.rs` | 聚合、晋升、衰减、归档、去重、治理（builtin curate）|
| `kb/lifecycle.rs` | `add` / `spark` 全家桶 / `approve` / `archive` / `invalidate` / `restore` |
| `kb/inspection.rs` | `inspect`：闭环健康指标聚合 |
| `storage/mod.rs` | rusqlite 后端：连接、事务、schema 初始化、向量缓存 |
| `storage/chunks.rs` | chunks / deps / 向量 / 成功事实表 CRUD |
| `storage/traces.rs` | usage_trace、episodic_log、feedback_events、confidence_evidence |
| `storage/evolution.rs` | evolve_requests 领取/租约、governance_proposals、蒸馏成本账本 |
| `storage/meta.rs` | meta 表读写（配置参数）|
| `storage/raw.rs` | 通用 SQL 帮助函数、`row_to_json` |
| `embedding.rs` | `EmbeddingProvider` trait + `DummyEmbeddingProvider`（hash 派生，测试用）|
| `llm.rs` | `HttpDistiller`（OpenAI 兼容 + Anthropic 单类型）、`LlmEmbeddingProvider`、HTTP 重试传输、蒸馏 Prompt |
| `refine.rs` | `Sanitizer` / `Refiner` / `Distiller` trait 与默认实现 |
| `errors.rs` | `InnateError` 枚举 |
| `mcp.rs` | MCP stdio server——14 工具，JSON-RPC 2.0 分发 |
| `cli.rs` | CLI 命令（clap），薄包装 |
| `daemon/` | 后台守护：`watch` 监听循环、`events` 事件解析、`process` 进程管理、`state` 状态库、`command` 子命令 |
| `install/` | 安装向导：`wizard` 主流程、`agents` 各 agent 配置、`skills` Skill/斜杠命令、`settings` LLM/Daemon 交互、`path` PATH 安装、`ui` TUI、`uninstall` |
| `backup/` | Cloudflare R2 备份/恢复/列举/清理（S3 兼容 + SigV4）|
| `upgrade.rs` | `innate upgrade`：GitHub Releases 自更新 |
| `migrate.rs` | Schema 迁移链 4.0 → 4.14，逐步原子执行 |
| `hook.rs` | `innate hook stop`：Claude Code Stop 钩子负载 → session.log 事件 |
| `paths.rs` | `~/.innate` 目录布局唯一真相源；`ensure_layout()` 迁移旧扁平布局 |
| `utils.rs` | `utc_now_iso()`、`gen_uuid()`、`content_hash()`、`sanitize()`、余弦相似度 |
| `settings.rs` | `settings.json` 解析（LLM / Embedding / Daemon / Backup）|
| `schema.sql` | 内嵌 schema v4.14，编译期 `include_str!` |

### 3.3 存储与向量检索

- 数据库为本地 SQLite（默认 `~/.innate/data/personal.db`，`--db` 可覆盖）。
- 向量以 `f32` BLOB 存入 `vec_content` 和 `vec_trigger` 两张表。
- 无 `sqlite-vec` 运行时依赖、无独立向量库；`storage` 将全部 embedding 载入内存，Rust 内计算余弦相似度，并使用进程内增量缓存。
- 规模定位：以约 **10 万 chunk** 为上限设计，拒绝 HNSW；超出则替换 `EmbeddingProvider` + `Storage` 实现。

### 3.4 模型与服务配置（`~/.innate/settings.json`）

| 段 | 字段 | 说明 |
|---|---|---|
| `llm` | `provider`（`anthropic` / 其他=OpenAI 兼容）、`base_url`、`model_id`、`api_key` | 蒸馏模型；`api_key` 支持 env 解析 |
| `embedding` | `provider`、`base_url`、`model_id`、`api_key`、`dim`（默认 `1536`）| OpenAI 兼容 `/v1/embeddings` |
| `daemon` | `watch_dirs[]`、`auto_start` | Linux 目录监听与自动启动 |
| `backup` | `enable`、`auto_backup_interval_hours`、`retention_days`、`min_backups`、`r2{...}` | Cloudflare R2 备份策略 |

通过 `open_kb()` 统一入口打开知识库时按设置注入远程模型；直接调用 `KnowledgeBase::open` 时使用 Dummy Embedding + Heuristic Distiller（离线/测试）。

### 3.5 文件系统布局（`~/.innate/`）

```text
~/.innate/
  settings.json            ← 用户配置（LLM / Embedding / Daemon / Backup）
  settings.schema.jsonc    ← settings.json 的 JSON Schema
  data/                    ← 数据库与运行时状态
    personal.db (+ -shm, -wal)   知识库主库
    daemon_state.sqlite          Daemon 私有状态（偏移量/inode/去重/错误）
    daemon.pid                   Daemon 进程号
    backup_state.json            上次备份时间缓存
    tmp/                         备份 VACUUM 临时副本等
  logs/                    ← daemon.log、mcp.log
  sessions/                ← session.log（Daemon 监听目录）
```

- 路径由 `paths.rs` 统一定义，唯一真相源；其他模块不得自行拼接 `~/.innate`。
- `paths::ensure_layout()` 在 CLI 与 MCP 启动时执行：建子目录、把旧扁平布局文件迁移到对应子目录。迁移幂等、best-effort、仅当目标不存在时搬，数据库随 `-shm`/`-wal` 一并迁移。
- `innate vacuum` 在 Curate 压缩与 trace 清理后回收磁盘（checkpoint WAL + VACUUM）。

## 4. 核心数据模型

### 4.1 知识块 `chunks`

| origin | 含义 | 初始状态 | 默认置信度 |
|---|---|---:|---:|
| `installed` | 安装的 Skill 知识 | `active` | `0.85` |
| `captured` | 手工或 Agent 捕获 | `active` 或 `pending` | `0.60` |
| `distilled` | 从任务日志蒸馏 | `pending` | `0.55`（脱敏时 `0.40`）|
| `spark` | 未成熟想法 | 独立成熟度状态 | 单独管理 |

通用状态：

```text
pending ──满足晋升条件──> active
   │                       │
   └────治理/低价值────────┴──> archived
```

Recall 排除 `archived` 和 `spark`。`pending` 允许进入候选集以完成冷启动，但融合分乘以 `0.60`，避免未验证知识与 active 等权竞争。

### 4.2 任务日志 `episodic_log`

每个 trace 对应一条日志，保存查询/输出/摘要/提名、任务状态与结果、使用的知识 ID 与归因、Recall 快照与上下文键、蒸馏状态/锁/成本。

任务状态：`recalled → running → completed`（或 `abandoned` / `timed_out`）。

蒸馏状态：

```text
open → new → screening → distilled
                  └────→ failed
open ──无有效材料/过期──> discarded
```

### 4.3 反馈与治理事实表

- `usage_trace`：retrieved、selected、refined、used、task_ok、task_fail。
- `feedback_events`：用户或 judge 的 up/down 事实（`(trace_id, chunk_id, signal)` 唯一索引，`INSERT OR IGNORE`）。
- `confidence_evidence`：按 trace 唯一保存、可重放的派生置信证据。
- `chunk_context_stats` / `_base`：知识在特定上下文的成功/失败/反馈计数（base 为 restore 观察窗口基线）。
- `governance_proposals`：由持续负反馈形成的治理提案。
- `evolve_requests`：等待 Evolve 消费的持久请求（含 lease、重试、原因）。
- `chunk_success_traces`：Curate 聚合后的成功使用事实。
- `distill_token_usage`：每次蒸馏尝试的 token 成本账本。
- `invalidated_hashes`：失效内容哈希黑名单。
- `deps`：知识依赖边（soft / hard）。

## 5. Recall：知识进入任务

### 5.1 检索流程

```text
query
  → 内容 embedding + trigger embedding
  → 两路向量候选合并
  → soft dependency 加分
  → 融合评分与 anti-trigger 惩罚
  → Top-K
  → 预算装包与 hard dependency 扩展（direct / closure）
  → density refill
  → 可选 trim / adapt
  → 写入 trace 与 open episodic log
```

### 5.2 融合评分

```text
score = 0.55*content_sim + 0.25*trigger_sim + 0.10*confidence + 0.15*context_score
```

- 权重和为 `1.05`，当前实现不再归一化（保持历史标定）。
- 命中 `anti_trigger_desc` 时融合分乘以 `0.6`。
- 候选数默认最多 `20`。Soft dependency 对候选内容相似度 +`0.05`；hard dependency 在装包阶段按 direct 或 closure 展开。
- `pending` 候选最终融合分乘以 `0.60`。

### 5.3 上下文分数

上下文键：`lowercase → 标点转空格 → 分词去停用词（a/an/and/for/in/of/on/the/to/with）→ 排序去重 → 空格连接 → SHA-256`。

```text
wins   = success_count + 2*positive_count
losses = failure_count + 2*negative_count
posterior       = (wins + 1) / (wins + losses + 2)
evidence_weight = min((wins + losses) / 5, 1)
context_score   = (posterior - 0.5) * 2 * evidence_weight
```

合并大小写、标点、停用词、词序差异，但不合并同义改写。

### 5.4 Trace

启用 trace 后原子写入：每候选 `retrieved`、最终返回的 `selected`、被裁剪的 `refined`、Spark retrieval，以及一条 `distill_state=open` 的 episodic log。Trace 是后续归因、反馈和蒸馏的关联主键。

## 6. Record：反馈闭环入口

> 实现注：`record()` 是 `record_detailed()` 的默认参数封装；后者承载完整 17 项参数（治理/归因/反馈精细字段）。整个方法体在单个 `BEGIN IMMEDIATE` 事务内完成，内部置信度与计数更新不单独提交。

### 6.1 输入

- `outcome`：`ok` / `fail` / `unknown`。
- `used` + `used_attribution`（`explicit` / `cited` / `inferred`）+ `used_complete`（默认 `true`）。
- `feedback_up` / `feedback_down` + `feedback_kind`（`user` / `judge`）+ actor / reason。
- `output` / `output_summary` / `nomination` / `task_state` / `priority` / `source`。

`unknown` 为可解析暂态，trace 保持 running/open，允许后续更新；已有最终结果后写入不同最终结果返回 `OutcomeConflict`。

### 6.2 使用归因

| 归因 | confidence signal strength |
|---|---:|
| `explicit` | `0.30` |
| `cited` | `0.25` |
| `inferred` | `0.15` |

- 成功任务被使用知识获正向隐式信号；失败任务保留使用事实，置信更新强度减半。
- 归因 ID 必须来自该 trace 实际 `selected` 的知识；retrieved-only 候选不可声明 used 或反馈。输入 ID 先去重。
- `used_complete=false`：增量合并，未出现知识不受罚。`used_complete=true`：完整快照，整体替换；未出现的 selected 知识收到 `target=0.0 / strength=0.08 / reason=selected_unused`（始终向下调整）。

### 6.3 显式反馈

| 来源 | strength |
|---|---:|
| 用户 | `1.0` |
| Judge | `0.6` |

- 先写 `feedback_events`，再写可重放 `confidence_evidence`（全局 confidence + 上下文统计 + 负反馈治理证据）。
- 置信度采用带强度 EMA，基础 alpha `0.2`；派生信号按 trace 唯一保存并从 `confidence_base` 重放——重复、乱序、used 修正不重复施加旧信号。
- 显式反馈按距上次使用时间获最高 `1.5` 新近度系数。
- 同次 Record 不允许同一知识同时 up/down；同 trace 后续反向信号视为修正（删旧重放新，不双累）。

### 6.4 日志是否进入蒸馏

任务完成且具备 `output_summary` / `nomination` / `output` 任一时进入 `new`；否则 `discarded`（`insufficient_material`），避免无依据内容进 LLM。支持乱序补写：被 `insufficient_material` / `abandoned` / `timed_out` 丢弃的日志补入材料后重回 `new`；过滤/失效/已蒸馏终态不会误重开。

### 6.5 Record 后的演化触发

事务提交后判断是否写入 `evolve_requests`：

- 可蒸馏日志数达 `5` → `threshold`。
- 已有日志且最老超过调度间隔 → `scheduled`。
- pending 治理提案数达 `3` → `governance`。
- 任一治理提案证据达归档阈值 `3` → `governance_ready`（单知识证据充分即入队，不再等多提案）。

Record 只在事务内写事实并提交请求，**不在调用链内同步访问 LLM**；请求由 daemon/CLI/MCP 的 Evolve 消费，避免反馈写入被网络延迟阻塞。

## 7. Evolve：从任务材料生成候选知识

### 7.1 触发模式

- `manual`：显式执行，不要求已有请求，保留运维 token 覆盖能力。
- `scheduled`：主动恢复到期失败日志、唤醒超调度间隔的 `new` 日志；无可领取请求时仍执行 Curate。
- `threshold`：按可蒸馏日志数触发。

Evolve 领取一条请求并设 lease；过期 running 请求可恢复。低于阈值或达自动蒸馏 token 上限时，蒸馏类请求保持 `pending` 并设 `next_retry_at`（不误标完成）；治理类请求在 Curate 完成后直接结束。

### 7.2 蒸馏事务

```text
claim new logs → screening
  → 调用 Distiller（HttpDistiller / Heuristic）
  → sanitize
  → 生成双向量（content + trigger）
  → 原子写入 chunk、vectors、日志终态
  → builtin Curate
```

- Distiller 按日志隔离执行，每条日志产出零到多个候选。每次调用接收同批、同 `context_key` 最多 4 条相关交互作为重复模式/冲突上下文，输出仍归属当前主日志。
- 单日志调用或解析失败只标记该日志 `failed`，同批其他继续。
- 失败日志 5 分钟冷却后由 scheduled 自动恢复并写 `distill_retry` 请求，单日志最多 3 次；`distill_attempts` / `distill_last_failed_at` 持久化，达上限不再循环，成功重试也不抹历史失败。每次尝试写 `distill_token_usage`，预算与 Inspect 用累计账本。
- 模型调用：30 秒超时、800 token 输出上限。HTTP 传输对网络错误、429、5xx 做指数退避重试（最多 3 次，尊重 `Retry-After`，封顶 30 秒）。
- `nomination` 是输入与来源标记，不默认视为可信人工内容；配 LLM 时仍经泛化与格式校验，启发式降级产物保持 `pending` 并受 Recall 降权。

> 实现注：`HttpDistiller` 单一类型按 `config.provider` 在 `call()` 内分派到 OpenAI 兼容（`/chat/completions`）或 Anthropic（`/v1/messages`），共享蒸馏循环、provenance 与重试传输（v0.1.9 合并自原 `OpenAiDistiller` / `AnthropicDistiller`，行为不变）。

## 8. Curate：让知识变好或退出

### 8.1 聚合与清理（固定原子顺序）

Curate 在单个 `BEGIN IMMEDIATE` 内，以一次性固定的 `cutoff_ts` 执行：

1. `aggregate_success_traces` → 写入 `chunk_success_traces` 事实表。
2. `aggregate_success_counts` → 派生 `used_success_count` / `last_success_at`。
3. `aggregate_counters` → 从 `usage_trace` 派生 `selected_count` / `used_count`。
4. 写 `meta.last_agg_ts = cutoff_ts` → `purge_usage_trace(ts < cutoff_ts)`。

紧凑归因/修正事实不删除、不在 Evolve 成功后折入新基线；仅清理可重建的 retrieved/refined 明细。Spark retrieval 保留用于重复出现统计。

同时处理：超时 `screening` 日志恢复 `failed`；超 `14` 天 open 日志进入 `discarded`/`timed_out`；超保留期终态日志清除大字段但保留紧凑 trace 身份与归因。

Restore 写 `evidence_cutoff_at`，将 confidence、计数、上下文统计重置到新人工观察窗口；历史事实保留审计，但 Curate/上下文/治理只重放 cutoff 之后的事实。

### 8.2 晋升

```text
used_success_count >= 3  且  distinct successful traces >= 2  且  confidence >= 0.60
```

蒸馏知识初始 `0.55` + 晋升线 `0.60`，故可仅靠后续成功使用越线：`distilled pending → Recall 命中 → Record used+ok → confidence 上升 → Curate promote → active`。

### 8.3 衰减与归档

非 protected active/pending 按 90 天半衰期向 floor `0.20` 衰减：

```text
new_confidence = floor + (confidence - floor) * 0.5 ^ (idle_days / 90)
```

floor `0.20` 低于归档线 `0.25`，使闲置知识可仅靠衰减进入低置信归档。

| 路径 | 默认条件 | state_reason |
|---|---|---|
| 低置信闲置 | confidence `< 0.25` 且闲置 `60` 天 | `low_confidence` |
| 重复入选未用 | selected ≥ `10` 且 confidence `< 0.5` | `repeated_selected_unused` |
| 长期未使用 | 创建后 `30` 天仍未使用 | `never_used` |
| 治理提案 | pending proposal evidence ≥ `3` | `governance_proposal` |
| 持续任务失败 | 使用 ≥ `5` 次、成功率 `< 20%`、confidence `< 0.35` | `sustained_task_failure` |
| 内容重复 | 相同 content hash，保留 protected 或更高置信项 | `duplicate` |

Protected 与 Spark 不走普通自动归档。Spark 走独立 `maturity`（`seed → sprouting → incubating`），Curate 的 archive/decay/confidence 逻辑全程过滤 `origin='spark'`。

### 8.4 负反馈治理

治理证据按反馈 strength 加权、90 天半衰期衰减；单 actor 净贡献限 `[-1,1]`；无 actor 时按 `anonymous:<source>` 聚合。Record 在新反馈到达时同步刷新对应 chunk，Curate 只重算仍 pending 的提案：

```text
净负分 < 2  或  有效 actor < 2 ：不创建 proposal
净负分 >= 2 且 有效 actor >= 2 ：创建 review_applicability proposal
净负分 >= 3 且 有效 actor >= 2 ：写入 governance_ready 请求
Curate ：归档知识并接受 proposal
```

直接 `5` 次负反馈归档为第二条保险路径（默认阈值下治理提案路径通常先发生）。

## 9. 知识生命周期 API

`kb/lifecycle.rs` 提供（对应 CLI/MCP 治理动作）：

- `add`：写入 captured 知识。`tvec = embed_trigger(trigger_desc or content)` 始终生成，绝不回退到截断 `cvec`。
- `spark` / `mature_spark` / `promote_spark` / `drop_spark`：想法的快速捕获与成熟晋升，Curate 豁免。
- `approve`：pending → active（标 `confidence_reason='manual_set'`）。
- `archive`：手动归档。
- `invalidate`：归档并把 content hash 加入 `invalidated_hashes` 黑名单。
- `restore`：恢复 active 并以 `evidence_cutoff_at` 开启新观察窗口、清零派生状态、清除失效黑名单条目。

## 10. 安装与集成（`innate install`）

`install/` 实现零额外依赖的 clack 风格 TUI，配置 Claude Code / Codex CLI / opencode 使用 Innate MCP server：

1. **Scope**：全局（所有项目）或仅当前项目。
2. **Agent 选择**：自动探测已安装 agent，多选。
3. **PATH 安装**：已在 PATH 则写 `innate`（可移植）；否则安装到 `~/.local/bin` 并写 shell profile 的 PATH 导出（Windows/Unix 分支）。
4. **Auto-allow**：可选自动放行 13 个知识类 MCP 工具，跳过权限提示。
5. **LLM 配置**：交互式写入 `settings.json`。
6. **Daemon watch dirs**：交互式配置监听目录。
7. **写配置**：按 agent 类型写 MCP server 配置（JSON / TOML），并对 Claude 额外安装 SKILL.md、斜杠命令、**Stop hook**（让 daemon 自动获得会话事件）。

配置写入加固（`a6d6fd6`）：`read_json_object` 对不可读/不可解析/非对象根返回 `Err`，绝不静默覆盖已有用户配置；JSONC 注释剥离与 TOML section 替换保留用户其余内容。

`innate uninstall` 反向移除上述配置。

## 11. Daemon（Linux）

后台日志/Hook 监听器，**不直接打开知识库**，全部经 CLI 子进程；私有状态在 `daemon_state.sqlite`（`watch_state` 偏移/inode、`processed_events` 去重、`trace_context` 会话 trace、`daemon_errors` 错误统计）。

- `daemon start --watch <dir>` / `status` / `stop`：`process.rs` 基于 fork + `/proc` 管理；非 Linux 返回明确错误。
- **监听循环**（`watch.rs`）：tail 续读（按 inode + offset 恢复），逐行解析。
- **事件解析**（`events.rs`）：优先解析 JSON 事件 `session_start` / `tool_success` / `tool_error` / `session_end` / `user_feedback` → `start` / `ok` / `fail` / `end` / `feedback`；JSON 失败时回退文本模式分类（Build successful / Error: / Session ended 等模式）。
- 事件按 `event_id` 幂等去重，会话内维护 `trace_id` 上下文，映射为 recall/record CLI 调用。

## 12. 备份（Cloudflare R2）

`backup/` 通过 S3 兼容 API + AWS SigV4 直连 R2（`https://{account_id}.r2.cloudflarestorage.com`，签名 region `auto`），零 AWS SDK 依赖：

- `innate backup run`：checkpoint 后上传数据库快照，并按策略 prune。
- `innate backup status` / `list` / `prune`：查看上次备份、列举远端快照、按 `retention_days` 清理但保底 `min_backups`（`protected_by_min` 报告被保护数）。
- `backup_state.json` 缓存上次备份时间/键。

## 13. 自更新与迁移

- `innate upgrade [--version] [--check]`（`upgrade.rs`）：从 GitHub Releases（`vima-tech/Innate`）下载当前平台预编译二进制，校验 SHA-256，原子替换运行中可执行文件，随后可选执行 `migrate`。不支持平台提示源码编译。
- `innate migrate`（`migrate.rs`）：内嵌迁移 SQL，链式 4.0 → 4.14。每步 `BEGIN IMMEDIATE … COMMIT` 原子执行，任一失败整步回滚，无半迁移状态；已在目标版本则幂等。

## 14. Hook

`innate hook stop`（`hook.rs`）：从 stdin 读 Claude Code Stop 钩子负载，逆序提取最后一条 user query（截 200 字）与最后一条 assistant 摘要（截 400 字），追加 `session_start` / `tool_success` / `session_end` 事件到 `sessions/session.log`，由 Daemon 消费。

## 15. 可观测性（`innate inspect`）

提供：chunk / log / rebuild queue / 债务比例；stale screening、蒸馏成本、recurring sparks；trace completion rate；usage annotation rate（仅以 completed trace 为分子）；trace use rate 与 selected-to-used rate；task success rate；feedback coverage 与 event 数；timed-out traces；pending evolve requests 与 governance proposals；confidence 分布；以及当前 `recall.*` / `curate.*` 配置参数。

闭环健康至少同时观察：Record 完成率、used 标注率、selected→used 转化率、显式反馈覆盖率、pending→active 晋升率、active→archived 淘汰率、evolve request 等待时间。

## 16. 并发与失败处理

- 关键写操作 `BEGIN IMMEDIATE`。
- Recall trace 与 open log 原子写入。
- Record 的结果/使用/反馈/上下文/治理更新同一事务完成。
- Evolve 先 claim 日志再模型调用与最终写入；蒸馏写 chunk + 双向量 + 日志终态保持原子；request 用 running lease 可恢复。
- Outcome 冲突显式报错，不做最后写入覆盖。
- Sanitizer 在知识持久化前执行。
- Migration 逐步原子。

## 17. 接口边界

### 17.1 CLI 命令

`recall` / `record` / `add` / `spark` / `evolve` / `inspect` / `approve` / `archive` / `invalidate` / `restore` / `mature-spark` / `promote-spark` / `drop-spark` / `backup` / `uninstall` / `upgrade` / `daemon` / `hook`（+ `migrate` / `vacuum`）。

### 17.2 MCP 工具（14）

| 工具 | Rust 方法 |
|---|---|
| `innate_recall` | `KnowledgeBase::recall` |
| `innate_record` | `KnowledgeBase::record` |
| `innate_add` | `KnowledgeBase::add` |
| `innate_spark` | `KnowledgeBase::spark` |
| `innate_evolve` | `KnowledgeBase::evolve` |
| `innate_inspect` | `KnowledgeBase::inspect` |
| `innate_approve` / `archive` / `invalidate` / `restore` | 治理 API |
| `innate_mature_spark` / `promote_spark` / `drop_spark` | spark 生命周期 |
| `innate_backup` | R2 备份 |

MCP 直接调用 Rust Core。配置 daemon `auto_start` 且存在 watch dirs 时，MCP 会尝试启动 daemon，失败不阻断主流程。

> 注：install 向导默认仅 auto-allow 13 个知识类工具，`innate_backup` 作为运维工具不在默认放行集。

### 17.3 SDK

- Python SDK 通过 `innate` CLI 子进程访问核心；`augmented` 装饰器可自动执行 recall / running record / 成功或失败 record。
- TypeScript SDK 同时提供 CLI 子进程与异步 MCP client 入口。
- SDK 不拥有独立业务规则；状态与治理规则以 Rust Core 为准。

## 18. 非显然实现约束（编码红线）

- **时间**：`utils::utc_now_iso()` 是唯一时间源，格式 `YYYY-MM-DDTHH:MM:SS.mmmZ`（固定 3 位毫秒）。所有 SQL 截止比较依赖该格式字典序。
- **`record()` distill_state 跃迁**：`open→new/discarded` 判定仅当 `distill_state=='open'`；二次调用不得把 `new`/`screening` 下调。
- **`record()` fresh-insert**：无既有 episodic_log 行时 `is_fresh_insert=true` 触发 `apply_outcome_implicit`，即便插入后 `existing_outcome==outcome`。
- **`record()` 事务**：整个方法体在单个 `BEGIN IMMEDIATE`，内部置信度/last_used 更新不自提交。
- **Curate aggregate 顺序固定**（§8.1），`cutoff_ts` 一次定值全程共享。
- **`add()` trigger 向量** 始终 `embed_trigger(trigger_desc or content)`，绝不回退截断 `cvec`。
- **Spark 豁免 Curate**：archive/decay/confidence 逻辑必须过滤 `origin='spark'`。
- **paths.rs 唯一拼接** `~/.innate`；其他模块新增需求在此加 helper。
- **Skill 双份存储**：`skills/innate-memory/SKILL.md`（公开源）与 `core/assets/SKILL.md`（二进制内嵌）必须逐字节一致，改动同步，提交前 `cmp` 校验。

## 19. 扩展点

`KnowledgeBase::open_with(...)` 可注入：

- `embedding: Arc<dyn EmbeddingProvider>` — 换 embedding 模型。
- `refiner: Arc<dyn Refiner>` — 在线裁剪/适配。
- `distiller: Arc<dyn Distiller>` — episodic log → chunk 提炼。
- `sanitizer: Arc<dyn Sanitizer>` — 写入前拒绝/脱敏。

所有调参旋钮在 `meta` 表（`recall.*` / `curate.*`），`open` 时一次性加载，`inspect` 打印当前值。

## 20. Schema 演进摘要（4.8 → 4.14）

> 完整逐版变化见 `Innate-设计文档-v0.1.8.md` §13；此处保留要点。

- **4.8 反馈闭环**：蒸馏初始 confidence `0.45→0.55`；晋升线 `0.65→0.60`；decay floor `0.30→0.20`；selected-unused target `0.30→0.00`；治理增加单 proposal ready；context key 规范化。
- **4.9 治理一致性**：`feedback_events` 唯一索引 + `INSERT OR IGNORE`；治理证据改净负反馈；仅实际归档的 proposal 进 `accepted`。
- **4.12 反馈飞轮**：trace 只能反馈其 Recall 快照内知识；outcome/used 乱序到达并按 trace 重放；`unknown` 可解析、ok/fail 可纠正；归因收紧到 selected；治理证据按 strength/actor/半衰期/单 actor 上限。
- **4.13 闭环可靠性**：移除成功 Evolve 后证据折叠；Record 仅提交事实与请求；`completed` 即进蒸馏判断；部分 used 保留各自 attribution；Restore 形成新治理分界。
- **4.14 飞轮可达性**：scheduled 自动恢复 failed 日志、为 aged new 建持久请求；低阈值/预算门改为延期请求；每次蒸馏写 token 账本；补料重开 `insufficient_material`；Restore 用 `evidence_cutoff_at`；匿名反馈不计入多人共识；related-log 限同 `context_key`。

## 21. 剩余边界与后续优先级

剩余边界：

1. Pending 仍允许 Recall（降权 `0.60`）以冷启动；长期未用/持续失败/低置信 pending 由 Curate 归档。
2. 上下文键已做词法归一，但同义表达仍需语义聚类。
3. 未提交 `used` 时不猜测 selected-unused；需 `used_complete` 明确。
4. 任务 outcome 默认在多 used chunk 间均分证据；更精确因果需 chunk 级显式反馈。
5. 治理共识只统计非空 actor；actor 身份由集成方提供，Core 暂不做外部认证。
6. 紧凑修正事实长期保留，数据库体积需有界修正窗口与可校验 checkpoint。

后续优先级（按闭环风险）：

1. 上下文统计从词法指纹升级为可控语义聚类。
2. 增加 chunk 级任务贡献权重，提高多知识任务因果归因精度。
3. 带 checkpoint 的长期事实压缩，在可重放/修正窗口与体积间平衡。
4. 治理 actor 绑定宿主认证身份，防伪造多 actor。

---

**文档结论：** 当前实现使用数据库 Schema 4.14、软件版本 0.1.9。源码已完成模块化重构，反馈事实、置信证据、上下文统计与使用计数形成可审计、可重放、支持乱序与修正的闭环；自动 Evolve 对失败/预算延期/低流量日志保持可达；Restore 从新证据窗口重新成长。安装、Daemon、备份、自更新、迁移与 Hook 构成完整的本地集成与运维面。剩余重点为语义上下文聚类、长期事实 checkpoint、可信 actor 身份与更细粒度因果贡献建模。
