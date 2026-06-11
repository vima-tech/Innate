-- v4.6: auditable feedback, task lifecycle, attribution, contextual outcomes,
-- evolve request queue, and MCP as a first-class event source.

ALTER TABLE usage_trace RENAME TO usage_trace_v452;
DROP INDEX IF EXISTS idx_trace_chunk;
DROP INDEX IF EXISTS idx_trace_tid;
DROP INDEX IF EXISTS idx_trace_event;
DROP INDEX IF EXISTS idx_trace_source;
DROP INDEX IF EXISTS idx_trace_chunk_used_once;
DROP INDEX IF EXISTS idx_trace_chunk_selected_once;
DROP INDEX IF EXISTS idx_trace_outcome_once;

CREATE TABLE usage_trace (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    trace_id      TEXT NOT NULL,
    chunk_id      TEXT,
    event         TEXT NOT NULL CHECK(event IN ('retrieved','selected','refined','used','task_ok','task_fail')),
    strength      REAL DEFAULT 1.0,
    similarity    REAL,
    tokens        INTEGER,
    rank          INTEGER,
    refine_mode   TEXT,
    attribution   TEXT CHECK(attribution IS NULL OR attribution IN ('explicit','cited','inferred')),
    source        TEXT NOT NULL DEFAULT 'sdk'
                  CHECK(source IN ('mcp','sdk','cli','hook','daemon','augmented')),
    ts            TEXT NOT NULL
);
INSERT INTO usage_trace
    (id, trace_id, chunk_id, event, strength, similarity, tokens, rank,
     refine_mode, attribution, source, ts)
SELECT id, trace_id, chunk_id, event, strength, similarity, tokens, rank,
       refine_mode, CASE WHEN event='used' THEN 'explicit' ELSE NULL END,
       source, ts
FROM usage_trace_v452;
DROP TABLE usage_trace_v452;

CREATE INDEX idx_trace_chunk  ON usage_trace(chunk_id);
CREATE INDEX idx_trace_tid    ON usage_trace(trace_id);
CREATE INDEX idx_trace_event  ON usage_trace(event);
CREATE INDEX idx_trace_source ON usage_trace(source);
CREATE UNIQUE INDEX idx_trace_chunk_used_once
  ON usage_trace(trace_id, chunk_id, event)
  WHERE event = 'used' AND chunk_id IS NOT NULL;
CREATE UNIQUE INDEX idx_trace_chunk_selected_once
  ON usage_trace(trace_id, chunk_id, event)
  WHERE event = 'selected' AND chunk_id IS NOT NULL;
CREATE UNIQUE INDEX idx_trace_outcome_once
  ON usage_trace(trace_id)
  WHERE event IN ('task_ok', 'task_fail') AND chunk_id IS NULL;

ALTER TABLE episodic_log RENAME TO episodic_log_v452;
DROP INDEX IF EXISTS idx_log_dstate;
DROP INDEX IF EXISTS idx_log_prio;
DROP INDEX IF EXISTS idx_log_trace;
DROP INDEX IF EXISTS idx_log_distill_run;
DROP INDEX IF EXISTS idx_log_screening_locked;
DROP INDEX IF EXISTS idx_log_distill_accounted;

CREATE TABLE episodic_log (
    id          TEXT PRIMARY KEY,
    trace_id    TEXT NOT NULL,
    lib_id      TEXT NOT NULL,
    ts          TEXT NOT NULL,
    query           TEXT,
    recall_snapshot TEXT,
    output          TEXT,
    output_summary  TEXT,
    outcome         TEXT,
    event_source TEXT NOT NULL DEFAULT 'sdk'
                 CHECK(event_source IN ('mcp','sdk','cli','hook','daemon','augmented')),
    task_state TEXT NOT NULL DEFAULT 'recalled'
        CHECK(task_state IN ('recalled','running','completed','abandoned','timed_out')),
    completed_at TEXT,
    usage_state TEXT NOT NULL DEFAULT 'unknown'
        CHECK(usage_state IN ('unknown','known_none','known_some')),
    used_ids TEXT,
    used_attribution TEXT
        CHECK(used_attribution IS NULL OR used_attribution IN ('explicit','cited','inferred')),
    context_key TEXT,
    nomination  TEXT,
    priority    INTEGER NOT NULL DEFAULT 0,
    distill_state TEXT NOT NULL DEFAULT 'open'
        CHECK(distill_state IN ('open','new','screening','distilled','discarded','failed')),
    distill_note  TEXT,
    distill_run_id    TEXT,
    distill_locked_at TEXT,
    distill_prompt_tokens     INTEGER,
    distill_completion_tokens INTEGER,
    distill_accounted_at      TEXT
);
INSERT INTO episodic_log
    (id, trace_id, lib_id, ts, query, recall_snapshot, output, output_summary,
     outcome, event_source, task_state, completed_at, usage_state, used_ids,
     used_attribution, context_key, nomination, priority, distill_state,
     distill_note, distill_run_id, distill_locked_at, distill_prompt_tokens,
     distill_completion_tokens, distill_accounted_at)
