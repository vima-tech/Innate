-- migrations/4.2_to_4.3.sql

-- 1. episodic_log.trace_id 改唯一索引
UPDATE episodic_log SET distill_state='discarded', distill_note='migration_dedup'
WHERE id NOT IN (
  SELECT MIN(id) FROM episodic_log GROUP BY trace_id
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_log_trace ON episodic_log(trace_id);

-- 2. 现有 open 状态行识别
UPDATE episodic_log SET distill_state='open'
WHERE distill_state='new'
  AND (output IS NULL AND output_summary IS NULL AND outcome IS NULL);

-- 3. source 字段 NOT NULL:现有 NULL source 改 'sdk'
UPDATE usage_trace SET source='sdk' WHERE source IS NULL;

-- 4a. 去重 used 事件(chunk 级),保留最早一条
DELETE FROM usage_trace
WHERE id NOT IN (
  SELECT MIN(id) FROM usage_trace
  WHERE event='used' AND chunk_id IS NOT NULL
  GROUP BY trace_id, chunk_id, event
) AND event='used' AND chunk_id IS NOT NULL;

-- 4b. 处理 outcome 冲突:同一 trace 存在 task_ok 和 task_fail 两行时,保留较早一条
DELETE FROM usage_trace
WHERE id NOT IN (
  SELECT MIN(id) FROM usage_trace
  WHERE event IN ('task_ok','task_fail') AND chunk_id IS NULL
  GROUP BY trace_id
) AND event IN ('task_ok','task_fail') AND chunk_id IS NULL;

-- 4c. 建幂等索引
CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_chunk_used_once
  ON usage_trace(trace_id, chunk_id, event)
  WHERE event='used' AND chunk_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_outcome_once
  ON usage_trace(trace_id)
  WHERE event IN ('task_ok', 'task_fail') AND chunk_id IS NULL;

INSERT OR REPLACE INTO meta VALUES ('schema_version', '4.3');
