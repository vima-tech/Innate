-- Migration 4.8 → 4.9
-- Issue 1: feedback_events idempotent key
--   Remove duplicate rows (keep earliest by rowid), then add UNIQUE constraint
--   so retried record() calls don't double-count confidence/context/governance.
DELETE FROM feedback_events
WHERE rowid NOT IN (
    SELECT MIN(rowid) FROM feedback_events GROUP BY trace_id, chunk_id, signal
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_feedback_idempotent
  ON feedback_events(trace_id, chunk_id, signal);

-- Issue 6: governance_proposals now tracks 'rejected' outcome (schema already has
-- the check constraint allowing 'accepted'|'rejected'|'pending'; no DDL needed
-- here since 'rejected' was always a valid value per the CHECK constraint).
-- The Rust curate 3d fix sets state='rejected' for ineligible chunks instead of
-- unconditionally setting 'accepted'.

INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '4.9');
