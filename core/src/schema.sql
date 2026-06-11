-- Innate knowledge layer schema v4.5.2 (Rust edition)
-- Replaces sqlite-vec virtual tables with BLOB columns + Rust cosine similarity.
-- All timestamp conventions from the original schema apply unchanged.
-- NOTE: PRAGMAs are set by configure_pragmas() at connection time; omitted here.

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', '4.5.2');

CREATE TABLE IF NOT EXISTS chunks (
    id            TEXT PRIMARY KEY,
    skill_name    TEXT,
    seq           INTEGER DEFAULT 0,
    content       TEXT NOT NULL,
    trigger_desc  TEXT,
    anti_trigger_desc TEXT,
    content_hash  TEXT NOT NULL,
    token_count   INTEGER,

    origin        TEXT NOT NULL CHECK(origin IN ('installed','distilled','captured','spark')),
    source        TEXT,
    maturity      TEXT,
    related_ids   TEXT,
    protected     INTEGER NOT NULL DEFAULT 0,
    state         TEXT NOT NULL DEFAULT 'active' CHECK(state IN ('pending','active','archived')),
    state_reason  TEXT,
    state_updated_at TEXT,

    confidence    REAL NOT NULL DEFAULT 0.5,
    confidence_reason TEXT,
    version       INTEGER NOT NULL DEFAULT 1,
    distilled_from TEXT,
    parent_id     TEXT,

    selected_count        INTEGER NOT NULL DEFAULT 0,
    used_count            INTEGER NOT NULL DEFAULT 0,
    used_success_count    INTEGER NOT NULL DEFAULT 0,
    success_trace_ids_count INTEGER NOT NULL DEFAULT 0,
    last_success_at       TEXT,
    last_agg_ts           TEXT,

    embed_version INTEGER NOT NULL DEFAULT 1,

    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    last_used_at  TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_chunks_distilled_from ON chunks(distilled_from) WHERE distilled_from IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_chunks_state   ON chunks(state);
CREATE INDEX IF NOT EXISTS idx_chunks_origin  ON chunks(origin);
CREATE INDEX IF NOT EXISTS idx_chunks_skill   ON chunks(skill_name);
CREATE INDEX IF NOT EXISTS idx_chunks_hash    ON chunks(content_hash);
CREATE INDEX IF NOT EXISTS idx_chunks_conf    ON chunks(confidence);
CREATE INDEX IF NOT EXISTS idx_chunks_embed_v ON chunks(embed_version);

CREATE TABLE IF NOT EXISTS invalidated_hashes (
    content_hash TEXT PRIMARY KEY,
    reason       TEXT,
    ts           TEXT NOT NULL
);

-- Vector tables: BLOB columns instead of sqlite-vec virtual tables.
-- Rust code handles cosine similarity directly.
CREATE TABLE IF NOT EXISTS vec_content (
    chunk_id  TEXT PRIMARY KEY,
    embedding BLOB NOT NULL
);

CREATE TABLE IF NOT EXISTS vec_trigger (
    chunk_id  TEXT PRIMARY KEY,
    embedding BLOB NOT NULL
);

CREATE TABLE IF NOT EXISTS deps (
    src       TEXT NOT NULL,
    dst       TEXT NOT NULL,
    kind      TEXT NOT NULL DEFAULT 'hard',
    dst_lib   TEXT,
    dst_ref   TEXT,
    PRIMARY KEY (src, dst, kind)
);
CREATE INDEX IF NOT EXISTS idx_deps_src ON deps(src);
CREATE INDEX IF NOT EXISTS idx_deps_dst ON deps(dst);

CREATE TABLE IF NOT EXISTS usage_trace (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    trace_id      TEXT NOT NULL,
    chunk_id      TEXT,
    event         TEXT NOT NULL CHECK(event IN ('retrieved','selected','refined','used','task_ok','task_fail')),
    strength      REAL DEFAULT 1.0,
    similarity    REAL,
    tokens        INTEGER,
    rank          INTEGER,
    refine_mode   TEXT,
    source        TEXT NOT NULL DEFAULT 'sdk'
                  CHECK(source IN ('sdk','cli','hook','daemon','augmented')),
    ts            TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_trace_chunk  ON usage_trace(chunk_id);
CREATE INDEX IF NOT EXISTS idx_trace_tid    ON usage_trace(trace_id);
CREATE INDEX IF NOT EXISTS idx_trace_event  ON usage_trace(event);
CREATE INDEX IF NOT EXISTS idx_trace_source ON usage_trace(source);
CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_chunk_used_once
  ON usage_trace(trace_id, chunk_id, event)
  WHERE event = 'used' AND chunk_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_chunk_selected_once
  ON usage_trace(trace_id, chunk_id, event)
  WHERE event = 'selected' AND chunk_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_outcome_once
  ON usage_trace(trace_id)
  WHERE event IN ('task_ok', 'task_fail') AND chunk_id IS NULL;

CREATE TABLE IF NOT EXISTS episodic_log (
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
                 CHECK(event_source IN ('sdk','cli','hook','daemon','augmented')),
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
CREATE INDEX IF NOT EXISTS idx_log_dstate ON episodic_log(distill_state);
CREATE INDEX IF NOT EXISTS idx_log_prio   ON episodic_log(priority);
CREATE UNIQUE INDEX IF NOT EXISTS idx_log_trace ON episodic_log(trace_id);
CREATE INDEX IF NOT EXISTS idx_log_distill_run ON episodic_log(distill_run_id);
CREATE INDEX IF NOT EXISTS idx_log_screening_locked
  ON episodic_log(distill_state, distill_locked_at)
  WHERE distill_state = 'screening';
CREATE INDEX IF NOT EXISTS idx_log_distill_accounted
  ON episodic_log(distill_accounted_at)
  WHERE distill_accounted_at IS NOT NULL;

CREATE TABLE IF NOT EXISTS chunk_success_traces (
    chunk_id  TEXT NOT NULL,
    trace_id  TEXT NOT NULL,
    ts        TEXT NOT NULL,
    PRIMARY KEY (chunk_id, trace_id)
);
CREATE INDEX IF NOT EXISTS idx_cst_chunk ON chunk_success_traces(chunk_id);