SELECT id, trace_id, lib_id, ts, query, recall_snapshot, output, output_summary,
       outcome, event_source,
       CASE WHEN outcome IS NULL THEN 'recalled' ELSE 'completed' END,
       CASE WHEN outcome IS NULL THEN NULL ELSE ts END,
       'unknown', NULL, NULL, NULL, nomination, priority, distill_state,
       distill_note, distill_run_id, distill_locked_at, distill_prompt_tokens,
       distill_completion_tokens, distill_accounted_at
FROM episodic_log_v452;
DROP TABLE episodic_log_v452;

CREATE INDEX idx_log_dstate ON episodic_log(distill_state);
CREATE INDEX idx_log_prio   ON episodic_log(priority);
CREATE UNIQUE INDEX idx_log_trace ON episodic_log(trace_id);
CREATE INDEX idx_log_distill_run ON episodic_log(distill_run_id);
CREATE INDEX idx_log_screening_locked
  ON episodic_log(distill_state, distill_locked_at)
  WHERE distill_state = 'screening';
CREATE INDEX idx_log_distill_accounted
  ON episodic_log(distill_accounted_at)
  WHERE distill_accounted_at IS NOT NULL;
CREATE INDEX idx_log_task_state ON episodic_log(task_state);
CREATE INDEX idx_log_context ON episodic_log(context_key);

CREATE TABLE feedback_events (
    id          TEXT PRIMARY KEY,
    trace_id    TEXT NOT NULL,
    chunk_id    TEXT NOT NULL,
    signal      TEXT NOT NULL CHECK(signal IN ('up','down')),
    strength    REAL NOT NULL,
    source      TEXT NOT NULL,
    actor       TEXT,
    reason      TEXT,
    context_key TEXT,
    ts          TEXT NOT NULL
);
CREATE INDEX idx_feedback_trace ON feedback_events(trace_id);
CREATE INDEX idx_feedback_chunk ON feedback_events(chunk_id);
CREATE INDEX idx_feedback_signal ON feedback_events(signal);

CREATE TABLE chunk_context_stats (
    chunk_id          TEXT NOT NULL,
    context_key       TEXT NOT NULL,
    success_count     INTEGER NOT NULL DEFAULT 0,
    failure_count     INTEGER NOT NULL DEFAULT 0,
    positive_feedback INTEGER NOT NULL DEFAULT 0,
    negative_feedback INTEGER NOT NULL DEFAULT 0,
    last_updated_at   TEXT NOT NULL,
    PRIMARY KEY (chunk_id, context_key)
);
CREATE INDEX idx_context_key ON chunk_context_stats(context_key);

CREATE TABLE governance_proposals (
    id              TEXT PRIMARY KEY,
    chunk_id        TEXT NOT NULL,
    proposal_type   TEXT NOT NULL,
    reason          TEXT NOT NULL,
    evidence_count  INTEGER NOT NULL DEFAULT 1,
    state           TEXT NOT NULL DEFAULT 'pending'
        CHECK(state IN ('pending','accepted','rejected')),
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_governance_pending
  ON governance_proposals(chunk_id, proposal_type)
  WHERE state='pending';

CREATE TABLE evolve_requests (
    id           TEXT PRIMARY KEY,
    reason       TEXT NOT NULL,
    state        TEXT NOT NULL DEFAULT 'pending'
        CHECK(state IN ('pending','running','completed','failed')),
    requested_at TEXT NOT NULL,
    leased_at    TEXT,
    completed_at TEXT,
    note         TEXT
);
CREATE INDEX idx_evolve_request_state
  ON evolve_requests(state, requested_at);
CREATE UNIQUE INDEX idx_evolve_single_active
  ON evolve_requests((1))
  WHERE state IN ('pending','running');

INSERT OR IGNORE INTO meta(key, value) VALUES ('recall.w_context', '0.05');
INSERT OR IGNORE INTO meta(key, value) VALUES ('evolve.schedule_interval_hours', '6');
INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '4.6');
