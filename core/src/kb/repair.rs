use super::*;

/// Outcome of [`KnowledgeBase::repair_traces`].
#[derive(Debug, Default, Clone)]
pub struct TraceRepairReport {
    /// `selected`/`retrieved` usage events deleted (daemon recalls that never
    /// injected their knowledge into a model context).
    pub daemon_events_deleted: usize,
    /// `open` episodic logs retired to `discarded` (daemon session traces +
    /// no-answer recalls that would otherwise sit in the open/distill pool).
    pub open_logs_retired: usize,
    /// Total `selected_count` across non-spark chunks before the repair.
    pub selected_before: i64,
    /// Total `selected_count` across non-spark chunks after the repair.
    pub selected_after: i64,
}

impl KnowledgeBase {
    /// One-shot data repair for trace pollution that predates the strict
    /// `selected` = "entered the model context" semantics (Priority 1).
    ///
    /// Before the fix, the daemon recalled on every session start, discarded the
    /// knowledge, and kept only the `trace_id` — yet `recall()` had already
    /// written per-chunk `selected`/`retrieved` events and an `open` episodic
    /// log. That inflated `selected_count` (which feeds the curate archive
    /// heuristic `selected_count >= N AND used_count = 0`) and stuffed the `open`
    /// pool that drives trace-completion stats. Empty hook recalls did the same.
    ///
    /// This repair, in one transaction:
    ///   1. deletes daemon-sourced `selected`/`retrieved` usage events;
    ///   2. recomputes `selected_count` from the cleaned facts (curate's formula);
    ///   3. retires orphaned `open` logs (daemon session traces + no-answer
    ///      recalls whose snapshot selected nothing) to `discarded`/`known_none`.
    ///
    /// Idempotent: a second run deletes nothing and recomputes the same counts.
    /// With `dry_run` the transaction is rolled back and the report reflects what
    /// *would* change.
    pub fn repair_traces(&self, dry_run: bool) -> Result<TraceRepairReport> {
        let sum_selected = || -> Result<i64> {
            Ok(self.storage.query_chunks_params(
                "SELECT COALESCE(SUM(selected_count), 0) AS s FROM chunks WHERE origin != 'spark'",
                rusqlite::params![],
            )?[0]["s"]
                .as_i64()
                .unwrap_or(0))
        };

        self.storage.begin_immediate()?;
        let outcome = (|| -> Result<TraceRepairReport> {
            let selected_before = sum_selected()?;

            // 1. Drop the false daemon selection facts.
            let daemon_events_deleted = self.storage.conn_execute_count(
                "DELETE FROM usage_trace
                 WHERE source = 'daemon' AND event IN ('selected', 'retrieved')",
                rusqlite::params![],
            )?;

            // 2. Recompute selected_count from the retained facts (mirrors the
            //    curate aggregate: base + post-cutoff live selected events).
            self.storage.conn_execute(
                "UPDATE chunks SET
                   selected_count = selected_count_base + COALESCE(
                     (SELECT COUNT(*) FROM usage_trace
                      WHERE chunk_id = chunks.id AND event = 'selected'
                        AND ts > COALESCE(chunks.evidence_cutoff_at, '')), 0)
                 WHERE origin != 'spark'",
                rusqlite::params![],
            )?;

            // 3. Retire orphaned open logs: daemon session traces and no-answer
            //    recalls (empty/absent selected snapshot) leave the open pool.
            let open_logs_retired = self.storage.conn_execute_count(
                "UPDATE episodic_log
                 SET distill_state = 'discarded',
                     usage_state = CASE WHEN usage_state = 'unknown'
                                        THEN 'known_none' ELSE usage_state END
                 WHERE distill_state = 'open'
                   AND (event_source = 'daemon'
                        OR recall_snapshot IS NULL
                        -- No-answer recall: nothing was surfaced. Require BOTH an
                        -- empty selected AND empty sparks list, otherwise a
                        -- spark-only recall (visible empty but sparks shown) would
                        -- be wrongly retired even though it surfaced knowledge.
                        OR (recall_snapshot LIKE '%\"selected\":[]%'
                            AND recall_snapshot LIKE '%\"sparks\":[]%'))",
                rusqlite::params![],
            )?;

            let selected_after = sum_selected()?;
            Ok(TraceRepairReport {
                daemon_events_deleted,
                open_logs_retired,
                selected_before,
                selected_after,
            })
        })();

        match outcome {
            Ok(report) => {
                if dry_run {
                    self.storage.rollback()?;
                } else {
                    self.storage.commit()?;
                }
                Ok(report)
            }
            Err(e) => {
                let _ = self.storage.rollback();
                Err(e)
            }
        }
    }
}
