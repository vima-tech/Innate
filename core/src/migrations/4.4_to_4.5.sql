-- migrations/4.4_to_4.5.sql
-- v4.5: episodic_log event_source (ADD COLUMN, don't drop old source) +
--       distill_run_id / distill_locked_at.

-- 1. Add event_source (ADD only — never DROP/RENAME the old source field).
ALTER TABLE episodic_log ADD COLUMN event_source TEXT NOT NULL DEFAULT 'sdk';
UPDATE episodic_log
SET event_source =
  CASE
    WHEN source IN ('sdk','cli','hook','daemon','augmented') THEN source
    ELSE 'sdk'
  END;

-- 2. Add distill claim columns.
ALTER TABLE episodic_log ADD COLUMN distill_run_id    TEXT;
ALTER TABLE episodic_log ADD COLUMN distill_locked_at TEXT;

INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '4.5');
