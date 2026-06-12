use super::*;

impl Storage {
    pub fn insert_chunk(&self, c: &ChunkRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO chunks (
                id, skill_name, seq, content, trigger_desc, anti_trigger_desc,
                content_hash, token_count, origin, source, maturity, related_ids,
                protected, state, state_reason, state_updated_at,
                confidence, confidence_base, confidence_reason, version, distilled_from,
                distill_provider, distill_model, distill_prompt_version, parent_id,
                selected_count, used_count, used_success_count,
                success_trace_ids_count, last_success_at, last_agg_ts,
                embed_version, created_at, updated_at, last_used_at
            ) VALUES (
                ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,
                ?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,
                ?26,?27,?28,?29,?30,?31,?32,?33,?34,?35
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
                c.confidence,
                c.confidence_reason,
                c.version,
                c.distilled_from,
                c.distill_provider,
                c.distill_model,
                c.distill_prompt_version,
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
            "UPDATE chunks
             SET confidence=?, confidence_base=?, confidence_reason=?, updated_at=?
             WHERE id=?",
            params![conf, conf, reason, now, id],
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

    // Chunk queries (aggregate / curate helpers)
    // ------------------------------------------------------------------

    pub(crate) fn query_chunks(&self, sql: &str) -> Result<Vec<Value>> {
        self.query_json(sql, params![])
    }

    pub(crate) fn query_chunks_params<P: rusqlite::Params>(
        &self,
        sql: &str,
        p: P,
    ) -> Result<Vec<Value>> {
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
}
