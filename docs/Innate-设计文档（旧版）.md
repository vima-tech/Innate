# Innate —— 自成长 Agent 程序性知识层 · 完整系统设计 v4.5.1

**版本 v4.5.1 · 状态:编码基线冻结并完成实现校准 · 最近校准:原子双向量写入、共享库只读、hard dep fail-closed、VectorStore 注入入口、Hook 会话 trace、CLI JSON 合同;无架构改动**

> 一句话定位:一个**可嵌入可外挂、自成长、引擎可换**的 agent 程序性知识层系统。
> 它不做编排(对 LangGraph / Claude Code / 裸 API 中立),只解一件事——
> **在有限 context 预算内,组装最相关、最精确的知识,并让这套知识随使用自我进化。**
> 接入形态不限于 SDK 嵌入——CLI 封装可让任意语言的 Agent 调用,Hook+Daemon 可在无法修改代码的封闭系统里实现无侵入自动化(详见 §九)。
>
> **它是什么?** Innate 管理的是**程序性知识**——「事情该怎么做、做更好」,而非「世界是什么样、用户喜欢什么」(那是 RAG / 对话记忆的领域)。它也不是静态的规则手册——规则不随使用变好。Innate 的核心是**置信度驱动的动态精炼**:知识从进库那一刻起就在被真实使用结果考核,好用的升权、失效的降权归档、灵感的孵化晋升——整个库随时间越用越准。

### 最重要的边界:图书管理员,不是阅读者兼编辑
Innate 是**极其聪明且克制的图书管理员**——负责存储、检索、离线整理、淘汰。它**不是阅读者兼编辑**——不判断意图、不在线裁剪文本、不做任务成败归因。路由、动态裁剪、归因的权力交还上层编排框架(LangGraph/Claude Code)。这条边界决定了几个关键设计:
- **Recall 同步路径只做纯数学**(向量 ANN + 标量过滤 + 可选图遍历),**绝不在同步路径调用任何 LLM/小模型**。
- **裁剪(trim/adapt)不是 Recall 的默认职责**:默认返回原始块,trim 仅作为调用方显式开启的可选项。
- **归因以显式信号为主**:Confidence 主要靠用户显式反馈(👍/👎)和离线 LLM-judge。

### 三条"零"原则(v3.3 强化,剥离平台影子)
1. **零主动行为** — SDK 永不自发行动。所有成长(evolve)、清理(curate)都由外部触发,内部不维护计数器自动触发、不起后台线程。被动嵌入式库。Daemon 是外部进程,不属于 SDK 内部,不违背本原则。
2. **零重依赖** — 不绑定 Presidio 等重型库。安全扫描是可注入钩子,默认仅极轻正则。embed 不可用直接报错,不内置 FTS 降级索引。
3. **零领域假设** — 不假设知识块之间一定有依赖关系。依赖能力默认关闭(deps 表空着零成本),只有显式需要的用户才启用。

### 底层对象是 chunk,不是 skill(v3.5 厘清)
Innate 的底层单元是 **chunk**,`skill` 只是 chunk 的一种组织形态(多 chunk + skill_name 分组)。知识进库有四种来源,对应 `origin`:
- **installed** — 人工安装的稳定 skill / seed 包
- **distilled** — 从 agent 使用日志离线蒸馏
- **captured** — 用户显式确认保存的知识(对话结论、判断、原则、经验),不必是标准 skill
- **spark(v3.7 新增)** — 用户随手记的**灵感/闪念**,是"尚未成立的可能性"而非"已成立的知识"

**spark 与前三者本质相反,需特殊处理**:前三种都是"已成立的知识",confidence 表达可信度,系统逻辑是"可信知识被召回强化、失效则衰减归档"。但灵感的价值不在"可信",在"待探索"——刚记下时你不知道它对不对。所以:
- **spark 不走 confidence/Curate 淘汰逻辑**(豁免)。若按低 confidence + 久未用归档,正好会把"待孵化的灵感"误杀。
- spark 走独立的 **maturity(成熟度)** 生命周期,只在用户**显式 drop 或 promote** 时离场。
- **spark 永远不参与**:confidence 排序加权、Curate 低分归档、knowledge debt ratio 计算、selected_unused 淘汰、pending→active 晋升规则。
- 详见 §二·七 灵感记录。

captured 补的是一个一直缺失的基础入口:**人和 AI 讨论后形成一段认可的结论,想直接存下、以后相关讨论自动召回**。这既不该走"日志→提名→蒸馏→pending"的绕路(你已确认价值),也不必整理成标准 skill 包(太重)。它是自成长知识层最基础的入口之一,不是平台化。

### 设计哲学:完整 ≠ 臃肿
完整 = 关键闭环不缺;克制 = 每个环节只加**最小必要机制**。Innate 是嵌入式知识层系统,不是知识治理平台。六条克制原则贯穿全文:

1. **状态少,原因多** — 只用 `pending|active|archived` 三态,靠 `state_reason` 表达细节,不上多级状态机。
2. **分数少,事件清楚** — 只用单一 `confidence`,靠 `confidence_reason` + 事件 `strength` 解释,不拆多维质量分。
3. **默认不自动污染** — distilled 默认 pending;转 active 需明确规则;Refine 不回写;Curate 不物理删。
4. **复杂治理整体替换,不做插件引擎** — 主架构不塞冲突/评测/policy/snapshot;Curator 是可整体注入替换的**单一对象**(一个 `run()`),不是插件列表调度器。
5. **API 少而硬** — 8 类 Public API 能力域覆盖核心闭环;CLI 子命令是同一能力域的命令行映射,不新增知识逻辑。`inspect()` 承担调试,不拆 explain/diff/replay/healthcheck。
6. **扩展点明确,不实现平台化** — 留 EmbeddingProvider / VectorStore / Refiner / Distiller / Curator 接口,默认实现简单。

> 五个核心闭环必须完整:**召回**(query→recall→context)、**观测**(context→use→trace)、**成长**(trace→distill→pending)、**治理**(usage→confidence→curate)、**安全**(pending/archived/不物理删)。除此之外的治理维度一律作为扩展点,不进出生版本。

---

## 零、系统定位厘清(v4.0 补)

> 本节解答一个高频困惑:**Innate 更像记忆系统还是技能管理系统?** 在 chunk 进库、confidence 演化、spark 孵化的语境下,它和传统 RAG 记忆、静态技能仓库的边界在哪里。

### 0.1 它不是什么

**不是陈述性记忆系统(Declarative Memory / RAG)**。传统记忆系统(如 Mem0)存的是「世界是什么样 / 用户偏好是什么」——客观事实、对话摘要、实体关系。Innate 对这类知识不感兴趣,也不存储。

**不是静态技能仓库(Static Skill Store / Cursor Rules)**。静态仓库里的规则是人手工写定的,不随使用反馈变化——好不好用取决于写规则那一刻的判断,之后不再演化。Innate 里每一块知识从进库起都在被真实使用结果考核。

### 0.2 它是什么

Innate 管理的是**程序性知识(Procedural Knowledge)**——「事情该怎么做、哪种方式在这个上下文里更有效」。这是认知科学里「知道怎么做(knowing how)」vs「知道是什么(knowing that)」的区分在 Agent 系统里的落地。

从嵌入形态理解最直观:Innate 是插在 Agent 和它的执行动作之间的一层——召回时提供「历史上证明有效的做法」,记录时观测「这次用了哪些知识、结果如何」,离线时精炼「哪些知识还值得继续推送、哪些已经失效」。

| 维度 | 传统记忆系统 (Mem0 / 对话记忆) | 传统技能仓库 (Cursor Rules / ClawHub) | Innate |
|---|---|---|---|
| **核心隐喻** | 日记本 / 档案柜 | 工具手册 / SOP 文件 | 肌肉记忆——用多了自然更准、久不用会生疏 |
| **存储内容** | 事实、偏好、历史摘要(是什么) | 静态规范、工具调用链(怎么做) | 带置信度的避坑指南、启发式策略(怎么做**更好**) |
| **写入方式** | 自动提取对话实体 / 纯追加 | 人工手写 / 社区下载 | 四入口(installed/distilled/captured/spark),显式反馈驱动置信度 |
| **淘汰机制** | 字符超限截断 / 向量覆盖 | 手动删除 / 版本覆盖 | 置信度衰减 + Curate 归档,不物理删 |
| **对 Agent 的价值** | 提供个性化上下文(让 Agent 懂你) | 提供执行能力(让 Agent 能调工具) | 提供判断力(让 Agent 越用越准) |

### 0.3 为什么 chunk 不是「记忆块」

「chunk」这个词在 RAG 时代特指「把长文本切碎、存进向量数据库的文本片段」——它是检索的基本单位,不含置信度、不含生命周期、不含反馈演化逻辑。

Innate 的 chunk 是**带置信度的程序性知识单元**,含完整生命周期(pending→active→archived)、置信度演化机制(EMA 更新 + 时效加权 + 时间衰减)、以及对灵感的单独处理(spark 的 maturity 生命周期)。它与 RAG chunk 的关系,大约等于「有考核体系的员工」与「人力资源记录里的一行」——名字相似,本质不同。

> **外部建议处理备注**:v4.0 收到一份建议将 chunk 重命名为 Trace→Spark→Instinct 三级实体的外部反馈。该建议的 procedural vs declarative 认知框架有价值(上方对比表部分采纳),但命名替换方案与现有架构存在根本冲突——术语歧义、API/Schema 牵动过大、且「Trace」与现有 `usage_trace` 同名异义。已在 §八 版本收口记录固化拒绝理由。

---

## 零·五、系统分层:Core / Adapter / Runtime(v4.1 新增)

> 本节解决 v4.0 的一个结构性认知冲突:文档一边说"SDK 零主动行为",一边又有 Daemon、Hook、CLI 这些看起来"主动"的东西。它们并不矛盾——它们属于不同的层。

Innate 不是单一 Python SDK,而是以 Core SDK 为中心的完整程序性知识层系统:

```
Innate System
├── Core SDK              唯一拥有知识层逻辑(recall/record/evolve/curate/confidence)
│   ├── Public API        8 类核心能力域
│   └── Storage           sqlite-vec 默认;5 个可替换扩展点
│
├── CLI Adapter           Core SDK 的命令行薄封装,不新增知识逻辑
│   └── innate <command>  跨语言调用入口;Shell/CI 均可调
│
├── Hook Integration      外部系统在关键事件点调用 CLI;不进入 SDK 内部
│   ├── 框架原生 Hook     agent_config.yaml 触发
│   └── Daemon 旁路监听   封闭系统(Cursor/网页 Agent)的无侵入接入形态
│
└── Runtime (Daemon)      外部独立进程;监听日志/事件 → 调 CLI → 写 usage_trace/episodic_log
    ├── 不拥有知识逻辑    所有操作通过 CLI Public API 进行
    ├── 不属于 SDK 内部   不起后台线程,不进 SDK core 包
    └── 可选可替换        不安装 Daemon 不影响 Core SDK + CLI 正常使用
```

**层边界的三条铁律:**
1. Core SDK — 唯一允许直接读写知识库的层;拥有 confidence 计算、Curate、distill 逻辑。
2. CLI Adapter — 只做参数解析 + 格式化输出,最终全部调用 Core SDK Public API,不写额外知识逻辑。
3. Runtime/Hook — 只能通过 CLI 与知识层交互,绝不绕过 CLI 直接操作知识库；Daemon 私有 offset/event 状态库除外。

这样同时成立:SDK 零主动行为;Daemon 可以监听事件;Hook 可以无侵入接入;CLI 可以跨语言调用。同一个 `.db` 文件被 SDK 和 CLI 共享读写不冲突(SQLite WAL 保证并发安全)。

---

## 一、它解决的五个真问题(来自生态实证)

| # | 痛点 | 出处 | 本系统的归属模块 |
|---|---|---|---|
| 1 | agent 判断不准「该不该加载某 skill」 | UCSB 论文实测瓶颈① | **Recall** + chunk 的 trigger 描述 |
| 2 | 检索回来的 skill 内容嘈杂/不精确 | UCSB 论文瓶颈② | **Curate**(离线 query-agnostic 精炼) |
| 3 | skill 需按当前任务适配才好用(提分最大) | 论文:57.7%→65.5% | **可选 Refine Hook / 上层编排显式适配**(Innate 提供接口,默认不参与在线适配) |
| 4 | skill 难写、易烂、缺质量门禁 | Sysdig | **Growth/Distill** + confidence + pending |
| 5 | agent 系统难调试、可靠性存疑 | 多方 | **Observe**(贯穿层,且回流喂 Curate) |

核心认知:痛点 2 与 3 是「同一件事的离线版与在线版」——都是让 skill 内容更精确。Curate 夜里整体擦库,Refine 现场针对性裁剪,互补不重复。

---

## 二、架构:四动词 + 一贯穿层 + 一底座

```
┌──────────────────────────────────────────────────────┐
│  Public API:  recall/record/evolve/approve/add/spark/inspect/@augmented │
├──────────────────────────────────────────────────────┤
│  Recall(同步·纯数学) ANN(双向量)→标量过滤→可选依赖→first-fit  │ 痛点1
│  Context Hook(可选·显式) trim/adapt —— 默认关闭,非Recall默认路径 │ 痛点3
│  Record(同步极轻) 只做SQL写入(append日志)+简单EMA更新confidence  │ 痛点5入口
│                    绝不触发LLM/embedding/向量计算;不更新物化计数器       │
│  Evolve(异步离线)                                         │
│     ├ Distill 蒸馏(四入口)→ 顺手生成 trigger 描述         │ 痛点4+1
│     └ Curate  精简(去噪/合并/降权/环·孤岛检测)            │ 痛点2
├──────────────────────────────────────────────────────┤
│  Observe(贯穿层):召回/使用/结果 trace,数据回流 Curate    │ 痛点5
├──────────────────────────────────────────────────────┤
│  Storage 抽象:默认 sqlite-vec,预留 libSQL 升级档          │
└──────────────────────────────────────────────────────┘
```

### 自成长闭环(SDK 的灵魂)
```
用 → Observe 观测 → Curate 依真实使用反馈精简 → 库变好
  → 下次召回更准 → 用得更对 →(循环)
```
没有 Observe,Curate 只能靠向量相似度盲猜该删谁;有了它,Curate 拿到「真实使用」这个 ground truth。

### 蒸馏四入口(均汇入同一 `evolve()`,永远异步)
| 入口 | 触发者 | 说明 |
|---|---|---|
| manual | 开发者 | 调试/收工时手动 |
| scheduled | 调度器 | 定时(如每晚) |
| threshold | 外部调度 | evolve 被调用时,内部仅 `COUNT(*) WHERE distill_state='new'` 达 N 才执行,否则快速返回。**不维护内存计数器、不维护 last_evolved 游标——只看 DB 里 new 日志数量** |
| **llm_nominated** | **LLM(as tool)** | agent 判断「这次值得学」→ 调 `kb.record(..., nomination=...)` 写高优先级日志(`priority` 未显式指定时默认 1)。**只提名,不写库**;蒸馏仍由后台 evolve worker 处理 |

LLM 提名铁律:它只往 episodic_log 写一条高优先级、带「为何值得学」理由的记录;真正入库仍走标准蒸馏管线(初筛模型 + 提炼模型 + confidence 评分 + pending 门禁)。**agent 负责提名,管线负责拍板**——避免 agent 往库里直接灌自我感觉良好的垃圾。

### Refine 三档(可选增强,默认关闭;调模型故不属 Recall 同步默认职责)
- `off`(**默认**):原样返回原始块,Recall 纯数学、零模型调用、零额外延迟
- `trim`:调用方显式开启,用廉价小模型按 query 删无关段落(不破坏 hard 闭包,见 §二·五A)
- `adapt`:调用方显式开启,用强模型改写贴合 query(贵慢)

**边界**:trim/adapt 都调模型,本质是编排层的上下文构建职责。Innate 默认不做,只提供可选钩子。把裁剪权交还上层——这是"图书管理员不当编辑"的体现。离线 Curate 可预处理过泛块,减少在线 trim 需求。

---

## 二·五、核心算法(均经沙箱验证)

> 这两块是"可据此编码"的关键。评审指出它们是原文档的黑洞,现补齐并验证。

### A. Recall 装包算法(双向量融合 → 依赖闭包 → first-fit 装包)

**第一步:双向量融合。** content_vec 与 trigger_vec 各自 ANN 检索得两路相似度,加权合并取并集重排:

```
fused_score(chunk) = w_c · sim_content + w_t · sim_trigger + w_f · confidence
默认 w_c=0.65, w_t=0.25, w_f=0.10   # content 主导;trigger 辅助"是否该用";confidence 低权重参与(不喧宾夺主)
```

**Recall 可配置参数(全部存入 `meta` 表，运行时读取，不改算法逻辑)**：

| 参数 key | 默认值 | 含义 |
|---|---|---|
| `recall.w_content` | 0.65 | content_vec 相似度权重 |
| `recall.w_trigger` | 0.25 | trigger_vec 相似度权重 |
| `recall.w_confidence` | 0.10 | confidence 参与融合权重 |
| `recall.top_k_candidates` | 20 | ANN 初筛候选数 |
| `recall.anti_trigger_penalty` | 0.6 | anti_trigger 命中时的降权系数 |
| `recall.density_refill` | true | 是否启用价值密度回填（first-fit 后的补充扫描）|

读取模式：`KnowledgeBase` 初始化时从 `meta` 表一次性加载，缓存为实例属性；`innate inspect` 库体检时打印当前值。**不支持运行时热更新**（更改后需重启 SDK 实例）。
**anti_trigger 的匹配方式(v3.4:top-K 内存匹配,连 FTS5 都不用)**:`anti_trigger_desc` **不建向量表、不预存向量、也不建 FTS5 虚拟表**(FTS5 要额外虚拟表 + 3 个同步触发器,是隐性臃肿)。它只在 top-K rerank 阶段做**内存匹配**:
```
1. content_vec + trigger_vec 先取 top-K 候选(通常 20~50 个)
2. 在 Python 内存里,对这 K 个候选的 anti_trigger_desc 做字符串包含/简单分词匹配
   (K 很小、anti_trigger_desc 很短,实测 <0.1ms,无需动用任何 DB 检索引擎)
3. 命中 → penalty:fused_score *= 0.6(默认降权)
```
**定位说明(v3.4)**:anti_trigger 是**低成本误召回抑制器,不承担完整语义否定判断**。出生版只有降权,不编码“强命中排除”语法。纯文本匹配命中不到时(如 query「美股上涨原因」未字面命中 anti「宏观市场新闻」),**宁可不惩罚,也不引入第三向量或在线模型**。这把"零额外结构、零模型"贯彻到底——连 FTS5 都省了。

**第二步:依赖处理(v3.3:默认完全关闭——零领域假设)。** 多数 skill 库的块之间并无强依赖,故依赖能力**默认不启用**,Recall 连一跳都不查,deps 表空着零成本。仅当调用方显式开启时才处理:
- **默认(`expand_deps=False`)**:不碰 deps,seed 即装包单元。
- **一跳(`expand_deps="direct"`)**:读 seed 所属库内的直接 hard deps,把 `seed + 直接hard依赖` 作为不可分割块;直接依赖超预算则丢弃 seed;**不递归展开依赖的依赖**。
- **完整闭包(`expand_deps="closure"`)**:在 seed 所属库内有界图遍历展开深度≤3 的完整 hard 闭包(带护栏防环),作为不可分割块。hard dep 严格库内闭包；跨库关联只能使用 soft dep。
> 出生版 SDK 在 Python 中执行有界图遍历,便于同时处理只读共享库。依赖能力本身保留(它是 skill 关联性精简的一部分),但**不强加给不需要的用户**。环/孤岛检测随 Curate 运行；依赖图为空时成本近似为零。

