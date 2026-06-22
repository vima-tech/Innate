# Innate 直觉模块偏差治理与优化实施文档

> 目标读者:Innate SDK 维护者
> 范围:`appraise()` 共振评估路径(直觉/critic),不含 `recall()` 图书管理员路径
> 版本基线:Cargo v0.1.11 / DB schema 4.14
> 设计约束:零主动行为、同步路径无 LLM、librarian 不做 editor、最小实现优先

---

## 0. 为什么直觉模块比 recall 更容易偏

两条路径用的是同一套向量相似度,但代价形态完全不同,这是全文的出发点:

- **recall(图书管理员)**:召回错了,actor 多读一段无关 skill,代价是 token 噪音,**可见、可丢弃**。
- **appraise(直觉/critic)**:共振错了,Verdict 给出一个**带依据感的方向性判断**。它不产出答案内容,所以错误不会直接污染答案——但会以更隐蔽的方式伤人:**让 actor 信任一个不该信的警告,或在该警告时沉默**。

直觉模块的偏差有三个独立来源,**必须分开治理**,混在一起谈就会写出一堆没用的通用建议:

| 偏差来源 | 本质 | 嵌入能不能修 |
|---|---|---|
| 假共振 (false resonance) | rich 嵌入相近但语境不同 | 修不动,只能检测+折损 |
| 校准自欺 (calibration self-deception) | 证据稀疏时给出有依据感的错误置信度 | 与嵌入无关,这是直觉模块**自己新增**的风险 |
| 自我强化 (self-reinforcement) | verdict 影响存储,存储反喂 verdict,直觉学习自己的过去错误 | 与嵌入无关,架构问题 |

**结论先行**:嵌入地板(similar ≠ relevant)是物理事实,不要把优化预算砸在「让共振更准」上,ROI 极低。真正的高杠杆动作全在后两类——它们恰恰是 RAG 没有、而 Innate 因为多了一层校准才引入的新风险。

---

## 1. 设计原则(不可违背的不变量)

这五条是后面所有方案的约束。任何改动违反其一,即使指标变好也驳回。

1. **弃权是一等公民**。直觉的第一能力是「说不知道」,不是「给方向」。一个干净弃权的 critic,永远优于一个自信误报的 critic。
2. **没度量就没校准**。无法测量的偏差不存在被修复的可能。每条 verdict 必须可被后续观测结果证伪。
3. **直觉永不自我确认**。进入证据账本、参与校准的,只能是**实际观测到的结果**,绝不能是**过去 verdict 推导出的结论**。(这是你 playbook 里「critic 独立调用避免自我确认偏差」在嵌入域的延伸。)
4. **稀疏证据回归基率**。证据不足时,置信度向**全局基率**回归,而不是向 0.5、也不是向少数样本回归。
5. **同步路径无 LLM**。校准、弃权、共振都是确定性算术,延迟可预测。不引入 LLM 做语义判断进 `appraise` 同步路径。

---

## 2. 方案总览(按杠杆排序)

| 编号 | 改进 | 治理的偏差 | 改动面 | 优先级 |
|---|---|---|---|---|
| A | Verdict 增加 Abstain 变体 + 四道弃权门 | 假共振 / 校准自欺 / 阈值 | 类型 + appraise | **P0** |
| B | verdict_log + 观测结果回填 → 可算 ECE | 度量缺失(全部) | schema + 写路径 | **P0** |
| C | confidence_evidence 加 provenance 标记 | 自我强化 | schema + 写路径 | **P0** |
| D | 基率锚定先验进 context_score_from_counts | 校准自欺 | 算法 | P1 |
| E | 学习到的校准映射,emit 时应用 | 校准自欺 | 算法 + schema | P1 |
| F | 双通道一致性门(rich vs signature) | 假共振 | appraise | P1 |
| G | 邻居离散度 → 置信度塑形 | 假共振 / 模糊性 | appraise | P1 |
| H | 负证据/反例闭环(和 actor 协作) | 系统性过度告警 | 跨系统 | P2 |
| I | 误差驱动的分区衰减 | 分布漂移 | curate | P2 |

**P0 是脊柱**:没有 A/B/C,直觉模块就是不可证伪的玄学,后面所有精修都无从谈起。先做这三个,把模块从「凭感觉」变成「可测量」,再谈 P1 的校准精修。

---

## 3. P0 详细方案

### 3.1 方案 A —— 弃权作为一等输出

