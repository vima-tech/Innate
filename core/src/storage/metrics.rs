use super::*;
use serde_json::json;

/// One operation_runs row to persist (P3a, schema 4.20). Holds only an aggregatable
/// summary — never prompt/response (raw LLM detail stays in `llm_trace.log`).
pub struct OperationRun {
    pub id: String,
    pub trace_id: Option<String>,
    pub op: String,
    pub source: Option<String>,
    pub agent: Option<String>,
    pub status: String, // ok / error / timeout
    pub error_kind: Option<String>,
    pub started_at: String,
    pub duration_ms: i64,
    pub counts_json: Option<String>,
    pub params_json: Option<String>,
}

/// Raw row used for windowed aggregation in inspect().
pub struct OpRunRow {
    pub op: String,
    pub status: String,
    pub error_kind: Option<String>,
    pub duration_ms: i64,
    pub source: Option<String>,
    pub agent: Option<String>,
    pub context: Option<String>,
}

impl Storage {
    pub fn insert_operation_run(&self, run: &OperationRun) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO operation_runs
             (id, trace_id, op, source, agent, status, error_kind,
              started_at, duration_ms, counts_json, params_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                run.id,
                run.trace_id,
                run.op,
                run.source,
                run.agent,
                run.status,
                run.error_kind,
                run.started_at,
                run.duration_ms,
                run.counts_json,
                run.params_json,
            ],
        )?;
        Ok(())
    }

    /// Raw rows since a cutoff, for aggregation (incl. source/agent/context dimensions).
    /// `context` is **trace-derived** — operation_runs has no context column, so it is
    /// pulled via a correlated lookup on `episodic_log.context_key` and is therefore only
    /// populated for ops that carry a `trace_id` (e.g. record); trace-less ops → None.
    pub fn operation_runs_since(&self, since_ts: &str) -> Result<Vec<OpRunRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT op, status, error_kind, duration_ms, source, agent,
                    (SELECT el.context_key FROM episodic_log el
                     WHERE el.trace_id = operation_runs.trace_id LIMIT 1) AS context
             FROM operation_runs WHERE started_at >= ?1",
        )?;
        let rows = stmt.query_map(params![since_ts], |r| {
            Ok(OpRunRow {
                op: r.get(0)?,
                status: r.get(1)?,
                error_kind: r.get::<_, Option<String>>(2)?,
                duration_ms: r.get(3)?,
                source: r.get::<_, Option<String>>(4)?,
                agent: r.get::<_, Option<String>>(5)?,
                context: r.get::<_, Option<String>>(6)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn count_operation_runs(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM operation_runs", [], |r| r.get(0))?)
    }

    /// Retention: drop run rows older than `before_ts` (called by curate).
    pub fn purge_operation_runs(&self, before_ts: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM operation_runs WHERE started_at < ?1",
            params![before_ts],
        )?)
    }

    pub fn insert_metric_snapshot(&self, ts: &str, kpis_json: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO metric_snapshots(ts, kpis) VALUES (?1, ?2)",
            params![ts, kpis_json],
        )?;
        Ok(())
    }

    /// Most recent snapshot `(ts, kpis_json)`, if any.
    pub fn latest_snapshot(&self) -> Result<Option<(String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT ts, kpis FROM metric_snapshots ORDER BY ts DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// Recent snapshots newest-first as `{ts, kpis:{…}}` rows (for the Web trend view).
    pub fn recent_snapshots(&self, limit: usize) -> Result<Vec<serde_json::Value>> {
        let mut stmt = self
            .conn
            .prepare("SELECT ts, kpis FROM metric_snapshots ORDER BY ts DESC LIMIT ?1")?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        Ok(rows
            .filter_map(|r| r.ok())
            .map(|(ts, kpis)| {
                let parsed: serde_json::Value =
                    serde_json::from_str(&kpis).unwrap_or(serde_json::Value::Null);
                json!({ "ts": ts, "kpis": parsed })
            })
            .collect())
    }

    /// Nearest snapshot at or before `ts` — the trend baseline for week-over-week deltas.
    pub fn snapshot_at_or_before(&self, ts: &str) -> Result<Option<(String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT ts, kpis FROM metric_snapshots WHERE ts <= ?1 ORDER BY ts DESC LIMIT 1",
                params![ts],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }
}

/// Map an `InnateError` to a bounded, aggregatable `error_kind` (design doc §5.3.1).
/// The vocabulary is closed so the "error_kind top list" groups cleanly; new failure
/// types must extend this table rather than inventing strings at the call site.
pub fn classify_error(e: &crate::errors::InnateError) -> &'static str {
    use crate::errors::InnateError as E;
    match e {
        E::EmbeddingUnavailable(m) => {
            if has_arrearage(m) {
                "embedding_arrearage"
            } else {
                "embedding_unavailable"
            }
        }
        E::Db(_) => {
            let s = e.to_string().to_lowercase();
            if s.contains("locked") || s.contains("busy") {
                "db_locked"
            } else {
                "db_error"
            }
        }
        E::Json(_) => "json_parse",
        E::ChunkNotFound(_) => "chunk_not_found",
        E::InvalidState(_) => "invalid_state",
        E::Io(_) => "io_error",
        E::Other(m) => classify_message(m),
    }
}

