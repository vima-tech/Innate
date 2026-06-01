-- migrations/4.0_to_4.1.sql
ALTER TABLE chunks ADD COLUMN used_success_count    INTEGER NOT NULL DEFAULT 0;
ALTER TABLE chunks ADD COLUMN success_trace_ids_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE chunks ADD COLUMN last_success_at       TEXT;
ALTER TABLE usage_trace ADD COLUMN source TEXT;
CREATE INDEX IF NOT EXISTS idx_chunks_embed_v ON chunks(embed_version);
CREATE INDEX IF NOT EXISTS idx_trace_source   ON usage_trace(source);
INSERT OR REPLACE INTO meta VALUES ('schema_version', '4.1');