当前 Verdict 给 valence + 风险点,强制表态。直觉「更容易偏」的最大根治,就是允许它干净地不表态。

```rust
pub enum Verdict {
    /// 有足够基础给出方向性判断
    Directional {
        valence: f32,           // [-1, 1]
        confidence: f32,        // 已过校准映射(方案 E)
        risks: Vec<RiskFlag>,
    },
    /// 没有基础,明确弃权 —— 这不是失败,是正确行为
    Abstain { reason: AbstainReason },
}

pub enum AbstainReason {
    WeakResonance,   // 门1:最近邻相似度低于地板
    FalseResonance,  // 门2:rich 近但 signature 远,疑似假共振
    SparseEvidence,  // 门3:命中邻居缺乏观测结果历史
    Conflicted,      // 门4:top-k 邻居结果分歧过大(真实模糊)
}
```

四道弃权门(短路顺序,任一不过即弃权):

```rust
fn appraise(&self, sit: &Situation) -> Verdict {
    let neighbors = self.resonate(sit.rich_embedding(), self.cfg.k);

    // 门1:共振地板 —— 根本没共振到东西
    if neighbors.top_similarity() < self.cfg.resonance_floor {
        return Verdict::Abstain { reason: AbstainReason::WeakResonance };
    }

    // 门2:双通道一致性 —— rich 近但 signature 远 = 疑似假共振(方案 F)
    if self.signature_agreement(sit.signature(), &neighbors) < self.cfg.signature_floor {
        return Verdict::Abstain { reason: AbstainReason::FalseResonance };
    }

    // 门3:证据充分性 —— 只数 ObservedOutcome(方案 C)
    if neighbors.observed_outcome_count() < self.cfg.min_evidence {
        return Verdict::Abstain { reason: AbstainReason::SparseEvidence };
    }

    // 门4:邻居一致性 —— 分歧过大说明本情境真实模糊(方案 G)
    let (valence, dispersion) = self.weighted_valence(&neighbors);
    if dispersion > self.cfg.conflict_ceiling {
        return Verdict::Abstain { reason: AbstainReason::Conflicted };
    }

    let raw_conf = self.confidence_from(&neighbors, dispersion);
    Verdict::Directional {
        valence,
        confidence: self.calibration_map.apply(raw_conf), // 方案 E
        risks: self.extract_risks(&neighbors),
    }
}
```

**为什么这是 P0 而非锦上添花**:门1 治假共振(没共振到就不瞎说),门3 治校准自欺(没证据就不装懂),门4 治真实模糊(分歧大本就该闭嘴)。四道门把三类偏差在**产出之前**全部拦掉一遍,成本只是几次比较运算。`appraise` 同步、无 LLM,完全符合约束。

弃权率本身是一个健康度信号:弃权率长期接近 0,说明门太松,直觉在硬撑;长期接近 1,说明证据池太薄,需要先喂数据。

### 3.2 方案 B —— verdict_log,让直觉可证伪

没有这张表,你永远不知道直觉是不是在偏。

```sql
-- schema 4.15
CREATE TABLE verdict_log (
    verdict_id        INTEGER PRIMARY KEY,
    situation_sig     BLOB NOT NULL,      -- coarse 签名,便于分区统计
    emitted_valence   REAL,               -- 弃权则 NULL
    emitted_conf      REAL,               -- 弃权则 NULL
    abstain_reason    TEXT,               -- 表态则 NULL
    emitted_at        INTEGER NOT NULL,
    observed_outcome  REAL,               -- 后续回填:实际结果 valence,[-1,1]
    outcome_observed_at INTEGER,          -- NULL = 尚未观测 / 反事实被审查
    outcome_provenance  TEXT              -- 'observed' | 'counterfactual_censored'
);
CREATE INDEX idx_verdict_sig ON verdict_log(situation_sig);
CREATE INDEX idx_verdict_pending ON verdict_log(outcome_observed_at)
    WHERE outcome_observed_at IS NULL;
```

回填语义是关键,别搞错:

- actor 在某个 verdict 后**实际采取了已知动作并观测到结果** → `observed`,计入校准。
- actor 因为 verdict 给了警告而**避开了动作、没有坏结果发生** → 这是**反事实被审查**(`counterfactual_censored`),**不能**当作「verdict 正确」计入校准。你没看到「不避开会怎样」,naive 地给自己记一功就是自我确认。