**第三步:装包(first-fit 主序 + 价值密度回填;trim 默认关闭)。** 主序按 fused_score 从高到低,逐个尝试把"seed + 其依赖"作为**不可分割整体**装入,装不下则标记跳过并继续(first-fit)。**v3.8 增补——剩余预算回填**:主序跑完后若仍有预算,把"被跳过的块"改按**价值密度**(fused_score / token_count)重排,用剩余预算回填高性价比小块。这是为了堵住 first-fit 一个真实缺口:**一个高分大块(如整本 manual)会霸占预算,挤掉多个分数略低但密度高的小块(如 prompt 片段)**——回填让缝隙被最划算的内容填满,而不浪费在"装不下大块后剩余的零头"上。

```python
# ---- 主序:fused_score 优先(保证可预测性)----
skipped = []
for seed in sorted_by_fused_score:            # 相似度软排序
    if seed in selected: continue
    # expand_deps: False=仅seed | "direct"=seed+直接hard | "closure"=有界完整闭包
    block = build_block(seed, expand_deps)
    cost  = sum(token_count[c] for c in block)
    if used + cost <= budget:
        add(block); continue                  # 装得下,整体装入
    if allow_trim and should_trim(seed, block, budget - used):  # 仅当调用方显式 allow_trim
        trimmed = trim(block, query, budget - used)
        if fits(trimmed) and deps_intact(trimmed):   # trim 后必须仍保持 hard 闭包完整
            add(trimmed); mark_refined(trimmed); continue
    skipped.append(block)                      # 当前装不下,记下待回填

# ---- v3.8 回填:剩余预算按"价值密度"补高性价比小块 ----
def density(block):
    return fused_score[block.seed] / max(block.cost, 1)   # 单位token的价值
for block in sorted(skipped, key=density, reverse=True):
    if block.cost <= (budget - used) and deps_intact(block):  # 闭包完整仍是硬约束
        add(block)                             # 高性价比小块填满缝隙
# 注:这仍是确定性的两遍扫描,不是 knapsack/DP——可预测性优先于绝对最优
```

**trim 的边界(v3.2 关键修正):**
- **默认关闭**。trim 调用模型,属编排层职责;仅当调用方显式 `recall(allow_trim=True)` 才启用。默认 Recall 是纯数学、零模型调用。
- **trim 不可破坏 hard 闭包**:只能裁剪块**内部的非关键段落**,不能删除整个 hard dependency、不能把硬依赖降级为可选。`deps_intact()` 校验闭包仍完整;不完整则跳过整个 block。
- **trim 预算分配**:具体段落裁剪策略由注入的 `Refiner` 实现。SDK 只接受“成员 id 完整、protected 内容原样、总预算可容纳”的结果；出生版不开放允许 protected 改写的参数。

**出生版本装包 = fused_score 主序 first-fit + 价值密度回填。trim/adapt 均非默认**,由调用方按需开启(adapt 永远显式)。回填是确定性两遍扫描,**仍不上 knapsack、不做动态规划**——可预测性优先于绝对最优。

### B. confidence 生命周期(初始化 → 反馈更新 → 时间衰减 → Curate 判定)

`confidence` 是全系统核心调节量(Curate 淘汰依据)。完整规则:

**初始化**(按来源):
```
installed  → 0.85   # 人写的稳定 skill,起点高
distilled  → 0.45   # 机器学的,中性偏低,靠表现挣信任
captured   → 0.60   # 用户确认保存(v3.5):比蒸馏高(你已认可),比installed低(未经长期验证)
```

**反馈更新**(EMA 更新,基础 α=0.2,按事件 strength 调节,并叠加时效加权;effective_α = 0.2 · strength · recency_w):

**信号分两层(v3.2:显式为主,隐式为辅)**。真实 Agent 里细粒度隐式归因噪声大,故 confidence 主要靠显式信号驱动:

| 层 | 事件 | target | strength | 说明 |
|---|---|---|---|---|
| **显式(主)** | user_thumbs_up | 1.0 | 1.0 | 用户显式 👍(经 `kb.record(feedback="up")`) |
| **显式(主)** | user_thumbs_down | 0.0 | 1.0 | 用户显式 👎 |
| **显式(主)** | judge_score | =分 | 0.8 | 离线 LLM-as-judge 评分 |
| 隐式(辅) | agent_used | 1.0 | 0.3 | agent 标记用了(弱,易误报) |
| 隐式(辅) | selected_unused | 0.3 | 0.1 | 进context没用(很弱) |
| 隐式(辅) | task_fail | 0.0 | 0.15 | 任务失败(很弱,难精确归因) |

隐式信号 strength 刻意调低——它们方向有参考价值,但**不让噪声大的隐式信号主导 confidence**,避免分数乱跳导致 Curate 误判。**Confidence 的可信变化主要来自显式反馈和离线评分。**

**时效加权(v3.8:让分数对"环境突变"反应更快,但守住保守边界)**。纯 EMA 是渐进平滑器,对"过去 100 次好用、最近连续失败"这类 regime 切换反应偏慢——分数跌不下来。v3.8 给显式信号叠一个**轻量时效因子** recency_w,让近期反馈权重更高:
```
recency_w = 1 + κ · 2^(−gap_days / W)        # gap_days 自 last_used_at;反馈越密集越近期,权重越高
默认 κ=0.5(放大上限 1.5×),W=14 天          # 只读已有 last_used_at,history-free,record 仍极轻
```
- **仅对显式信号生效**(👍/👎/judge);**隐式信号 recency_w ≡ 1**——隐式本就噪声大,绝不让它再被时效放大(呼应"弱化隐式归因")。
- **硬上限 1.5×**:最多把有效 α 从 0.2 抬到 0.3,远低于会"被噪声带飞"的程度。单次反馈仍温和;只有**密集的近期反馈**(active regime)才接近上限,从而在环境突变时更快收敛。
- 直觉:刚用过又立刻被 👎 的块(gap 小)说明"现在不灵了",该比一条迟来的反馈更有力;长期没动过的块收到一条反馈则退回基础 α,保持保守。
> 这恰好对应"有效性时变"——一条规则过去对、现在错时,时效加权让它退场更及时;而 1.5× 的硬上限保证不因几次抖动误杀。算一笔账:0.95 分的块连吃 👎,基础 α 需 5 次跌破 0.25 归档线,加权后约 4 次——加速温和,不激进。

**task_ok/task_fail 的归因**(trace 级,不绑单块、不平均奖励):used 的块按上表弱更新;selected 未 used 极弱更新;retrieved 未 selected 不更新。但**任务成败本身只作弱信号**,真正拉动 confidence 的是显式反馈。

**confidence_reason 推荐格式(v4.4:轻量枚举,不加新表)**:
`confidence_reason` 存字符串,推荐格式 `reason_code:detail`。reason_code 枚举:

| code | 触发场景 |
|---|---|
| `user_up` | 用户显式 👍 |
| `user_down` | 用户显式 👎 |
| `judge_score` | 离线 LLM-as-judge 评分,detail 填分值(如 `judge_score:0.82`) |
| `agent_used` | agent 标记 used |
| `selected_unused` | 进 context 但未 used |
| `task_fail` | 任务失败(弱) |
| `decay` | 时间衰减,detail 填 idle_days(如 `decay:90d`) |
| `restore` | kb.restore() 人工恢复 |
| `manual_set` | kb.approve() 或直接人工设置 |
| `init` | 初始化时按 origin 设定初始值 |

示例:`judge_score:0.75` / `decay:60d` / `user_down` / `init:distilled`。inspect() 和调试时可直接解析 reason_code 做统计,不需要额外字段。

**state_reason 推荐格式(v4.5:与 confidence_reason 统一风格)**:
`state_reason` 同样存字符串，推荐格式 `reason_code:detail`，reason_code 枚举:

| code | 含义 | 典型触发 |
|---|---|---|
| `approved` | 人工批准 | `kb.approve()` |
| `repeated_success` | 晋升三护栏触发 | aggregate 后满足 used_success≥3 等 |
| `low_confidence` | 置信度低且久未用 | Curate 归档 |
| `never_used` | 从未进入 context | Curate 归档 |
| `repeated_selected_unused` | 反复进 context 但从不使用 | Curate 归档 |
| `duplicate:<canonical_id>` | 内容哈希重复，保留更优版本 | Curate 去重 |
| `invalidated:<reason>` | 人工判定为错误 | `kb.invalidate()` |
| `embedding_pending:target=<state>` | 写入时 embedding 失败，记录目标态 | add()/spark() embedding 失败 |
| `embedding_rebuilt` | embedding 后补成功，恢复目标态 | `evolve --rebuild-embeddings` |
| `restore` | 人工恢复已归档块 | `kb.restore()` |
| `init:<origin>` | 新块初始化 | add()/promote_spark()/distill() |
| `dropped:<reason>` | 人工放弃灵感 | `kb.drop_spark()` |

示例:`embedding_pending:target=active` / `duplicate:chunk_abc123` / `invalidated:wrong_logic`。同一字段同时只有一个 state_reason，不拼接多个。`archive(reason=...)` 的人工原因允许保留调用方自由文本。

**episodic_log.distill_note 推荐格式(v4.5:终态原因专字段)**:

| code | 含义 | 典型触发 |
|---|---|---|
| `no_record_timeout` | open 行超 TTL 无 record | Curate recover_logs |
| `insufficient_material` | 最低可蒸馏条件不满足 | record() open→discarded / Distill |
| `screened_out` | Distiller 廉价筛选拒绝 | evolve() Distill 阶段 |
| `sanitize_discard` | sanitize 钩子拒绝蒸馏结果 | evolve() Distill 阶段 |
| `invalidated_hash` | 蒸馏结果命中作废黑名单 | evolve() Distill 阶段 |
| `embedding_failed` | 蒸馏结果无法生成完整双向量 | evolve() Distill 阶段 |
| `distill_failed:<reason>` | Distiller 或管线异常 | evolve() Distill 阶段 |
| `screening_timeout:<run_id>` | screening 超时(worker 崩溃) | Curate recover_logs |
| `migration_dedup` | 迁移时保留非最早 episodic_log 行 | 迁移脚本 |

**显式 feedback 落到哪些 chunk(v3.3 明确)**:`kb.record(feedback=...)` 支持 trace 级和 chunk 级两种粒度:
```
feedback="up" / "down"            # trace 级
feedback={"up":["chunk_a"]}       # chunk 级(精确指定)
```
trace 级 feedback 的落点规则(v3.4:宁可漏奖,不可错奖):
```
thumbs_up:  仅 used 的块 → 强更新(target=1.0)
            若本次无 used → 忽略该强更新,不更新任何块 confidence
            (不再 fallback 奖励 selected 前 N——避免 agent 靠常识答题、却错奖无关块,污染 Curate)
thumbs_down: 仅 used 的块 → 强降(target=0.0);selected 未 used 的块 → 不降
```
**confidence 的含金量靠精确归因捍卫**:只有明确 `used` 的块才享受显式反馈的强更新。没有 used 标记时,宁可这次不更新,也不向无关块发"虚高分"。chunk 级 feedback 则只更新显式指定的块。
出生版不单独持久化 feedback 事件；它只在本次 `record()` 内驱动 confidence EMA。需要反馈审计历史时，应在上层 Hook 事件源保留。

**时间衰减**(Curate 例行,久未使用向中性下限收敛):
```
floor = 0.3                                   # 收敛到下限而非归零
confidence = floor + (confidence - floor) · 0.5^(days_idle / 90)
```
衰减到 0.3 而非 0,是为了**不误杀"老但有用"的 skill**——它只是降权,一旦再被成功使用,反馈更新会把它拉回来。

**晋升规则(v3.1:加护栏防假阳性)**:
```
installed              → active(且 protected)
distilled              → pending(默认不自动污染)
kb.approve(chunk_id)   → active(人工)
pending → active 需同时满足三条护栏:
    used_success_count ≥ 3               # 已用且任务成功的次数(v4.1:物化字段)
    AND success_trace_ids_count ≥ 2      # 来自不同 trace 的成功(v4.1:物化字段,防同trace刷次数)
    AND confidence ≥ 0.65                # 防止低质块靠次数蒙混
  (reason=repeated_success)
```
人工状态转换同样收紧：`approve()` 只把 `pending` 提升为 `active`，`restore()` 只把
`archived` 恢复为 `active`；对已经 `active` 的调用幂等返回。`restore()` 不能绕过
pending 审核门禁。
三护栏比单纯计数稳得多:拦住"agent 错误声明 used"和"同类低质任务重复使用"两种假阳性(沙箱验证)。
> **v4.1 字段闭合说明**:晋升规则中的 `used_success_count` 和 `success_trace_ids_count(distinct)` 在 v4.0 DDL 里缺失导致规则无法直接查询。v4.1 在 chunks 表新增三个物化计数字段(见 §四 DDL),异步 aggregate 阶段一并更新。

**Curate 归档判定(v3.3:三条规则对象不重叠,显式处理 NULL;protected 永远豁免)**:
```
if protected:                                              → 保留
# low_confidence 只作用于"用过但现在低分且久未用"的块(last_used_at 非空)
if last_used_at IS NOT NULL and confidence<0.25 and idle_days>60:  → archived(low_confidence)
                                                                    (idle_days 自 last_used_at)
# repeated_selected_unused:反复进 context 但不被用
if selected_count≥10 and used_count=0 and confidence<0.5:  → archived(repeated_selected_unused)
# never_used:创建后从没进过 context(last_used_at 必为 NULL)
if used_count=0 and selected_count=0 and age_days>30:      → archived(never_used)
                                                            (age_days 自 created_at)
else:                                                      → 保留
```

**Curate 可配置参数(同样存入 `meta` 表)**：

| 参数 key | 默认值 | 对应规则 |
|---|---|---|
| `curate.low_conf_threshold` | 0.25 | low_confidence 归档线 |
| `curate.low_conf_idle_days` | 60 | low_confidence 闲置天数 |
| `curate.repeat_select_min` | 10 | repeated_selected_unused 最少 selected 次数 |
| `curate.repeat_select_conf_max` | 0.5 | repeated_selected_unused confidence 上限 |
| `curate.never_used_age_days` | 30 | never_used 存活天数 |
| `curate.open_ttl_days` | 7 | open 行无 record 超时天数 |
| `curate.screening_timeout_minutes` | 30 | stale screening 超时阈值 |
| `curate.promote_used_success_min` | 3 | pending→active 晋升最少成功次数 |
| `curate.promote_confidence_min` | 0.65 | pending→active 晋升置信度下限 |

与 Recall 参数一样：初始化时从 `meta` 加载，缓存为实例属性，不支持热更新。
**三规则不重叠**:`low_confidence` 仅管 last_used_at 非空的块;last_used_at 为 NULL 的块由 `never_used`(从没selected)或 `repeated_selected_unused`(selected过但没used)处理。`last_used_at` 只在 used/显式正反馈更新。
判定结果是**归档(state=archived)+ state_reason,而非物理删除**,可恢复。

### 两种淘汰强度:渐进失宠 vs 快速作废(v3.6)
上面的自动归档是为**"逐渐失效"**设计的(过时、没人用),靠时间衰减 + Curate 慢慢降权。但还有一种需求它处理太慢:**人已明确判定"这条是错的/已失效",想立即清除**。

算一下慢在哪:captured 知识初始 confidence 0.6,EMA(α=0.2)下每次 👎 仅降到 0.8 倍:
```
0.6 →👎 0.48 →👎 0.384 →👎 0.307 →👎 0.246(才低于0.25归档线)
```
**要 👎 四次才归档,期间错误知识一直在召回池污染对话。** EMA 是"渐进平滑器",刻意让单次反馈不剧烈——这对"质量缓变"对,对"我确认它错了"则太慢。

所以区分两种作废动作(都仍不物理删除):
| 动作 | 语义 | 效果 | 用于 |
|---|---|---|---|
| **archive**(已有) | "不再活跃" | state=archived,confidence 不动,可恢复 | 失效但无害(过时偏好、旧版规则) |
| **invalidate**(v3.6 新增) | "这是错的" | 一步:state=archived + **confidence 归零** + state_reason=`invalidated:<原因>`,**绕过 EMA 立即生效** | 确认错误/有害(被证伪的判断、错误规则) |

**invalidate 的三重防护(防错误信息"换皮重入"):**
1. **同 hash 连带**:相同 content_hash 的变体一并作废(reason=`invalidated:same_hash`)。
2. **重入黑名单**:作废的 content_hash 进 `invalidated_hashes` 表;后续 add/distill 写入同 hash 内容被拦。
3. **衍生提示**:顺 `parent_id`/`distilled_from` 查出由它衍生的块,**提示但不自动删**(自动删有误伤风险,交人裁定)。`inspect(chunk_id)` 可查一条知识的全部关联/衍生,便于一次清干净。

invalidate 是"人的一票否决",和 capture(人确认存入)是对偶——都是**人的显式判断直接作用于库,不被迫走渐进自动机制**。它仍不物理删除:保留"我曾判定这是错的"本身有价值,且防误删可恢复。人工 `restore()` 一个被 invalidate 的块时同步删除对应 `invalidated_hashes` 行，表示人工撤销此前的一票否决；`confidence=0.0` 保留，恢复后的块重新靠反馈建立信任。

---

## 二·六、Distill / Curate 的最小集 + 安全(克制版)

### Distill:只做四步(不做完整 pipeline)
```
collect logs → screen(廉价模型判是否值得学) → distill → write pending
```
Distiller 输出必须含:`content` / `trigger_desc` / `anti_trigger_desc`。管线统一补齐 `confidence` 和 `source_log_id(distilled_from)`，避免把门禁逻辑下放给可替换 Distiller。
不做:validate/score/shadow/promote 多阶段流水线——晋升靠 §二·五 的简单规则。

### Curate:内置八件事(可整体替换,非插件列表)
```
1. **aggregate + purge_usage_trace(同一事务)**  开始时固定 cutoff_ts(本轮截止时间戳),所有聚合用半开区间
              ts >= last_agg_ts AND ts < cutoff_ts(v4.5.1:防漏计 race);
              · selected_count / used_count — 从 usage_trace 半开区间增量聚合
              · used_success_count / success_trace_ids_count / last_success_at — **从 chunk_success_traces 事实表派生**,不读 raw usage_trace
              · chunk_success_traces 本步幂等写入(INSERT OR IGNORE)
              · 将 cutoff_ts 写入 meta.last_agg_ts
              · 删 ts<cutoff_ts 的 usage_trace 明细；spark 的 retrieved 明细保留
              `BEGIN IMMEDIATE` 覆盖聚合、水位推进和明细清理；等于 cutoff_ts 的 trace 留给下一轮，避免毫秒精度边界漏计。
2. **recover_logs**  stale screening 超时恢复(distill_locked_at 超可配置阈值,默认 30 分钟的 screening 行改 failed);
              open 行 ts 超过 TTL(默认7天)仍未 record 则改 discarded(reason='no_record_timeout')
3. archive   低 confidence(低于阈值 + 久未用)
              never_used(used_count=0 且 selected_count=0 且 age>30d)
             repeated_selected_unused(selected≥10 且 used=0 且 confidence<0.5)
4. dedupe    content_hash 相同 → 保留 confidence 高或 protected 的;其余 archived(duplicate),parent_id 指向 canonical;spark 永不参与
5. decay     久未使用的 confidence 时间衰减
6. promote   pending 满足重复成功和 confidence 门槛 → active
7. cycle     依赖图环/孤岛检测;只报告,不自动改写图
8. **purge_old_logs**  物理删 distill_state∈(distilled,discarded,failed) 且 ts 早于当前时间 30 天的 episodic_log
```

