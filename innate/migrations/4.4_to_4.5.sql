-- migrations/4.4_to_4.5.sql

-- 1. episodic_log.event_source:ADD COLUMN,不 DROP/RENAME 旧 source 字段
ALTER TABLE episodic_log ADD COLUMN event_source TEXT NOT NULL DEFAULT 'sdk';

UPDATE episodic_log
SET event_source =
  CASE
    WHEN source IN ('sdk','cli','hook','daemon','augmented') THEN source
    ELSE 'sdk'
  END;

-- 2. episodic_log 加 distill_run_id / distill_locked_at
ALTER TABLE episodic_log ADD COLUMN distill_run_id   TEXT;
ALTER TABLE episodic_log ADD COLUMN distill_locked_at TEXT;

-- 3. 更新 schema_version
INSERT OR REPLACE INTO meta VALUES ('schema_version', '4.5');
