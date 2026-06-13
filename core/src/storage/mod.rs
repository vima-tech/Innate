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
use crate::utils::{dot_product, l2_normalize, unpack_embedding};

mod chunks;
mod evolution;
mod meta;
mod raw;
mod traces;

const EXPECTED_SCHEMA_VERSION: &str = "4.14";

// Embedded SQL schema — no external files needed.
const SCHEMA_SQL: &str = include_str!("../schema.sql");

type VectorEntries = Vec<(String, Vec<f32>)>;
type VectorCache = RefCell<Option<VectorEntries>>;

/// A single dependency edge: `(dst, kind, dst_lib)`.
pub type DepEdge = (String, String, Option<String>);

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvolveRequestClaim {
    pub id: String,
    pub reason: String,
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
        // In-place cache upserts from the aborted transaction may not have
        // persisted; drop caches so the next search reloads committed state.
        self.invalidate_vector_caches();
        Ok(())
    }

    // ------------------------------------------------------------------
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
    pub distill_provider: Option<String>,
    pub distill_model: Option<String>,
    pub distill_prompt_version: Option<String>,
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
    pub used_complete: bool,
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
         PRAGMA busy_timeout=5000;
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