**计数一致性(v3.4 裁决)**:`selected_count`/`used_count` **不在 record 时同步更新**(避免热门块的"读-改-写"行锁竞争,保 record 纯 append 铁律),改由异步 aggregate 阶段批量聚合。purge 严格在 aggregate 之后、且只删已聚合(`ts < cutoff_ts`)的明细；`ts = cutoff_ts` 留给下一轮。Curate 判定直接读物化计数器,不扫 trace。

**刻意不做**:merge 语义块、split、rewrite trigger、contradiction 检测、LLM 矛盾判定(成本高误判多)。

### Curator 替换协议(v4.1 补完整接口)
需定制治理时,**整体注入一个 Curator 对象**替换内置实现,而非传入 `curators=[...]` 插件列表。这是"留接口、不实现微框架"。接口约定:

```python
class Curator:
    """可整体替换的治理器。实现此接口即可完全接管 Curate 行为。"""
    def run(self, kb: "KnowledgeBase", scope: "CurateScope") -> "CurateReport":
        """
        kb    — 对当前知识库的只读访问句柄(写操作通过 kb.approve/archive/invalidate)
        scope — 本次 Curate 的范围参数(可选 origin / skill_name / 全库)
        返回  — CurateReport(见下),供 inspect() 体检展示
        """
        ...

@dataclass
class CurateScope:
    origin: Optional[str] = None        # 限定 origin(如只跑 distilled)
    skill_name: Optional[str] = None    # 限定 skill 分组
    dry_run: bool = False               # True = 只计算,不写库

@dataclass
class CurateReport:
    archived: List[str]      # 被归档的 chunk_id 列表
    deduped: List[str]       # 被去重的 chunk_id 列表
    decayed: List[str]       # confidence 被衰减的 chunk_id 列表
    cycles: List[List[str]]  # 检测到的依赖环(chunk_id 链)
    orphans: List[str]       # 孤立节点(仅启用依赖时)
    warnings: List[str]      # 文字告警
    stats: Dict[str, Any]    # 聚合统计:{"archived_count":N, "decayed_count":N, "purged_traces":N, ...}
```
注入方式:`kb = KnowledgeBase("...", curator=MyCurator())`。未传入时使用内置实现。

### 安全:可注入的 sanitize 钩子(v3.3:零重依赖;v4.4:覆盖所有写入路径)
不绑定 Presidio 等重型检测库(安全策略与业务强相关,不该固化在知识层)。**所有内容写入路径统一经过 sanitize**:
```
sanitize(content) -> (cleaned_content, action)   # action: allow | redact | discard
```
- **默认实现**:极轻正则,零依赖,默认不裸奔。覆盖两类风险：

  ```python
  # 默认 sanitize 正则规则（实现时直接复制）
  _SECRET_PATTERNS = [
      r'sk-[A-Za-z0-9]{20,}',          # OpenAI/Anthropic API key
      r'AKIA[0-9A-Z]{16}',              # AWS access key
      r'ghp_[A-Za-z0-9]{36}',           # GitHub personal access token
      r'(?i)bearer\s+[A-Za-z0-9\-._~+/]+=*',  # Bearer token
      r'(?i)password\s*[:=]\s*\S+',     # password: xxx
  ]
  _INJECTION_PATTERNS = [
      r'(?i)ignore\s+(all\s+)?previous\s+instructions?',
      r'(?i)system\s*prompt\s*[:：]',
      r'(?i)you\s+are\s+now\s+(?:a\s+)?(?:different|new)',
  ]

  def default_sanitize(content: str):
      for pat in _INJECTION_PATTERNS:
          if re.search(pat, content):
              return content, 'discard'
      cleaned = content
      redacted = False
      for pat in _SECRET_PATTERNS:
          cleaned, count = re.subn(pat, '[REDACTED]', cleaned)
          redacted = redacted or count > 0
      return cleaned, 'redact' if redacted else 'allow'
  ```

  injection 命中优先 `discard`，不能因同一内容还包含密钥而降级为 `redact`；密钥脱敏遍历全部模式，避免一次写入中残留第二类密钥。

- **可替换**:用户注入更强的 scanner(接 Presidio 等)。
- **可关闭**:`KnowledgeBase(sanitize=None)` 显式传 `None` 完全跳过，**不建议用于生产**；适用于受控内网环境或性能敏感场景。中间态：传入 `sanitize=my_fn` 替换而非叠加。

**覆盖的写入路径及 redact/discard 落点**:

| 写入路径 | allow | redact | discard |
|---|---|---|---|
| `add(source=manual/chat)` | 正常写 active/pending | 脱敏后可 active,confidence 上限 0.4 | 不写 chunk |
| `add(source=agent)` | 强制 pending(已有规则) | 脱敏后强制 pending | 不写 chunk |
| `spark()` | 正常写 spark | 脱敏后保存,不影响 maturity | 不写 spark |
| `promote_spark()` | 正常晋升 | 重新 sanitize 后脱敏晋升 | 拒绝晋升,maturity 保持原状 |
| `distill()`(evolve 内部) | 写 pending | 脱敏写 pending,confidence ≤ 0.4 | 不写 chunk,episodic_log.distill_state=discarded + distill_note='sanitize_discard' |

**不做**:policy_events 表、quarantine 状态、权限引擎。promote_spark() 单独 sanitize 的原因:spark 入库时经过一次 sanitize,但从灵感变成正式知识时内容可能已被用户编辑——晋升是第二次写入机会,需重新过钩子。

---

## 二·七、灵感记录(v3.7 · 方案A:记录 + 唤起)

灵感是"尚未成立的可能性",核心价值是**别忘了 + 在对的时机被重新唤起**——而不是被当成可信知识推送,也不该被自动淘汰机制杀掉。本版做方案 A(轻量记录 + 主动唤起),灵感间碰撞(方案 B)留接口位、暂不实现。

### spark 的生命周期:maturity,不用 confidence
```
seed(火花) → sprouting(在长) → incubating(孵化中)
                                      ├→ promoted(已孵化成 captured/skill)
                                      └→ dropped(明确放弃)
```
- spark **豁免 Curate 的 confidence 淘汰**(实测:Curate 见 origin='spark' 直接保留)。灵感只在**显式 drop**(放弃)或 **promote**(孵化)时离场,绝不因"低分久未用"被归档误杀。
- maturity 由用户逐级推进(`seed → sprouting → incubating`),系统不自动改、不允许跨级跳过(零主动行为)。
- **spark 的 confidence 字段存在但语义为 NULL/无效**——schema 统一存储,但任何读取 confidence 的逻辑(Curate/fused_score加权/debt_ratio)必须先过滤 origin='spark'。

### 记录:kb.spark()(capture 的低门槛变体)
```python
kb.spark("也许可以用方言模型做小商户语音订单录入")
```
- 最小输入只要 content;trigger/anti_trigger 可选(灵感往往还没想清楚边界)。
- origin=spark,maturity=seed,**不计 confidence**。
- **入库时自动 recall 一次**:找出库里语义相关的 skill/note,存进 `related_ids` 作为"关联线索"(实测复用双向量召回,零新增机制)。这就实现了你要的"灵感和已有技能产生联系"——语义层面的关联是召回天生能做的。

### 唤起:灵感的真正价值(本版重点)
灵感功能最常见的失败结局是"记下就再不看了——变成灵感的坟墓"。Innate 的破解之道:**它是会主动召回的系统,让灵感在相关语境下被重新撞见**,而非躺在单独列表里等人想起。
```python
ctx = kb.recall(query, include_sparks=True)   # 默认 False;开启则额外带出相关灵感
# 返回里灵感与知识分开标记:knowledge=[...] / sparks=[💡...]
```
- 灵感**不混入知识召回结果**(不污染可信知识),而是单独标记为"💡 相关灵感",由调用方/agent 决定是否参考。
- 这才是"灵感和已有技能产生联系"最有力的形式:**不是数据结构上的关联,是认知时机上的关联**——你思考相关问题时,三个月前的灵感主动浮现。

### 孵化与放弃
```python
kb.mature_spark(spark_id, to="sprouting")  # 人工前向推进;可再推进到 incubating
kb.promote_spark(spark_id, to="note")   # 灵感成熟 → 转 captured note(或 skill)
kb.drop_spark(spark_id, reason="...")   # 明确放弃 → maturity=dropped(不物理删,可溯)
```
- promote:原 spark 标 maturity=promoted,新生 captured/skill 的 `parent_id` 指回原 spark(血缘可溯,灵感功成身退)。
- promote 若命中已有 active/pending 知识的 content_hash,直接复用已有 chunk_id 并将 spark 标为 promoted,不制造重复块。
- drop:只标记,不物理删除(保留"曾有这个想法"本身有价值)。dropped/promoted 的 spark 不再被 recall 唤起。

### 软孵化:反复浮现 → 提示,不自动升级(v3.8)
有人会想"灵感被关联/唤起够多次就自动升 maturity"。**本版明确不做自动升级**:关联多 ≠ 成熟,常常只是这个灵感涉及面广,自动升会把"广而浅"误判成"熟"。改为更克制的**软孵化**:
- `inspect()` 统计每个 spark 被 `include_sparks` **唤起的累计次数**——复用 usage_trace 的召回事件计数(唤起的 spark 记一条 `retrieved` trace,chunk_id=spark_id),**零新增结构**。Curate purge 明确保留这类事实行，避免累计次数被清零。
- 累计次数超阈值时**仅提示**:"💡 这个灵感反复浮现,要不要看看 / 考虑孵化",不自动改 maturity。
- 是否 `promote_spark` 仍由人拍板——延续"**自动检测、人工裁定**"的一贯原则,系统绝不替你升级一个还没想清楚的想法。

### 边界(守克制)
- 本版只做"记录 + 唤起 + 孵化",**灵感↔灵感碰撞(方案B)不实现**——仅预留:未来可加 `kb.spark_collision(id)` 找"相关但不同源"的灵感。现在不做,避免过早复杂化。
- spark 不参与 distill(它是原始闪念,不该被机器改写);可被 invalidate(发现是错的想法时)。

---

## 三、地基层(九条,全部经实测或压测确认)

| # | 地基假设 | 状态 |
|---|---|---|
| 1 | 引擎 = sqlite-vec 基线 | 实测可跑/跨库/零依赖 |
| 2 | 存储抽象 + libSQL 升级门 | 实测 3万chunk(1024维)撞延迟墙,非过度设计 |
| 3 | 规模线 = chunk数×维度;1024维舒适区 1-2万/库 | **实测**(见下表) |
| 4 | 跨库 = 只读挂载共享库,各库独立 ANN 后在 SDK 合并统一排序；ATTACH + UNION 保留为可验证升级选项 | **出生版已实现** |
| 5 | 单库单文件 + WAL(episodic_log 与 chunks 同库) | **实测**:WAL下写期间读9.4万次,p99 0.236ms,读不阻塞写 |
| 6 | 本体 = 统一 Chunk + protected(Curate 不淘汰 protected) | 已拍板 |
| 7 | 依赖 = hard 有界图遍历(深度护栏) + 库间 soft 引用;环/孤岛检测 | **实测通过** |
| 8 | 双向量 content_vec + trigger_vec | 实测;trigger 可低维降延迟 |
| 9 | embedding 可演进:版本字段 + 离线重建工具 | 字段已入 schema |

### 实测延迟(M-class 容器,内存库,1024维,双向量 top-20)
| 规模 | 平均 | p95 | 判断 |
|---|---|---|---|
| 1万 chunk | 36 ms | 37 ms | 同步舒适 |
| 3万 chunk | 110 ms | 113 ms | 体感临界,考虑升级 libSQL |
| 5万 chunk | 181 ms | 186 ms | 暴力搜索偏慢,应上 ANN |

**关键洞见:延迟瓶颈来自维度而非纯数量。** trigger_vec 只需粗筛「该不该用我」,可用 256 维(content 用 1024),显著降低双向量总延迟。

---

## 四、数据 Schema(核心路径与 v4.4/v4.5 新增路径均已回归覆盖)

### 主库(单库单文件,WAL;chunks + deps + usage_trace + episodic_log 同库)
- `meta` — 库元信息:lib_id / lib_role(personal|shared) / **schema_version** / content_dim / trigger_dim / embed_model / embed_version
- `chunks` — 统一知识块。关键字段见下方 DDL。
- `vec_content` / `vec_trigger` — 双 vec0 虚拟表(维度建库时注入)。**anti_trigger 不建向量表**,只在 top-K rerank 用(§二·五A)
- `deps` — 依赖图。`kind`(hard 闭包 | soft 库间软引用)/ `dst_lib` / `dst_ref`。**装包只强制展开 hard 闭包;soft 仅作提示:目标库在只读挂载列表且预算允许时作为普通候选加分,解析失败不阻塞 seed**
- `usage_trace` — Observe。`event`(retrieved|selected|refined|used|task_ok|task_fail)/ `strength` / `tokens` / `rank` / `refine_mode` / `similarity` / **`source`(sdk|cli|hook|daemon|augmented)**。**注:task_ok/task_fail 为 trace 级事件,chunk_id 可空**

### episodic_log(v3.2:并入主库,不再物理分离)
- `episodic_log` — 蒸馏原料,**与 chunks 同库**(实测 WAL 下 append 不阻塞 recall 的 select)。关键字段:`query` / `recall_snapshot`(JSON:{retrieved:[...], selected:[...]}) / `output` / `output_summary` / `outcome` / `event_source`(sdk|cli|hook|daemon|augmented) / `nomination` / `priority` / `distill_state`(open|new|screening|distilled|discarded|failed) / `distill_note`
- **append-mostly 语义**:每个 trace 只插入一行；`record()` 可补齐 output/output_summary/outcome/nomination/priority，Distill 仅更新状态、锁和 token 估算字段

### Distill 的读写路径(单库内,v3.2 简化)
episodic_log 与 chunks 同库后,Distill 是**单库内操作**,流程更简单:
```
1. 原子 claim:事务内领取一批 new 日志并标记 screening,防并发 evolve 抢同一批
2. 算:初筛 → 提炼生成 chunk + trigger_desc + anti_trigger_desc + confidence(过安全 guard)
3. 写:成品 chunk 以 state='pending' 写入;chunk.distilled_from = 日志.id
4. 回标:日志 distill_state 改 'distilled' 或 'discarded' 或 'failed'(+ distill_note)
   -- failed:模型/工具失败,distill_note 记因;failed 是终态,需人工重置为 new 才可重试
   -- open 行不参与:recall 发生但 record 未补全,不可蒸馏
```

**原子 claim 模式(步骤1的 SQL)**:SQLite 单写串行,但 evolve 可能多进程或多线程触发。用 `BEGIN IMMEDIATE` + 专用字段 `distill_run_id`/`distill_locked_at` 保证原子领取:
```sql
-- Python 层生成唯一 run_id
-- run_id = str(uuid.uuid4())
-- batch_size = 20(可配置)
-- locked_at = utc_now_iso()

BEGIN IMMEDIATE;

UPDATE episodic_log
SET distill_state      = 'screening',
    distill_run_id     = :run_id,
    distill_locked_at  = :locked_at
    -- distill_note 不写 run_id:归还给终态失败/丢弃原因专用
WHERE id IN (
  SELECT id FROM episodic_log
  WHERE distill_state = 'new'
  ORDER BY priority DESC, ts ASC
  LIMIT :batch_size
);

COMMIT;

-- 领取自己的批次(同 run_id)
SELECT * FROM episodic_log
WHERE distill_run_id = :run_id
  AND distill_state  = 'screening';
```
`BEGIN IMMEDIATE` 在写入时排他占锁，保证两个 evolve worker 不会同时更新同一批行。`distill_run_id` 专字段标识，`distill_note` 归还给终态原因专用。

**stale screening 超时恢复**:worker 崩溃后 screening 行会卡死，Curate 的 purge_logs 步骤负责恢复:
```sql
-- 在 purge_logs 步骤开头执行(在 aggregate 之后)
-- screening_timeout 默认 30 分钟,可在 meta 配置
UPDATE episodic_log
SET distill_state  = 'failed',
    distill_note   = 'screening_timeout:' || distill_run_id,
    distill_run_id = NULL,
    distill_locked_at = NULL
WHERE distill_state = 'screening'
  AND distill_locked_at < strftime('%Y-%m-%dT%H:%M:%fZ', :now_iso, '-30 minutes');
```
- `:now_iso` 由本轮 Curate 开始时的 `utc_now_iso()` 固定生成；同轮 recovery 使用同一 UTC 边界，不在 SQL 执行过程中漂移
- 超时行改 `failed`（终态），`distill_note` 记录是哪个 run_id 超时
- 不自动重试（防无限循环）；需人工或运维脚本将 `distill_state` 重置为 `new`
- `inspect()` 输出 `stale_screening_count`（当前 screening 且 locked_at 超阈值的行数）作为健康信号
- **可用单库事务**保证 3-4 原子性(同库,无跨库 ACID 问题——这是回归单库的额外好处)。
- 幂等:`distilled_from` 唯一索引保证重跑不产生重复(见 DDL)。
- Distill 离线异步,不在 Recall 路径内。

### 完整 DDL —— 主库 `schema.sql`(v4.1:补统计字段闭合晋升规则)

