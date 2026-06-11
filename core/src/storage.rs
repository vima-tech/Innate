//! SQLite storage layer.
//!
//! Replaces sqlite-vec virtual tables with ordinary BLOB columns + pure-Rust
//! cosine similarity, keeping the schema otherwise identical to v4.5.1.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, Row};
use serde_json::Value;

use crate::errors::{InnateError, Result};
use crate::utils::{cosine_similarity, unpack_embedding};

const EXPECTED_SCHEMA_VERSION: &str = "4.5.1";

// Embedded SQL schema — no external files needed.
const SCHEMA_SQL: &str = include_str!("schema.sql");

pub struct Storage {
    pub db_path: PathBuf,
    conn: Connection,
    pub content_dim: usize,
    pub trigger_dim: usize,
}

impl Storage {
    pub fn open(db_path: impl AsRef<Path>, content_dim: usize, trigger_dim: usize) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&db_path)?;
        configure_pragmas(&conn)?;
        let mut s = Self { db_path, conn, content_dim, trigger_dim };
        s.init_schema()?;
        Ok(s)
    }

    pub fn open_readonly(db_path: impl AsRef<Path>) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();
        let conn = Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.pragma_update(None, "query_only", "ON")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let s = Self { db_path, conn, content_dim: 1024, trigger_dim: 256 };
        Ok(s)
    }

    fn init_schema(&mut self) -> Result<()> {
        let has_meta: bool = self.conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='meta'",
            [],
            |r| r.get::<_, i64>(0),
        )? > 0;

        if !has_meta {
            // Wrap schema creation in a transaction for atomicity.
            self.conn.execute_batch("BEGIN IMMEDIATE")?;
            let r = self.conn.execute_batch(SCHEMA_SQL);
            if r.is_ok() {
                self.conn.execute_batch("COMMIT")?;
            } else {
                let _ = self.conn.execute_batch("ROLLBACK");
                r?;
            }
            return Ok(());
        }

        let current: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .optional()?;

        let current = current.ok_or_else(|| {
            InnateError::Other("meta table missing schema_version".into())
        })?;

        let cur = ver_tuple(&current);
        let exp = ver_tuple(EXPECTED_SCHEMA_VERSION);

        match cur.cmp(&exp) {
            std::cmp::Ordering::Equal => Ok(()),
            std::cmp::Ordering::Greater => {
                // Forward-compat: newer schema, warn but allow.
                eprintln!(
                    "[innate] warning: db schema {current} > expected {EXPECTED_SCHEMA_VERSION}"
                );
                Ok(())
            }
            std::cmp::Ordering::Less => {
                Err(InnateError::Other(format!(
                    "Database schema {current} is older than {EXPECTED_SCHEMA_VERSION}; \
                     run `innate migrate` to upgrade."
                )))
            }
        }
    }

    // ------------------------------------------------------------------
    // Transactions
    // ------------------------------------------------------------------

    pub fn begin_immediate(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        Ok(())
    }

    pub fn commit(&self) -> Result<()> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    pub fn rollback(&self) -> Result<()> {
        self.conn.execute_batch("ROLLBACK")?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Meta
    // ------------------------------------------------------------------

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key=?", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES (?,?)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_meta_or(&self, key: &str, default: &str) -> String {
        self.get_meta(key).ok().flatten().unwrap_or_else(|| default.to_string())
    }

    // ------------------------------------------------------------------
    // Chunk CRUD
    // ------------------------------------------------------------------

    pub fn insert_chunk(&self, c: &ChunkRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO chunks (
                id, skill_name, seq, content, trigger_desc, anti_trigger_desc,
                content_hash, token_count, origin, source, maturity, related_ids,
                protected, state, state_reason, state_updated_at,
                confidence, confidence_reason, version, distilled_from, parent_id,
                selected_count, used_count, used_success_count,
                success_trace_ids_count, last_success_at, last_agg_ts,
                embed_version, created_at, updated_at, last_used_at
            ) VALUES (
                ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,
                ?13,?14,?15,?16,?17,?18,?19,?20,?21,
                ?22,?23,?24,?25,?26,?27,?28,?29,?30,?31
            )",
            params![
                c.id, c.skill_name, c.seq, c.content, c.trigger_desc,
                c.anti_trigger_desc, c.content_hash, c.token_count,
                c.origin, c.source, c.maturity, c.related_ids, c.protected,
                c.state, c.state_reason, c.state_updated_at, c.confidence,
                c.confidence_reason, c.version, c.distilled_from, c.parent_id,
                c.selected_count, c.used_count, c.used_success_count,
                c.success_trace_ids_count, c.last_success_at, c.last_agg_ts,
                c.embed_version, c.created_at, c.updated_at, c.last_used_at
            ],
        )?;
        Ok(())
    }

    pub fn insert_vec_content(&self, chunk_id: &str, emb: &[u8]) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO vec_content(chunk_id, embedding) VALUES (?,?)",
            params![chunk_id, emb],
        )?;
        Ok(())
    }

    pub fn insert_vec_trigger(&self, chunk_id: &str, emb: &[u8]) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO vec_trigger(chunk_id, embedding) VALUES (?,?)",
            params![chunk_id, emb],
        )?;
        Ok(())
    }

    pub fn get_chunk(&self, id: &str) -> Result<Option<Value>> {
        let row = self.conn.query_row(
            "SELECT * FROM chunks WHERE id=?",
            [id],
            row_to_json,
        );
        match row {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn update_chunk_state(
        &self,
        id: &str,
        state: &str,
        reason: Option<&str>,
        now: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE chunks SET state=?, state_reason=?, state_updated_at=?, updated_at=? WHERE id=?",
            params![state, reason, now, now, id],
        )?;
        Ok(())
    }

    pub fn update_chunk_confidence(&self, id: &str, conf: f64, reason: Option<&str>, now: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE chunks SET confidence=?, confidence_reason=?, updated_at=? WHERE id=?",
            params![conf, reason, now, id],
        )?;
        Ok(())
    }

    pub fn update_chunk_last_used(&self, id: &str, now: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE chunks SET last_used_at=?, updated_at=? WHERE id=?",
            params![now, now, id],
        )?;
        Ok(())
    }

    pub fn get_chunk_by_hash(&self, hash: &str) -> Result<Option<Value>> {
        let row = self.conn.query_row(
            "SELECT * FROM chunks WHERE content_hash=? LIMIT 1",
            [hash],
            row_to_json,
        );
        match row {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // ------------------------------------------------------------------
    // Vector search (pure-Rust cosine similarity, replaces sqlite-vec)
    // ------------------------------------------------------------------

    pub fn search_vec_content(&self, query: &[f32], limit: usize) -> Result<Vec<(String, f32)>> {
        self.search_vec("vec_content", query, limit)
    }

    pub fn search_vec_trigger(&self, query: &[f32], limit: usize) -> Result<Vec<(String, f32)>> {
        self.search_vec("vec_trigger", query, limit)
    }

    fn search_vec(&self, table: &str, query: &[f32], limit: usize) -> Result<Vec<(String, f32)>> {
        let sql = format!("SELECT chunk_id, embedding FROM {table}");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut results: Vec<(String, f32)> = stmt
            .query_map([], |r| {
                let id: String = r.get(0)?;
                let blob: Vec<u8> = r.get(1)?;
                Ok((id, blob))
            })?
            .filter_map(|r| r.ok())
            .map(|(id, blob)| {
                let v = unpack_embedding(&blob);
                let sim = cosine_similarity(query, &v);
                (id, sim)
            })
            .collect();
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        Ok(results)
    }

    // ------------------------------------------------------------------
    // Invalidated hashes
    // ------------------------------------------------------------------

    pub fn is_hash_invalidated(&self, hash: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT count(*) FROM invalidated_hashes WHERE content_hash=?",
            [hash],
            |r| r.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn insert_invalidated_hash(&self, hash: &str, reason: Option<&str>, ts: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO invalidated_hashes(content_hash, reason, ts) VALUES (?,?,?)",
            params![hash, reason, ts],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Usage trace
    // ------------------------------------------------------------------

    pub fn insert_usage_trace(
        &self,
        trace_id: &str,
        chunk_id: Option<&str>,
        event: &str,
        strength: f64,
        similarity: Option<f64>,
        tokens: Option<i64>,
        rank: Option<i64>,
        source: &str,
        ts: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO usage_trace
             (trace_id, chunk_id, event, strength, similarity, tokens, rank, source, ts)
             VALUES (?,?,?,?,?,?,?,?,?)",
            params![trace_id, chunk_id, event, strength, similarity, tokens, rank, source, ts],
        )?;
        Ok(())
    }

    pub fn get_outcome_for_trace(&self, trace_id: &str) -> Result<Option<String>> {
        let row = self.conn.query_row(
            "SELECT event FROM usage_trace
             WHERE trace_id=? AND event IN ('task_ok','task_fail') AND chunk_id IS NULL
             LIMIT 1",
            [trace_id],
            |r| r.get::<_, String>(0),
        );
        match row {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn purge_usage_trace(&self, before_ts: &str) -> Result<usize> {
        // Preserve spark 'retrieved' traces — they power soft-incubation counts (§二·七).
        let n = self.conn.execute(
            "DELETE FROM usage_trace
             WHERE ts < ?
             AND NOT (event = 'retrieved'
                      AND chunk_id IN (SELECT id FROM chunks WHERE origin='spark'))",
            [before_ts],
        )?;
        Ok(n)
    }

    // ------------------------------------------------------------------
    // Episodic log
    // ------------------------------------------------------------------

    pub fn upsert_episodic_log(&self, log: &EpisodicLogRow) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO episodic_log
             (id, trace_id, lib_id, ts, query, recall_snapshot, output,
              output_summary, outcome, event_source, nomination, priority,
              distill_state, distill_note)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                log.id, log.trace_id, log.lib_id, log.ts, log.query,
                log.recall_snapshot, log.output, log.output_summary,
                log.outcome, log.event_source, log.nomination, log.priority,
                log.distill_state, log.distill_note
            ],
        )?;
        Ok(())
    }

    pub fn get_episodic_log(&self, trace_id: &str) -> Result<Option<Value>> {
        let row = self.conn.query_row(
            "SELECT * FROM episodic_log WHERE trace_id=?",
            [trace_id],
            row_to_json,
        );
        match row {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn update_episodic_log_state(
        &self,
        trace_id: &str,
        state: &str,
        note: Option<&str>,
        outcome: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE episodic_log
             SET distill_state=?, distill_note=COALESCE(?,distill_note),
                 outcome=COALESCE(?,outcome),
                 distill_run_id=NULL, distill_locked_at=NULL
             WHERE trace_id=?",
            params![state, note, outcome, trace_id],
        )?;
        Ok(())
    }

    /// Update by primary-key id (used after distill where we have the row id, not trace_id).
    pub fn update_episodic_log_state_by_id(
        &self,
        id: &str,
        state: &str,
        note: Option<&str>,
        outcome: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE episodic_log
             SET distill_state=?, distill_note=COALESCE(?,distill_note),
                 outcome=COALESCE(?,outcome),
                 distill_run_id=NULL, distill_locked_at=NULL
             WHERE id=?",
            params![state, note, outcome, id],
        )?;
        Ok(())
    }

    /// Claim a batch of 'new' logs for distillation: mark them 'screening' atomically.
    /// Returns the claimed rows (with distill_run_id set to run_id).
    pub fn claim_distill_batch(&self, run_id: &str, limit: usize, locked_at: &str) -> Result<Vec<Value>> {
        // BEGIN IMMEDIATE must be held by caller; this is called inside a transaction.
        self.conn.execute(
            "UPDATE episodic_log
             SET distill_state='screening', distill_run_id=?, distill_locked_at=?
             WHERE id IN (
               SELECT id FROM episodic_log
               WHERE distill_state='new'
               ORDER BY priority DESC, ts ASC
               LIMIT ?
             )",
            params![run_id, locked_at, limit as i64],
        )?;
        self.query_json(
            "SELECT * FROM episodic_log WHERE distill_run_id=? AND distill_state='screening'",
            params![run_id],
        )
    }

    pub fn query_episodic_logs_open(&self, limit: usize) -> Result<Vec<Value>> {
        self.query_json(
            "SELECT * FROM episodic_log WHERE distill_state='new' ORDER BY priority DESC, ts ASC LIMIT ?",
            params![limit as i64],
        )
    }

    // ------------------------------------------------------------------
    // Chunk queries (aggregate / curate helpers)
    // ------------------------------------------------------------------

    pub fn query_chunks(&self, sql: &str) -> Result<Vec<Value>> {
        self.query_json(sql, params![])
    }

    pub fn query_chunks_params<P: rusqlite::Params>(&self, sql: &str, p: P) -> Result<Vec<Value>> {
        self.query_json(sql, p)
    }

    // ------------------------------------------------------------------
    // Deps
    // ------------------------------------------------------------------

    pub fn get_deps(&self, chunk_id: &str) -> Result<Vec<(String, String, Option<String>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT dst, kind, dst_lib FROM deps WHERE src=?"
        )?;
        let rows = stmt.query_map([chunk_id], |r| {
            Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?, r.get::<_,Option<String>>(2)?))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn get_reverse_deps(&self, chunk_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT src FROM deps WHERE dst=?"
        )?;
        let rows = stmt.query_map([chunk_id], |r| r.get::<_,String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn insert_dep(&self, src: &str, dst: &str, kind: &str, dst_lib: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO deps(src,dst,kind,dst_lib) VALUES (?,?,?,?)",
            params![src, dst, kind, dst_lib],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Chunk success traces (aggregate fact table)
    // ------------------------------------------------------------------

    pub fn upsert_chunk_success_trace(&self, chunk_id: &str, trace_id: &str, ts: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO chunk_success_traces(chunk_id, trace_id, ts) VALUES (?,?,?)",
            params![chunk_id, trace_id, ts],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Shared library support
    // ------------------------------------------------------------------

    pub fn attach_shared(&self, path: &str, alias: &str) -> Result<()> {
        self.conn.execute_batch(&format!(
            "ATTACH DATABASE '{}' AS '{alias}'", path.replace('\'', "''")
        ))?;
        Ok(())
    }

    pub fn lib_id(&self) -> Result<String> {
        Ok(self
            .get_meta("lib_id")?
            .unwrap_or_else(|| "unknown".to_string()))
    }

    // ------------------------------------------------------------------
    // Generic helpers
    // ------------------------------------------------------------------

    fn query_json<P: rusqlite::Params>(&self, sql: &str, p: P) -> Result<Vec<Value>> {
        let mut stmt = self.conn.prepare(sql)?;
        let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let rows = stmt.query_map(p, |r| row_to_json_with_names(r, &names))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn execute(&self, sql: &str) -> Result<()> {
        self.conn.execute_batch(sql)?;
        Ok(())
    }

    /// Execute a parameterised statement (not batch); returns rows-affected count.
    pub fn conn_execute<P: rusqlite::Params>(&self, sql: &str, p: P) -> Result<()> {
        self.conn.execute(sql, p)?;
        Ok(())
    }
}

// ------------------------------------------------------------------
// Row types
// ------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct ChunkRow {
    pub id: String,
    pub skill_name: Option<String>,
    pub seq: i64,
    pub content: String,
    pub trigger_desc: Option<String>,
    pub anti_trigger_desc: Option<String>,
    pub content_hash: String,
    pub token_count: Option<i64>,
    pub origin: String,
    pub source: Option<String>,
    pub maturity: Option<String>,
    pub related_ids: Option<String>,
    pub protected: i64,
    pub state: String,
    pub state_reason: Option<String>,
    pub state_updated_at: Option<String>,
    pub confidence: f64,
    pub confidence_reason: Option<String>,
    pub version: i64,
    pub distilled_from: Option<String>,
    pub parent_id: Option<String>,
    pub selected_count: i64,
    pub used_count: i64,
    pub used_success_count: i64,
    pub success_trace_ids_count: i64,
    pub last_success_at: Option<String>,
    pub last_agg_ts: Option<String>,
    pub embed_version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Default)]
pub struct EpisodicLogRow {
    pub id: String,
    pub trace_id: String,
    pub lib_id: String,
    pub ts: String,
    pub query: Option<String>,
    pub recall_snapshot: Option<String>,
    pub output: Option<String>,
    pub output_summary: Option<String>,
    pub outcome: Option<String>,
    pub event_source: String,
    pub nomination: Option<String>,
    pub priority: i64,
    pub distill_state: String,
    pub distill_note: Option<String>,
}

// ------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------

fn configure_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA foreign_keys=ON;
         PRAGMA synchronous=NORMAL;",
    )?;
    Ok(())
}

fn ver_tuple(v: &str) -> (u32, u32, u32) {
    let parts: Vec<u32> = v.split('.').filter_map(|s| s.parse().ok()).collect();
    (
        parts.first().copied().unwrap_or(0),
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    )
}

/// Convert a rusqlite Row to serde_json::Value using column names from statement.
fn row_to_json_with_names(row: &Row, names: &[String]) -> rusqlite::Result<Value> {
    let mut map = serde_json::Map::new();
    for (i, name) in names.iter().enumerate() {
        let v = row_value_at(row, i);
        map.insert(name.clone(), v);
    }
    Ok(Value::Object(map))
}

fn row_to_json(row: &Row) -> rusqlite::Result<Value> {
    let count = row.as_ref().column_count();
    let mut map = serde_json::Map::new();
    for i in 0..count {
        let name = row.as_ref().column_name(i)?.to_string();
        let v = row_value_at(row, i);
        map.insert(name, v);
    }
    Ok(Value::Object(map))
}

fn row_value_at(row: &Row, i: usize) -> Value {
    // Try types in preference order
    if let Ok(v) = row.get::<_, Option<String>>(i) {
        return v.map(Value::String).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.get::<_, Option<i64>>(i) {
        return v.map(|n| Value::Number(n.into())).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.get::<_, Option<f64>>(i) {
        return v
            .and_then(|f| serde_json::Number::from_f64(f))
            .map(Value::Number)
            .unwrap_or(Value::Null);
    }
    Value::Null
}

trait OptionalExt<T> {
    fn optional(self) -> rusqlite::Result<Option<T>>;
}
impl<T> OptionalExt<T> for rusqlite::Result<T> {
    fn optional(self) -> rusqlite::Result<Option<T>> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
