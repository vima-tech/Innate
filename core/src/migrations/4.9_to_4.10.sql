-- Migration 4.9 → 4.10
-- Issue 6: incremental decay — add last_decayed_at to track when decay was last applied.
--   Previously each curate run applied full days_idle decay to the already-decayed value,
--   compounding faster than the designed 90-day half-life. Now delta_days = now - last_decayed_at.
ALTER TABLE chunks ADD COLUMN last_decayed_at TEXT;

INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '4.10');
