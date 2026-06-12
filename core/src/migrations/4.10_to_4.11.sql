-- 4.10 → 4.11: drop unique constraint on chunks.distilled_from to support
-- multi-chunk distillation (one episodic_log may produce N ≥ 1 knowledge chunks).
DROP INDEX IF EXISTS idx_chunks_distilled_from;
CREATE INDEX IF NOT EXISTS idx_chunks_distilled_from ON chunks(distilled_from) WHERE distilled_from IS NOT NULL;
INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '4.11');
