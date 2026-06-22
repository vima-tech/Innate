-- 4.16 → 4.17 — agent 来源维度:记录知识/经验来自哪个 agent 工具
-- (claude-code / codex / opencode / gemini-cli ...)。与接入通道
-- source/event_source(mcp/cli/hook/daemon/...)正交:通道说"走哪个入口",
-- agent 说"哪个 agent 产品在驱动"。可空 TEXT,不加 CHECK 枚举(agent 生态会演进)。
--
-- ALTER TABLE ADD COLUMN 无 IF NOT EXISTS,故两列由 migrate.rs 在 column_exists
-- 守卫后条件添加(与 4.12 copy_last_used / 4.15 provenance 同模式),对合成的部分
-- schema 测试夹具不致命。本文件只负责版本号推进。
UPDATE meta SET value='4.17' WHERE key='schema_version';
