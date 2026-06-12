-- 4.12 -> 4.13: bounded distillation retries and durable failure metrics.
ALTER TABLE episodic_log
  ADD COLUMN distill_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE episodic_log
  ADD COLUMN distill_last_failed_at TEXT;
ALTER TABLE evolve_requests
  ADD COLUMN last_failed_at TEXT;

CREATE INDEX IF NOT EXISTS idx_log_distill_last_failed
  ON episodic_log(distill_last_failed_at)
  WHERE distill_last_failed_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_evolve_last_failed
  ON evolve_requests(last_failed_at)
  WHERE last_failed_at IS NOT NULL;

INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '4.13');
