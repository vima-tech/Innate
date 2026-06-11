-- v4.5.2: account Distill token cost by actual completion time.

ALTER TABLE episodic_log ADD COLUMN distill_accounted_at TEXT;

CREATE INDEX IF NOT EXISTS idx_log_distill_accounted
  ON episodic_log(distill_accounted_at)
  WHERE distill_accounted_at IS NOT NULL;

INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '4.5.2');
