# Innate — 自成长 Agent 程序性知识层

[English](README.md)

> **一句话定位**: 一个**可嵌入可外挂、自成长、引擎可换**的 agent 程序性知识层系统。  
> 它不做编排(对 LangGraph / Claude Code / 裸 API 中立), 只解一件事——**在有限 context 预算内, 组装最相关、最精确的知识, 并让这套知识随使用自我进化。**

Innate 管理的不是「世界是什么样 / 用户偏好是什么」那种**陈述性记忆**, 也不是一次写定再也不变的**静态技能仓库**。它管理的是**程序性知识(Procedural Knowledge)**——「事情该怎么做, 哪种方式在这个上下文里更有效」。每一块知识从进库起都在被真实使用结果考核: 好用的升权, 失效的降权归档, 灵感可孵化晋升, 整库越用越准。

---

## 安装

### 一行安装（Linux / macOS）

```bash
curl -fsSL https://raw.githubusercontent.com/vima-tech/Innate/main/install.sh | sh
```

自动检测平台并下载预编译二进制，验证 sha256 校验和，安装到 `~/.local/bin`，并运行 `innate install` 配置 Agent 集成——一步完成。

可选参数：

```bash
# 固定安装版本
curl -fsSL https://raw.githubusercontent.com/vima-tech/Innate/main/install.sh | INNATE_VERSION=0.1.5 sh

# 仅安装二进制，跳过 Agent 配置
curl -fsSL https://raw.githubusercontent.com/vima-tech/Innate/main/install.sh | NO_INNATE_SETUP=1 sh

# 指定安装目录
curl -fsSL https://raw.githubusercontent.com/vima-tech/Innate/main/install.sh | INNATE_DIR=/usr/local/bin sh
```

### 其他安装方式

```bash
# Rust（推荐）
cargo install innate

# Python 安装器——自动下载预编译二进制，无需 Rust
pip install innate-ai

# npm 安装器——自动下载预编译二进制，无需 Rust
npm install -g @vima_tech/innate

# 从源码编译
cd core && cargo build --release
cp target/release/innate ~/.local/bin/
```

验证安装：

```bash
innate inspect
```

### 配置 Agent 集成

二进制在 PATH 上之后，运行交互式向导配置 MCP 服务并安装 `innate-memory` agent skill（一行安装脚本已自动执行此步骤）：

```bash
innate install
```

自动检测 Claude Code、Codex CLI 和 opencode。

### 让 Agent 帮你安装

将以下提示词粘贴到 Claude Code（或任何拥有 shell 访问权限的 Agent）中：

```
运行以下命令安装 Innate：
curl -fsSL https://raw.githubusercontent.com/vima-tech/Innate/main/install.sh | sh
该脚本会自动下载二进制、验证校验和并配置 MCP 服务和 agent skill。
完成后用 `innate inspect` 验证安装。
```

### Python SDK（程序化调用）

```bash
pip install innate-py
```

### TypeScript SDK（程序化调用）

```bash
npm install @innate/sdk
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

### Daemon (Linux only)

后台守护进程，监听指定目录下的日志文件和 JSON Hook 事件，自动桥接到知识层：

```bash
innate daemon start --watch /path/to/log/dir
innate daemon status
innate daemon stop
```

Daemon 通过 subprocess 调用 `innate` CLI，不直接打开知识库；其自身状态（文件偏移、已处理事件、会话 trace）记录在独立的 `daemon_state.sqlite` 中。

### 卸载

```bash
innate uninstall              # 交互式，保留数据
innate uninstall -y           # 跳过确认，保留数据
innate uninstall -y --purge-data  # 同时删除 ~/.innate/ 数据目录
```

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

## 核心闭环

五个核心闭环必须完整, 缺一就不算"知识层":

| 闭环 | 路径 |
|---|---|
| **召回** | `query → recall → context`（双向量余弦相似度 + 标量过滤，同步纯数学） |
| **观测** | `context → use → trace`（记录哪些块被用了、结果如何） |
| **成长** | `trace → distill → pending`（离线蒸馏新经验） |
| **治理** | `usage → confidence → curate`（EMA 置信度更新 + Curate 归档） |
| **安全** | `pending/archived/不物理删`（默认 sanitize 钩子，黑名单 + red action） |

> 默认同步召回路径**绝不调用任何 LLM/小模型**——Innate 是图书管理员，不是阅读者兼编辑。只有调用方显式开启可选 Refiner 时才执行 trim/adapt。

---

## 接入 Agent (MCP 配置)

推荐方式: MCP 服务 — Agent 通过 MCP 工具直接操作知识库, 无需 CLI 权限。

### Agent 工作规范

配置完成后, 将以下工作规范发送给 Agent:

| 操作类别 | MCP 工具 |
|---|---|
| Agent 可直接执行 | `innate_recall` · `innate_record` · `innate_evolve` · `innate_inspect` |
| 确认后执行 | `innate_add` · `innate_spark` |
| 仅人工治理 | `innate_approve` · `innate_archive` · `innate_invalidate` · `innate_restore` · `innate_mature_spark` · `innate_promote_spark` · `innate_drop_spark` |

**工作流程**

- 每次任务开始前调用 `innate_recall(query="<任务意图>")`, 将结果纳入计划
- 任务结束后调用 `innate_record(trace_id=..., outcome="ok"|"fail")`, 闭合 trace
- 发现值得保留的经验或灵感时, 先提炼并向用户确认; 得到同意后调用 `innate_add` 或 `innate_spark`
- 判断知识已失效时, 只提出治理建议, 不直接执行治理动作
- 会话结束时调用 `innate_evolve(trigger="manual")` 触发蒸馏

---

## Python SDK

```python
from innate import KnowledgeBase