```sql
-- ============================================================
-- Innate 知识层 —— 单库 Schema (sqlite-vec)
-- 每个知识库 = 一个 .db 文件(WAL);chunks/deps/usage_trace/episodic_log 全部同库
-- v4.1:补 used_success_count / success_trace_ids_count / last_success_at 闭合晋升三护栏
--       usage_trace 加 source 字段追溯调用来源
--       meta 加 schema_version
-- v4.2:episodic_log 加 output_summary;usage_trace 加幂等 UNIQUE INDEX
--       新增 chunk_success_traces 事实表(持久化成功 trace 集合,purge 安全)
--       Daemon 持久状态分离至 .innate/daemon_state.sqlite
-- v4.3:episodic_log.distill_state 新增 open/screening/failed 三态
--       episodic_log.trace_id 改 UNIQUE INDEX
--       usage_trace 幂等索引拆两条(source/NULL 漏洞修复),source 改 NOT NULL DEFAULT 'sdk'
--       used_success_count/success_trace_ids_count 统一从 chunk_success_traces 派生
-- v4.4:usage_trace outcome 互斥索引(同 trace 只能有一个 task_ok 或 task_fail)
--       episodic_log.source 改名 event_source(sdk|cli|hook|daemon|augmented)
--       chunks.last_agg_ts 标注 DEPRECATED
--       embed_version=0 标记写入时 embedding 失败(待 rebuild)
-- v4.5:episodic_log 加 distill_run_id / distill_locked_at(screening 并发锁专字段)
--       distill_note 归还给终态原因专用
--       state_reason 推荐枚举格式(embedding_pending:target= 等)
-- v4.5.1:aggregate 改 cutoff_ts 半开区间(防漏计 race)
--         全库时间统一 ISO 8601 UTC: strftime('%Y-%m-%dT%H:%M:%fZ','now')
--         episodic_log 加 distill_run_id / screening 查询索引
--         usage_trace 加 selected 幂等唯一索引
-- ============================================================

PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA synchronous = NORMAL;

-- ----- 元信息:库自身 + schema + embedding 版本 -----
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', '4.5.1');
-- ⚠️ 时间格式统一约定(v4.5.1):全库所有 TEXT 时间字段统一使用 ISO 8601 UTC 格式
--    精度统一为毫秒: YYYY-MM-DDTHH:MM:SS.mmmZ  (三位毫秒,不多不少)
--    正确(SQL内): strftime('%Y-%m-%dT%H:%M:%fZ','now')  → "2024-01-15T08:30:00.000Z"
--    正确(Python写入): 只调 utc_now_iso() 封装函数,禁止在业务层散落 datetime.utcnow()
--    禁止: datetime('now')                        → "2024-01-15 08:30:00"(空格分隔,非 T/Z)
--    禁止: datetime.utcnow().isoformat() + 'Z'   → 精度不定(0/3/6位小数均可能出现)
--    禁止: date('now')                            → 仅日期,无时间
--    禁止: 本地时间(无 'utc' 修饰符)
--    原因:SQLite TEXT 时间靠字典序比较,格式或精度不一致会导致 ts > :last_ts 等比较静默出错
-- 预置 key:
--   lib_id          TEXT  -- uuid
--   lib_role        TEXT  -- personal | shared
--   schema_version  TEXT  -- 当前 schema 版本,如 "4.5.1"(迁移时比对,见 §四·五)
--   content_dim     TEXT  -- "1024"
--   trigger_dim     TEXT  -- "256"
--   embed_model     TEXT  -- 嵌入模型标识
--   embed_version   TEXT  -- 整数,递增;向量重建时递增

-- ============================================================
-- 核心实体:统一 Chunk
-- ============================================================
CREATE TABLE IF NOT EXISTS chunks (
    id            TEXT PRIMARY KEY,
    skill_name    TEXT,
    seq           INTEGER DEFAULT 0,
    content       TEXT NOT NULL,
    trigger_desc  TEXT,
    anti_trigger_desc TEXT,
    content_hash  TEXT NOT NULL,
    token_count   INTEGER,                   -- 约 len(content)//4;install/distill 写入时计算

    -- 生命周期
    origin        TEXT NOT NULL CHECK(origin IN ('installed','distilled','captured','spark')),
    source        TEXT,                      -- captured 来源(chat|manual|doc|agent);spark/distilled 可空
    maturity      TEXT,                      -- 仅 spark 用(seed|sprouting|incubating|promoted|dropped)
    related_ids   TEXT,                      -- 仅 spark 用:入库时关联的相关知识块id(逗号分隔)
    protected     INTEGER NOT NULL DEFAULT 0,
    state         TEXT NOT NULL DEFAULT 'active' CHECK(state IN ('pending','active','archived')),
    state_reason  TEXT,
    state_updated_at TEXT,

    -- 质量与演化
    confidence    REAL NOT NULL DEFAULT 0.5,
    confidence_reason TEXT,
    version       INTEGER NOT NULL DEFAULT 1,
    distilled_from TEXT,
    parent_id     TEXT,

    -- 物化计数器(异步 aggregate 批量更新,record 不碰)
    selected_count        INTEGER NOT NULL DEFAULT 0,
    used_count            INTEGER NOT NULL DEFAULT 0,
    -- v4.1 新增:支撑晋升三护栏的精确计数
    used_success_count    INTEGER NOT NULL DEFAULT 0,  -- 被 used 且 task_ok 的 trace 数
    success_trace_ids_count INTEGER NOT NULL DEFAULT 0,-- 不同 trace_id 的成功数(distinct)
    last_success_at       TEXT,                        -- 最后一次 task_ok 且 used 的时间
    -- v4.4 DEPRECATED:last_agg_ts 已无实际用途
    -- 聚合水位线统一使用 meta.last_agg_ts(全局,无需 per-chunk 粒度)
    -- 保留字段避免迁移 DDL 破坏,但实现不应读写此字段
    last_agg_ts           TEXT,  -- DEPRECATED: use meta.last_agg_ts

    -- embedding 版本
    embed_version INTEGER NOT NULL DEFAULT 1,

    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    last_used_at  TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_chunks_distilled_from ON chunks(distilled_from) WHERE distilled_from IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_chunks_state   ON chunks(state);
CREATE INDEX IF NOT EXISTS idx_chunks_origin  ON chunks(origin);
CREATE INDEX IF NOT EXISTS idx_chunks_skill   ON chunks(skill_name);
CREATE INDEX IF NOT EXISTS idx_chunks_hash    ON chunks(content_hash);
CREATE INDEX IF NOT EXISTS idx_chunks_conf    ON chunks(confidence);
CREATE INDEX IF NOT EXISTS idx_chunks_embed_v ON chunks(embed_version);  -- v4.1:重建时快速定位陈旧向量

-- 重入黑名单:invalidate 作废的 content_hash
CREATE TABLE IF NOT EXISTS invalidated_hashes (
    content_hash TEXT PRIMARY KEY,
    reason       TEXT,
    ts           TEXT NOT NULL
);

-- ============================================================
-- 双向量(trigger 低维降延迟;维度由 meta 决定)
-- ============================================================
CREATE VIRTUAL TABLE IF NOT EXISTS vec_content USING vec0(
    chunk_id TEXT PRIMARY KEY,
    embedding float[1024]
);
CREATE VIRTUAL TABLE IF NOT EXISTS vec_trigger USING vec0(
    chunk_id TEXT PRIMARY KEY,
    embedding float[256]
);

-- ============================================================
-- 依赖图
-- ============================================================
CREATE TABLE IF NOT EXISTS deps (
    src       TEXT NOT NULL,
    dst       TEXT NOT NULL,
    kind      TEXT NOT NULL DEFAULT 'hard',  -- hard | soft
    dst_lib   TEXT,
    dst_ref   TEXT,
    PRIMARY KEY (src, dst, kind)
);
CREATE INDEX IF NOT EXISTS idx_deps_src ON deps(src);
CREATE INDEX IF NOT EXISTS idx_deps_dst ON deps(dst);

-- ============================================================
-- Observe 观测(v4.1:加 source 字段追溯调用来源)
-- ============================================================
CREATE TABLE IF NOT EXISTS usage_trace (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    trace_id      TEXT NOT NULL,
    chunk_id      TEXT,
    event         TEXT NOT NULL CHECK(event IN ('retrieved','selected','refined','used','task_ok','task_fail')),
    strength      REAL DEFAULT 1.0,
    similarity    REAL,
    tokens        INTEGER,
    rank          INTEGER,
    refine_mode   TEXT,
    -- v4.1 新增:调用来源,用于审计和问题排查
    -- v4.3:改 NOT NULL DEFAULT 'sdk'——source 是审计字段不应为幂等维度,但不能 NULL
    source        TEXT NOT NULL DEFAULT 'sdk'
                  CHECK(source IN ('sdk','cli','hook','daemon','augmented')),
    ts            TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_trace_chunk  ON usage_trace(chunk_id);
CREATE INDEX IF NOT EXISTS idx_trace_tid    ON usage_trace(trace_id);
CREATE INDEX IF NOT EXISTS idx_trace_event  ON usage_trace(event);
CREATE INDEX IF NOT EXISTS idx_trace_source ON usage_trace(source);  -- v4.1
-- v4.3 幂等约束(拆两条,修复 v4.2 source/NULL 漏洞):
-- ① chunk 级 used:同一 trace 对同一 chunk 的 used 只写一次
CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_chunk_used_once
  ON usage_trace(trace_id, chunk_id, event)
  WHERE event = 'used' AND chunk_id IS NOT NULL;
-- ② chunk 级 selected:同一 trace 对同一 chunk 的 selected 只写一次
--    selected 参与 Curate repeated_selected_unused 规则,重复写入会误触归档
CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_chunk_selected_once
  ON usage_trace(trace_id, chunk_id, event)
  WHERE event = 'selected' AND chunk_id IS NOT NULL;
-- ③ trace 级 outcome:同一 trace 只能有一个 outcome 事件(task_ok 或 task_fail 二选一)
-- 关键:约束在 trace_id 上,不在 event 上——防止同一 trace 同时存在 task_ok 和 task_fail
CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_outcome_once
  ON usage_trace(trace_id)
  WHERE event IN ('task_ok', 'task_fail') AND chunk_id IS NULL;
-- 语义:一个 trace 只能有一个最终结果。record() 遇到重复 outcome 时:
--   - 同 outcome 重复写入 → INSERT OR IGNORE 幂等忽略
--   - 不同 outcome 重复写入 → 应用层抛 OutcomeConflictError,拒绝写入
-- source 字段仅用于审计查询,不参与幂等约束

-- ============================================================
-- Episodic Log(v3.2:并入主库)
-- ============================================================
CREATE TABLE IF NOT EXISTS episodic_log (
    id          TEXT PRIMARY KEY,
    trace_id    TEXT NOT NULL,
    lib_id      TEXT NOT NULL,
    ts          TEXT NOT NULL,
    -- recall() 预写字段(同步写入,构成蒸馏原料的上半部分)
    query           TEXT,
    recall_snapshot TEXT,                 -- JSON:{retrieved:[chunk_id,...], selected:[chunk_id,...]}
    -- record() 补写字段(agent 执行完才知道,UPDATE 同一行)
    output          TEXT,                 -- Agent 原始输出(可选;大时只传 output_summary)
    output_summary  TEXT,                 -- Agent 输出摘要(v4.2:蒸馏主原料;Hook/CLI 接入推荐)
    outcome         TEXT,                 -- ok|fail|unknown
    -- v4.4:改名 event_source,语义明确"这条日志从哪个接入层进来"
    -- 不再混用 'auto';默认 'sdk'(与 usage_trace.source 对齐)
    event_source TEXT NOT NULL DEFAULT 'sdk'
                 CHECK(event_source IN ('sdk','cli','hook','daemon','augmented')),
    nomination  TEXT,
    priority    INTEGER NOT NULL DEFAULT 0,
    -- v4.3:状态机收紧。recall()写 open;record()补 outcome 后按最低材料门禁改 new/discarded;evolve()只读 new
    -- open:recall已发生，等待record补全，不可蒸馏
    -- new:record已补outcome且通过最低材料门禁，可进入distill队列
    -- screening:distill正在运行(防并发重入)
    -- distilled / discarded / failed:终态
    distill_state TEXT NOT NULL DEFAULT 'open'
        CHECK(distill_state IN ('open','new','screening','distilled','discarded','failed')),
    distill_note  TEXT,    -- 失败/丢弃/安全原因(终态才写,不再复用为 run_id 标识)
    -- v4.5:screening 并发锁字段,独立于 distill_note
    distill_run_id    TEXT,  -- evolve worker 的 run_id(UUID);screening 期间写入,终态后清空
    distill_locked_at TEXT,  -- claim 时间戳(ISO 8601);超时检测用
    distill_prompt_tokens     INTEGER,
    distill_completion_tokens INTEGER
);
CREATE INDEX IF NOT EXISTS idx_log_dstate ON episodic_log(distill_state);
CREATE INDEX IF NOT EXISTS idx_log_prio   ON episodic_log(priority);
-- v4.3:trace_id 改为唯一索引;record() UPDATE WHERE trace_id=? 只会命中一行
-- 保留 id 作为内部行标识(distilled_from 引用的是 id,不改外键)
CREATE UNIQUE INDEX IF NOT EXISTS idx_log_trace ON episodic_log(trace_id);
-- v4.5.1:claim 后按 distill_run_id 取回自己的批次
CREATE INDEX IF NOT EXISTS idx_log_distill_run
  ON episodic_log(distill_run_id);
-- v4.5.1:Curate 查询 stale screening(distill_state + locked_at 组合查询)
CREATE INDEX IF NOT EXISTS idx_log_screening_locked
  ON episodic_log(distill_state, distill_locked_at)
  WHERE distill_state = 'screening';
```

> 维度注入:`float[1024]` / `float[256]` 为占位,新建库时按 EmbeddingProvider 的 `content_dim` / `trigger_dim` 替换并写入 `meta`。已有库若 provider 维度不匹配则明确报错，不能等到向量查询时静默失败。

### aggregate 阶段更新新字段(v4.1/v4.2)

**根本解法**:引入 `chunk_success_traces` 事实表，每次 aggregate 时幂等写入 (chunk_id, trace_id) 成功对。这样 `success_trace_ids_count` 直接计算事实表行数——**不依赖 usage_trace 原始明细，purge 后仍安全**。

```sql
-- ============================================================
-- 事实表:持久化每个 chunk 的成功 trace 集合
-- (在主库 schema.sql 中建，aggregate 幂等写入，purge 后仍有效)
-- ============================================================
CREATE TABLE IF NOT EXISTS chunk_success_traces (
    chunk_id  TEXT NOT NULL,
    trace_id  TEXT NOT NULL,
    ts        TEXT NOT NULL,
    PRIMARY KEY (chunk_id, trace_id)   -- 天然幂等:重复 INSERT OR IGNORE 安全
);
CREATE INDEX IF NOT EXISTS idx_cst_chunk ON chunk_success_traces(chunk_id);

-- ============================================================
-- aggregate 步骤(Curate 内部,purge_logs 之前执行)
-- ============================================================

-- 0. 固定本轮窗口边界(v4.5.1:cutoff_ts 防漏计 race)
--    在 Python 里读取:
--      last_ts    = db.execute(
--          "SELECT COALESCE(value,'1970-01-01T00:00:00.000Z') FROM meta WHERE key='last_agg_ts'"
--      ).fetchone()[0]
--      cutoff_ts  = utc_now_iso()   # 统一封装函数,见 §六·七
--      -- (等价 SQL: SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now'))
--
--    关键:cutoff_ts 在 aggregate 开始时固定,所有聚合用半开区间 ts >= :last_ts AND ts < :cutoff_ts
--    这样本轮开始后或恰好等于 cutoff_ts 的 trace 留给下一轮,不会因毫秒精度边界漏计

-- 1. 幂等写入成功 trace 对到事实表(半开区间窗口 + PRIMARY KEY 天然去重)
INSERT OR IGNORE INTO chunk_success_traces(chunk_id, trace_id, ts)
SELECT u.chunk_id, u.trace_id, MAX(u.ts)
FROM usage_trace u
WHERE u.event = 'used'
  AND u.ts >= :last_ts
  AND u.ts < :cutoff_ts
  AND (
    EXISTS (SELECT 1 FROM usage_trace t
            WHERE t.trace_id = u.trace_id AND t.event = 'task_ok')
    OR EXISTS (SELECT 1 FROM episodic_log l
               WHERE l.trace_id = u.trace_id AND l.outcome = 'ok')
  )
GROUP BY u.chunk_id, u.trace_id;

-- 2. 全部三个计数字段统一从事实表派生(v4.3:不再增量加 raw trace)
--    used_success_count = 事实表行数(每个成功 trace 计一次,同 trace 内多次 used 不重复计)
--    success_trace_ids_count = 同上(两字段现在语义完全一致,保留两个字段以便晋升规则可读)
--    last_success_at = 事实表最新 ts
UPDATE chunks SET
  used_success_count      = (SELECT COUNT(*) FROM chunk_success_traces WHERE chunk_id = chunks.id),
  success_trace_ids_count = (SELECT COUNT(*) FROM chunk_success_traces WHERE chunk_id = chunks.id),
  last_success_at         = (SELECT MAX(ts)  FROM chunk_success_traces WHERE chunk_id = chunks.id)
WHERE id IN (SELECT DISTINCT chunk_id FROM chunk_success_traces);

-- 3. 更新 selected_count / used_count(常规增量,使用同一半开区间窗口)
UPDATE chunks SET
  selected_count = selected_count + (
    SELECT COUNT(*) FROM usage_trace
    WHERE chunk_id = chunks.id AND event = 'selected'
      AND ts >= :last_ts AND ts < :cutoff_ts
  ),
  used_count = used_count + (
    SELECT COUNT(*) FROM usage_trace
    WHERE chunk_id = chunks.id AND event = 'used'
      AND ts >= :last_ts AND ts < :cutoff_ts
  )
WHERE id IN (
  SELECT DISTINCT chunk_id FROM usage_trace
  WHERE ts >= :last_ts AND ts < :cutoff_ts
);

-- 4. 将 cutoff_ts 写入 meta 作为新水位线(purge 依赖此值)
INSERT OR REPLACE INTO meta VALUES ('last_agg_ts', :cutoff_ts);

-- 5. purge_logs 在此之后执行(只删 ts < :cutoff_ts 的明细,本轮之后及边界上的写入不碰)
```
> **v4.3 设计决策**:将 `used_success_count` 和 `success_trace_ids_count` 统一从 `chunk_success_traces` 事实表派生，两者现在语义一致（"有多少个不同的成功 trace 曾 used 过这个 chunk"）。保留两个字段是为了让晋升规则语义清晰可读，不造成混淆。若未来需要区分"成功使用次数（同 trace 内多次 used 分别计）"和"不同成功 trace 数"，则需引入 `chunk_success_events` 事实表——当前设计中两者相同，不提前复杂化。

---

## 四·五、版本化与迁移策略(v4.1 新增)

> 完整系统必须说清楚:库结构变了怎么办?换了 embedding 模型怎么办?

### Schema 版本管理

`meta` 表的 `schema_version` 记录当前数据库结构版本(如 `"4.5.1"`)。SDK 启动时:
1. 读 `schema_version`,与代码预期版本对比。
2. 版本一致 → 直接使用。
3. 版本低于预期 → 执行对应迁移脚本(migrations/ 目录),迁移后更新 `schema_version`。
4. 版本高于预期 → 警告但不阻塞(向前兼容),新字段 SDK 不识别则忽略。

**迁移原则**:所有结构变更仅使用 ADD COLUMN / CREATE INDEX / CREATE TABLE,**永不 DROP COLUMN / ALTER TABLE 改类型**——保持向后兼容且可回滚。允许为兼容历史数据执行必要的 UPDATE 归一化,以及在建立幂等唯一索引前删除重复 usage_trace 明细。新增的必填列提供 DEFAULT;可空列直接兼容老数据。runner 对每个迁移 step 显式包裹 `BEGIN IMMEDIATE ... COMMIT`；任何语句失败都回滚整个 step，不能留下半迁移结构。

v4.0 → v4.1 迁移示例:
```sql
-- migrations/4.0_to_4.1.sql
ALTER TABLE chunks ADD COLUMN used_success_count    INTEGER NOT NULL DEFAULT 0;
ALTER TABLE chunks ADD COLUMN success_trace_ids_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE chunks ADD COLUMN last_success_at       TEXT;
ALTER TABLE usage_trace ADD COLUMN source TEXT;
CREATE INDEX IF NOT EXISTS idx_chunks_embed_v ON chunks(embed_version);
CREATE INDEX IF NOT EXISTS idx_trace_source   ON usage_trace(source);
INSERT OR REPLACE INTO meta VALUES ('schema_version', '4.1');
```

v4.1 → v4.2 迁移示例:
```sql
-- migrations/4.1_to_4.2.sql
ALTER TABLE episodic_log ADD COLUMN output_summary TEXT;
CREATE TABLE IF NOT EXISTS chunk_success_traces (
    chunk_id  TEXT NOT NULL,
    trace_id  TEXT NOT NULL,
    ts        TEXT NOT NULL,
    PRIMARY KEY (chunk_id, trace_id)
);
CREATE INDEX IF NOT EXISTS idx_cst_chunk ON chunk_success_traces(chunk_id);
INSERT OR REPLACE INTO meta VALUES ('schema_version', '4.2');
```

