# innate-memory

**Agent 的认知蓝图 — 告诉 Agent 何时、如何调用 Innate CLI。**

> **术语区分**: Innate 内部把知识块的一种组织形态称为"skill"(origin=installed)。  
> 这里的**接入 Skill** 是面向 Agent 框架的 **Skill 配置文件**——告诉 Agent 何时、如何调用 Innate 的 CLI。二者层次不同，不要混淆。

---

## ① 元数据层 — 精准触发

```yaml
name: innate-memory
version: 4.5.1
description: >
  【读取触发】执行复杂任务前/排查历史 Bug/参考过往模式/避免重复踩坑时激活。
  【写入触发】用户要求"记录灵感"/"保存思路"/"以后就按这个来"/"记住这个教训",
  或成功解决复杂问题后需要提炼经验时,立即提取核心信息执行写入。
  即使未明确提及"记忆",只要涉及历史经验复用或新知识沉淀,均应激活。
```

---

## ② 核心逻辑层 — 工作流 + 安全围栏

### 任务前召回

```bash
# 机器集成(取 trace_id 用于后续 record):
innate recall "<任务核心意图>" --top 5 --format json
# 从 JSON 输出取 trace_id, 召回结果注入 context

# 若 Agent 框架支持 prompt 注入:
innate recall "<任务核心意图>" --top 5 --format prompt
# prompt 格式末尾有 <!-- innate_trace_id: xxx --> 可解析 trace_id
```

召回结果作为约束纳入当前计划; 高置信块优先, 低置信块参考不强制。

### 灵感结构化记录(禁止原话存储)

碎片化灵感提炼为"核心观点 + 适用场景 + 待验证假设", 再:

```bash
innate spark "<提炼后内容>"
```

### 经验沉淀(复杂问题成功解决后)

提炼可复用的代码模式或排查步骤:

```bash
innate add "<经验>" --kind note --source agent
```

(默认 pending, 等人工 approve 或 Evolve 晋升规则确认)

---

## 🚫 安全围栏

- 禁止自行执行 `innate approve` / `archive` / `invalidate` / `restore` /
  `mature-spark` / `promote-spark` / `drop-spark`
  (人工治理专属; 仅在人明确要求该动作时执行)
- `innate add --source agent` 只写 pending, 不得绕过审核
- `--feedback up|down` 仅在人明确给出反馈时传入, 不得从任务成败自行推断强反馈
- CLI 返回 `exit_code != 0`: 读 stderr 修正一次, 仍失败则放弃, **绝不阻塞主任务**
- 禁止在未经测试验证的情况下将 Agent 总结的经验标记为高置信度

---

## ③ 写入防漏层 — 反思检查(防"说了就忘")

在结束长对话或回答"收到/好的"之前, 内心评估:

> "刚才是否产生了新的代码模式、避坑指南或业务灵感?"

若是, 主动提议: "我已提炼了一条 Spark/经验, 是否需要存入 Innate?"

——**提议给人确认, 不默默写入**。人说 yes 才调 `innate spark` / `innate add`。

反思检查的落点是**提议, 不是自动行动**——完全符合"零主动行为"。