基于这张表能算出直觉模块的全部健康指标(见第 5 节)。这是整个优化的仪表盘,先建表,后面 P1 才有依据。

### 3.3 方案 C —— provenance 标记,切断自我强化

`confidence_evidence` 当前应该是「累积证据 → 算可信度」。危险在于:如果 verdict 推导出的结论也回流进这个账本,直觉就会**学习自己的过去判断**,形成自我强化的回声室——它越确信,越多同向证据,越确信。

最小改动:给每条证据打来源标记,只让观测结果参与可信度计算。

```rust
pub enum Provenance {
    ObservedOutcome,   // 实际发生的结果 —— 唯一可计入校准的来源
    VerdictDerived,    // verdict 推导,仅留痕,绝不计入 context_score
    Imported,          // 外部导入的先验,需显式标注权重
}
```

```sql
ALTER TABLE confidence_evidence ADD COLUMN provenance TEXT
    NOT NULL DEFAULT 'observed';
```

```rust
// context_score_from_counts 只统计 ObservedOutcome
fn evidence_counts(&self, sig: &Signature) -> (u32, u32) {
    self.ledger
        .filter(|e| e.sig == *sig && e.provenance == Provenance::ObservedOutcome)
        .fold((0, 0), |(pos, neg), e| match e.valence_sign() {
            Sign::Pos => (pos + 1, neg),
            Sign::Neg => (pos, neg + 1),
        })
}
```

这条改动几乎零成本(加一列 + 过滤一下),却堵死了直觉模块最阴险的偏差通道。**这是你 playbook 第 7 节那条原则在嵌入域的硬化版本**。

---

## 4. P1 详细方案(让直觉真正被校准)

### 4.1 方案 D —— 基率锚定先验

`context_score_from_counts` 已经是证据加权的贝叶斯形式。问题在先验:稀疏证据时应该回归到**坏结果的全局基率**,而不是 0.5 或少数样本。

设全局负向结果基率为 `p0`,先验强度(伪观测数)为 `m`:

```
prior = Beta(α0, β0),  其中 α0 = m·p0,  β0 = m·(1 - p0)
posterior_mean = (α0 + n_pos) / (α0 + β0 + n_pos + n_neg)
```

`m` 是直觉模块的「谦逊度」旋钮:`m` 越大,需要越多观测才敢偏离基率。冷启动期把 `m` 调大,直觉就会乖乖回归基率而不是被三五个样本带跑。这是治校准自欺最干净的一招,且完全复用你现有的贝叶斯机器。

### 4.2 方案 E —— 学习到的校准映射

光有先验不够,还要用 verdict_log 的实际命中率反过来修正。流程:

1. 从 verdict_log 取所有 `observed` 记录,按 `emitted_conf` 分桶(如 10 桶)。
2. 每桶算「声称置信度」vs「实际命中率」。
3. 拟合单调映射(等渗回归,或先用最简分桶查表),存进 `calibration_map` 表。
4. `appraise` emit 时,`raw_conf → calibration_map.apply(raw_conf)`。

理想直觉:声称 0.7 的 verdict,坏结果约 70% 真的发生。**Expected Calibration Error (ECE) 是直觉模块的头号体检指标**。先用分桶查表落地,等数据量起来再换等渗回归,别一上来就上复杂拟合(过度工程)。

### 4.3 方案 F —— 双通道一致性门(已在门2)

你的 dual-path 本来就有:rich situation 喂嵌入/共振,coarse signature 喂校准。把它**改造成假共振检测器**:

- rich 嵌入说「近」,signature 也说「近」 → 可信共振,放行。
- rich 说「近」但 signature 说「远」 → **典型假共振信号**,折损置信度或直接弃权。

```rust
fn signature_agreement(&self, sig: &Signature, neighbors: &Neighbors) -> f32 {
    // 邻居中 signature 也匹配的比例 —— 低 = rich 嵌入在撒谎
    neighbors.iter()
        .filter(|n| sig.coarse_match(&n.signature))
        .count() as f32 / neighbors.len() as f32
}
```

这是用你已有的两个通道,零新增依赖,专打假共振。嵌入地板修不动,但**两个独立通道同时被骗的概率,远低于单通道**。

### 4.4 方案 G —— 邻居离散度塑形置信度(已在门4)

不要把 top-k 邻居盲目平均成一个 verdict,要看**它们之间的分歧**:

- 邻居高度一致 → 高置信度。
- 邻居严重分歧 → 这本身是信号:**本情境是真实模糊的**,该降置信度甚至弃权(门4)。

