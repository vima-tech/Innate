-- 4.20 → 4.21 — 可观测性 P3b:状态型 KPI 每日快照 metric_snapshots。
-- 与 operation_runs(事件流)互补:debt_ratio / pending 最老年龄 / confidence 分布
-- 等是状态比值,无法从事件流重建,需独立快照表以算趋势(周环比)。
-- 由 curate 末尾写一行,inspect() 读最近一条 + N 天前一条算 delta。幂等可重复。
CREATE TABLE IF NOT EXISTS metric_snapshots (
    ts   TEXT PRIMARY KEY,   -- utc_now_iso()
    kpis TEXT NOT NULL        -- 紧凑 JSON
);

UPDATE meta SET value='4.21' WHERE key='schema_version';
