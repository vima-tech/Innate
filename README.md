# Innate — 自成长 Agent 程序性知识层

> **一句话定位**: 一个**可嵌入可外挂、自成长、引擎可换**的 agent 程序性知识层系统。  
> 它不做编排(对 LangGraph / Claude Code / 裸 API 中立), 只解一件事——**在有限 context 预算内, 组装最相关、最精确的知识, 并让这套知识随使用自我进化。**

## 接入 Agent（一行安装）

```bash
npx skills add vima-tech/Innate
```

安装后 Claude Code 等支持 [Agent Skills 开放标准](https://agentskills.io/specification)的 Agent 会自动获得 `innate-memory` skill——知道何时召回、何时记录、何时成长，无需手动配置。

> 前提：本机已安装 `innate` CLI（`pip install innate-ai`）。

### 让 Agent 帮你完成安装（完整提示词）

将以下内容直接发送给你的 Agent：

---

> 请帮我安装并初始化 Innate 知识层，步骤如下：
>
> ```bash
> pip install innate-ai       # 安装 CLI
> npx skills add vima-tech/Innate   # 安装 Agent Skill
> innate inspect              # 验证，应输出空库健康报告
> ```
>
> **权限说明（Agent 受限写入）**
> 你可以直接执行召回、记录、成长和体检。知识写入需先提议并获得我的确认；治理动作由我人工执行：
>
> | 操作类别 | 命令 |
> |---|---|
> | Agent 可直接执行 | `innate recall` · `innate record` · `innate evolve` · `innate inspect` |
> | 确认后执行 | `innate add ... --source agent` · `innate spark` |
> | 仅人工治理 | `innate approve` · `innate archive` · `innate invalidate` · `innate restore` · `innate promote-spark` · `innate drop-spark` |
>
> **工作规范**
> - 每次任务开始前执行 `innate recall "<任务意图>" --format json`，将结果纳入计划
> - 任务结束后执行 `innate record <trace_id> --outcome ok|fail`，闭合 trace
> - 发现值得保留的经验或灵感时，先提炼并向我确认；得到同意后执行 `innate add ... --source agent` 或 `innate spark`
> - 判断知识已失效时，只提出治理建议和命令，不直接执行
> - 会话结束时执行 `innate evolve --trigger manual` 触发蒸馏

---

## 快速开始

```bash
# 安装 CLI
pip install innate-ai

# 1. 写入知识
innate add "Python 列表推导式比 map/filter 更易读" --kind note --trigger "python 列表处理"

# 2. 召回知识
innate recall "python 列表优化" --budget 2000 --format json

# 3. 记录使用结果
innate record <trace_id> --outcome ok --feedback up

# 4. 触发成长(蒸馏 + 治理)
innate evolve --trigger manual

# 5. 库体检
innate inspect
```

## Python SDK

```python
from innate import KnowledgeBase

kb = KnowledgeBase("personal.db")

# 写入
cid = kb.add("经验内容", kind="note", trigger_desc="触发场景")
sid = kb.spark("一个灵感")

# 召回
ctx = kb.recall("任务描述", budget=6000)
for chunk in ctx.knowledge:
    print(chunk["content"])

# 记录使用
kb.record(ctx.trace_id, outcome="ok", used=[cid], feedback="up")

# 成长
kb.evolve(trigger="manual")

# 体检
print(kb.inspect())
```

## 兼容性

- Core SDK 与 CLI 需要 Python 3.10+。
- Runtime Daemon 是可选组件，当前依赖 `os.fork` 和 `/proc`，仅支持 Linux。不启用 Daemon 不影响 SDK、CLI 或 Agent Skill 使用。

## 系统架构

```
Innate System
├── Core SDK              知识层逻辑(recall/record/evolve/curate/confidence)
│   ├── Public API        8 个核心方法
│   └── Storage           sqlite-vec 默认; 5 个可替换扩展点
├── CLI Adapter           Core SDK 的命令行薄封装, 1:1 映射
├── Hook Integration      外部系统事件触发 CLI
├── Runtime (Daemon)      外部独立进程; 监听日志/事件
└── skills/               Agent Skills 标准接入层
    └── innate-memory/    npx skills add vima-tech/Innate
```

## 核心特性

- **双向量召回**: content_vec(1024维) + trigger_vec(256维), 融合排序
- **置信度驱动**: EMA 更新 + 时效加权 + 时间衰减, 知识越用越准
- **零主动行为**: SDK 永不自发行动, 所有成长由外部触发
- **安全可注入**: sanitize 钩子覆盖所有写入路径, 默认零重依赖
- **灵感系统**: spark 记录灵感, 独立 maturity 生命周期, 相关语境下自动唤起
- **闭环完整**: 召回 → 观测 → 成长 → 治理 → 安全, 五个闭环不缺
- **一行接入**: `npx skills add vima-tech/Innate` 即可让 Agent 具备完整知识层行为

## 设计文档

详见 [`docs/Innate-设计文档-v4.5.1.md`](docs/Innate-设计文档-v4.5.1.md)。

## License

MIT
