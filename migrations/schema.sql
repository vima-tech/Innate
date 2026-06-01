-- ============================================================
-- Innate 知识层 —— 单库 Schema (sqlite-vec)
-- 每个知识库 = 一个 .db 文件(WAL);chunks/deps/usage_trace/episodic_log 全部同库
--
-- ⚠️ 时间格式统一约定 (v4.5.1):
--    全库所有 TEXT 时间字段统一使用 ISO 8601 UTC 格式
--    精度统一为毫秒: YYYY-MM-DDTHH:MM:SS.mmmZ  (三位毫秒, 不多不少)
--    正确 (SQL 内):   strftime('%Y-%m-%dT%H:%M:%fZ','now')  → "2024-01-15T08:30:00.000Z"
--    正确 (Python):   只调 utc_now_iso() 封装函数, 禁止在业务层散落 datetime.utcnow()
--    禁止: datetime('now')                          → "2024-01-15 08:30:00" (空格分隔)
--    禁止: datetime.utcnow().isoformat() + 'Z'       → 精度不定 (0/3/6 位小数)
--    禁止: date('now')                                → 仅日期, 无时间
--    禁止: 本地时间 (无 'utc' 修饰符)
--    原因: SQLite TEXT 时间靠字典序比较, 格式/精度不一致会导致 ts > :last_ts
--          等比较静默出错.
-- ============================================================

PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA synchronous = NORMAL;

-- ----- 元信息:库自身 + schema + embedding 版本 -----
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', '4.5.1');

-- 预置 key:
--   lib_id          TEXT  -- uuid
--   lib_role        TEXT  -- personal | shared
--   schema_version  TEXT  -- 当前 schema 版本,如 "4.5.1"
--   content_dim     TEXT  -- "1024"
--   trigger_dim     TEXT  -- "256"
--   embed_model     TEXT  -- 嵌入模型标识
--   embed_version   TEXT  -- 整数,递增;向量重建时递增

-- ============================================================
-- 核心实体:统一 Chunk
-- ============================================================
CREATE TABLE IF NOT EXISTS chunks (
    id            TEXT PRIMARY KEY,
    skill_name    TEXT,
    seq           INTEGER DEFAULT 0,
    content       TEXT NOT NULL,
    trigger_desc  TEXT,
    anti_trigger_desc TEXT,
    content_hash  TEXT NOT NULL,
    token_count   INTEGER,

    -- 生命周期
    origin        TEXT NOT NULL CHECK(origin IN ('installed','distilled','captured','spark')),
    source        TEXT,
    maturity      TEXT,
    related_ids   TEXT,
    protected     INTEGER NOT NULL DEFAULT 0,
    state         TEXT NOT NULL DEFAULT 'active' CHECK(state IN ('pending','active','archived')),
    state_reason  TEXT,
    state_updated_at TEXT,

    -- 质量与演化
    confidence    REAL NOT NULL DEFAULT 0.5,
    confidence_reason TEXT,
    version       INTEGER NOT NULL DEFAULT 1,
    distilled_from TEXT,
    parent_id     TEXT,

    -- 物化计数器(异步 aggregate 批量更新,record 不碰)
    selected_count        INTEGER NOT NULL DEFAULT 0,
    used_count            INTEGER NOT NULL DEFAULT 0,
    used_success_count    INTEGER NOT NULL DEFAULT 0,
    success_trace_ids_count INTEGER NOT NULL DEFAULT 0,
    last_success_at       TEXT,
    last_agg_ts           TEXT,  -- DEPRECATED: use meta.last_agg_ts

    -- embedding 版本
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

-- 重入黑名单:invalidate 作废的 content_hash
CREATE TABLE IF NOT EXISTS invalidated_hashes (
    content_hash TEXT PRIMARY KEY,
    reason       TEXT,
    ts           TEXT NOT NULL
);

-- ============================================================
-- 双向量(trigger 低维降延迟;维度由 meta 决定)
-- ============================================================
CREATE VIRTUAL TABLE IF NOT EXISTS vec_content USING vec0(
    chunk_id TEXT PRIMARY KEY,
    embedding float[1024]
);
CREATE VIRTUAL TABLE IF NOT EXISTS vec_trigger USING vec0(
    chunk_id TEXT PRIMARY KEY,
    embedding float[256]
);

-- ============================================================
-- 依赖图
-- ============================================================
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

-- ============================================================
-- Observe 观测
-- ============================================================
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
-- 幂等约束
CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_chunk_used_once
  ON usage_trace(trace_id, chunk_id, event)
  WHERE event = 'used' AND chunk_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_chunk_selected_once
  ON usage_trace(trace_id, chunk_id, event)
  WHERE event = 'selected' AND chunk_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_trace_outcome_once
  ON usage_trace(trace_id)
  WHERE event IN ('task_ok', 'task_fail') AND chunk_id IS NULL;

-- ============================================================
-- Episodic Log
-- ============================================================
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
    distill_completion_tokens INTEGER
);
CREATE INDEX IF NOT EXISTS idx_log_dstate ON episodic_log(distill_state);
CREATE INDEX IF NOT EXISTS idx_log_prio   ON episodic_log(priority);
CREATE UNIQUE INDEX IF NOT EXISTS idx_log_trace ON episodic_log(trace_id);
CREATE INDEX IF NOT EXISTS idx_log_distill_run ON episodic_log(distill_run_id);
CREATE INDEX IF NOT EXISTS idx_log_screening_locked
  ON episodic_log(distill_state, distill_locked_at)
  WHERE distill_state = 'screening';

-- ============================================================
-- 事实表:持久化每个 chunk 的成功 trace 集合
-- ============================================================
CREATE TABLE IF NOT EXISTS chunk_success_traces (
    chunk_id  TEXT NOT NULL,
    trace_id  TEXT NOT NULL,
    ts        TEXT NOT NULL,
    PRIMARY KEY (chunk_id, trace_id)
);
CREATE INDEX IF NOT EXISTS idx_cst_chunk ON chunk_success_traces(chunk_id);
