# Innate — 自成长 Agent 程序性知识层

> **一句话定位**: 一个**可嵌入可外挂、自成长、引擎可换**的 agent 程序性知识层系统。  
> 它不做编排(对 LangGraph / Claude Code / 裸 API 中立), 只解一件事——**在有限 context 预算内, 组装最相关、最精确的知识, 并让这套知识随使用自我进化。**

Innate 管理的不是「世界是什么样 / 用户偏好是什么」那种**陈述性记忆**, 也不是一次写定再也不变的**静态技能仓库**。它管理的是**程序性知识(Procedural Knowledge)**——「事情该怎么做, 哪种方式在这个上下文里更有效」。每一块知识从进库起都在被真实使用结果考核: 好用的升权, 失效的降权归档, 灵感可孵化晋升, 整库越用越准。

## 核心闭环

五个核心闭环必须完整, 缺一就不算"知识层":

- **召回**: `query → recall → context`(双向量 ANN + 标量过滤 + 可选依赖闭包, 同步纯数学)
- **观测**: `context → use → trace`(记录哪些块被用了、结果如何)
- **成长**: `trace → distill → pending`(离线蒸馏新经验)
- **治理**: `usage → confidence → curate`(EMA 置信度更新 + Curate 归档)
- **安全**: `pending/archived/不物理删`(默认 sanitize 钩子零重依赖, 黑名单 + 红action)

> 默认同步召回路径**绝不调用任何 LLM/小模型**——Innate 是图书管理员, 不是阅读者兼编辑。只有调用方显式开启可选 Refiner 时才执行 trim/adapt。路由、动态裁剪、归因的权力交还上层编排框架。

---

## 安装

### Rust 核心 (推荐)

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

配置后 Agent 可直接使用 `innate_recall`、`innate_record` 等 MCP 工具, 无需 CLI 命令。

---

## 接入 Agent (一键配置)

将以下提示词直接发送给你的 Agent:

> 请帮我安装并初始化 Innate 知识层, 步骤如下:
>
> ```bash
> pip install innate-ai              # 安装 CLI
> npx skills add vima-tech/Innate    # 安装 Agent Skill
> innate inspect                     # 验证, 应输出空库健康报告
> ```
>
> **权限说明 (Agent 受限写入)**
> 你可以直接执行召回、记录、成长和体检。知识写入需先提议并获得我的确认; 治理动作由我人工执行:
>
> | 操作类别 | 命令 |
> |---|---|
> | Agent 可直接执行 | `innate recall` · `innate record` · `innate evolve` · `innate inspect` |
> | 确认后执行 | `innate add ... --source agent` · `innate spark` |
> | 仅人工治理 | `innate approve` · `innate archive` · `innate invalidate` · `innate restore` · `innate mature-spark` · `innate promote-spark` · `innate drop-spark` |
>
> **工作规范**
> - 每次任务开始前执行 `innate recall "<任务意图>" --format json`, 将结果纳入计划
> - 任务结束后执行 `innate record <trace_id> --outcome ok|fail`, 闭合 trace
> - 发现值得保留的经验或灵感时, 先提炼并向我确认; 得到同意后执行 `innate add ... --source agent` 或 `innate spark`
> - 判断知识已失效时, 只提出治理建议和命令, 不直接执行
> - 会话结束时执行 `innate evolve --trigger manual` 触发蒸馏

---

## 快速开始

```bash
# 1. 写入知识 (note 写为 active, skill 写为 active+protected)
innate add "Python 列表推导式比 map/filter 更易读" --kind note --trigger "python 列表处理"

# 2. 召回知识 (返回 top 块, 含 trace_id)
innate recall "python 列表优化" --budget 2000 --format json

# 3. 补全 trace (闭合经验链路)
innate record <trace_id> --outcome ok --used <chunk_id1>,<chunk_id2>
# 仅当人明确给出反馈时再补强信号:
innate record <trace_id> --feedback up --used <chunk_id1>,<chunk_id2>

# 4. 触发成长 (蒸馏 + 治理)
innate evolve --trigger manual
# 或: 重建缺失的 embedding 向量
innate evolve --rebuild-embeddings

# 5. 体检 (健康信号 + 建议命令)
innate inspect
# 或查看某个 chunk / trace 详情
innate inspect <chunk_id>
innate inspect <trace_id>
```

