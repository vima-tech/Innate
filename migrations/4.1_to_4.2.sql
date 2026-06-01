-- migrations/4.1_to_4.2.sql
ALTER TABLE episodic_log ADD COLUMN output_summary TEXT;
CREATE TABLE IF NOT EXISTS chunk_success_traces (
    chunk_id  TEXT NOT NULL,
    trace_id  TEXT NOT NULL,
    ts        TEXT NOT NULL,
    PRIMARY KEY (chunk_id, trace_id)
);
CREATE INDEX IF NOT EXISTS idx_cst_chunk ON chunk_success_traces(chunk_id);
INSERT OR REPLACE INTO meta VALUES ('schema_version', '4.2');
