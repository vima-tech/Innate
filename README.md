# Innate — 自成长 Agent 程序性知识层

> **一句话定位**: 一个**可嵌入可外挂、自成长、引擎可换**的 agent 程序性知识层系统。  
> 它不做编排(对 LangGraph / Claude Code / 裸 API 中立), 只解一件事——**在有限 context 预算内, 组装最相关、最精确的知识, 并让这套知识随使用自我进化。**

Innate 管理的不是「世界是什么样 / 用户偏好是什么」那种**陈述性记忆**, 也不是一次写定再也不变的**静态技能仓库**。它管理的是**程序性知识(Procedural Knowledge)**——「事情该怎么做, 哪种方式在这个上下文里更有效」。每一块知识从进库起都在被真实使用结果考核: 好用的升权, 失效的降权归档, 灵感可孵化晋升, 整库越用越准。

## 核心闭环

五个核心闭环必须完整, 缺一就不算"知识层":

- **召回**: `query → recall → context`(双向量余弦相似度 + 标量过滤, 同步纯数学)
- **观测**: `context → use → trace`(记录哪些块被用了、结果如何)
- **成长**: `trace → distill → pending`(离线蒸馏新经验)
- **治理**: `usage → confidence → curate`(EMA 置信度更新 + Curate 归档)
- **安全**: `pending/archived/不物理删`(默认 sanitize 钩子, 黑名单 + red action)

> 默认同步召回路径**绝不调用任何 LLM/小模型**——Innate 是图书管理员, 不是阅读者兼编辑。只有调用方显式开启可选 Refiner 时才执行 trim/adapt。

---

## 安装

### Rust 核心 (唯一运行时)

```bash
# 从源码编译
cd innate-rs && cargo build --release
cp target/release/innate ~/.local/bin/   # 加入 PATH

# 验证
innate inspect
```

### Python SDK

```bash
pip install innate-py   # 或 pip install -e sdks/python/
```

### TypeScript SDK

```bash
npm install @innate/sdk  # 或 npm install ./sdks/typescript/
```

### MCP 服务 (Claude Code / Claude Desktop)

在 `.claude/settings.json` 中添加:

```json
{
  "mcpServers": {
    "innate": {
      "command": "innate",
      "args": ["mcp"]
    }
  }
}
```

配置后 Agent 可直接使用 `innate_recall`、`innate_record` 等 MCP 工具, 无需手动调用 CLI。

---

## 接入 Agent (MCP 配置)

**推荐方式: MCP 服务** — Agent 通过 MCP 工具直接操作知识库, 无需 CLI 权限。

### 步骤

1. 编译并安装 Rust 二进制:

```bash
cd innate-rs && cargo build --release
cp target/release/innate ~/.local/bin/
```

2. 在 `.claude/settings.json` 添加 MCP 配置:

```json
{
  "mcpServers": {
    "innate": {
      "command": "innate",
      "args": ["mcp"],
      "env": {
        "INNATE_DB": "/path/to/your/personal.db"
      }
    }
  }
}
```

3. 验证初始化:

```bash
innate inspect   # 应输出空库健康报告
```

### Agent 工作规范

配置完成后, 将以下工作规范发送给 Agent:

> **知识层使用规范**
>
> | 操作类别 | MCP 工具 |
> |---|---|
> | Agent 可直接执行 | `innate_recall` · `innate_record` · `innate_evolve` · `innate_inspect` |
> | 确认后执行 | `innate_add` · `innate_spark` |
> | 仅人工治理 | `innate_approve` · `innate_archive` · `innate_invalidate` · `innate_restore` · `innate_mature_spark` · `innate_promote_spark` · `innate_drop_spark` |
>
> **工作流程**
> - 每次任务开始前调用 `innate_recall(query="<任务意图>")`, 将结果纳入计划
> - 任务结束后调用 `innate_record(trace_id=..., outcome="ok"|"fail")`, 闭合 trace
> - 发现值得保留的经验或灵感时, 先提炼并向用户确认; 得到同意后调用 `innate_add` 或 `innate_spark`
> - 判断知识已失效时, 只提出治理建议, 不直接执行治理动作
> - 会话结束时调用 `innate_evolve(trigger="manual")` 触发蒸馏

---

## 快速开始

```bash
# 1. 写入知识
innate add "Python 列表推导式比 map/filter 更易读" --kind note --trigger "python 列表处理"

# 2. 召回知识 (返回 top 块, 含 trace_id)
innate recall "python 列表优化" --budget 2000 --format json

# 3. 补全 trace (闭合经验链路)
innate record <trace_id> --outcome ok --used <chunk_id1>,<chunk_id2>

# 4. 触发成长 (蒸馏 + 治理)
innate evolve --trigger manual

# 5. 体检 (健康信号 + 建议命令)
innate inspect
```