---

## Python SDK

```python
from innate import KnowledgeBase

# 多库: 个人库可读写, 共享库只读挂载
kb = KnowledgeBase("personal.db", shared=["shared.db"])

# 1. 写入
note_id = kb.add("经验内容", kind="note", trigger_desc="触发场景")
skill_id = kb.add("./erp-parsing.skill", kind="skill")  # 自动读文件 + 设 skill_name
spark_id = kb.spark("一个待探索的灵感")  # 走独立 maturity 生命周期

# 2. 召回 (同步纯数学)
ctx = kb.recall(
    "任务描述",
    budget=6000,
    include_sparks=True,         # 同步带出相关灵感 (不占 knowledge budget)
    expand_deps="closure",       # false | direct | closure (hard 闭包)
    libs=["personal", "shared"], # 跨库检索
)
for chunk in ctx.knowledge:
    print(chunk["id"], chunk["content"])
for spark in ctx.sparks:
    print("💡", spark["content"])

# 3. 记录使用
kb.record(
    ctx.trace_id,
    outcome="ok",
    used=[note_id],
    feedback="up",                 # 显式 👍/👎 (强信号, 主导 confidence)
)

# 4. 成长
result = kb.evolve(trigger="manual")
print("distilled:", result["distilled"], "curate:", result["curate"])

# 5. 治理 (人工操作; 下列 *_id 均为示例占位符, 实际取自 inspect 或 record 返回)
# pending_id  = inspect() 查到的 state='pending' 的 chunk id
# archived_id = inspect() 查到的 state='archived' 的 chunk id
kb.approve(pending_id)             # pending → active
kb.archive(note_id, reason="stale")
kb.invalidate(note_id, reason="逻辑错误")  # 归档 + 黑名单
kb.restore(archived_id)                  # 若此前 invalidate, 同步撤销 hash 黑名单

# 6. 灵感生命周期
kb.mature_spark(spark_id, to="sprouting")    # seed → sprouting
kb.mature_spark(spark_id, to="incubating")   # sprouting → incubating
new_id = kb.promote_spark(spark_id, to="note")  # 转正为 captured note
kb.drop_spark(spark_id, reason="已证伪")       # 放弃

# 7. 体检
report = kb.inspect()              # 库健康信号
detail = kb.inspect(chunk_id=note_id)  # 块详情 (含 parent / distilled_from 衍生)
trace  = kb.inspect(trace_id=ctx.trace_id)  # trace 详情 (含 usage_trace 时序)

# 8. 装饰器模式: 自动 recall + 注入 context + 解析 outcome
@kb.augmented(budget=6000)
def answer(query: str, context) -> dict:
    chunks = "\n".join(c["content"] for c in context.knowledge)
    # ... 用 context 决策 ...
    return {"outcome": "ok", "output_summary": "解决了 X"}

answer("如何在 FastAPI 里流式返回 SSE?")
```

---

## CLI 子命令一览

CLI 是 SDK Public API 的**薄封装**, 不新增任何知识层逻辑——只做参数解析和格式化输出。

