-- 混合检索 FTS5 词法通道(由 migrate.rs 在列存在守卫后条件执行)。
-- standalone(自包含)FTS5:`id` UNINDEXED,检索直接返回 chunk id。可重复执行。
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    id UNINDEXED, content, trigger_desc, skill_name,
    tokenize='unicode61'
);

CREATE TRIGGER IF NOT EXISTS chunks_fts_ai AFTER INSERT ON chunks BEGIN
    INSERT INTO chunks_fts(rowid, id, content, trigger_desc, skill_name)
    VALUES (new.rowid, new.id, new.content, new.trigger_desc, new.skill_name);
END;
CREATE TRIGGER IF NOT EXISTS chunks_fts_ad AFTER DELETE ON chunks BEGIN
    DELETE FROM chunks_fts WHERE rowid = old.rowid;
END;
CREATE TRIGGER IF NOT EXISTS chunks_fts_au AFTER UPDATE ON chunks BEGIN
    DELETE FROM chunks_fts WHERE rowid = old.rowid;
    INSERT INTO chunks_fts(rowid, id, content, trigger_desc, skill_name)
    VALUES (new.rowid, new.id, new.content, new.trigger_desc, new.skill_name);
END;

-- 幂等回填:清空后从既有 chunks 全量重灌。
DELETE FROM chunks_fts;
INSERT INTO chunks_fts(rowid, id, content, trigger_desc, skill_name)
SELECT rowid, id, content, trigger_desc, skill_name FROM chunks;
