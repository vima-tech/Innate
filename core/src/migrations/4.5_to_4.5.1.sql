-- migrations/4.5_to_4.5.1.sql
-- v4.5.1: distill run indices + selected idempotency index + timestamp format migration.

-- 1. Indices for stale-screening recovery.
CREATE INDEX IF NOT EXISTS idx_log_distill_run
  ON episodic_log(distill_run_id);
CREATE INDEX IF NOT EXISTS idx_log_screening_locked
  ON episodic_log(distill_state, distill_locked_at)
  WHERE distill_state = 'screening';

-- 2. Deduplicate 'selected' traces, then build idempotency index.
DELETE FROM usage_trace
WHERE id NOT IN (
  SELECT MIN(id) FROM usage_trace
  WHERE event = 'selected' AND chunk_id IS NOT NULL
  GROUP BY trace_id, chunk_id, event
) AND event = 'selected' AND chunk_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_chunk_selected_once
  ON usage_trace(trace_id, chunk_id, event)
  WHERE event = 'selected' AND chunk_id IS NOT NULL;

-- 3. Timestamp format migration: "YYYY-MM-DD HH:MM:SS" → "YYYY-MM-DDTHH:MM:SS.000Z"
--    (lexicographic ordering fix; GLOB ensures only old-format rows are touched)
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

INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '4.5.1');
