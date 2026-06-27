-- 4.19 → 4.20 — 可观测性 P3a:运行事件表 operation_runs。
-- 进程内每次操作(recall/appraise/record/evolve/curate/distill/embed/hook_recall)的
-- 摘要:状态、耗时、error_kind、计数。不存 prompt/response(原始明细仍在 llm_trace.log)。
-- 由 curate 按 meta `metrics.retain_days` 清理旧行,防无界增长。幂等可重复。
CREATE TABLE IF NOT EXISTS operation_runs (
    id          TEXT PRIMARY KEY,
    trace_id    TEXT,
    op          TEXT NOT NULL,       -- recall/appraise/record/evolve/curate/distill/embed/hook_recall
    source      TEXT,
    agent       TEXT,
    status      TEXT NOT NULL,       -- ok/error/timeout
    error_kind  TEXT,
    started_at  TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    counts_json TEXT,
    params_json TEXT
);
CREATE INDEX IF NOT EXISTS idx_opruns_op  ON operation_runs(op);
CREATE INDEX IF NOT EXISTS idx_opruns_ts  ON operation_runs(started_at);
CREATE INDEX IF NOT EXISTS idx_opruns_tid ON operation_runs(trace_id);

UPDATE meta SET value='4.20' WHERE key='schema_version';