v4.2 → v4.3 迁移示例:
```sql
-- migrations/4.2_to_4.3.sql

-- 1. episodic_log.trace_id 改唯一索引
--    先检查是否存在重复:如有重复 trace_id,保留最早一条;
--    其余改 discarded 并重写 trace_id,避免唯一索引创建失败
UPDATE episodic_log
SET trace_id=trace_id || ':migration_dedup:' || id,
    distill_state='discarded',
    distill_note='migration_dedup'
WHERE id NOT IN (
  SELECT MIN(id) FROM episodic_log GROUP BY trace_id
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_log_trace ON episodic_log(trace_id);

-- 2. episodic_log.distill_state:现有 'new' 行全部保留;CHECK 约束在新建表时生效
--    SQLite ALTER TABLE 不支持加 CHECK,但 DEFAULT 修改无法 ALTER;
--    实践上:CHECK 约束由应用层写入保证,迁移只改 DEFAULT(通过重建表实现,但代价高)
--    简化方案:仅在应用层约束,不重建表;把现有 open 状态行用 migration 补写:
UPDATE episodic_log SET distill_state='open'
WHERE distill_state='new'
  AND (output IS NULL AND output_summary IS NULL AND outcome IS NULL);
-- 说明:有 query/recall_snapshot 但无 output/outcome 的是 "recall 发生但 record 未完成" 的行

-- 3. source 字段 NOT NULL:现有 NULL source 改 'sdk'(历史写入的默认值)
UPDATE usage_trace SET source='sdk' WHERE source IS NULL;

-- 4. 拆幂等索引(必须先去重,否则 CREATE UNIQUE INDEX 失败)
-- 4a. 去重 used 事件(chunk 级),保留最早一条
DELETE FROM usage_trace
WHERE id NOT IN (
  SELECT MIN(id) FROM usage_trace
  WHERE event='used' AND chunk_id IS NOT NULL
  GROUP BY trace_id, chunk_id, event
) AND event='used' AND chunk_id IS NOT NULL;

-- 4b. 处理 outcome 冲突:同一 trace 存在 task_ok 和 task_fail 两行时,保留较早一条
--     (先出现的 outcome 视为"最终结果",后来的冲突行删除)
DELETE FROM usage_trace
WHERE id NOT IN (
  SELECT MIN(id) FROM usage_trace
  WHERE event IN ('task_ok','task_fail') AND chunk_id IS NULL
  GROUP BY trace_id            -- 按 trace 分组,每个 trace 只保留最早一条 outcome
) AND event IN ('task_ok','task_fail') AND chunk_id IS NULL;

-- 4c. 建幂等索引
CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_chunk_used_once
  ON usage_trace(trace_id, chunk_id, event)
  WHERE event='used' AND chunk_id IS NOT NULL;
-- outcome 互斥:同一 trace 只能有一个 outcome(task_ok 或 task_fail,二选一)
CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_outcome_once
  ON usage_trace(trace_id)
  WHERE event IN ('task_ok','task_fail') AND chunk_id IS NULL;

INSERT OR REPLACE INTO meta VALUES ('schema_version', '4.3');
```
> **迁移铁律**:先去重、再建唯一索引。SQLite `CREATE UNIQUE INDEX` 会扫描全表，历史重复行直接导致失败，不存在"只约束新增行"的说法。去重时保留最早一条（MIN(id)），其余物理删除——这是 usage_trace 明细唯一可以物理删除的场合（迁移去重，非业务删除）。

v4.3 → v4.4 迁移示例:
```sql
-- migrations/4.3_to_4.4.sql

-- 1. usage_trace outcome 互斥:先去重冲突的 outcome 行,再建索引
--    (已存在 idx_trace_outcome_once 的库跳过 CREATE INDEX 即可)
DELETE FROM usage_trace
WHERE id NOT IN (
  SELECT MIN(id) FROM usage_trace
  WHERE event IN ('task_ok','task_fail') AND chunk_id IS NULL
  GROUP BY trace_id
) AND event IN ('task_ok','task_fail') AND chunk_id IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_outcome_once
  ON usage_trace(trace_id)
  WHERE event IN ('task_ok','task_fail') AND chunk_id IS NULL;

INSERT OR REPLACE INTO meta VALUES ('schema_version', '4.4');
```

v4.4 → v4.5 迁移示例:
```sql
-- migrations/4.4_to_4.5.sql

-- 1. episodic_log.event_source:ADD COLUMN 方式,不 DROP/RENAME 旧 source 字段
--    (与 chunks.last_agg_ts 弃用策略一致;保持"迁移只 ADD"原则)
ALTER TABLE episodic_log ADD COLUMN event_source TEXT NOT NULL DEFAULT 'sdk';

-- 从旧 source 字段迁移值(合法值直接复制,非法值降级为 'sdk')
UPDATE episodic_log
SET event_source =
  CASE
    WHEN source IN ('sdk','cli','hook','daemon','augmented') THEN source
    ELSE 'sdk'
  END;
-- 旧 source 字段保留不删,标注 DEPRECATED;实现层不再读写 source

-- 2. episodic_log 加 distill_run_id / distill_locked_at
ALTER TABLE episodic_log ADD COLUMN distill_run_id   TEXT;
ALTER TABLE episodic_log ADD COLUMN distill_locked_at TEXT;

-- 3. 更新 schema_version
INSERT OR REPLACE INTO meta VALUES ('schema_version', '4.5');
```

v4.5 → v4.5.1 迁移示例:
```sql
-- migrations/4.5_to_4.5.1.sql

-- 1. 补 distill_run_id/locked_at 查询索引
CREATE INDEX IF NOT EXISTS idx_log_distill_run
  ON episodic_log(distill_run_id);
CREATE INDEX IF NOT EXISTS idx_log_screening_locked
  ON episodic_log(distill_state, distill_locked_at)
  WHERE distill_state = 'screening';

-- 2. 补 selected 幂等唯一索引
--    先去重(若历史有重复 selected 行)
DELETE FROM usage_trace
WHERE id NOT IN (
  SELECT MIN(id) FROM usage_trace
  WHERE event = 'selected' AND chunk_id IS NOT NULL
  GROUP BY trace_id, chunk_id, event
) AND event = 'selected' AND chunk_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_chunk_selected_once
  ON usage_trace(trace_id, chunk_id, event)
  WHERE event = 'selected' AND chunk_id IS NOT NULL;

-- 3. 时间格式迁移(⚠️ 不可跳过:旧格式 "YYYY-MM-DD HH:MM:SS" 与新格式 "YYYY-MM-DDTHH:MM:SS.000Z"
--    字典序不兼容——空格 < T，导致 ts > :last_ts 等时间比较静默出错)
--
--    若为全新库(无历史运行数据):无需执行此步骤，新写入直接使用 strftime 即可。
--    若存在历史运行数据:必须将所有旧格式时间字段转为 ISO 8601 UTC 格式。
--
--    最小 SQL 迁移(将 "YYYY-MM-DD HH:MM:SS" 转为 "YYYY-MM-DDTHH:MM:SS.000Z"):
--    注意：GLOB 匹配确保只转换旧格式行，已是 ISO 格式的行不受影响。

UPDATE usage_trace
  SET ts = replace(ts, ' ', 'T') || '.000Z'
  WHERE ts GLOB '????-??-?? ??:??:??';

UPDATE episodic_log
  SET ts = replace(ts, ' ', 'T') || '.000Z'
  WHERE ts GLOB '????-??-?? ??:??:??';
UPDATE episodic_log
  SET distill_locked_at = replace(distill_locked_at, ' ', 'T') || '.000Z'
  WHERE distill_locked_at GLOB '????-??-?? ??:??:??';

UPDATE chunks
  SET created_at = replace(created_at, ' ', 'T') || '.000Z'
  WHERE created_at GLOB '????-??-?? ??:??:??';
UPDATE chunks
  SET updated_at = replace(updated_at, ' ', 'T') || '.000Z'
  WHERE updated_at GLOB '????-??-?? ??:??:??';
UPDATE chunks
  SET last_used_at = replace(last_used_at, ' ', 'T') || '.000Z'
  WHERE last_used_at IS NOT NULL AND last_used_at GLOB '????-??-?? ??:??:??';
UPDATE chunks
  SET last_success_at = replace(last_success_at, ' ', 'T') || '.000Z'
  WHERE last_success_at IS NOT NULL AND last_success_at GLOB '????-??-?? ??:??:??';
UPDATE chunks
  SET state_updated_at = replace(state_updated_at, ' ', 'T') || '.000Z'
  WHERE state_updated_at IS NOT NULL AND state_updated_at GLOB '????-??-?? ??:??:??';
UPDATE chunks
  SET last_agg_ts = replace(last_agg_ts, ' ', 'T') || '.000Z'
  WHERE last_agg_ts IS NOT NULL AND last_agg_ts GLOB '????-??-?? ??:??:??';

CREATE TABLE IF NOT EXISTS invalidated_hashes (
  content_hash TEXT PRIMARY KEY,
  reason       TEXT,
  ts           TEXT NOT NULL
);
UPDATE invalidated_hashes
  SET ts = replace(ts, ' ', 'T') || '.000Z'
  WHERE ts GLOB '????-??-?? ??:??:??';

UPDATE chunk_success_traces
  SET ts = replace(ts, ' ', 'T') || '.000Z'
  WHERE ts GLOB '????-??-?? ??:??:??';

UPDATE meta
  SET value = replace(value, ' ', 'T') || '.000Z'
  WHERE key = 'last_agg_ts'
    AND value GLOB '????-??-?? ??:??:??';

--    更稳的方案：在应用层读出时间字段后解析为 datetime 对象再重写为 ISO UTC，
--    避免 SQL 字符串替换遇到秒数带小数（如 "00.500"）时格式不一致。
--
--    兼容性验证(迁移完成后运行)：
--      SELECT COUNT(*) FROM usage_trace WHERE ts NOT GLOB '????-??-??T??:??:??.???Z';
--      SELECT COUNT(*) FROM chunks     WHERE created_at NOT GLOB '????-??-??T??:??:??.???Z';
--    两者返回 0 才视为迁移完成；否则补跑迁移或人工修正。
--    迁移策略始终只 ADD COLUMN / UPDATE，不 DROP 任何字段，确保向后兼容可回滚。

INSERT OR REPLACE INTO meta VALUES ('schema_version', '4.5.1');
```

### Embedding 版本管理

`meta.embed_version` + `chunks.embed_version` 共同管理向量的时效性:

| 场景 | 处理 |
|---|---|
| 换了 embedding 模型 | `meta.embed_version` 自增;新写入 chunk 用新版本;老 chunk embed_version 仍旧 |
| Recall 遇到 `chunk.embed_version < meta.embed_version` | 跳过陈旧向量(不用旧相似度排序) + 记一条 warning 到 inspect |
| 手动触发重建 | `innate evolve --rebuild-embeddings` → 批量对 `embed_version=0`（写入失败待补）或 `embed_version < meta.embed_version`（模型升级）的 chunk 重建向量 |
| 重建期间 | 老向量不物理删除;重建完成才更新 chunk.embed_version;Recall 在重建期间可能返回结果偏少(inspect 会提示 "X chunks pending embed rebuild") |
| 重建完成判定 | `SELECT COUNT(*) FROM chunks WHERE embed_version < (SELECT value FROM meta WHERE key='embed_version')` = 0 |

**embedding 重建不阻塞正常使用**:重建是离线异步操作,Recall 期间仍可用(只是部分旧 chunk 被跳过)。

---

## 五、Public API 契约(8 类能力域,少而硬)

```python
kb = KnowledgeBase("personal.db", shared=["shared.db"])  # 多库:个人读写 + 共享只读

# 1. 读(同步,纯数学,零模型调用)。trace=True 返回带解释结果
ctx = kb.recall(query, budget=6000, libs=["personal","shared"], trace=True,
                expand_deps=False, allow_trim=False, refine_mode="off",
                include_sparks=False)  # 开启则额外带出相关灵感,与知识分开标记(💡)

# 2. 写日志(同步极轻)
#    recall()  负责写:retrieved / selected / refined(usage_trace)
#              同时预写 episodic_log(query, recall_snapshot) —— Distill 原料的根
#    record()  负责补:output / output_summary / outcome / used / feedback / nomination
#              补写到 recall() 已创建的同一条 episodic_log 行(by trace_id)
#    CLI 调用 record 时:trace_id 是贯穿关联键,query/recall_snapshot 已由 recall() 写入,
#              CLI 常用入口可补 query/output_summary/outcome/used/feedback/nomination/priority
kb.record(trace_id,               # 贯穿 recall 和后续事件的关联键
          query=None,             # 仅在无 recall() 预写行时使用(Hook/Daemon 直接 record 场景)
                                  # 若 episodic_log 已有该 trace_id 的预写行,此参数被忽略
          output=None,            # Agent 原始输出(可选;大时传 output_summary)
          output_summary=None,    # Agent 输出摘要(蒸馏原料;Hook/CLI 场景推荐传此字段)
          outcome=None,           # ok|fail|unknown
          used=None,              # agent 声明用了哪些 chunk_id(可选,弱信号)
          feedback=None,          # 显式 👍/👎(强信号,主导 confidence)
          nomination=None,        # LLM 提名"为何值得学"
          priority=0)             # 提名优先级;nomination 非空且未显式指定时按 1 入队

# 3. 成长(异步,纯执行)
kb.evolve(trigger="manual")  # manual | scheduled | threshold

# 4. 人工治理动作
kb.approve(chunk_id)
kb.archive(chunk_id, reason="stale")
kb.invalidate(chunk_id, reason="逻辑错误")
kb.restore(chunk_id)

# 5/6. 写入外部确认的知识
kb.add(content="...",
       kind="note",           # note=对话沉淀/人工知识 | skill=标准包
       trigger_desc=None,
       anti_trigger_desc=None,
       source="chat",         # chat | manual | doc | agent
       skill_name=None)
kb.add("erp-parsing.skill", kind="skill")  # 已存在文件时读入内容,文件 stem 作为默认 skill_name

# 灵感记录
kb.spark("也许可以用方言模型做小商户语音订单录入")
kb.mature_spark(spark_id, to="sprouting")  # 可继续推进到 incubating
kb.promote_spark(spark_id, to="note")
kb.drop_spark(spark_id, reason="不可行")

# 7. 调试/自省
kb.inspect(chunk_id="...")
kb.inspect(trace_id="...")
kb.inspect()  # 库体检:含知识债务比/灵感提示/embed重建状态/本周期成本预估

# 8. 低心智入口
@kb.augmented(budget=6000)
def my_agent(query, context): ...
#  outcome/feedback 是异步后置的,装饰器无法自动知道,需二选一:
#   (a) 被装饰函数返回 {"result":..., "outcome":"ok"},装饰器解析补全 record
#   (b) 装饰器只生成 trace_id 注入,业务层拿到反馈后显式 kb.record(trace_id=..., feedback="up")
```

### record() 参数职责分层说明(v4.1 明确,CLI-SDK 一致)

`recall()` 执行时写两件事:① `usage_trace` 里的 `retrieved`/`selected`/`refined` 事件;② `episodic_log` 里预写一行(query + recall_snapshot),为后续蒸馏占坑。`record()` 无需重传 query/retrieved/selected——它只补充 agent 执行完才知道的信息:

| 参数 | 写入者 | 写什么 | 说明 |
|---|---|---|---|
| trace_id | recall() 生成 | 关联键 | 贯穿 recall→record→evolve 整个链路 |
| query | 调用方→record() | episodic_log.query(仅无预写行时) | **仅 Hook/Daemon 直接 record 无 recall 时**传入;有预写行时此参数被忽略 |
| retrieved/selected | recall() 内部 | usage_trace(retrieved/selected) | record 不需要传 |
| query / recall_snapshot | recall() 内部 | episodic_log 预写行(distill_state='open') | 有 recall 时 record 不需要传 query |
| output | 调用方→record() | episodic_log.output | Agent 原始输出(可空) |
| output_summary | 调用方→record() | episodic_log.output_summary | 输出摘要(蒸馏主原料;Hook/CLI 推荐) |
| outcome | 调用方→record() | episodic_log.outcome + distill_state 门禁 | ok/fail/unknown;**record() 补完 outcome 后才把 open 改 new/discarded** |
| used | 调用方→record() | usage_trace(used) | agent 声明使用的 chunk |
| feedback | 调用方→record() | confidence EMA 更新 | 显式 👍/👎 |
| nomination | LLM→record() | episodic_log.nomination+priority | 提名值得学 |

> **为什么 recall() 预写 episodic_log？** 若 recall() 只写 usage_trace，而 output/outcome 由 record() 写到新行，则 query 和 output 落在两张表的两行，Distill 需跨表 JOIN 才能还原"这次任务完整上下文"。recall() 预创建 episodic_log 行，record() UPDATE 同一行补全——Distill 读到的每条记录已经是完整的 query+recall_snapshot+output+outcome，**一行即一个完整蒸馏单元**。

**CLI 与 SDK 的常用 record 路径一致**（Python SDK 额外支持完整 `output` 和 chunk 级 feedback dict）:
```bash
# CLI 调用(trace_id 由上游 recall --format json 产生并传入)
innate record <trace_id> [--outcome ok|fail] [--output-summary "..."] [--used c1,c2]
              [--feedback up|down] [--nomination "..."] [--priority N]
```
```python
# SDK 调用(等价)
kb.record(trace_id, output_summary="...", outcome="ok", used=["c1","c2"], feedback="up")
```
CLI 不传 query/recall_snapshot（recall 阶段已写入），也不传 retrieved/selected（usage_trace 已有）。

### @augmented 边界(v3.3 明确)
它只负责"召回注入"和"基础 trace(retrieved/selected)"的自动记录。**任务成败和用户反馈是异步后置的,装饰器不能魔法般得知**,必须靠返回值约定或显式补 record 闭环。避免开发者对装饰器产生"全托管"的错误预期。

### inspect() 的五个健康信号
1. **知识债务比(Knowledge Debt Ratio)** — `(pending 数 + 僵尸块数) / 有效总数`。僵尸块 = 创建超过 7 天且 confidence [0.4, 0.6] 的 active 块；新写入 captured note 有 7 天缓冲期。spark 完全排除，不进入分子或分母。比率走高 = 蒸馏进得多、晋升/淘汰跟不上。
2. **反复浮现的灵感提示** — spark 被唤起累计次数超阈值时提示"💡 要不要看看"。
3. **embed 重建状态** — `X chunks pending embed rebuild`(embed_version=0 或落后 meta 的块数)。
4. **本周期 Distill 预估成本** — `distill_prompt_tokens` + `distill_completion_tokens` 汇总。
5. **stale screening 数** — `distill_state='screening' 且 distill_locked_at 超配置阈值`（默认 30 分钟）的行数；非零表示有 worker 崩溃卡死，需 Curate 或人工干预。

**inspect() 输出格式与建议命令(CLI 实现规范)**

`innate inspect` 库体检输出采用分区结构，每个异常信号附带可直接执行的建议命令。不新增子命令，只让现有输出更具操作性：

```
innate inspect
─────────────────────────────────────────────
📚 知识库: personal.db  (chunks: 312 active / 47 pending / 23 archived)
─────────────────────────────────────────────
✅ 知识债务比         0.12  (< 0.3, 正常)
⚠️  embed 重建队列    8 chunks 待补向量
   → 建议执行: innate evolve --rebuild-embeddings
💡 灵感提示           3 个 spark 反复浮现
   → 建议执行: innate inspect <spark_id>  查看详情
🔴 stale screening    2 条日志卡死 (> 30min)
   → 建议执行: innate evolve --trigger manual  触发 Curate 清理
💰 本周期蒸馏成本     ~3,200 tokens (上限 50,000)
─────────────────────────────────────────────
```

