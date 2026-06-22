-- 4.15 → 4.16 — 混合检索:为 chunks 增加 FTS5 词法/BM25 通道。
-- FTS5 的建表/触发器/回填依赖 chunks 拥有 content/trigger_desc/skill_name 列,
-- 故由 migrate.rs 在列存在守卫后条件执行 `4.16_fts.sql`(与 4.12 copy_last_used /
-- 4.15 provenance 同模式),保证对合成的部分 schema 测试夹具也不致命。
-- 本文件只负责版本号推进。
UPDATE meta SET value='4.16' WHERE key='schema_version';
