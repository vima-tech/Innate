-- 4.17 → 4.18 — 关联实体索引(SAG 启发)用于 ACT-R 扩散激活(spreading activation)。
-- 每个 chunk 贡献高信号 token(错误码/flag/路径/代码符号)作为 entity;共享同一
-- entity 的两个 chunk 即建立关联边。recall 经这些边扩散激活,把"仅靠表面相似度
-- 无法触达、但经共享实体相连"的知识也召回。无重型知识图,无 LLM:由
-- entities::extract_entities 在每次 chunk 写入时确定性填充。
--
-- 建表 DDL 自带 IF NOT EXISTS、可重复执行;存量 chunk 的实体回填由 migrate.rs 在
-- 本 SQL 之后用 Rust(extract_entities)条件执行(与 4.16 FTS 回填同模式)。
CREATE TABLE IF NOT EXISTS chunk_entities (
    chunk_id TEXT NOT NULL,
    entity   TEXT NOT NULL,
    etype    TEXT,
    weight   REAL NOT NULL DEFAULT 1.0,
    PRIMARY KEY (chunk_id, entity)
);
CREATE INDEX IF NOT EXISTS idx_chunk_entities_entity ON chunk_entities(entity);
CREATE INDEX IF NOT EXISTS idx_chunk_entities_chunk  ON chunk_entities(chunk_id);

UPDATE meta SET value='4.18' WHERE key='schema_version';