/// Classify a free-text error message (daemon shell-out / HTTP wrappers / `Other`).
pub fn classify_message(m: &str) -> &'static str {
    let lm = m.to_lowercase();
    if has_arrearage(m) {
        "embedding_arrearage"
    } else if lm.contains("no such file") || lm.contains("text file busy") {
        "spawn_failed"
    } else if lm.contains("timeout") || lm.contains("deadline") {
        "llm_timeout"
    } else if lm.contains("status: 4") || lm.contains("status: 5") || lm.contains("http error") {
        "llm_http_error"
    } else if lm.contains("locked") || lm.contains("busy") {
        "db_locked"
    } else {
        "other"
    }
}

fn has_arrearage(m: &str) -> bool {
    let lm = m.to_lowercase();
    lm.contains("arrearage") || m.contains("欠费")
}

/// Aggregate windowed operation rows into a JSON summary: count + p50/p95 latency +
/// ok/error/timeout split **broken down by op / source / agent / context** (design doc
/// §5.3 "按 source/agent/context 分解性能"), plus a global `error_kind` top list. Pure
/// function over already-fetched rows so it is unit-testable without a db. `by_context`
/// is trace-derived (operation_runs has no context column) so it only covers ops that
/// carry a trace_id — group_perf skips None keys, so trace-less ops just don't appear.
pub fn aggregate_ops(rows: &[OpRunRow]) -> serde_json::Value {
    let mut err_kind: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    for r in rows {
        if r.status != "ok" {
            if let Some(k) = r.error_kind.as_deref() {
                *err_kind.entry(k).or_insert(0) += 1;
            }
        }
    }
    let mut top: Vec<(&str, i64)> = err_kind.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    let error_kind_top: Vec<serde_json::Value> = top
        .into_iter()
        .take(10)
        .map(|(k, n)| json!({"error_kind": k, "count": n}))
        .collect();
    json!({
        "by_op": group_perf(rows, |r| Some(r.op.as_str())),
        "by_source": group_perf(rows, |r| r.source.as_deref()),
        "by_agent": group_perf(rows, |r| r.agent.as_deref()),
        "by_context": group_perf(rows, |r| r.context.as_deref()),
        "error_kind_top": error_kind_top,
    })
}

/// Group rows by a key extractor and emit per-group count + status split + p50/p95.
/// Rows whose key is `None` (e.g. an unattributed source/agent) are skipped, so the
/// breakdown only reports dimensions that were actually recorded.
fn group_perf<'a>(
    rows: &'a [OpRunRow],
    key: impl Fn(&'a OpRunRow) -> Option<&'a str>,
) -> serde_json::Value {
    use std::collections::HashMap;
    let mut g: HashMap<&str, (Vec<i64>, i64, i64, i64)> = HashMap::new();
    for r in rows {
        let Some(k) = key(r) else { continue };
        let e = g.entry(k).or_default();
        e.0.push(r.duration_ms);
        match r.status.as_str() {
            "ok" => e.1 += 1,
            "timeout" => e.3 += 1,
            _ => e.2 += 1,
        }
    }
    let mut out = serde_json::Map::new();
    for (k, (mut durs, ok, err, timeout)) in g {
        durs.sort_unstable();
        let total = ok + err + timeout;
        out.insert(
            k.to_string(),
            json!({
                "count": total,
                "ok": ok,
                "error": err,
                "timeout": timeout,
                "success_rate": if total > 0 { (ok as f64 / total as f64 * 1000.0).round() / 1000.0 } else { 0.0 },
                "p50_ms": percentile(&durs, 50),
                "p95_ms": percentile(&durs, 95),
            }),
        );
    }
    serde_json::Value::Object(out)
}

/// Nearest-rank percentile over a pre-sorted ascending slice. Empty → 0.
fn percentile(sorted: &[i64], p: usize) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p * sorted.len() + 99) / 100; // ceil(p% * n), nearest-rank
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}