实现规则：
- ✅ 正常 / ⚠️ 警告 / 🔴 错误 三档图标对应信号严重程度
- 每条 ⚠️/🔴 信号下附一行 `→ 建议执行: <可复制命令>`，不新增任何子命令
- 数值展示：绝对数 + 百分比/上限对比，让状态一眼可判
- `innate inspect <chunk_id>` 和 `innate inspect <trace_id>` 输出详情时，末尾同样附"相关操作"提示；普通知识给出 `approve/archive/invalidate/restore`，spark 给出 `mature/promote/drop/invalidate`

### kb.add 的语义(v3.5:统一写入入口)
| kind | origin | 默认 state | 默认 confidence | protected |
|---|---|---|---|---|
| skill | installed | active | 0.85 | 1 |
| note | captured | active | 0.60 | 0 |
| (distill 走 evolve,非 add) | distilled | pending | 0.45 | 0 |

### 可替换扩展点(原则 6:可扩展 ≠ 平台化)
```
EmbeddingProvider   嵌入模型(可换)
VectorStore         存储后端(sqlite-vec 默认,libSQL 预留——地基#2)
Refiner             在线精炼器(默认关闭;启用时提供 trim/adapt 实现)
Distiller           蒸馏器(出生版本默认启发式提炼;可注入 LLM 实现)
Curator             清理器(默认内置最小集;复杂治理整体替换,见 §二·六)
```
出生版通过 `KnowledgeBase(..., storage_factory=StorageSubclass)` 注入 SQL-compatible
VectorStore；默认实现为 sqlite-vec `Storage`。替代实现需兼容现有事务与聚合 helper，
不在本版扩展为插件框架。

### Trace 事件的写入时序(职责边界)
| 阶段 | 写入方 | 写什么 |
|---|---|---|
| `recall()` 内部 | SDK | `retrieved`/`selected`/`refined`(usage_trace);**预写 episodic_log 一行**(query, recall_snapshot, **distill_state='open'**) |
| agent 执行后 | 调用方 → `record()` | `used`/`task_ok`/`task_fail`(usage_trace);**UPDATE episodic_log** 补 output/output_summary/outcome；outcome 补完后按材料门禁改 `new` 或 `discarded`;EMA 更新 confidence |
| `evolve()` | SDK | 只读 **distill_state='new'** 蒸馏(open 行不参与);先改 screening 防并发重入;回标 distilled/discarded/failed;aggregate 计数器 |

> **record() 写入逻辑(v4.5)**:整个函数在 `BEGIN IMMEDIATE` 事务内执行，保证 usage_trace 和 episodic_log 的 outcome 操作原子一致——任一步骤失败则全部回滚。
>
> 1. `BEGIN IMMEDIATE`（排他写锁）
> 2. `SELECT id, outcome FROM episodic_log WHERE trace_id=?`
> 3. **outcome 冲突检查**（若调用方传入了 outcome 参数）:
>    - `episodic_log.outcome` 为空 → 允许写入
>    - `episodic_log.outcome` 与传入值相同 → 幂等忽略 outcome 部分，继续处理其他字段
>    - `episodic_log.outcome` 与传入值**不同** → 回滚事务，抛 `OutcomeConflictError`；不更新任何表
> 4. **usage_trace outcome 写入**（INSERT OR IGNORE 配合 idx_trace_outcome_once 唯一索引）
> 5. **有预写行** → `UPDATE episodic_log SET output_summary=?,outcome=?,event_source=? WHERE trace_id=?`
>    **无预写行**（Hook/Daemon 直接 record）→ `INSERT INTO episodic_log(...,query=?,distill_state='open')`
> 6. **open → new/discarded 判断**（仅在 outcome 补完后执行；只补摘要或提名时继续保持 open）:
>    - 满足以下**任一**条件 → `distill_state='new'`
>      - `output_summary` 非空 / `nomination` 非空 / `used` 非空且 outcome≠unknown / `output` 非空
>    - 否则 → `distill_state='discarded'`，`distill_note='insufficient_material'`
> 7. `COMMIT`
>
> `episodic_log.outcome` 一旦被写入非空值，后续 record() 调用传入不同 outcome 时**必须**在步骤3拦截——两张表的 outcome 必须保持一致，任何部分成功都会导致数据分裂。

---

## 六、安全与不可逆性原则

- **Curate 永不物理删除**:只 `state=archived` 降权归档,可恢复;合并保留原始块指针。
- **蒸馏默认进 pending**:新蒸馏块需一次确认才转 active(跑稳后可放开)。垃圾进垃圾出是最大风险,故默认保守。
- **Refine 不污染库**:在线产物只作用于本次返回,绝不回写。在线/离线两条精炼路径隔离。
- **hard 闭包双保险**:有界图遍历的深度上限护栏(即使有环也不爆栈)+ Curate 例行环检测。触达深度上限时**丢弃 seed 而非截断闭包**——half-dep block 比空结果危险，"宁可不召回"是硬原则。
- **protected 豁免**:人写的 installed skill 不被自成长机制误伤。

---

## 六·五、健壮性:失败降级与边界行为

自成长系统长期运行,外部依赖(embed 服务、Refine 的 LLM)必然偶发失败。降级策略:

| 故障 | 降级行为 | 原则 |
|---|---|---|
| embed 服务不可用(recall 时) | recall 抛 `EmbeddingUnavailable` 异常,由调用方决定兜底;**SDK 不内置 FTS 降级索引** | 报错交还上层,SDK 零增加 |
| `add()` embedding 失败 | 仍写 chunk,`embed_version=0`;`state` 和 `state_reason` 按原始意图写入但编码目标态，格式 `embedding_pending:target=<intended_state>`（如 `embedding_pending:target=active`）;rebuild 成功后恢复为 intended_state，`state_reason='embedding_rebuilt'` | 人工输入不因 embed 失败丢失；rebuild 后知道该去 active 还是 pending |
| `spark()` embedding 失败 | 仍写 spark,`embed_version=0`,`state_reason='embedding_pending:target=active'`;暂不参与 `include_sparks` 向量召回;rebuild 成功后 state_reason 改 `embedding_rebuilt` | spark 记录不丢失,召回暂时缺席 |
| `distill()` embedding 失败 | **不写 chunk**;`episodic_log.distill_state='failed'`,`distill_note='embedding_failed'`;需重置为 new 后重试 | 蒸馏结果依赖向量,无向量不写半成品 |
| recall 遇到 `embed_version=0` | 跳过该 chunk + inspect 提示"X chunks pending embed rebuild" | 与陈旧向量处理一致 |
| Refine 的 LLM 超时/失败 | 自动回落 `refine=off`,返回原始块 | Refine 是增强非必需 |
| Distill 模型失败 | 该日志条目改 `distill_state='failed'`,`distill_note` 记录失败原因;failed 行不参与下次 evolve 自动重试(防无限循环);需人工或运维脚本将其重置为 `new` 后方可重试;Curate purge 也会在 TTL 后清理 failed 行 | failed 是明确终态,不静默重试 |
| 库为空(冷启动) | recall 返回空 context + 明确标志位 `empty=True` | 优雅空返回,不报错 |
| 有界遍历触达深度上限(hard dep 闭包不完整) | **丢弃该 seed/block**，写一条 `event='retrieved', refine_mode='skipped:dep_depth_limit'` 到 usage_trace；不送半截 hard 依赖进 context | 宁可不召回，不给半截依赖 |
| hard dep 缺失、已归档、为 spark、向量陈旧或跨库 | **丢弃该 seed/block**，写一条 `event='retrieved', refine_mode='skipped:hard_dep_unavailable'` 到 usage_trace | hard 闭包 fail-closed，不把不完整规则送进 context |
| embed 版本不一致(陈旧向量) | Recall 跳过 `embed_version < meta.embed_version` 的块 + inspect 提示 pending rebuild 数量;重建期间结果偏少但不阻塞 | 不阻塞,可后台重建 |
| Hook 重复触发(同一事件被多次调用) | Daemon 以 `event_id`(日志行 hash + 文件 inode + offset)去重,已处理 event_id 跳过;框架原生 Hook 依赖 agent 框架的幂等保障;`innate record` 对同 trace_id 的同 outcome 重复调用幂等 | event_id 幂等,多次写 trace 无害 |
| Daemon 进程崩溃 | CLI 调用失败不阻塞主 Agent;Daemon 独立重启后从 `last_processed_offset` 继续,不重放已处理事件 | 知识层失败不等于任务失败 |

**空库冷启动**:新建知识库默认空,recall 返回空集而非异常。建议接入时先 `kb.add(pack, kind="skill")` 导入现成包作为起点,避免"上来就没货"的体验。

**幂等性**:Distill 靠 `distilled_from`(唯一索引)去重;install 靠 `content_hash` 应用层去重。重复导入同一 skill 不产生重复 chunk。`add()` 去重只检查正式 knowledge chunk，不把同内容 spark 当作已落库知识。add/distill 写入前还须查 `invalidated_hashes`,命中则拒绝——防被作废的错误信息换皮重入。

**成长成本可控(v3.3)**:Distill 把 token 消耗记入 episodic_log;`inspect()` 体检输出本周期预估成本。可在 `meta` 设 `max_distill_tokens_per_period`,evolve 执行前累计超额则跳过 threshold 触发的蒸馏——让成长可量化、可熔断。

---

## 六·六、已知优化方向(非阻塞,记录备忘)

- **vec0 主键**:当前 `chunk_id TEXT`,实测可跑;sqlite-vec 对 INTEGER rowid 性能更佳,规模增长后可引入"TEXT uuid ↔ INTEGER rowid"映射表优化。
- **装包**:出生版用 first-fit 主序 + 价值密度回填;v3.8 的回填已堵住"大块挤占小块"的主要缺口。若未来仍需更优 token 利用率,可探索受限 knapsack,但需权衡可预测性,非必需。
- **trigger 低维**:content 1024 / trigger 256 已定;可进一步实测 trigger 128 维的召回质量损失是否可接受。

---

## 六·七、实现注意事项(编码时必读,非架构改动)

**时间生成：封装唯一函数，不散落调用**

文档规定全库时间统一为毫秒精度 ISO 8601 UTC（`YYYY-MM-DDTHH:MM:SS.mmmZ`）。实现时必须封装一个唯一入口，所有 Python 写入路径只调这一个函数：

```python
def utc_now_iso() -> str:
    """全库统一时间生成函数。输出毫秒精度 ISO 8601 UTC。
    禁止在业务层直接调用 datetime.utcnow()、time.time() 或 SQLite datetime('now')。
    """
    from datetime import datetime, timezone
    dt = datetime.now(timezone.utc)
    # 固定毫秒精度(3位)——isoformat() 可能输出 0/3/6 位小数，不稳定
    return dt.strftime('%Y-%m-%dT%H:%M:%S.') + f'{dt.microsecond // 1000:03d}Z'

# 输出示例: "2026-06-01T12:00:00.123Z"
```

**为什么不能混用 Python 和 SQLite 生成时间：**

| 写法 | 可能输出 | 问题 |
|---|---|---|
| `datetime.utcnow().isoformat() + 'Z'` | `...000Z` 或 `...123456Z` | 精度不定（0~6位小数均可能） |
| `strftime('%Y-%m-%dT%H:%M:%fZ','now')` | `...000Z`（毫秒） | 仅 SQL 层使用 |
| `utc_now_iso()`（封装） | `...123Z`（固定毫秒） | ✅ 唯一正确做法 |

SQLite TEXT 时间靠字典序比较：`"...000Z" < "...123456Z"` 字典序成立（因为 `0 < 1`），但如果混入 `"...123456Z"` 这类六位精度的值，与三位精度值比较在极端情况下可能出错（如 `"...100000Z"` vs `"...99Z"` 这类边界）。统一毫秒精度彻底消除这个隐患。

---

## 七、非阻塞标定项 / 下一步候选

> 出生版功能已补齐。以下为真实数据标定和产品策略候选,不属于未实现功能。

1. **trigger / anti_trigger 描述生成质量**:蒸馏时如何写好「我何时用 / 何时不该用」——写不好则痛点1解不掉。需 prompt 设计 + 抽检。**(性价比最高的下一步:一头连召回准度、一头连蒸馏质量)**
2. **融合权重 + strength 数值标定**:w_c/w_t/w_f、各事件 strength、α、half_life、晋升阈值(used_success≥3 等)均为合理初值,需真实数据回归。
3. **简单晋升规则的阈值**:`used_success≥3 转 active`、`selected≥10 且 used=0 且 conf<0.5 归档` 的具体数字待实跑校准。
4. **个人块晋升 shared 的策略**:产品策略,非地基。
5. **v3.8 新增机制的实测标定**:装包价值密度回填、confidence 时效因子(κ=0.5 / W=14d)、知识债务比阈值、灵感"反复浮现"提示阈值——均为合理初值,需沙箱回归 + 真实数据校准后并入 §附 已验证清单。
### 实现验收 checklist（v4.5.1 升级：从"测试策略"改为验收门禁）

不单独建 Evaluation 子系统，但以下路径**必须在编码完成后验证通过才可视为实现完成**。四类测试框架不变，具体覆盖点如下：

**基础正确性（原有验证路径）**
- confidence EMA 更新始终有界 [0,1]
- Curate 三归档规则对象不重叠（low_confidence / never_used / repeated_selected_unused）
- 晋升三护栏（used_success≥3 + distinct_trace≥2 + confidence≥0.65）拦住假阳性
- recall 不超 budget；装包 hard 闭包完整；Curate 不物理删除任何 chunk
- Distill 重跑 distilled_from 唯一索引；install 同 hash 不重复写

**v4.4/v4.5 新增必验路径（编码完成后必须覆盖）**
- aggregate cutoff_ts：T0 写入水位线、T1 写入 trace（ts 在 T0 和 cutoff 之间）→ 下一轮 aggregate 必须捕获该 trace，不漏计
- outcome 冲突事务回滚：record() 传不同 outcome → 两张表（usage_trace + episodic_log）均无变化，OutcomeConflictError 正确抛出
- stale screening 超时恢复：手动写入 screening 行（distill_run_id='test-run-uuid'），将 distill_locked_at 设为 31 分钟前 → Curate purge_logs 将其改为 failed，distill_note='screening_timeout:test-run-uuid'（格式为 'screening_timeout:' || distill_run_id）
- embedding_pending rebuild 恢复：add() embedding 失败 → state_reason='embedding_pending:target=active' → evolve --rebuild-embeddings → state=active, state_reason='embedding_rebuilt'
- sanitize 覆盖：add/spark/promote_spark/distill 四路各传入明显密钥（sk-xxx）→ 验证 redact 路径正确落点
- v4.4→v4.5 迁移：在测试库执行迁移脚本 → event_source 正确迁移，旧 source 字段保留，schema_version=4.5
- selected/used 幂等：同一 (trace_id, chunk_id, event=selected) 写两次 → 第二次 INSERT OR IGNORE 静默忽略，selected_count aggregate 后 = 1
- 最低蒸馏条件：record() 只传 outcome=ok 无其他内容 → episodic_log.distill_state='discarded', distill_note='insufficient_material'

---

## 八、版本收口记录

### v4.5.1 编码前勘误 · 最终补丁(本版)

**触发原因**:外部 LLM 对 v4.5 做了编码前最后审查，识别出 4 个实现阶段容易踩的坑和 1 个文档升级点。本版按建议全部修复，主题：编码前勘误，不加任何新能力。**此版本后不再做架构层补丁，下一步进入实现。**

**外部建议裁决（5 条全部采纳）：**

1. **P0 aggregate cutoff_ts** — 完全采纳并在实现复核时校正边界。aggregate 开始时固定 `cutoff_ts`，所有 SQL 使用半开区间 `ts >= :last_ts AND ts < :cutoff_ts`，水位线写入 cutoff_ts 而非 `now()`。聚合、水位推进和 raw trace 清理放入同一 `BEGIN IMMEDIATE` 事务；边界上的 trace 留给下一轮，关闭毫秒精度漏计窗口。
2. **P0/P1 时间格式统一** — 采纳方案 A。全库所有 `datetime('now')` 替换为 `strftime('%Y-%m-%dT%H:%M:%fZ','now')`，确保 SQLite TEXT 字典序比较可靠。DDL meta 表注释处新增时间格式统一约定，明确禁止 `datetime('now')`（空格分隔格式）和本地时间。
3. **P1 distill_run_id/locked_at 补索引** — 完全采纳。新增两条索引：`idx_log_distill_run`（按 run_id 取回自己的 claim 批次）和 `idx_log_screening_locked`（partial index，仅 screening 状态，加速 Curate 超时检测查询）。
4. **P1 selected 幂等索引** — 完全采纳。新增 `idx_trace_chunk_selected_once`（`trace_id, chunk_id, event WHERE event='selected'`）。理由：selected 参与 Curate `repeated_selected_unused` 归档规则，重复写入可能错误触发归档；与 used 索引对称，逻辑一致。
5. **P2 测试策略升级为实现前 checklist** — 完全采纳。§七"测试策略"改为"实现前必通 checklist"，保留四类测试框架，新增 8 条 v4.4/v4.5 必验路径（含 aggregate cutoff_ts 不漏计、outcome 冲突事务回滚、stale screening 恢复、embedding rebuild 恢复、sanitize 四路、迁移验证、selected 幂等、最低蒸馏条件），作为开工验收门禁。

> **v4.0→v4.5.1 演化总结**:从系统分层定义（v4.0）到状态机完整收口（v4.3）到写入安全与并发控制（v4.4）到编码前硬化（v4.5）到最终勘误（v4.5.1）——核心架构一直未变，只是把每一轮会出 bug 的地方逐一补完。现在的文档已经可以直接作为实现参考。

---

### v4.5 编码前硬化补丁

**触发原因**:外部 LLM 对 v4.4 做了生产就绪度审查，识别出编码阶段最容易踩的 6 个坑（screening worker 崩溃卡死、distill_note 字段复用风险、rebuild 后状态恢复不明确、outcome 双表不一致、state_reason 无规范、迁移脚本用 RENAME 违反 ADD 原则），以及 2 个文档准确性问题（验证声明过强、event_source 迁移方案有风险）。本版全部修复，主题：工程硬化，不加新能力。

**外部建议裁决：**

**✅ P0 全部采纳（2 条，最关键）**:

1. **P0-1/P0-2 stale screening + distill_note 复用** — 采纳建议 B（加字段）。`episodic_log` 新增 `distill_run_id TEXT`（worker UUID）和 `distill_locked_at TEXT`（claim 时间戳），claim SQL 改写这两个专字段；`distill_note` 归还给终态原因专用（distilled/discarded/failed 写入时才更新）。Curate `purge_logs` 步骤新增 stale screening 检测：`distill_locked_at < now - 30min` 的行改 `failed`，`distill_note='screening_timeout:<run_id>'`，run_id/locked_at 字段清空。`inspect()` 新增第5个健康信号：`stale_screening_count`。

**✅ P1 全部采纳（4 条）**:

