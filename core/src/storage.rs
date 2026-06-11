//! SQLite storage layer.
//!
//! Replaces sqlite-vec virtual tables with ordinary BLOB columns + pure-Rust
//! cosine similarity, keeping the schema otherwise aligned with v4.5.x.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, Row};
use serde_json::Value;

use crate::errors::{InnateError, Result};
use crate::utils::{cosine_similarity, unpack_embedding};

const EXPECTED_SCHEMA_VERSION: &str = "4.6";

// Embedded SQL schema — no external files needed.
const SCHEMA_SQL: &str = include_str!("schema.sql");

type VectorEntries = Vec<(String, Vec<f32>)>;
type VectorCache = RefCell<Option<VectorEntries>>;

pub struct Storage {
    pub db_path: PathBuf,
    conn: Connection,
    pub content_dim: usize,
    pub trigger_dim: usize,
    /// Pre-parsed in-memory caches for vector search; None = cold (not loaded or invalidated).
    vec_content_cache: VectorCache,
    vec_trigger_cache: VectorCache,
    /// Last observed vector revision. Only vector writes advance this value.
    vector_cache_revision: Cell<Option<i64>>,
}

impl Storage {
    pub fn open(db_path: impl AsRef<Path>, content_dim: usize, trigger_dim: usize) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&db_path)?;
        configure_pragmas(&conn)?;
        let mut s = Self {
            db_path,
            conn,
            content_dim,
            trigger_dim,
            vec_content_cache: RefCell::new(None),
            vec_trigger_cache: RefCell::new(None),
            vector_cache_revision: Cell::new(None),
        };
        s.init_schema()?;
        Ok(s)
    }

    pub fn open_readonly(db_path: impl AsRef<Path>) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();
        let conn = Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.pragma_update(None, "query_only", "ON")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let s = Self {
            db_path,
            conn,
            content_dim: 1024,
            trigger_dim: 256,
            vec_content_cache: RefCell::new(None),
            vec_trigger_cache: RefCell::new(None),
            vector_cache_revision: Cell::new(None),
        };
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

        let current = current
            .ok_or_else(|| InnateError::Other("meta table missing schema_version".into()))?;

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
                // Delegate to the proper migration chain which handles all steps atomically.
                let applied = crate::migrate::run_migrations(&self.db_path)?;
                if !applied.is_empty() {
                    eprintln!("[innate] auto-migrated: {}", applied.join(", "));
                }
                Ok(())
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
        self.get_meta(key)
            .ok()
            .flatten()
            .unwrap_or_else(|| default.to_string())
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
                c.id,
                c.skill_name,
                c.seq,
                c.content,
                c.trigger_desc,
                c.anti_trigger_desc,
                c.content_hash,
                c.token_count,
                c.origin,
                c.source,
                c.maturity,
                c.related_ids,
                c.protected,
                c.state,
                c.state_reason,
                c.state_updated_at,
                c.confidence,
                c.confidence_reason,
                c.version,
                c.distilled_from,
                c.parent_id,
                c.selected_count,
                c.used_count,
                c.used_success_count,
                c.success_trace_ids_count,
                c.last_success_at,
                c.last_agg_ts,
                c.embed_version,
                c.created_at,
                c.updated_at,
                c.last_used_at
            ],
        )?;
        Ok(())
    }

    pub fn insert_vec_content(&self, chunk_id: &str, emb: &[u8]) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO vec_content(chunk_id, embedding) VALUES (?,?)",
            params![chunk_id, emb],
        )?;
        *self.vec_content_cache.borrow_mut() = None;
        Ok(())
    }

    pub fn insert_vec_trigger(&self, chunk_id: &str, emb: &[u8]) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO vec_trigger(chunk_id, embedding) VALUES (?,?)",
            params![chunk_id, emb],
        )?;
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES ('vector_revision', '1')
             ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1",
            [],
        )?;
        *self.vec_trigger_cache.borrow_mut() = None;
        Ok(())
    }

    pub fn get_chunk(&self, id: &str) -> Result<Option<Value>> {
        let row = self
            .conn
            .query_row("SELECT * FROM chunks WHERE id=?", [id], row_to_json);
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

    pub fn update_chunk_confidence(
        &self,
        id: &str,
        conf: f64,
        reason: Option<&str>,
        now: &str,
    ) -> Result<()> {
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
        self.search_vec(&self.vec_content_cache, "vec_content", query, limit)
    }

    pub fn search_vec_trigger(&self, query: &[f32], limit: usize) -> Result<Vec<(String, f32)>> {
        self.search_vec(&self.vec_trigger_cache, "vec_trigger", query, limit)
    }

    fn search_vec(
        &self,
        cache_cell: &VectorCache,
        table: &str,
        query: &[f32],
        limit: usize,
    ) -> Result<Vec<(String, f32)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        self.refresh_vector_caches_if_changed()?;

        // Populate cache on first access after open or invalidation.
        if cache_cell.borrow().is_none() {
            let sql = format!("SELECT chunk_id, embedding FROM {table}");
            let mut stmt = self.conn.prepare(&sql)?;
            let entries: Vec<(String, Vec<f32>)> = stmt
                .query_map([], |r| {
                    let id: String = r.get(0)?;
                    let blob: Vec<u8> = r.get(1)?;
                    Ok((id, blob))
                })?
                .filter_map(|r| r.ok())
                .map(|(id, blob)| (id, unpack_embedding(&blob)))
                .collect();
            *cache_cell.borrow_mut() = Some(entries);
        }

        let cache = cache_cell.borrow();
        let entries = cache.as_ref().unwrap();

        // Compute similarities, then partial-sort to bring top-limit to the front (O(N log K)).
        let mut results: Vec<(String, f32)> = entries
            .iter()
            .map(|(id, v)| (id.clone(), cosine_similarity(query, v)))
            .collect();
        if results.len() > limit {
            results.select_nth_unstable_by(limit - 1, |a, b| {
                b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
            });
            results.truncate(limit);
        }
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(results)
    }

    fn refresh_vector_caches_if_changed(&self) -> Result<()> {
        let current = self
            .get_meta("vector_revision")?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let previous = self.vector_cache_revision.replace(Some(current));
        if previous.is_some_and(|revision| revision != current) {
            *self.vec_content_cache.borrow_mut() = None;
            *self.vec_trigger_cache.borrow_mut() = None;
        }
        Ok(())
    }

    /// Fetch multiple chunks by id in one query; returns a map of id → chunk JSON.
    pub fn get_chunks_by_ids(&self, ids: &[&str]) -> Result<HashMap<String, Value>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("SELECT * FROM chunks WHERE id IN ({placeholders})");
        let mut stmt = self.conn.prepare(&sql)?;
        let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| {
            row_to_json_with_names(r, &names)
        })?;
        let mut map = HashMap::with_capacity(ids.len());
        for row in rows.filter_map(|r| r.ok()) {
            if let Some(id) = row.get("id").and_then(Value::as_str) {
                map.insert(id.to_string(), row);
            }
        }
        Ok(map)
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

    pub fn insert_invalidated_hash(
        &self,
        hash: &str,
        reason: Option<&str>,
        ts: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO invalidated_hashes(content_hash, reason, ts) VALUES (?,?,?)",
            params![hash, reason, ts],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Usage trace
    // ------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn insert_usage_trace(
        &self,
        trace_id: &str,
        chunk_id: Option<&str>,
        event: &str,
        strength: f64,
        similarity: Option<f64>,
        refine_mode: Option<&str>,
        tokens: Option<i64>,
        rank: Option<i64>,
        attribution: Option<&str>,
        source: &str,
        ts: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO usage_trace
             (trace_id, chunk_id, event, strength, similarity, refine_mode, tokens, rank, attribution, source, ts)
             VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            params![trace_id, chunk_id, event, strength, similarity, refine_mode, tokens, rank, attribution, source, ts],
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
              output_summary, outcome, event_source, task_state, completed_at,
              usage_state, used_ids, used_attribution, context_key, nomination, priority,
              distill_state, distill_note)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
            params![
                log.id,
                log.trace_id,
                log.lib_id,
                log.ts,
                log.query,
                log.recall_snapshot,
                log.output,
                log.output_summary,
                log.outcome,
                log.event_source,
                log.task_state,
                log.completed_at,
                log.usage_state,
                log.used_ids,
                log.used_attribution,
                log.context_key,
                log.nomination,
                log.priority,
                log.distill_state,
                log.distill_note
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

    /// Patch content fields on an existing episodic_log row (補写: output_summary, nomination, etc.)
    pub fn patch_episodic_log_content(
        &self,
        trace_id: &str,
        query: Option<&str>,
        output: Option<&str>,
        output_summary: Option<&str>,
        nomination: Option<&str>,
        priority: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE episodic_log
             SET output_summary = COALESCE(?, output_summary),
                 nomination     = COALESCE(?, nomination),
                 output         = COALESCE(?, output),
                 query          = COALESCE(?, query),
                 priority       = MAX(priority, ?)
             WHERE trace_id = ?",
            params![
                output_summary,
                nomination,
                output,
                query,
                priority,
                trace_id
            ],
        )?;
        Ok(())
    }

    pub fn update_trace_lifecycle(
        &self,
        trace_id: &str,
        task_state: &str,
        completed_at: Option<&str>,
        usage_state: Option<&str>,
        used_ids: Option<&str>,
        used_attribution: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE episodic_log
             SET task_state=?,
                 completed_at=COALESCE(?, completed_at),
                 usage_state=COALESCE(?, usage_state),
                 used_ids=COALESCE(?, used_ids),
                 used_attribution=COALESCE(?, used_attribution)
             WHERE trace_id=?",
            params![
                task_state,
                completed_at,
                usage_state,
                used_ids,
                used_attribution,
                trace_id
            ],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_feedback_event(
        &self,
        id: &str,
        trace_id: &str,
        chunk_id: &str,
        signal: &str,
        strength: f64,
        source: &str,
        actor: Option<&str>,
        reason: Option<&str>,
        context_key: Option<&str>,
        ts: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO feedback_events
             (id, trace_id, chunk_id, signal, strength, source, actor, reason, context_key, ts)
             VALUES (?,?,?,?,?,?,?,?,?,?)",
            params![
                id,
                trace_id,
                chunk_id,
                signal,
                strength,
                source,
                actor,
                reason,
                context_key,
                ts
            ],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_context_stat(
        &self,
        chunk_id: &str,
        context_key: &str,
        success: i64,
        failure: i64,
        positive: i64,
        negative: i64,
        now: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO chunk_context_stats
             (chunk_id, context_key, success_count, failure_count,
              positive_feedback, negative_feedback, last_updated_at)
             VALUES (?,?,?,?,?,?,?)
             ON CONFLICT(chunk_id, context_key) DO UPDATE SET
               success_count=success_count+excluded.success_count,
               failure_count=failure_count+excluded.failure_count,
               positive_feedback=positive_feedback+excluded.positive_feedback,
               negative_feedback=negative_feedback+excluded.negative_feedback,
               last_updated_at=excluded.last_updated_at",
            params![
                chunk_id,
                context_key,
                success,
                failure,
                positive,
                negative,
                now
            ],
        )?;
        Ok(())
    }

    pub fn context_score(&self, chunk_id: &str, context_key: &str) -> Result<f64> {
        let row = self
            .conn
            .query_row(
                "SELECT success_count, failure_count, positive_feedback, negative_feedback
                 FROM chunk_context_stats WHERE chunk_id=? AND context_key=?",
                params![chunk_id, context_key],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((success, failure, positive, negative)) = row else {
            return Ok(0.0);
        };
        let wins = success as f64 + positive as f64 * 2.0;
        let losses = failure as f64 + negative as f64 * 2.0;
        let evidence = wins + losses;
        let posterior = (wins + 1.0) / (evidence + 2.0);
        let evidence_weight = (evidence / 5.0).min(1.0);
        Ok((posterior - 0.5) * 2.0 * evidence_weight)
    }

    pub fn upsert_governance_proposal(
        &self,
        id: &str,
        chunk_id: &str,
        proposal_type: &str,
        reason: &str,
        evidence_count: i64,
        now: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO governance_proposals
             (id, chunk_id, proposal_type, reason, evidence_count, state, created_at, updated_at)
             VALUES (?,?,?,?,?,'pending',?,?)
             ON CONFLICT(chunk_id, proposal_type) WHERE state='pending'
             DO UPDATE SET reason=excluded.reason,
                           evidence_count=excluded.evidence_count,
                           updated_at=excluded.updated_at",
            params![
                id,
                chunk_id,
                proposal_type,
                reason,
                evidence_count,
                now,
                now
            ],
        )?;
        Ok(())
    }

    pub fn request_evolve(&self, id: &str, reason: &str, now: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO evolve_requests(id, reason, state, requested_at)
             VALUES (?,?,'pending',?)",
            params![id, reason, now],
        )?;
        Ok(())
    }

    pub fn claim_evolve_request(&self, now: &str, stale_before: &str) -> Result<Option<String>> {
        self.conn.execute(
            "UPDATE evolve_requests
             SET state='pending', leased_at=NULL, note='lease_recovered'
             WHERE state='running' AND leased_at < ?",
            [stale_before],
        )?;
        Ok(self
            .conn
            .query_row(
                "UPDATE evolve_requests
                 SET state='running', leased_at=?
                 WHERE id=(
                   SELECT id FROM evolve_requests
                   WHERE state='pending' ORDER BY requested_at ASC LIMIT 1
                 ) AND state='pending'
                 RETURNING id",
                [now],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn finish_evolve_request(
        &self,
        id: &str,
        state: &str,
        note: Option<&str>,
        now: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE evolve_requests SET state=?, completed_at=?, note=? WHERE id=?",
            params![state, now, note, id],
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

    #[allow(clippy::too_many_arguments)]
    pub fn finish_distill_log(
        &self,
        id: &str,
        state: &str,
        note: Option<&str>,
        prompt_tokens: i64,
        completion_tokens: i64,
        accounted_at: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE episodic_log
             SET distill_state=?, distill_note=?,
                 distill_prompt_tokens=?, distill_completion_tokens=?,
                 distill_accounted_at=?,
                 distill_run_id=NULL, distill_locked_at=NULL
             WHERE id=?",
            params![
                state,
                note,
                prompt_tokens,
                completion_tokens,
                accounted_at,
                id
            ],
        )?;
        Ok(())
    }

    /// Claim a batch of 'new' logs for distillation: mark them 'screening' atomically.
    /// Returns the claimed rows (with distill_run_id set to run_id).
    pub fn claim_distill_batch(
        &self,
        run_id: &str,
        limit: usize,
        locked_at: &str,
    ) -> Result<Vec<Value>> {
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
        let mut stmt = self
            .conn
            .prepare("SELECT dst, kind, dst_lib FROM deps WHERE src=?")?;
        let rows = stmt.query_map([chunk_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn get_reverse_deps(&self, chunk_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT src FROM deps WHERE dst=?")?;
        let rows = stmt.query_map([chunk_id], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn insert_dep(
        &self,
        src: &str,
        dst: &str,
        kind: &str,
        dst_lib: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO deps(src,dst,kind,dst_lib) VALUES (?,?,?,?)",
            params![src, dst, kind, dst_lib],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Chunk success traces (aggregate fact table)
    // ------------------------------------------------------------------

    pub fn upsert_chunk_success_trace(
        &self,
        chunk_id: &str,
        trace_id: &str,
        ts: &str,
    ) -> Result<()> {
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
            "ATTACH DATABASE '{}' AS '{alias}'",
            path.replace('\'', "''")
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
    pub task_state: String,
    pub completed_at: Option<String>,
    pub usage_state: String,
    pub used_ids: Option<String>,
    pub used_attribution: Option<String>,
    pub context_key: Option<String>,
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
         PRAGMA synchronous=NORMAL;
         PRAGMA cache_size=-65536;
         PRAGMA mmap_size=268435456;
         PRAGMA temp_store=memory;",
    )?;
    // Validate WAL mode was accepted (some VFS/filesystems silently downgrade).
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    if mode != "wal" {
        return Err(crate::errors::InnateError::Other(format!(
            "WAL mode required but got '{mode}'; check filesystem support"
        )));
    }
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
            .and_then(serde_json::Number::from_f64)
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
