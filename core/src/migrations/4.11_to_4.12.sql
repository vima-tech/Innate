-- 4.11 → 4.12: auditable/replayable feedback, usage completeness,
-- distillation provenance, and retryable evolve requests.
ALTER TABLE chunks ADD COLUMN confidence_base REAL NOT NULL DEFAULT 0.5;
UPDATE chunks SET confidence_base=confidence;
ALTER TABLE chunks ADD COLUMN distill_provider TEXT;
ALTER TABLE chunks ADD COLUMN distill_model TEXT;
ALTER TABLE chunks ADD COLUMN distill_prompt_version TEXT;
ALTER TABLE chunks ADD COLUMN selected_count_base INTEGER NOT NULL DEFAULT 0;
ALTER TABLE chunks ADD COLUMN used_count_base INTEGER NOT NULL DEFAULT 0;
ALTER TABLE chunks ADD COLUMN used_success_count_base INTEGER NOT NULL DEFAULT 0;
ALTER TABLE chunks ADD COLUMN last_used_base TEXT;
UPDATE chunks
SET selected_count_base=MAX(
      0,
      selected_count - (
        SELECT COUNT(*) FROM usage_trace u
        WHERE u.chunk_id=chunks.id AND u.event='selected'
      )
    ),
    used_count_base=MAX(
      0,
      used_count - (
        SELECT COUNT(*) FROM usage_trace u
        WHERE u.chunk_id=chunks.id AND u.event='used'
      )
    ),
    used_success_count_base=MAX(
      0,
      used_success_count - (
        SELECT COUNT(DISTINCT u.trace_id)
        FROM usage_trace u
        WHERE u.chunk_id=chunks.id
          AND u.event='used'
          AND (
            EXISTS (
              SELECT 1 FROM usage_trace ok
              WHERE ok.trace_id=u.trace_id
                AND ok.event='task_ok'
                AND ok.chunk_id IS NULL
            )
            OR EXISTS (
              SELECT 1 FROM episodic_log e
              WHERE e.trace_id=u.trace_id AND e.outcome='ok'
            )
          )
      )
    );
CREATE TABLE IF NOT EXISTS chunk_success_traces (
    chunk_id TEXT NOT NULL,
    trace_id TEXT NOT NULL,
    ts TEXT NOT NULL,
    PRIMARY KEY (chunk_id, trace_id)
);
CREATE INDEX IF NOT EXISTS idx_cst_chunk ON chunk_success_traces(chunk_id);
DELETE FROM chunk_success_traces;

ALTER TABLE episodic_log ADD COLUMN used_complete INTEGER NOT NULL DEFAULT 1;

CREATE TABLE IF NOT EXISTS confidence_evidence (
    id          TEXT PRIMARY KEY,
    trace_id    TEXT,
    chunk_id    TEXT NOT NULL,
    kind        TEXT NOT NULL,
    target      REAL NOT NULL,
    alpha       REAL NOT NULL,
    reason      TEXT NOT NULL,
    context_key TEXT,
    ts          TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_confidence_evidence_trace
  ON confidence_evidence(trace_id, chunk_id, kind)
  WHERE trace_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_confidence_evidence_chunk
  ON confidence_evidence(chunk_id, ts, id);

CREATE TABLE IF NOT EXISTS chunk_context_stats_base (
    chunk_id          TEXT NOT NULL,
    context_key       TEXT NOT NULL,
    success_count     INTEGER NOT NULL DEFAULT 0,
    failure_count     INTEGER NOT NULL DEFAULT 0,
    positive_feedback INTEGER NOT NULL DEFAULT 0,
    negative_feedback INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (chunk_id, context_key)
);
INSERT OR REPLACE INTO chunk_context_stats_base
SELECT c.chunk_id, c.context_key, c.success_count, c.failure_count,
       MAX(0, c.positive_feedback - COALESCE((
         SELECT COUNT(*) FROM feedback_events f
         WHERE f.chunk_id=c.chunk_id AND f.context_key=c.context_key AND f.signal='up'
       ), 0)),
       MAX(0, c.negative_feedback - COALESCE((
         SELECT COUNT(*) FROM feedback_events f
         WHERE f.chunk_id=c.chunk_id AND f.context_key=c.context_key AND f.signal='down'
       ), 0))
FROM chunk_context_stats c;

ALTER TABLE governance_proposals ADD COLUMN evidence_score REAL NOT NULL DEFAULT 0;
ALTER TABLE governance_proposals ADD COLUMN actor_count INTEGER NOT NULL DEFAULT 0;

ALTER TABLE evolve_requests ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE evolve_requests ADD COLUMN next_retry_at TEXT;
ALTER TABLE evolve_requests ADD COLUMN priority INTEGER NOT NULL DEFAULT 0;
DROP INDEX IF EXISTS idx_evolve_single_active;
DROP INDEX IF EXISTS idx_evolve_request_state;
CREATE INDEX IF NOT EXISTS idx_evolve_request_state
  ON evolve_requests(state, priority DESC, requested_at);
CREATE UNIQUE INDEX IF NOT EXISTS idx_evolve_pending_reason
  ON evolve_requests(reason)
  WHERE state='pending';

INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '4.12');
