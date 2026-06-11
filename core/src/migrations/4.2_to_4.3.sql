-- migrations/4.2_to_4.3.sql
-- v4.3: episodic_log trace_id unique index + distill_state 5-state machine +
--       usage_trace source NOT NULL + idempotent unique indices.

-- 1. Deduplicate episodic_log trace_id before creating unique index.
UPDATE episodic_log
SET trace_id      = trace_id || ':migration_dedup:' || id,
    distill_state = 'discarded',
    distill_note  = 'migration_dedup'
WHERE id NOT IN (
  SELECT MIN(id) FROM episodic_log GROUP BY trace_id
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_log_trace ON episodic_log(trace_id);

-- 2. Normalize distill_state: rows with query/recall_snapshot but no output/outcome
--    were stuck in 'new' prior to the open state; reclassify as 'open'.
UPDATE episodic_log SET distill_state = 'open'
WHERE distill_state = 'new'
  AND (output IS NULL AND output_summary IS NULL AND outcome IS NULL);

-- 3. Normalize source NOT NULL.
UPDATE usage_trace SET source = 'sdk' WHERE source IS NULL;

-- 4a. Deduplicate 'used' traces (keep earliest per trace_id+chunk_id+event).
DELETE FROM usage_trace
WHERE id NOT IN (
  SELECT MIN(id) FROM usage_trace
  WHERE event = 'used' AND chunk_id IS NOT NULL
  GROUP BY trace_id, chunk_id, event
) AND event = 'used' AND chunk_id IS NOT NULL;

-- 4b. Deduplicate outcome traces (keep earliest per trace_id).
DELETE FROM usage_trace
WHERE id NOT IN (
  SELECT MIN(id) FROM usage_trace
  WHERE event IN ('task_ok','task_fail') AND chunk_id IS NULL
  GROUP BY trace_id
) AND event IN ('task_ok','task_fail') AND chunk_id IS NULL;

-- 4c. Build idempotent unique indices.
CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_chunk_used_once
  ON usage_trace(trace_id, chunk_id, event)
  WHERE event = 'used' AND chunk_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_outcome_once
  ON usage_trace(trace_id)
  WHERE event IN ('task_ok','task_fail') AND chunk_id IS NULL;

INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '4.3');