用已经召回的数据,几乎零额外成本,却同时治了假共振(混进来的无关邻居会拉高离散度被发现)和真实模糊。

---

## 5. 度量与验证(没有这节,前面全是空谈)

接入你 playbook 第 8 节的可观测性,直觉模块**必须**上这套仪表盘:

| 指标 | 定义 | 健康区间 | 信号 |
|---|---|---|---|
| **ECE** | 声称置信度 vs 实际命中率的加权偏差 | < 0.1 | 头号指标,超标=直觉在自欺 |
| 可靠性曲线 | 每个置信度桶的命中率散点 | 贴近对角线 | 系统性高/低估一眼可见 |
| 弃权率 | Abstain / 总调用 | 视域定,但不应趋 0 或 1 | 趋0=硬撑,趋1=证据荒 |
| 弃权精度 | 弃权样本中「表态本会错」的比例 | 越高越好 | 低=门误伤了好判断 |
| Verdict 精确/召回 | 仅对非弃权,against 观测结果 | 随域 | 区分过度告警 vs 漏报 |
| 假共振代理率 | rich-sim 高但 observed_outcome 背离的比例 | 越低越好 | 直接量化假共振 |
| 分区校准漂移 | 按 signature 分区,ECE 随时间变化 | 平稳 | 上升=该区分布漂移 |

**验证闭环**:每次发版后用 verdict_log 重算 ECE 和可靠性曲线,与上版对比。校准变差即使其他指标变好,也驳回——因为直觉的核心价值就是「可信地知道自己几斤几两」。

---

## 6. 实施路线(最小先行)

```
Phase 0(脊柱,1 个里程碑)
  A 弃权 + 四门骨架(门1/门3 先上,门2/门4 留桩)
  B verdict_log 建表 + emit 时写入 + 观测回填接口
  C provenance 列 + context_score 过滤
  → 交付物:直觉模块可被 ECE 证伪。在此之前不做任何精修。

Phase 1(校准,1 个里程碑)
  D 基率锚定先验(冷启动调大 m)
  E 分桶校准映射 + emit 应用
  F 门2 双通道一致性接线
  G 门4 离散度塑形接线
  → 交付物:ECE < 0.1,假共振代理率可测且下降。

Phase 2(精修,数据起来后再做)
  H 负证据闭环(需 AutoForge 回报「看着危险但没事」的反例)
  I 误差驱动分区衰减(替代纯 90 天时间半衰期)
  E' 等渗回归替换分桶查表
```

每个 Phase 结束跑一次第 5 节全套指标,不达标不进下一阶段。

---

## 7. 明确不做什么(对抗过度工程)

这节和上面同样重要。以下方向**主动放弃**,理由附后:

1. **不给同步 `appraise` 接 LLM 做语义判断**。违反无-LLM-同步约束,引入不可预测延迟,且 LLM critic 自己也有偏差,治标不治本。
2. **不堆更强嵌入模型 / 重排器去「修共振准度」**。嵌入地板是物理事实,这条路 ROI 极低。先把弃权和校准做了,同样的偏差用算术就压下去了。
3. **不上多模型 critic 集成(ensemble)**。方案 G 的邻居离散度已经用单模型拿到了「分歧即信号」的同等收益,集成纯属重复造轮子。
4. **不让 verdict 反写进证据账本**(方案 C 的反面)。这是原则 3,任何「让直觉从自己判断里学习」的便利写法都禁止。
5. **不追求消灭假共振**。它源于嵌入,消灭不了。目标是**检测 + 优雅降级为弃权**,不是清零。
6. **Phase 2 之前不碰自适应衰减**。90 天时间半衰期够用,误差驱动衰减需要先有 Phase 1 的校准数据才有意义,提前做是无依据的复杂度。

---

## 附:本文档与既有架构的一致性自检

- ✅ 同步路径无 LLM:所有改动都是确定性算术。
- ✅ librarian 不做 editor:直觉只产出 Verdict,弃权也只是「不产出方向」,从不改写内容。
- ✅ 零数据丢失:VerdictDerived 证据仍留痕(只是不计入校准),弃权样本仍入 verdict_log。
- ✅ 零厂商锁定:schema 4.15 仍是单 SQLite WAL,无新外部依赖。
- ✅ 延续 playbook 原则:本文档是「critic 独立调用避免自我确认偏差」在嵌入共振域的硬化与可度量化。