---

## Python SDK

```python
from innate import KnowledgeBase

kb = KnowledgeBase("personal.db")  # 或通过 INNATE_DB 环境变量指定

# 1. 写入
note_id = kb.add("经验内容", kind="note", trigger_desc="触发场景")
spark_id = kb.spark("一个待探索的灵感", trigger_desc="相关场景")

# 2. 召回 (同步纯数学)
ctx = kb.recall(
    "任务描述",
    budget=6000,
    include_sparks=True,
)
for chunk in ctx.knowledge:
    print(chunk["id"], chunk["content"])

# 3. 记录使用
kb.record(
    ctx.trace_id,
    outcome="ok",
    used=[note_id],
    output_summary="解决了 X",
)

# 4. 成长
result = kb.evolve(trigger="manual")

# 5. 治理 (人工操作)
kb.approve(pending_id)
kb.archive(note_id, reason="stale")
kb.invalidate(note_id, reason="逻辑错误")
kb.restore(archived_id)

# 6. 灵感生命周期
kb.mature_spark(spark_id, to="sprouting")
kb.mature_spark(spark_id, to="incubating")
new_id = kb.promote_spark(spark_id, to="note")
kb.drop_spark(spark_id, reason="已证伪")

# 7. 体检
report = kb.inspect()
```

---

## TypeScript SDK

```typescript
import { KnowledgeBase, McpClient } from "@innate/sdk";

// CLI subprocess 模式 (同步, 适合脚本)
const kb = new KnowledgeBase({ dbPath: "personal.db" });

const ctx = kb.recall("任务描述", { budget: 6000 });
kb.record(ctx.trace_id, { outcome: "ok", used: [chunkId] });
kb.add("经验内容", { kind: "note", triggerDesc: "触发场景" });
kb.evolve("manual");
const report = kb.inspect();

// MCP 客户端模式 (异步, 适合 Agent 集成)
const client = new McpClient({ dbPath: "personal.db" });
await client.initialize();

const result = await client.recall("任务描述", { budget: 6000 });
await client.record(result.trace_id, { outcome: "ok" });
await client.add("新知识", { kind: "note" });
await client.inspect();

client.close();
```

---

## CLI 子命令一览

CLI 是 SDK Public API 的**薄封装**, 不新增任何知识层逻辑——只做参数解析和格式化输出。

| 子命令 | 能力域 | 说明 |
|---|---|---|
| `innate recall <query>` | 读 | `--budget` · `--top` · `--include-sparks` · `--format text\|json` |
| `innate record <trace_id>` | 写 | `--outcome ok\|fail\|unknown` · `--used` · `--output-summary` · `--nomination` · `--source` |
| `innate evolve` | 成长 | `--trigger manual\|scheduled\|threshold` |
| `innate inspect` | 调试 | 库体检: 5 个健康信号 + 当前参数 |
| `innate add <content>` | 写入 | `--kind note\|skill` · `--trigger` · `--anti-trigger` · `--skill-name` · `--source` |
| `innate spark <content>` | 灵感 | `--trigger` |
| `innate mature-spark <id> <to>` | 治理 | to: `sprouting\|incubating` (只允许前向) |
| `innate promote-spark <id>` | 治理 | `--to note\|skill` |
| `innate drop-spark <id>` | 治理 | `--reason` |
| `innate approve <id>` | 治理 | pending → active |
| `innate archive <id>` | 治理 | `--reason` |
| `innate invalidate <id>` | 治理 | `--reason` (归档 + 黑名单) |
| `innate restore <id>` | 治理 | archived → active; 若此前 invalidate, 同步撤销 hash 黑名单 |
| `innate mcp` | 集成 | 启动 MCP stdio 服务 (JSON-RPC 2.0), 供 Claude Code / Desktop 使用 |

### `recall` 输出格式

- `text` — 人类可读, 不含 trace 信息
- `json` — 机器可读, 含 `trace_id` / `knowledge` / `sparks` / `empty` 字段

### `inspect` 输出

库体检 (无参) — 5 个健康信号:
- 知识债务比 (含僵尸块, < 0.3 正常)
- embed 重建队列 (待补向量的 chunk 数)
- 灵感提示 (反复浮现的 spark id)
- stale screening (卡死的 distill 日志)
- 本周期蒸馏成本 (token 估算)

