-- 4.22 → 5.0 — 活知识库重构：经历 / 规则 / 验证分离（docs/Innate-活知识库重构-整体改进方案-v1.md）。
--
-- 只做加法：新表 + 新列，不改写、不删除任何既有数据。列的添加（chunks.signals /
-- chunks.source_projects / episodic_log.project / episodic_log.session_id）在
-- migrate.rs 里按 column_exists 条件执行，因为 SQLite 的 ALTER ADD COLUMN 没有
-- IF NOT EXISTS。迁移前 migrate.rs 会用 VACUUM INTO 留一份 *.pre_5.0.bak。

-- 经历：一次任务里的一段「波折」（命令报错 → 尝试 → 恢复）或一次用户纠正。
-- 它是规则的证据，不进默认召回。id 由会话与锚点确定性生成，重复解析同一
-- transcript 不会产生重复行。
CREATE TABLE IF NOT EXISTS episodes (
    id           TEXT PRIMARY KEY,
    kind         TEXT NOT NULL CHECK(kind IN ('struggle','correction')),
    session_id   TEXT,
    project      TEXT,
    agent        TEXT,
    ts           TEXT NOT NULL,
    signals      TEXT NOT NULL DEFAULT '[]',
    trigger_text TEXT NOT NULL,
    attempts     TEXT,
    resolution   TEXT,
    state        TEXT NOT NULL DEFAULT 'new'
        CHECK(state IN ('new','ruled','no_rule','expired')),
    rule_id      TEXT,
    embedding    BLOB,
    created_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_episodes_state ON episodes(state, ts);
CREATE INDEX IF NOT EXISTS idx_episodes_rule ON episodes(rule_id);

-- 验证：某条规则的某个版本在某个会话里被展示、观察、判定的记录。
-- shown → observed（取到了随后的命令结果）→ 评审/agent 判定。
-- 只有 supported 计入成熟；contradicted 在 resolved_at 为空时暂停成熟资格。
CREATE TABLE IF NOT EXISTS rule_validations (
    id           TEXT PRIMARY KEY,
    chunk_id     TEXT NOT NULL,
    rule_version INTEGER NOT NULL DEFAULT 1,
    verdict      TEXT NOT NULL CHECK(verdict IN
        ('shown','observed','applied','supported','contradicted','irrelevant','unknown')),
    observation  TEXT,
    source       TEXT NOT NULL CHECK(source IN ('agent','judge','hook')),
    channel      TEXT,
    session_id   TEXT,
    project      TEXT,
    trace_id     TEXT,
    tool_use_id  TEXT,
    created_at   TEXT NOT NULL,
    judged_at    TEXT,
    resolved_at  TEXT
);
CREATE INDEX IF NOT EXISTS idx_rv_chunk ON rule_validations(chunk_id, verdict);
CREATE INDEX IF NOT EXISTS idx_rv_session ON rule_validations(session_id, chunk_id);
CREATE INDEX IF NOT EXISTS idx_rv_open ON rule_validations(verdict, created_at)
    WHERE verdict IN ('shown','observed');

UPDATE meta SET value='5.0' WHERE key='schema_version';