| 子命令 | 能力域 | 说明 |
|---|---|---|
| `innate recall <query>` | 读 | `--budget` · `--top` · `--include-sparks` · `--expand-deps` · `--format text\|json\|prompt` |
| `innate record <trace_id>` | 写 | `--query` · `--outcome ok\|fail\|unknown` · `--output-summary` · `--used` · `--feedback up\|down` · `--nomination` · `--priority` · `--source cli\|hook\|daemon\|augmented` |
| `innate evolve` | 成长 | `--trigger manual\|scheduled\|threshold` · `--rebuild-embeddings` |
| `innate inspect [target]` | 调试 | 无参=库体检; 传 chunk_id / trace_id 查详情 |
| `innate add <content>` | 写入 | `--kind note\|skill` · `--trigger` · `--anti-trigger` · `--skill-name` · `--source chat\|manual\|doc\|agent` |
| `innate spark <content>` | 灵感 | `--trigger` · `--anti-trigger` |
| `innate mature-spark <id>` | 治理 | `--to sprouting\|incubating` (只允许前向) |
| `innate promote-spark <id>` | 治理 | `--to note\|skill` |
| `innate drop-spark <id>` | 治理 | `--reason` |
| `innate approve <id>` | 治理 | pending → active |
| `innate archive <id>` | 治理 | `--reason` |
| `innate invalidate <id>` | 治理 | `--reason` (归档 + 黑名单) |
| `innate restore <id>` | 治理 | archived → active; 若此前 invalidate, 同步撤销 hash 黑名单 |
| `innate daemon start` | 守护 | `--watch <dir>` (可重复) · `--db` · `--pid-file` · `--log-file` · `--state-db` |
| `innate daemon stop` | 守护 | `--pid-file` |
| `innate daemon status` | 守护 | `--pid-file` · `--state-db` |

### `recall` 输出格式

- `text` — 人类可读, 不输出 trace 信息
- `json` — 机器可读, 含 `trace_id` / `selected` / `chunks` / `sparks` 字段
- `prompt` — 可直接拼进 system prompt, 末尾含 HTML 注释 `<!-- innate_trace_id: xxx -->` 供后续提取

### `inspect` 三种视图

1. **库体检 (无参)** — 5 个健康信号:
   - 知识债务比 (含僵尸块, < 0.3 正常)
   - embed 重建队列 (待补向量的 chunk 数)
   - 灵感提示 (反复浮现的 spark id)
   - stale screening (卡死的 distill 日志)
   - 本周期蒸馏成本 (token 估算)
   - 末尾打印当前 `recall_params` / `curate_params` 便于调参
2. **chunk 详情** — 块基础信息 + `related`(parent 衍生 + distilled_from 衍生) + 末尾建议操作命令
3. **trace 详情** — episodic_log 主体 + usage_trace 时序 + 末尾建议补全/蒸馏命令

---

## 系统架构

```
Innate System
├── Core SDK              唯一拥有知识层逻辑 (recall/record/evolve/curate/confidence)
│   ├── Public API        8 类核心能力域:
│   │                     1. recall  2. record  3. evolve  4. add/spark
│   │                     5. 治理 (approve/archive/invalidate/restore/spark 生命周期)
│   │                     6. inspect  7. @augmented 装饰器  8. 多库挂载 (shared)
│   └── Storage           sqlite-vec 默认实现, 5 个可替换扩展点:
│                         · EmbeddingProvider (默认 DummyEmbeddingProvider)
│                         · VectorStore (Protocol, 通过 storage_factory 注入)
│                         · Refiner (默认 NullRefiner; allow_trim/adapt 时启用)
│                         · Distiller (默认 HeuristicDistiller, 启发式无 LLM)
│                         · Curator (整体替换对象, 不是插件列表)
│
├── Migrations            schema.sql + 4.0→4.1→4.2→4.3→4.4→4.5→4.5.1 自动迁移
│                         (innate/migrations/, 随包安装)
│
├── CLI Adapter           Core SDK 的命令行薄封装 (不新增知识层逻辑)
│   └── innate <command>  跨语言调用入口; Shell/CI 均可调
│
├── Runtime (Daemon)      可选外部进程, Linux only (os.fork + /proc)
│   ├── 日志监听          扫 *.log, 提取 innate_trace_id 关联 active trace
│   ├── 错误汇聚          同类异常连续 3 次自动记 fail
│   ├── Hook JSON         session_start / session_end / tool_success / tool_error / user_feedback
│   └── 日志轮转          RotatingFileHandler 默认 10MB × 5 份
│
└── skills/innate-memory/ Agent Skills 标准接入层
    └── npx skills add vima-tech/Innate
```