2. **P1-3 embedding rebuild 后 state 恢复** — 采纳 state_reason 编码方案（不加新字段）。写入时 embedding 失败改为：`state_reason='embedding_pending:target=<intended_state>'`（如 `embedding_pending:target=active`）。rebuild 成功后解析 target，恢复到 intended_state，`state_reason` 改为 `'embedding_rebuilt'`。
3. **P1-4 record() 双表 outcome 保护 + 事务** — 完全采纳。整个 record() 在 `BEGIN IMMEDIATE` 内执行；outcome 冲突检查顺序：先读 episodic_log.outcome，相同则幂等忽略，不同则回滚抛 `OutcomeConflictError`（两张表均不更新）。任一步骤失败全部回滚，消除半失败状态。
4. **P1-5 原因字段枚举** — 采纳，与 confidence_reason 统一 `reason_code:detail` 风格。实现复核时进一步拆清：chunk 生命周期原因写 `state_reason`；蒸馏日志终态原因写 `episodic_log.distill_note`，不混用字段。
5. **P2-7 event_source 迁移改 ADD + 弃用** — 完全采纳。v4.4→v4.5 迁移脚本改为 `ALTER TABLE ADD COLUMN event_source` + `UPDATE SET event_source = CASE ... END`，旧 `source` 字段保留标注 DEPRECATED（与 chunks.last_agg_ts 处理方式一致）。同步补完 v4.3→v4.4 迁移脚本（缺失的迁移段）。

**✅ P2 全部采纳（2 条）**:

6. **P2-6 沙箱验证声明降级** — 采纳。§四 标题、§附 标题当时将 v4.4/v4.5 新增路径列为后续回归项；这些路径现已全部加入回归覆盖。

**不采纳**：建议中未列出需拒绝的内容；所有 6+2 条均不涉及平台化扩展，全部属于工程硬化。

> **总结**:v4.5 是 Innate 的编码前最终硬化补丁。从 v4.0 的系统分层定义到 v4.5 的并发锁字段和 state_reason 枚举，核心架构始终未变，只是在每一轮把实现时真正会出问题的地方逐一收口。现在的文档已经可以直接作为实现参考，不需要再做大的补丁。

---

### v4.4 写入安全、并发锁定与语义一致性收口

**触发原因**:外部 LLM 对 v4.3 做了写入安全和并发控制层面的深度审查，发现 3 个 P0 实现陷阱（outcome 可同时为 ok 和 fail、screening 无原子领取导致并发抢批、open→new 无质量门禁产生低质蒸馏候选）和 5 个 P1/P2 语义一致性缺口。本版逐条裁决修复，不改核心架构。

**外部建议裁决：**

**✅ P0 全部采纳（3 条）**:

1. **P0-1 task_ok/task_fail 互斥** — 完全采纳，方案调整。建议两条分事件索引改为一条 outcome-level 索引：`UNIQUE INDEX idx_trace_outcome_once ON usage_trace(trace_id) WHERE event IN ('task_ok','task_fail') AND chunk_id IS NULL`。语义：同一 trace 只能有一个最终结果。record() 遇重复：同 outcome → INSERT OR IGNORE 幂等忽略；不同 outcome → 应用层抛 OutcomeConflictError 拒绝写入。迁移脚本去重逻辑同步更新（按 trace 分组保留最早一条 outcome）。
2. **P0-2 screening 原子 claim** — 完全采纳。补 `BEGIN IMMEDIATE` + `distill_run_id` 的原子领取模式：事务内 UPDATE 标记 screening 并在 distill_note 写入 run_id，再 SELECT WHERE distill_note = 'run_id:...' 取自己的批次。不使用 SQLite RETURNING（兼容性更广），不新增字段（复用 distill_note）。
3. **P0-3 最低可蒸馏条件** — 完全采纳。record() 补完 outcome 后，判断是否满足任一条件（output_summary 非空 / nomination 非空 / used 非空且 outcome≠unknown / output 非空）；不满足则 `distill_state='discarded'`，`distill_note='insufficient_material'`。只有 outcome 没有上下文的 trace（如 Daemon 只捕获 ok/fail 无摘要）不进入蒸馏队列。

**✅ P1/P2 全部采纳（5 条，1 条部分）**:

4. **P1-4 episodic_log.source 语义拆分** — 部分采纳。`source` 改名 `event_source`（接入层来源，枚举与 usage_trace.source 对齐：sdk|cli|hook|daemon|augmented，NOT NULL DEFAULT 'sdk'）。建议的 `distill_source` 字段**拒绝**：蒸馏入口已由 `evolve(trigger=...)` 参数表达，episodic_log 是蒸馏原料而非蒸馏结果，在原料表里记录蒸馏原因是错误的语义位置。
5. **P1-5 sanitize 覆盖所有写入路径** — 采纳。add/spark/promote_spark/distill 四路统一过 sanitize，各路 redact/discard 落点明确（表格形式）。promote_spark 单独 sanitize 的原因：从灵感变正式知识时内容可能已被编辑，是第二次写入机会。
6. **P1-6 写入时 embedding 失败策略** — 采纳。`add()`/`spark()` 失败时仍写 chunk，`embed_version=0` 标记待补，不丢人工输入；`distill()` 失败时不写 chunk（蒸馏结果强依赖向量）。`evolve --rebuild-embeddings` 同时处理 `embed_version=0` 和版本落后两类情况。
7. **P1-7 chunks.last_agg_ts 弃用** — 采纳。DDL 加 `DEPRECATED` 注释，聚合水位线统一使用 `meta.last_agg_ts`，实现不应读写此字段。保留字段避免迁移破坏。
8. **P2-9 confidence_reason 枚举** — 采纳轻量版。定义推荐格式 `reason_code:detail`，10 个 reason_code 枚举（user_up/user_down/judge_score/agent_used/selected_unused/task_fail/decay/restore/manual_set/init），字段仍存字符串，不加新表。inspect() 可按 reason_code 前缀统计，调试友好。

> **总结**:v4.4 的 9 条改动覆盖了写入安全（outcome 互斥、sanitize 全路径）、并发控制（screening 原子 claim）、数据质量门禁（最低可蒸馏条件）和文档语义一致性（aggregate 两路分述、event_source 命名、last_agg_ts 弃用、confidence_reason 枚举）。改动均为增量，无核心架构调整。Schema 版本升至 4.4。

---

### v4.3 状态机与幂等性最终收口

**触发原因**:外部 LLM 对 v4.2 做了 SQLite/状态机层面的深度审查，发现 5 个 P0 实现陷阱（其中两个会在真实库上直接出错：CREATE UNIQUE INDEX 遇历史重复行失败、evolve 蒸馏半成品 open 日志）和 1 个 P1 API 清晰度缺口。本版逐条裁决并修复，不改核心架构。

**外部建议裁决：**

**✅ P0 全部采纳（5 条）**:

1. **P0-1 open→new 状态机防竞态** — 完全采纳。`distill_state` 新增三态：`open`（recall 预写，不可蒸馏）/ `screening`（distill 进行中，防并发重入）/ `failed`（模型失败终态，需人工重置为 new 才可重试）。recall() 写 `open`，record() 补完 outcome 后改 `new`，evolve() 只读 `new`。purge_logs 加 TTL 兜底：open 行超 7 天无 record 改 `discarded(no_record_timeout)`。
2. **P0-2 trace_id UNIQUE INDEX** — 采纳加唯一索引（不改 PRIMARY KEY，保留 `id` 供 `distilled_from` 外键引用）。`idx_log_trace` 由普通索引升 UNIQUE INDEX，保证 `record() UPDATE WHERE trace_id=?` 只命中一行。
3. **P0-3 幂等索引漏洞修复** — 完全采纳。`source` 字段改 `NOT NULL DEFAULT 'sdk'`（防 NULL != NULL 漏洞）；幂等索引拆两条：chunk 级（`trace_id, chunk_id, event` WHERE event='used'）和 trace 级（`trace_id, event` WHERE event IN ('task_ok','task_fail') AND chunk_id IS NULL）；`source` 不参与幂等约束维度（审计用，不做去重键）。
4. **P0-4 迁移脚本先去重再建索引** — 完全采纳。补 v4.2→v4.3 完整迁移脚本：① 先处理 episodic_log.trace_id 重复行；② 处理 distill_state='open' 的识别逻辑；③ source NULL 改 'sdk'；④ **DELETE 重复 usage_trace 行后再 CREATE UNIQUE INDEX**（这条是必须的，SQLite 遇历史重复行会直接报错）；⑤ 补充"迁移铁律"说明。
5. **P0-5 used_success_count 统一从事实表派生** — 完全采纳。废弃增量加 raw trace 的方式，两个计数字段（`used_success_count`、`success_trace_ids_count`）统一从 `chunk_success_traces` 事实表派生，语义现在完全一致（"有多少不同成功 trace 用过这个 chunk"）。保留两个字段仅为晋升规则可读性。额外补充了 `selected_count`/`used_count` 的增量 SQL（原文档此处缺失）。

**✅ P1 采纳（1 条）**:

6. **P1-6 record() 加 query 参数** — 采纳。`kb.record(query=None, ...)` 参数在无 recall 预写行时（纯 Hook/Daemon 接入场景）使用；有预写行时被忽略。写入逻辑明确为：先查 episodic_log WHERE trace_id=?，有则 UPDATE，无则 INSERT（带 query 字段）。参数映射表同步更新，`outcome` 一行加注"补完 outcome 时触发 open→new 转换"。

> **总结**:v4.3 是幂等性和状态机的最终收口。这 6 处改动如不修复，会在真实实现中产生：竞态蒸馏半成品、重复写入虚增统计、迁移脚本炸库、两个计数字段来源不一致导致晋升规则不可信等具体 bug。改动均为增量，无任何核心架构调整。Schema 版本升至 4.3。

---

### v4.2 闭环正确性补丁

**触发原因**:外部 LLM 对 v4.1 做了系统性 P0/P1 审查，发现 8 个影响实现正确性的闭环缺口，非"风格优化"而是实现会出 bug 的问题。本版逐条裁决并修复。

**外部建议裁决：**

**✅ P0 全部采纳（4 条，影响实现正确性）**:

1. **P0-1 record/output 断裂** — 采纳，方案调整。建议引入 `trace_sessions` 新表；实际解法：`recall()` 预写 `episodic_log`（query + recall_snapshot），`record()` UPDATE 同一行补 `output`/`output_summary`/`outcome`。无需新表，单库事务保证原子性，Distill 读到的每条 episodic_log 都是完整蒸馏单元。`record()` 签名加 `output`/`output_summary` 参数，CLI 加 `--output-summary` 选项。
2. **P0-2 success_trace_ids_count purge 后丢数** — 完全采纳。引入 `chunk_success_traces`(chunk_id, trace_id) 事实表，aggregate 时幂等写入；`success_trace_ids_count` 直接 COUNT 事实表行数，不依赖 usage_trace 原始明细，purge 后永远安全。同步修复 COALESCE 兜底首次 aggregate NULL 问题（两个真实 bug，均已修复）。
3. **P0-3 record 幂等性** — 部分采纳。加 `usage_trace` 的 UNIQUE INDEX（`trace_id, chunk_id, event, source`，仅约束 used/task_ok/task_fail）作为 DB 层最小防护，防止重复 record 污染计数。`processed_events` 表由 Daemon 自管（在 daemon_state.sqlite），不强制进知识库——既够用又不过度规范化。
4. **P0-4 hard 闭包截断改丢弃** — 完全采纳。"截断 hard 依赖闭包后送半截 block 进 context"风险高于不召回。改为：有界遍历触达深度上限 → 丢弃 seed，写 `event='retrieved', refine_mode='skipped:dep_depth_limit'` 到 usage_trace。"宁可不召回，不给半截依赖"写进安全原则（§六）和故障矩阵（§六·五）。

**✅ P1 全部采纳（4 条，实现清晰度/完整性）**:

5. **P1-5 Daemon 持久状态落地** — 采纳。明确 Daemon 状态不进知识库，存 `~/.innate/daemon_state.sqlite`（可通过 `--state-db` 覆盖）。给出完整 DDL：`watch_state`（记偏移量 + inode）+ `processed_events`（event_id 去重）。多 watch 目录独立记录，不串扰。
6. **P1-6 Hook Event 加 output_summary** — 采纳。Hook 通常拿不到完整 output，但摘要足够蒸馏。Hook Event JSON Schema 加 `output_summary` 字段；框架原生 Hook YAML 示例同步更新，传 `{summary}` / `{error_context}` 占位符。
7. **P1-7 --format prompt 的 trace_id 交接** — 采纳。明确三格式语义：`--format json` = 机器集成唯一推荐（含完整 trace_id/selected/chunks JSON）；`--format prompt` = System Prompt 注入，末尾附 `<!-- innate_trace_id: xxx -->` 隐藏注释块供解析；`--format text` = 纯人读，不承担 trace 交接。
8. **P1-8 innate.skill.md 用了未定义的 --top** — 采纳。CLI 定义补 `--top N` 参数（最多返回 N 个块，默认不限，与 --budget 共同约束）；innate.skill.md 示例同步更新，补充 `--format json` 和 `--format prompt` 两种用法说明。

> **总结**:v4.2 修复了 v4.1 的 8 个闭环缺口，其中 4 个 P0 级若不修复会在实现中直接出 bug（output 拿不到无法蒸馏、distinct-trace 计数 purge 后归零、重复 record 污染统计、半截依赖进 context）。改动均为增量，无核心架构调整。

---

### v4.1 系统工程收口

**触发原因**:v4.0 设计内容已经覆盖 Core SDK + CLI + Hook + Daemon + Governance 完整系统,但文档结构仍以"SDK"视角组织,导致几个结构性认知冲突:① 说"SDK 零主动行为"但又有 Daemon;② 说"8 个 API"但 CLI 有十几条子命令;③ 晋升三护栏依赖 `used_success_count` / `distinct_success_traces` 但 Schema DDL 里缺失对应字段。本版是工程层面的收口,不改核心架构。

**外部 LLM 建议评估(本轮)**:收到一份较为系统的重构建议。裁决如下:

**采纳(7 条)**:
1. **补 Core/Adapter/Runtime 分层说明** — 新增 §零·五,一节文字解决"SDK 说零主动却有 Daemon"的认知冲突,无架构改动。
2. **record() 参数职责分层明确** — 明确 recall() 写 retrieved/selected，record() 补 output/output_summary/outcome/used/feedback/nomination；trace_id 是跨层关联键。见 §五 record 参数说明表。
3. **Schema 补统计字段闭合晋升规则** — chunks 加 `used_success_count` / `success_trace_ids_count` / `last_success_at` 三个物化字段,DDL 与晋升三护栏逻辑对齐。
4. **usage_trace 加 source 字段** — 追溯每条 trace 来自 sdk/cli/hook/daemon/augmented,零成本审计。
5. **Curator 替换协议补完整接口** — 补 Python dataclass 接口(CurateScope + CurateReport),"整体替换"不再是口号。见 §二·六。
6. **补 embedding 版本化与迁移策略** — 新增 §四·五:schema_version 迁移规范、embed_version 重建规则、重建期间 Recall 行为。
7. **spark/confidence 隔离写成显式硬规则** — 在 §二·七 加一条"spark 的 confidence 字段存在但语义为 NULL/无效",任何读 confidence 的逻辑必须先过滤 origin='spark'。

**本轮复查补正(2 条,内部审查发现)**:
A. **修正 aggregate SQL 执行顺序** — `chunk_success_traces` 事实表写入与计数器更新必须在 raw usage_trace 清理前执行；聚合、水位推进和清理现处于同一事务。见 §四 aggregate 阶段。
B. **补故障矩阵两条** — §六·五 增加"Hook 重复触发"(event_id 幂等去重策略)和"Distill 模型失败"的完整降级路径(`distill_note` 记因 + aggregate 跳过未完成条目)。反馈建议明确提出了 hook_duplicate / distill_failure 两种故障模式,均为真实缺口。

**部分采纳(2 条)**:
8. **"8个API"改"8类能力域"** — 实现校准后统一使用"8 类 Public API 能力域"表述；CLI 子命令是同一能力域的命令行映射,不新增知识逻辑。
9. **Hook 升级为事件协议** — 在 §九 补 Hook Event JSON Schema(规范 event_id/trace_id/event_type/payload 字段),但不升为独立协议文档——篇幅克制,够用即止。

**拒绝(3 条)**:
10. **强行重排为12节结构** — 拒。现有章节叙述逻辑完整(从定位→算法→Schema→API→接入形态),强行重排破坏已有叙述流,收益低风险高。v4.1 在现有结构上增量插入 §零·五 和 §四·五 即可。
11. **测试与故障矩阵单独成节** — 部分降级。测试策略并入 §七(验收 checklist),故障降级已在 §六·五,不单独成节——与"SDK 不是平台"一致。
12. **meta 加 schema_version/embed_version 等整体版本化** — 已采纳迁移策略的核心内容,但不把 CLI/SDK/Daemon 各版本兼容矩阵写成完整文档——过度规范化,由包管理(semantic versioning)约束即可。

> **总结**:v4.1 在保持"功能完整、边界克制"原则下,补齐了 v4.0 的工程收口缺口。改动均为增量,无任何核心架构调整。

### v4.0 厘清系统定位 + 外部建议评估

**触发原因**:「chunk」和「记忆(memory)」概念在实际使用中产生混淆。本版新增 §零 专门回答"定位"问题,无架构改动。

**外部建议评估**:采纳 procedural vs declarative 认知框架 + 三列横向对比表;拒绝 Trace→Spark→Instinct 实体重命名(与现有 usage_trace 术语冲突)、拒绝「本能进化引擎」命名(与零主动行为原则冲突)。

### v3.9 新增接入模式:SDK 嵌入 / CLI / Hook 三轨

核心认知:Innate 的知识层设计已完整,但接入形态只有 SDK 嵌入一条路——封闭系统和非 Python 项目无法接入。本版补齐三轨并行体系(§九)。

关键设计取舍:`--source agent` 强制 pending;Tool Error Hook 改写弱信号而非自动强降分;Daemon 是外部进程;接入 Skill 术语与 Innate 内部 skill 严格区分;Session End Hook 只触发 evolve。

### v3.8 吸收外部反馈:补真实缺口,拒平台影子

收 3(装包价值密度回填/confidence 时效加权/知识债务比)、降级 1(灵感隐式孵化→inspect 提示)、拒 4(FTS5/冷热分离/OnnxTinyML/parent_id改JSON)。

### v3.7 补"灵感记录"(方案A)
### v3.6 补"错误信息快速作废"通道(invalidate)
### v3.5 补"人工知识捕获"入口(captured)
### v3.4 编码实现约束收口
### v3.3 剥离"平台影子"
### v3.2 架构边界收敛
### v3.1 工程一致性收口
### v3.0 瘦身记录

---

## 九、接入模式:SDK 嵌入 / CLI / Hook + Daemon 三轨(v3.9 / v4.1 扩充)

SDK 是核心;CLI 是 SDK 的薄壳;Hook/Daemon 是 CLI 的外部触发器——三层各司其职,互不污染。
SDK 的"零主动行为"在任何接入形态下都不变:Daemon 是外部进程,SDK 本体永不起后台线程、永不自发行动。

### 三种接入形态

| 形态 | 适用场景 | 接入代价 | 典型环境 |
|---|---|---|---|
| **SDK 嵌入** | Python Agent 项目,可直接修改代码 | `from innate import KnowledgeBase` | LangGraph / Claude Code / 裸 API |
| **CLI 调用** | 任意语言、Shell 脚本、CI 流程 | 进程调用 + stdout/stderr 解析 | 任意 Agent / 工具链 |
| **Hook + Daemon** | 封闭系统,无法修改底层代码 | OS 层守护进程 + 日志旁路监听 | Cursor / 网页 SaaS Agent |

