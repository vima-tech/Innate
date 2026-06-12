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
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Execute a parameterised statement (not batch); returns rows-affected count.
    pub(crate) fn conn_execute<P: rusqlite::Params>(&self, sql: &str, p: P) -> Result<()> {
        self.conn.execute(sql, p)?;
        Ok(())
    }

    pub(crate) fn conn_execute_count<P: rusqlite::Params>(&self, sql: &str, p: P) -> Result<usize> {
        Ok(self.conn.execute(sql, p)?)
    }
}
