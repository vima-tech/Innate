use super::*;

impl Storage {
    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT value FROM meta WHERE key=?")?;
        Ok(stmt.query_row([key], |r| r.get(0)).optional()?)
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
}
