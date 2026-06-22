-- 4.14 → 4.15 — 直觉模块偏差治理(方案 B/C/E/H 的 schema 受け皿)
-- 全部向后兼容、可重复执行(IF NOT EXISTS / 幂等列添加)。

-- 方案 C —— provenance 标记:只让观测结果参与可信度计算,切断自我强化。
-- 新增列默认 'observed',旧数据全部视为观测,行为零变化(守卫先就位)。
-- 注:ALTER TABLE ADD COLUMN 无 IF NOT EXISTS,故由 migrate.rs 做列存在守卫后条件执行
--     (与 4.12 的 copy_last_used 特例同模式),保证迁移可重复执行。

-- 方案 B —— verdict_log:让直觉可证伪(ECE / 可靠性曲线 / 弃权精度的唯一数据源)。
CREATE TABLE IF NOT EXISTS verdict_log (
    verdict_id          TEXT PRIMARY KEY,
    trace_id            TEXT NOT NULL,         -- 串联 appraise → record
    situation_sig       TEXT NOT NULL,         -- coarse 签名(context_key),便于分区统计
    emitted_valence     TEXT,                  -- affirm|caution|mixed|neutral;弃权则 NULL
    emitted_conf        REAL,                  -- 已过校准映射的置信度;弃权则 NULL
    emitted_strength    REAL,                  -- 聚合 strength(max fused)
    emitted_tier        TEXT,                  -- weak|medium|strong
    abstain_reason      TEXT,                  -- 表态则 NULL;否则弃权门名
    emitted_at          TEXT NOT NULL,
    observed_outcome    REAL,                  -- 后续回填:实际结果 valence,[-1,1]
    outcome_observed_at TEXT,                  -- NULL = 尚未观测 / 反事实被审查
    outcome_provenance  TEXT                   -- 'observed' | 'counterfactual_censored'
);
CREATE INDEX IF NOT EXISTS idx_verdict_sig ON verdict_log(situation_sig);
CREATE INDEX IF NOT EXISTS idx_verdict_trace ON verdict_log(trace_id);
CREATE INDEX IF NOT EXISTS idx_verdict_pending ON verdict_log(outcome_observed_at)
    WHERE outcome_observed_at IS NULL;

-- 方案 E —— 学习到的校准映射:emit 时把 raw_conf → calibrated_conf。
-- 分桶查表(bucket = floor(raw_conf*bins));curate 周期性重算。
CREATE TABLE IF NOT EXISTS calibration_map (
    bucket          INTEGER PRIMARY KEY,       -- 0..bins-1
    claimed_lo      REAL NOT NULL,             -- 桶下界(声称置信度)
    claimed_hi      REAL NOT NULL,             -- 桶上界
    observed_rate   REAL NOT NULL,             -- 实际命中率
    n               INTEGER NOT NULL,          -- 桶内观测样本数
    updated_at      TEXT NOT NULL
);

UPDATE meta SET value='4.15' WHERE key='schema_version';
