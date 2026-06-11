-- migrations/4.3_to_4.4.sql
-- v4.4: ensure outcome uniqueness index (may already exist from 4.2→4.3).
DELETE FROM usage_trace
WHERE id NOT IN (
  SELECT MIN(id) FROM usage_trace
  WHERE event IN ('task_ok','task_fail') AND chunk_id IS NULL
  GROUP BY trace_id
) AND event IN ('task_ok','task_fail') AND chunk_id IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_outcome_once
  ON usage_trace(trace_id)
  WHERE event IN ('task_ok','task_fail') AND chunk_id IS NULL;

INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '4.4');
