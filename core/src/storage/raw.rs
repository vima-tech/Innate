use super::*;

impl Storage {
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

    pub(in crate::storage) fn query_json<P: rusqlite::Params>(
        &self,
        sql: &str,
        p: P,
    ) -> Result<Vec<Value>> {
        let mut stmt = self.conn.prepare(sql)?;
        let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let rows = stmt.query_map(p, |r| row_to_json_with_names(r, &names))?;
        // Fail-closed: a row that fails to decode aborts the query rather than
        // silently vanishing from the result set.
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Execute a parameterised statement (not batch); returns rows-affected count.
    pub(crate) fn conn_execute<P: rusqlite::Params>(&self, sql: &str, p: P) -> Result<()> {
        self.conn.execute(sql, p)?;
        Ok(())
    }

    pub(crate) fn conn_execute_count<P: rusqlite::Params>(&self, sql: &str, p: P) -> Result<usize> {
        Ok(self.conn.execute(sql, p)?)
    }

    /// On-disk size of the main database file in bytes (`page_count * page_size`).
    pub fn db_size_bytes(&self) -> Result<i64> {
        let page_count: i64 = self.conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let page_size: i64 = self.conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        Ok(page_count * page_size)
    }

    /// Reclaim space freed by curate's log compaction and trace purges: checkpoint
    /// and truncate the WAL, then VACUUM to return free pages to the OS. Returns
    /// `(before_bytes, after_bytes)`. Must run outside any transaction.
    pub fn vacuum(&self) -> Result<(i64, i64)> {
        let before = self.db_size_bytes()?;
        self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        self.conn.execute_batch("VACUUM;")?;
        self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        // VACUUM rebuilds the file but preserves all rows; in-memory vector caches
        // stay valid (same data), so they are intentionally left untouched.
        let after = self.db_size_bytes()?;
        Ok((before, after))
    }
}