### CLI 接口(SDK Public API 的薄封装)

CLI **不新增任何知识层逻辑**;所有命令最终调 SDK 的 Public API,仅做参数解析和格式化输出。

```bash
# ── 读 ──────────────────────────────────────────────────────
innate recall "<query>" [--budget 6000] [--top N] [--include-sparks]
              [--format text|json|prompt] [--expand-deps false|direct|closure]
# --format json  : 机器集成唯一推荐格式;输出 JSON 含 trace_id / selected / chunks 列表
#                  后续 Hook/record 必须从此格式取 trace_id
# --format prompt: 供 Session Start 场景直接拼入 System Prompt;
#                  在 prompt 文本末尾附加隐藏元信息块,供 Agent 提取 trace_id:
#                    <!-- innate_trace_id: <uuid> -->
#                    <!-- innate_selected: c1,c2,c3 -->
#                  若 Agent/框架无法解析注释块,则 --format prompt 不承担 trace 交接,
#                  后续需改用 --format json 再调 innate record
# --format text  : 纯文本,仅供人阅读,不输出 trace_id(不用于机器集成)
# --top N        : 最多返回 N 个召回 seed(默认不限,由 --budget 控制截止);
#                  开启 hard deps 时依赖闭包不受 N 截断,闭包完整优先

# ── 写日志 ──────────────────────────────────────────────────
# trace_id 由上游 recall 产生(--format json 时输出);CLI 不需传 retrieved/selected
innate record <trace_id> [--outcome ok|fail|unknown]
              [--query "..."] [--output-summary "..."] [--nomination "..."]
              [--priority N]
              [--feedback up|down]
              [--used <chunk_id,chunk_id,...>] [--source cli|hook|daemon|augmented]

# ── 成长 ────────────────────────────────────────────────────
innate evolve [--trigger manual|scheduled|threshold]
innate evolve --rebuild-embeddings  # 触发陈旧向量重建(v4.1)

# ── 人工治理 ─────────────────────────────────────────────────
innate approve <chunk_id>
innate archive <chunk_id>    [--reason "..."]
innate invalidate <chunk_id> [--reason "..."]
innate restore <chunk_id>

# ── 写入知识 ─────────────────────────────────────────────────
innate add "<content>" [--kind note|skill]
           [--trigger "..."] [--anti-trigger "..."]
           [--skill-name "..."]
           [--source chat|manual|doc|agent]
# ⚠️ --source agent 强制 state=pending,不绕人工确认

innate spark "<content>"
innate mature-spark <spark_id>  --to sprouting|incubating
innate promote-spark <spark_id> [--to note|skill]
innate drop-spark <spark_id>    [--reason "..."]

# ── 调试 / 体检 ───────────────────────────────────────────────
innate inspect [<chunk_id>|<trace_id>]   # 无参数 = 库体检

# ── Daemon 控制 ───────────────────────────────────────────────
innate daemon start [--watch <log_dir>]... [--db <path>] [--pid-file <path>]
                    [--state-db <path>] [--log-file <path>]
innate daemon stop
innate daemon status [--state-db <path>]
```

**`--source agent` 强制 pending 的原因**:agent 自动提炼的内容未经人确认,和 distilled 一样有质量风险——不应直接污染 active 知识池。强制 pending 后,走 `innate approve` 或 Evolve 的简单晋升规则才能变 active。

**CLI 的失败处理约定**:退出码非 0 时写 stderr;调用方读 stderr 尝试修正一次;若仍失败则放弃,**绝不阻塞主任务**。Innate 是辅助层,知识调用失败不应成为 Agent 的硬依赖。

### Daemon 运行协议(v4.1 补完整)

Daemon 是**外部可选进程**,独立于 SDK core。不安装 Daemon 不影响 Core SDK + CLI 正常使用。Daemon 的知识层动作全部通过 CLI Public API 完成——它不直接操作知识库,不拥有知识逻辑。offset/inode/event_id 等旁路监听状态单独写入 Daemon 私有 SQLite。

**平台边界**:内置 Daemon 当前依赖 `os.fork` 和 `/proc`,基线仅支持 Linux。非 Linux 环境继续使用 Core SDK + CLI,或替换为平台原生 Runtime adapter。

**Daemon 的职责边界**:
- ✅ 监听日志目录 / Hook event 文件 / stdout 捕获
- ✅ 正则/规则匹配事件模式后调 CLI
- ✅ 写 usage_trace / episodic_log(via CLI record)
- ❌ 不直接调用 `approve` / `invalidate`(人工治理专属)
- ❌ 不直接读写知识库(只通过 CLI；Daemon 私有状态库除外)
- ❌ 不拥有 confidence/Curate 逻辑

**启动与配置**:
```bash
innate daemon start \
  --watch ~/.cursor/logs/ \    # 监听日志目录(可多个)
  --db ~/knowledge/personal.db \
  --pid-file /tmp/innate-daemon.pid
```

**监听事件 → 触发动作映射**:

| 日志模式 | 触发动作 | 接入层 / source 标记 |
|---|---|---|
| `Build successful` / `Tests passed` / `✓ [N] passed` | `innate record {trace_id} --outcome ok` | daemon |
| `SyntaxError` / `Error:` / 连续 N 次同类报错 | `innate record {trace_id} --outcome fail` | daemon |
| Session Start 信号(IDE 启动/新对话) | `innate recall "{project}" --format prompt` 结果注入 | 框架 Hook / 自定义 Daemon adapter |
| 会话关闭信号 / IDE 退出事件 | `innate evolve --trigger manual` | 框架 Hook / 自定义 Daemon adapter |

**持久状态存储**:Daemon 自身状态**不进入知识库**（不污染知识层数据），单独保存在 `~/.innate/daemon_state.sqlite`（可通过 `--state-db` 覆盖路径）。结构极简：

内置 Daemon 的 JSON watcher 会把 Session Start 的 prompt 写入 `daemon.log` 并保存
trace context；它无法直接修改外部 Agent 进程的上下文。真正的 prompt 注入由框架
Hook 或自定义 Runtime adapter 完成。

```sql
-- ~/.innate/daemon_state.sqlite  (Daemon 私有，不是知识库的一部分)
CREATE TABLE IF NOT EXISTS watch_state (
    watch_path            TEXT PRIMARY KEY,   -- 具体日志文件路径,不是目录
    last_processed_offset INTEGER NOT NULL DEFAULT 0,
    last_processed_inode  TEXT,
    updated_at            TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS processed_events (
    event_id   TEXT PRIMARY KEY,   -- 日志行 hash + 文件 inode + offset
    watch_path TEXT,
    trace_id   TEXT,
    event_type TEXT,
    ts         TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS trace_context (
    watch_path TEXT PRIMARY KEY,
    trace_id   TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

**幂等与去重**:每个捕获事件生成 `event_id`（日志行内容 hash + 文件 inode + offset），写入 `processed_events`（PRIMARY KEY 去重）。`watch_state.watch_path` 记录具体日志文件而非目录，因此同一目录下多个 `.log` 文件各自维护 offset/inode；文件截断或 inode 改变时从 0 继续。`trace_context` 保存每个监听文件当前会话的 trace，Session Start 设置、Session End 清除，使旁路日志的 ok/fail 能关联到同一 recall。重启后**不重放已处理行**。多 watch 目录、多 db 各自独立记录，不会串扰。

**失败策略**:CLI 调用失败时最多重试一次（指数退避），仍失败则记入 `daemon.log` + 更新 `watch_state.updated_at`，**不阻塞主 Agent 进程**。Daemon 自身崩溃不影响 SDK/CLI 正常工作，独立重启即可。

**日志轮转（Log Rotation）**:Daemon 长期运行时 `daemon.log` 可能过大。Daemon 不自己实现轮转，优先复用 OS 级工具：

```
# 推荐方案 A（Linux/macOS）：交给 logrotate
# /etc/logrotate.d/innate-daemon
~/.innate/daemon.log {
    daily
    rotate 7
    compress
    missingok
    notifempty
    copytruncate     # 不需要 Daemon 重启，直接截断原文件
}

# 推荐方案 B（Python 标准库，Daemon 内部使用 RotatingFileHandler）
import logging.handlers
handler = logging.handlers.RotatingFileHandler(
    '~/.innate/daemon.log',
    maxBytes=10 * 1024 * 1024,   # 10 MB
    backupCount=5,
)
```

实现选择：出生版本内置方案 B，使用 Python 标准库 `RotatingFileHandler` 保证跨环境可用；部署环境可额外使用方案 A。两种方案均不侵入 SDK Core。`--log-file` 参数覆盖默认路径。

**状态检查**:
```bash
innate daemon status
# 输出:状态(running/stopped) / PID / 已监听日志文件 / 已处理事件数 / 最后处理时间 / 错误数
```

### Hook 事件协议(v4.1 补)

Hook 从外部系统调用 CLI 时,统一的事件载体格式(用于框架原生 Hook 的 context 注入和 Daemon 的事件记录):

```json
{
  "event_id":      "uuid-唯一事件id-幂等去重用",
  "event_type":    "session_start|tool_success|tool_error|session_end|user_feedback",
  "trace_id":      "uuid-贯穿recall和record的关联键",
  "query":         "当前任务/请求描述",
  "output_summary": "Agent输出摘要(可选;蒸馏主原料;完整output太大时传此字段)",
  "outcome":       "ok|fail|unknown",
  "used":          ["chunk_id_1", "chunk_id_2"],
  "feedback":      "up|down",
  "nomination":    "为何值得进入蒸馏队列(可选)",
  "priority":      7,
  "metadata":      {}
}
```

`event_id` 用于幂等去重（Hook 被重复触发时 Daemon 过滤已处理 event_id）。`trace_id` 是跨 recall/record/Hook/Daemon 的统一关联键——recall 产生，后续所有事件携带。`output_summary` 是 Hook 接入场景 Distill 的主原料——Hook 通常拿不到完整 output，但一段摘要足够让蒸馏模型提炼可复用知识。

**框架原生 Hook 实现**:
```yaml
# agent_config.yaml
hooks:
  on_session_start:
    - command: "innate recall {project_name} --format prompt --budget 4000"
  on_task_success:
    - command: "innate record {trace_id} --outcome ok --output-summary '{summary}'"
  on_task_failure:
    - command: "innate record {trace_id} --outcome fail --output-summary '{error_context}'"
  on_session_end:
    - command: "innate evolve --trigger manual"
```

**Daemon 旁路监听**(封闭系统,形态 B):
```bash
innate daemon start --watch ~/.cursor/logs/ --db ~/knowledge/personal.db
```

### 接入 Skill:`innate.skill.md`(Agent 的认知蓝图)

> **术语区分**:Innate 内部把知识块的一种组织形态称为"skill"(origin=installed,按 skill_name 分组)。  
> 这里的**接入 Skill** 是面向 Agent 框架(如 Claude Code)的 **Skill 配置文件**——告诉 Agent 何时、如何调用 Innate 的 CLI。二者层次不同,不要混淆。

`innate.skill.md` 是 **Agent 的行为护栏**,分三层:

**① 元数据层 — 精准触发**
```yaml
name: innate-memory
version: 4.5.1
description: >
  【读取触发】执行复杂任务前/排查历史 Bug/参考过往模式/避免重复踩坑时激活。
  【写入触发】用户要求"记录灵感"/"保存思路"/"以后就按这个来"/"记住这个教训",
  或成功解决复杂问题后需要提炼经验时,立即提取核心信息执行写入。
  即使未明确提及"记忆",只要涉及历史经验复用或新知识沉淀,均应激活。
```

**② 核心逻辑层 — 工作流 + 安全围栏**
```markdown
## 核心工作流

### 任务前召回
# 机器集成(取 trace_id 用于后续 record):
innate recall "<任务核心意图>" --top 5 --format json
# 从 JSON 输出取 trace_id,召回结果注入 context
# 若 Agent 框架支持 prompt 注入:
innate recall "<任务核心意图>" --top 5 --format prompt
# prompt 格式末尾有 <!-- innate_trace_id: xxx --> 可解析 trace_id
召回结果作为约束纳入当前计划;高置信块优先,低置信块参考不强制。

### 灵感结构化记录(禁止原话存储)
碎片化灵感提炼为"核心观点 + 适用场景 + 待验证假设",再:
innate spark "<提炼后内容>"

### 经验沉淀(复杂问题成功解决后)
提炼可复用的代码模式或排查步骤:
innate add "<经验>" --kind note --source agent
(默认 pending,等人工 approve 或 Evolve 晋升规则确认)

## 🚫 安全围栏

- 禁止自行执行 innate approve / archive / invalidate / restore / mature-spark / promote-spark / drop-spark
  (人工治理专属；仅在人明确要求该动作时执行)
- innate add --source agent 只写 pending,不得绕过审核
- --feedback up|down 仅在人明确给出反馈时传入,不得从任务成败自行推断强反馈
- CLI 返回 exit_code != 0:读 stderr 修正一次,仍失败则放弃,绝不阻塞主任务
- 禁止在未经测试验证的情况下将 Agent 总结的经验标记为高置信度
```

**③ 写入防漏层 — 反思检查(防"说了就忘")**
```markdown
## 写入防漏自检
在结束长对话或回答"收到/好的"之前,内心评估:
"刚才是否产生了新的代码模式、避坑指南或业务灵感?"
若是,主动提议:"我已提炼了一条 Spark/经验,是否需要存入 Innate?"
——提议给人确认,不默默写入。人说 yes 才调 innate spark / innate add。
```
反思检查的落点是**提议,不是自动行动**——完全符合"零主动行为"。

### 边界(守克制)
- **CLI 是薄壳**:不新增知识层逻辑,最终全部调 SDK Public API；CLI 暴露面是常用 shell 参数子集。
- **Daemon 是外部进程**:SDK 不内置守护进程,不起后台线程。Daemon 位于独立可选模块,可按需拆包,不进 SDK core。
- **agent 来源强制 pending**:CLI `--source agent` 写入强制 state=pending,不绕人工确认;与 SDK 层"默认不自动污染"一致。
- **Hook 不替代显式 feedback**:Hook 只写弱信号(`outcome ok/fail`,strength ≤ 0.3);👍/👎 显式 feedback 仍需人发起——那才是拉动 confidence 最有力的信号。
- **接入 Skill ≠ Innate 内部的 skill**:前者是 Agent 框架的调用规则文件,后者是 Innate 存储的知识块(origin=installed)。两个层次,不混用术语。

---

## 附:已验证事实清单(沙箱实测；实现校准后更新)
- ✅ sqlite-vec 安装可用(v 系列),Python 绑定正常
- ✅ 跨库 KNN:出生版只读挂载共享库、逐库 ANN、SDK 合并统一排序已通过；ATTACH + UNION 作为升级选项已验证语法
- ✅ 双向量延迟实测:1万/3万/5万 chunk × 1024维(数据见上表)
- ✅ hard 依赖有界图遍历 + 深度护栏防环 + 环检测 + 孤岛检测全通过
- ✅ 完整 schema 建库无语法错;端到端(插chunk→双向量→依赖→Recall→Observe)跑通
- ✅ **Recall 装包算法**:双向量融合 + 依赖处理 + first-fit装包,断言不超预算、闭包完整
- ✅ **confidence 生命周期**:EMA 更新 1000 次随机反馈始终有界 [0,1];时间衰减收敛中性下限;Curate 判定逻辑(含 protected 豁免)正确
- ✅ **v3 单分数 + strength 调节**:2000 次随机更新有界;effective_α=α·strength 区分强弱信号
- ✅ **v3 简单晋升规则**:used_success≥3 转 active、selected≥10/used=0/conf<0.5 归档(三护栏),可判定
- ✅ **v3.1 trace-level 归因**:task_ok/fail 按 used/selected/retrieved 分层更新,不平均奖励每块(避免过度奖励)
- ✅ **v3.1 晋升三护栏**:used_success≥3 + distinct_traces≥2 + conf≥0.65,拦住"同trace刷次数"与"低信心蒙混"两类假阳性
- ✅ **v3.1 归档用 selected**:进过context才计数 + conf<0.5护栏,不误伤"常被检索但没进context"的块
- ✅ **v3 Schema**:chunks 新增 anti_trigger_desc/state_reason/confidence_reason/token_count/parent_id、usage_trace 拆 retrieved/selected 加 strength/tokens/rank,建库无误
- ✅ **v3.8 机制**:装包回填/confidence 时效加权/债务比/灵感提示均有回归覆盖;spark 唤起事实跨 Curate purge 保留
- ✅ **v4.1 新增字段**:used_success_count/success_trace_ids_count/last_success_at/usage_trace.source 已完成建库验证 + aggregate 回归
- ✅ **Curator 替换接口**:CurateScope(origin/skill_name/dry_run) 已落实;dry_run 保证只读;依赖启用时输出 cycle/orphan
- ✅ **2026-06-01 实现校准补充**:动态 vec0 维度注入、连续迁移链验证、spark Curate 全豁免、protected 去重优先级、hard deps 三跳边界、trim 闭包校验、CLI UUID trace inspect、Daemon 每文件 offset/inode 去重与失败重试
- ✅ **2026-06-01 完整性复核补充**:chunk+双向量原子写入、向量重建失败保留旧值、共享库只读且不隐式创建、重复 trace 迁移安全去重、hard dep 不可用 fail-closed、跨库 soft dep 解析、trim/adapt protected 防改写、spark maturity 人工前向推进、VectorStore 工厂注入、Hook JSON 会话 trace 贯穿与结束清理、CLI JSON `selected/chunks` 合同
- ✅ **2026-06-01 严格复核补充**:record 两阶段补全保持 open、nomination 默认高优先级且 CLI/Hook 可覆盖、aggregate 水位推进与 raw trace 清理原子提交、人工 archive 与 Curate protected 豁免分层、inspect 展示配置化 screening timeout
- ✅ **2026-06-01 边界复核补充**:hard dep 严格所属库内闭包、aggregate 改半开窗口避免同毫秒漏计、延迟补录 used 关联持久化 outcome、v4.5.1 时间迁移使用正确 GLOB 通配符、`recall(top=0)` empty 标志按可见结果计算
- ✅ **2026-06-02 再次严格复核补充**:`confidence=0` 不再误回退默认值、trim 对已入包共享 hard dep 只计算增量成本、v4.5.1 时间迁移覆盖保留时间列并兼容缺少黑名单表的旧库、默认 sanitize 组合输入按 injection 优先拒绝且全量脱敏
- ✅ **2026-06-02 独立复核补充**:invalidate 后 restore 同步撤销 hash 黑名单并修复历史不一致、spark maturity 逐级推进、protected 块完全豁免 Curate decay、recovery 使用同轮固定 UTC 边界、迁移 step 失败原子回滚、cycle 检测改显式栈支持深依赖链、Skill 禁止从 outcome 自行推断强 feedback
- ✅ **v4.4/v4.5 新增路径**:
  - outcome 互斥索引（idx_trace_outcome_once）行为
  - screening 原子 claim（BEGIN IMMEDIATE + distill_run_id/distill_locked_at）
  - stale screening 超时恢复（Curate purge_logs 超时检测 SQL）
  - embedding_pending:target=<state> 编码与 rebuild 后恢复逻辑
  - record() BEGIN IMMEDIATE 事务中 outcome 冲突检查 + 双表一致性
  - event_source 迁移脚本（ADD COLUMN + UPDATE + 旧列弃用）
  - 全路径 sanitize（add/spark/promote_spark/distill 四路）
  - 最低可蒸馏条件（open→discarded insufficient_material 分支）
