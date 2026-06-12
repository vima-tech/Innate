-- 4.13 -> 4.14: durable distill-attempt accounting and baseline repair.
ALTER TABLE chunks ADD COLUMN evidence_cutoff_at TEXT;

CREATE TABLE IF NOT EXISTS distill_token_usage (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    log_id            TEXT NOT NULL,
    prompt_tokens     INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    outcome           TEXT NOT NULL,
    accounted_at      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_distill_usage_accounted
  ON distill_token_usage(accounted_at);
CREATE INDEX IF NOT EXISTS idx_distill_usage_log
  ON distill_token_usage(log_id);

INSERT INTO distill_token_usage(
    log_id, prompt_tokens, completion_tokens, outcome, accounted_at
)
SELECT id,
       COALESCE(distill_prompt_tokens, 0),
       COALESCE(distill_completion_tokens, 0),
       distill_state,
       distill_accounted_at
FROM episodic_log
WHERE distill_accounted_at IS NOT NULL
  AND NOT EXISTS (
    SELECT 1 FROM distill_token_usage usage
    WHERE usage.log_id=episodic_log.id
  );

-- Early 4.12 databases copied aggregate counters directly into their bases.
-- Repair only rows whose base still equals the aggregate while replayable facts exist.
UPDATE chunks
SET selected_count_base=MAX(
      0,
      selected_count_base - (
        SELECT COUNT(*) FROM usage_trace u
        WHERE u.chunk_id=chunks.id AND u.event='selected'
      )
    )
WHERE selected_count_base=selected_count
  AND EXISTS (
    SELECT 1 FROM usage_trace u
    WHERE u.chunk_id=chunks.id AND u.event='selected'
  );

UPDATE chunks
SET used_count_base=MAX(
      0,
      used_count_base - (
        SELECT COUNT(*) FROM usage_trace u
        WHERE u.chunk_id=chunks.id AND u.event='used'
      )
    )
WHERE used_count_base=used_count
  AND EXISTS (
    SELECT 1 FROM usage_trace u
    WHERE u.chunk_id=chunks.id AND u.event='used'
  );

UPDATE chunks
SET used_success_count_base=MAX(
      0,
      used_success_count_base - (
        SELECT COUNT(DISTINCT u.trace_id)
        FROM usage_trace u
        WHERE u.chunk_id=chunks.id
          AND u.event='used'
          AND EXISTS (
            SELECT 1 FROM usage_trace ok
            WHERE ok.trace_id=u.trace_id
              AND ok.event='task_ok'
              AND ok.chunk_id IS NULL
          )
      )
    )
WHERE used_success_count_base=used_success_count
  AND EXISTS (
    SELECT 1 FROM usage_trace u
    WHERE u.chunk_id=chunks.id AND u.event='used'
  );

-- A copied last_used_at that is itself backed by a retained event is not a base fact.
UPDATE chunks
SET last_used_base=NULL
WHERE last_used_base IS NOT NULL
  AND last_used_base=(
    SELECT MAX(u.ts) FROM usage_trace u
    WHERE u.chunk_id=chunks.id AND u.event='used'
  );

INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '4.14');