末尾打印当前 `recall_params` / `curate_params` 便于调参。

### MCP 工具 (`innate mcp` 暴露的 13 个工具)

| MCP 工具 | 对应能力 |
|---|---|
| `innate_recall` | 召回知识 |
| `innate_record` | 记录 trace |
| `innate_add` | 写入知识块 |
| `innate_spark` | 创建灵感 |
| `innate_evolve` | 触发成长 |
| `innate_inspect` | 库体检 |
| `innate_approve` / `innate_archive` / `innate_invalidate` / `innate_restore` | 治理 |
| `innate_mature_spark` / `innate_promote_spark` / `innate_drop_spark` | 灵感生命周期 |

---

## 系统架构

```
Innate System
├── Rust 核心 (innate-rs/)
│   ├── KnowledgeBase (lib)     8 类 Public API, SQLite + 纯 Rust 余弦相似度
│   ├── CLI (innate <cmd>)      clap 薄封装, 参数解析 → KnowledgeBase 调用
│   └── MCP Server (innate mcp) JSON-RPC 2.0 over stdio, 13 个 MCP 工具
│
├── 向量搜索                    f32 BLOB 存储 + 全扫描余弦相似度 (零 native 扩展)
│   ├── vec_content             内容向量 (BLOB)
│   └── vec_trigger             触发向量 (BLOB)
│
├── SDKs
│   ├── sdks/python/            innate-py  — subprocess wrapper, 零额外依赖
│   └── sdks/typescript/        @innate/sdk — CLI subprocess + async MCP client
│
└── skills/innate-memory/       SKILL.md (MCP 工具为主, CLI 为 fallback)
```

**依赖方向严格向下**: `SDK → CLI → KnowledgeBase`。SDKs 通过 subprocess 调用 CLI 二进制; MCP server 直接调用 KnowledgeBase lib。

---

## 核心特性

- **双向量召回**: `content_vec` + `trigger_vec`, 按 `w_content / w_trigger / w_confidence` 融合排序
- **置信度驱动**: EMA 更新 + 时效加权 (显式信号) + 时间衰减, 知识越用越准
- **零 native 扩展**: 向量存为 f32 BLOB, Rust 原生余弦相似度, 无 sqlite-vec / C 扩展依赖
- **MCP 原生集成**: `innate mcp` 暴露 13 个工具, Claude Code / Claude Desktop 开箱可用
- **hard dep fail-closed**: 召回时若 hard 依赖不可用/被归档, 直接丢弃整个 seed, **绝不返回半截闭包**
- **零主动行为**: SDK 永不自发行动, 所有成长由外部触发 (evolve trigger: manual / scheduled / threshold)
- **sanitize 三态合同**: 钩子返回 `(cleaned, action)`, `action ∈ {allow, redact, discard}`; `discard` 拒绝写入, `redact` 落点 confidence 上限 0.4
- **spark 独立生命周期**: maturity = `seed → sprouting → incubating`, 仅 `promote` / `drop` 时离场; 不参与 confidence 排序, 不被低分归档
- **原子双向量写入**: chunk + content_vec + trigger_vec 同 `BEGIN IMMEDIATE` 事务写入, 任一失败回滚
- **schema 自动迁移**: 启动时自动 apply 增量迁移, 库空直接 exec schema.sql

---

## 兼容性

- 核心运行时: Rust 1.70+ (bundled SQLite, 零外部运行时依赖)
- Python SDK: Python 3.8+ (subprocess, 零额外依赖)
- TypeScript SDK: Node.js 18+ (child_process, 零额外依赖)
- 数据库默认位置: `~/.innate/personal.db` (可通过 `INNATE_DB` 环境变量或 `--db` 覆盖)
- 平台: Linux / macOS / Windows (WSL)

---

## 文档

- [`docs/Innate-设计文档-v4.5.1.md`](docs/Innate-设计文档-v4.5.1.md) — 完整系统设计 (权威基线)
- [`skills/innate-memory/SKILL.md`](skills/innate-memory/SKILL.md) — Agent Skill 元数据与使用规范

---

## 开发

```bash
# 编译
cd innate-rs && cargo build --release

# 运行全量测试
cd innate-rs && cargo test

# 运行单个测试
cd innate-rs && cargo test test_add_and_recall

# 检视默认知识库
innate inspect
```

测试按职责分组: `add_and_recall` · `spark_and_promote` · `record_state_machine` · `invalidate_cascade` · `inspect_returns_counts` · `evolve_smoke` + 3 个 utils 测试。

## License

MIT
