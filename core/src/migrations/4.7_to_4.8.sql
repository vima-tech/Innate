-- Migration 4.7 → 4.8
-- Feedback loop fixes:
--   promote_confidence_min  0.65 → 0.60  (makes auto-promotion reachable via implicit signals)
--   decay_floor             new  = 0.20  (floor below archive threshold, re-enables 3a path)

INSERT OR REPLACE INTO meta(key, value) VALUES ('curate.promote_confidence_min', '0.60');
INSERT OR IGNORE INTO meta(key, value) VALUES ('curate.decay_floor', '0.20');
INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '4.8');