**依赖方向严格向下**: `daemon → CLI → core`。Daemon 自身不直接操作知识库, 全部通过 `subprocess` 调 `innate` CLI。Core 是唯一能读写知识库 SQLite 的层;Daemon 仅可读写自身私有状态 SQLite。

---

## 核心特性

- **双向量召回**: `content_vec` (默认 1024 维) + `trigger_vec` (默认 256 维), 按 `w_content / w_trigger / w_confidence` 融合排序
- **置信度驱动**: EMA 更新 + 时效加权 (显式信号) + 时间衰减, 知识越用越准
- **hard dep fail-closed**: 召回时若 hard 依赖不可用/被归档/跨库, 直接丢弃整个 seed, **绝不返回半截闭包**
- **soft dep 提示式**: soft 依赖仅作候选加分, 不强制装包, 跨库引用解析失败不阻塞 seed
- **零主动行为**: SDK 永不自发行动, 所有成长由外部触发 (evolve trigger: manual / scheduled / threshold)
- **零重依赖**: 默认 sanitize 仅用 5 条密钥规则 + 3 条 injection 规则, 知识写入路径全部经过钩子; 不绑定 Presidio 等重型库
- **sanitize 三态合同**: 钩子返回 `(cleaned, action)`, `action ∈ {allow, redact, discard}`; `discard` 拒绝写入, `redact` 落点 confidence 上限 0.4
- **spark 独立生命周期**: maturity = `seed → sprouting → incubating`, 仅 `promote` / `drop` 时离场; 不参与 confidence 排序, 不被低分归档, 不算知识债务
- **多库挂载**: `KnowledgeBase("personal.db", shared=["shared.db"])` — 个人库可读写, 共享库只读 (read-only PRAGMA + 缺少 Innate schema 时拒绝打开)
- **原子双向量写入**: chunk + content_vec + trigger_vec 同 SAVEPOINT 写入, 任一失败回滚
- **schema 自动迁移**: 启动时按 4.0→4.5.1 顺序 apply migrations, 库空直接 exec schema.sql, 未来版本向前兼容
- **一行接入**: `npx skills add vima-tech/Innate` 让 Agent 具备完整知识层行为

---

## 兼容性

- Core SDK 与 CLI: Python 3.10+
- 默认存储后端: [sqlite-vec](https://github.com/asg017/sqlite-vec) (随包安装)
- Runtime Daemon: 可选组件, 当前依赖 `os.fork` 和 `/proc`, **仅支持 Linux**; 不启用 Daemon 不影响 SDK / CLI / Agent Skill 使用
- 数据库默认位置: `~/.innate/personal.db` (可通过 `INNATE_DB` 环境变量或 `--db` 覆盖)

---

## 文档

- [`docs/Innate-设计文档-v4.5.1.md`](docs/Innate-设计文档-v4.5.1.md) — 完整系统设计 (权威基线)
- [`docs/innate.skill.md`](docs/innate.skill.md) — Agent Skill 中文说明
- [`skills/innate-memory/SKILL.md`](skills/innate-memory/SKILL.md) — Agent Skill 元数据 (供 `npx skills add` 解析)

---

## 开发

```bash
pip install -e ".[dev]"            # editable install
python -m pytest tests/            # 跑全量测试
python -m pytest tests/test_cli.py -q
python -m compileall -q innate tests
python -m innate --help
innate inspect                     # 检视默认知识库
```

测试按职责分组: `test_core.py` (核心) · `test_cli.py` (CLI) · `test_boundaries.py` (边界) · `test_cross_lib.py` (跨库) · `test_v451_compliance.py` / `test_v451_gaps.py` (v4.5.1 校准) · `test_v4_paths.py` · `test_design_alignment.py` · `test_completion_contracts.py` · `test_augmented.py`。

## License

MIT
