# Innate — 自成长 Agent 程序性知识层

> **一句话定位**: 一个**可嵌入可外挂、自成长、引擎可换**的 agent 程序性知识层系统。  
> 它不做编排(对 LangGraph / Claude Code / 裸 API 中立), 只解一件事——**在有限 context 预算内, 组装最相关、最精确的知识, 并让这套知识随使用自我进化。**

## 快速开始

```bash
pip install -e .

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

## 系统架构

```
Innate System
├── Core SDK              知识层逻辑(recall/record/evolve/curate/confidence)
│   ├── Public API        8 个核心方法
│   └── Storage           sqlite-vec 默认; 5 个可替换扩展点
├── CLI Adapter           Core SDK 的命令行薄封装, 1:1 映射
├── Hook Integration      外部系统事件触发 CLI
└── Runtime (Daemon)      外部独立进程; 监听日志/事件
```

## 核心特性

- **双向量召回**: content_vec(1024维) + trigger_vec(256维), 融合排序
- **置信度驱动**: EMA 更新 + 时效加权 + 时间衰减, 知识越用越准
- **零主动行为**: SDK 永不自发行动, 所有成长由外部触发
- **安全可注入**: sanitize 钩子覆盖所有写入路径, 默认零重依赖
- **灵感系统**: spark 记录灵感, 独立 maturity 生命周期, 相关语境下自动唤起
- **闭环完整**: 召回 → 观测 → 成长 → 治理 → 安全, 五个闭环不缺

## 设计文档

详见 [`docs/Innate-设计文档-v4.5.1.md`](docs/Innate-设计文档-v4.5.1.md)。

## License

MIT