kb = KnowledgeBase("personal.db")  # 或通过 INNATE_DB 环境变量指定

# 写入
note_id = kb.add("经验内容", kind="note", trigger_desc="触发场景")
spark_id = kb.spark("一个待探索的灵感", trigger_desc="相关场景")

# 召回 (同步纯数学)
ctx = kb.recall("任务描述", budget=6000, include_sparks=True)
for chunk in ctx.knowledge:
    print(chunk["id"], chunk["content"])

# 记录使用
kb.record(ctx.trace_id, outcome="ok", used=[note_id], output_summary="解决了 X")

# 成长
result = kb.evolve(trigger="manual")

# 治理 (人工操作)
kb.approve(pending_id)
kb.archive(note_id, reason="stale")
kb.invalidate(note_id, reason="逻辑错误")
kb.restore(archived_id)

# 灵感生命周期
kb.mature_spark(spark_id, to="sprouting")
kb.mature_spark(spark_id, to="incubating")
new_id = kb.promote_spark(spark_id, to="note")
kb.drop_spark(spark_id, reason="已证伪")

# 体检
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
| `innate install` | 安装 | 交互式向导——配置 agent、安装二进制、安装 skill |
| `innate uninstall` | 卸载 | 从所有已配置 agent 和 PATH 移除 Innate |
| `innate daemon start` | 集成 | 后台日志/Hook 监听守护进程 `--watch <dir>` (Linux only) |
| `innate daemon status` | 集成 | 查看 daemon 运行状态与错误统计 |
| `innate daemon stop` | 集成 | 停止运行中的 daemon |

### MCP 工具 (`innate mcp` 暴露的 13 个工具)

| MCP 工具 | 对应能力 |
|---|---|
| `innate_recall` | 召回知识——任务开始时首先调用 |
| `innate_record` | 记录 trace 结果——任务完成后最后调用 |
| `innate_add` | 写入知识块 |
| `innate_spark` | 创建灵感（想法/假设） |
| `innate_evolve` | 触发成长——会话结束时调用 |
| `innate_inspect` | 库体检 |
| `innate_approve` / `innate_archive` / `innate_invalidate` / `innate_restore` | 治理 |
| `innate_mature_spark` / `innate_promote_spark` / `innate_drop_spark` | 灵感生命周期 |

---

## 系统架构

四种接入模块，共用一套 Rust KnowledgeBase Core：

```
MCP    (innate mcp)           ──────────────────────────┐
CLI    (innate <cmd>)          ──────────────────────────┤──> KnowledgeBase Core ──> SQLite
SDKs   (Python / TypeScript)   ──> CLI ─────────────────┤
Daemon (innate daemon start)   ──> CLI ─────────────────┘
```

| 模块 | 实现方式 | 说明 |
|---|---|---|
| **MCP** | 直接调用 Core lib | JSON-RPC 2.0 over stdio；13 个工具；供 Claude Code / Claude Desktop 使用 |
| **CLI** | 直接调用 Core lib | clap 薄封装；完整 Public API 的命令行入口 |
| **Python SDK** | subprocess 调用 CLI | `innate-py`，零额外依赖 |
| **TypeScript SDK** | subprocess 调用 CLI + async MCP client | `@innate/sdk`，Node.js 18+ |
| **Daemon** | subprocess 调用 CLI | 后台监听日志/JSON Hook；自动触发 recall/record/evolve；事件幂等、会话 trace、错误统计、断点续读（Linux only） |

```
Innate System
├── Rust 核心 (core/)
│   ├── KnowledgeBase (lib)      8 类 Public API, SQLite + 纯 Rust 余弦相似度
│   ├── CLI (innate <cmd>)       clap 薄封装, 参数解析 → KnowledgeBase 调用
│   ├── MCP Server (innate mcp)  JSON-RPC 2.0 over stdio, 13 个 MCP 工具
│   └── Daemon (innate daemon)   后台日志/Hook 监听, subprocess → CLI (Linux only)
│
├── 向量搜索                     f32 BLOB 存储 + 全扫描余弦相似度 (零 native 扩展)
│   ├── vec_content              内容向量 (BLOB)
│   └── vec_trigger              触发向量 (BLOB)
│
├── SDKs
│   ├── sdks/python/             innate-py  — subprocess → CLI, 零额外依赖
│   └── sdks/typescript/         @innate/sdk — CLI subprocess + async MCP client
│
└── skills/innate-memory/        SKILL.md (MCP 工具为主, CLI 为 fallback)
```

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
- Daemon: Linux only (依赖 `/proc` 与 `fork`；非 Linux 平台返回明确错误)
- 数据库默认位置: `~/.innate/personal.db` (可通过 `INNATE_DB` 环境变量或 `--db` 覆盖)
- 平台: Linux / macOS / Windows；Daemon 仅 Linux

---

## 文档

- [`docs/Innate-设计文档-v4.5.1.md`](docs/Innate-设计文档-v4.5.1.md) — 完整系统设计（权威基线）
- [`skills/innate-memory/SKILL.md`](skills/innate-memory/SKILL.md) — Agent Skill 元数据与使用规范

---

## 开发

```bash
# 编译
cd core && cargo build --release

# 运行全量测试
cd core && cargo test

# 运行单个测试
cd core && cargo test test_add_and_recall

# 检视默认知识库
innate inspect
```

测试按职责分组: `add_and_recall` · `spark_and_promote` · `record_state_machine` · `invalidate_cascade` · `inspect_returns_counts` · `evolve_smoke` + 3 个 utils 测试。

---

## License

MIT
