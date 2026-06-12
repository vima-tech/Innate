-- v4.7: tighten closed-loop — governance-driven archival, sustained negative
-- feedback archival, and rebalanced recall weights (context 0.05→0.15).

-- Rebalance recall scoring weights: shift 0.10 from content to context.
UPDATE meta SET value='0.55' WHERE key='recall.w_content';
UPDATE meta SET value='0.15' WHERE key='recall.w_context';

-- New curate thresholds (INSERT OR IGNORE so manual overrides are preserved).
INSERT OR IGNORE INTO meta(key, value) VALUES ('curate.governance_archive_threshold', '3');
INSERT OR IGNORE INTO meta(key, value) VALUES ('curate.negative_feedback_archive_threshold', '5');
INSERT OR IGNORE INTO meta(key, value) VALUES ('evolve.governance_pending_threshold', '3');

INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '4.7');
