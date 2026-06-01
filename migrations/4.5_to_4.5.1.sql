-- migrations/4.5_to_4.5.1.sql

-- 1. 补 distill_run_id/locked_at 查询索引
CREATE INDEX IF NOT EXISTS idx_log_distill_run
  ON episodic_log(distill_run_id);
CREATE INDEX IF NOT EXISTS idx_log_screening_locked
  ON episodic_log(distill_state, distill_locked_at)
  WHERE distill_state = 'screening';

-- 2. 补 selected 幂等唯一索引
DELETE FROM usage_trace
WHERE id NOT IN (
  SELECT MIN(id) FROM usage_trace
  WHERE event = 'selected' AND chunk_id IS NOT NULL
  GROUP BY trace_id, chunk_id, event
) AND event = 'selected' AND chunk_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_chunk_selected_once
  ON usage_trace(trace_id, chunk_id, event)
  WHERE event = 'selected' AND chunk_id IS NOT NULL;

-- 3. 时间格式迁移(旧格式 "YYYY-MM-DD HH:MM:SS" → ISO 8601 UTC)
UPDATE usage_trace
  SET ts = replace(ts, ' ', 'T') || '.000Z'
  WHERE ts GLOB '____-__-__ __:__:__';

UPDATE episodic_log
  SET ts = replace(ts, ' ', 'T') || '.000Z'
  WHERE ts GLOB '____-__-__ __:__:__';
UPDATE episodic_log
  SET distill_locked_at = replace(distill_locked_at, ' ', 'T') || '.000Z'
  WHERE distill_locked_at GLOB '____-__-__ __:__:__';

UPDATE chunks
  SET created_at = replace(created_at, ' ', 'T') || '.000Z'
  WHERE created_at GLOB '____-__-__ __:__:__';
UPDATE chunks
  SET updated_at = replace(updated_at, ' ', 'T') || '.000Z'
  WHERE updated_at GLOB '____-__-__ __:__:__';
UPDATE chunks
  SET last_used_at = replace(last_used_at, ' ', 'T') || '.000Z'
  WHERE last_used_at IS NOT NULL AND last_used_at GLOB '____-__-__ __:__:__';
UPDATE chunks
  SET last_success_at = replace(last_success_at, ' ', 'T') || '.000Z'
  WHERE last_success_at IS NOT NULL AND last_success_at GLOB '____-__-__ __:__:__';

UPDATE chunk_success_traces
  SET ts = replace(ts, ' ', 'T') || '.000Z'
  WHERE ts GLOB '____-__-__ __:__:__';

UPDATE meta
  SET value = replace(value, ' ', 'T') || '.000Z'
  WHERE key = 'last_agg_ts'
    AND value GLOB '____-__-__ __:__:__';

INSERT OR REPLACE INTO meta VALUES ('schema_version', '4.5.1');
