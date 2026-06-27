-- 4.18 → 4.19 — 可观测性 P0:为多窗口(1d/7d/30d)派生指标补 ts 索引。
-- inspect() 将按时间窗口对 episodic_log / usage_trace / feedback_events 做条件聚合,
-- 这三表此前均无 ts 索引。
--
-- 注意:索引的 CREATE 由 migrate.rs 在本 SQL 之后用 column_exists 守卫条件执行
-- (与 4.16 FTS / 4.17 entities 回填同模式),因为部分迁移测试夹具的这些表缺 ts 列,
-- 直接 CREATE INDEX ON <表>(ts) 会硬失败(IF NOT EXISTS 只防索引名、不防列)。
-- 真实库三表恒有 ts,索引照常建立;schema.sql 亦已带这些索引供新建库直接使用。
UPDATE meta SET value='4.19' WHERE key='schema_version';
