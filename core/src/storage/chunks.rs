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
        self.note_vector_write(&self.vec_content_cache, chunk_id, emb)
    }

    pub fn insert_vec_trigger(&self, chunk_id: &str, emb: &[u8]) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO vec_trigger(chunk_id, embedding) VALUES (?,?)",
            params![chunk_id, emb],
        )?;
        self.note_vector_write(&self.vec_trigger_cache, chunk_id, emb)
    }

    /// Record a single vector write: bump the shared revision (so *other*
    /// processes drop their caches), upsert the one entry into the warm cache
    /// in place (so *this* long-lived process keeps its cache instead of
    /// reloading the whole corpus), then re-sync the local revision tracker so
    /// our own bump does not trip `refresh_vector_caches_if_changed`.
    ///
    /// A cold cache (None) is left cold — the next search loads everything,
    /// including this row. This keeps bulk paths (e.g. full re-embed) O(N) by
    /// invalidating once up front rather than upserting per write.
    fn note_vector_write(&self, cache: &VectorCache, chunk_id: &str, emb: &[u8]) -> Result<()> {
        self.bump_vector_revision()?;
        if let Some(entries) = cache.borrow_mut().as_mut() {
            let mut v = unpack_embedding(emb);
            l2_normalize(&mut v);
            match entries.iter_mut().find(|(id, _)| id == chunk_id) {
                Some(slot) => slot.1 = v,
                None => entries.push((chunk_id.to_string(), v)),
            }
        }
        self.sync_vector_revision()
    }

    /// Monotonically advance `meta.vector_revision`. Any vector write must call
    /// this so that other processes detect the change and drop their in-memory
    /// caches on the next search (see `refresh_vector_caches_if_changed`).
    fn bump_vector_revision(&self) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES ('vector_revision', '1')
             ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1",
            [],
        )?;
        Ok(())
    }

    /// Align the local revision tracker with the persisted value so an in-place
    /// cache update performed by this process is not discarded on the next search.
    fn sync_vector_revision(&self) -> Result<()> {
        let current = self
            .get_meta("vector_revision")?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        self.vector_cache_revision.set(Some(current));
        Ok(())
    }

    /// Drop both in-memory vector caches and reset the revision tracker. Used on
    /// transaction rollback (in-place upserts may not have persisted) and before
    /// bulk re-embed loops (to avoid O(N²) in-place upserts on a warm cache).
    pub(crate) fn invalidate_vector_caches(&self) {
        *self.vec_content_cache.borrow_mut() = None;
        *self.vec_trigger_cache.borrow_mut() = None;
        self.vector_cache_revision.set(None);
    }

    /// Paginated chunk listing for the web viewer. Filters by exact `state` and
    /// `origin` when provided; returns a compact projection (content truncated to
    /// a preview) ordered newest-first. Read-only — never mutates.
    pub fn list_chunks(
        &self,
        state: Option<&str>,
        origin: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Value>> {
        let mut sql = String::from(
            "SELECT id, skill_name, seq, origin, state, state_reason, maturity, \
             confidence, token_count, protected, selected_count, used_count, \
             used_success_count, substr(content, 1, 280) AS content_preview, \
             created_at, updated_at, last_used_at \
             FROM chunks",
        );
        let mut clauses: Vec<&str> = Vec::new();
        if state.is_some() {
            clauses.push("state = :state");
        }
        if origin.is_some() {
            clauses.push("origin = :origin");
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY created_at DESC LIMIT :limit OFFSET :offset");

        let mut stmt = self.conn.prepare(&sql)?;
        let names: Vec<String> = stmt.column_names().into_iter().map(String::from).collect();
        let mut params: Vec<(&str, &dyn rusqlite::ToSql)> = Vec::new();
        if let Some(s) = state.as_ref() {
            params.push((":state", s));
        }
        if let Some(o) = origin.as_ref() {
            params.push((":origin", o));
        }
        let limit_i = limit as i64;
        let offset_i = offset as i64;
        params.push((":limit", &limit_i));
        params.push((":offset", &offset_i));

        let rows = stmt.query_map(params.as_slice(), |r| row_to_json_with_names(r, &names))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn get_chunk(&self, id: &str) -> Result<Option<Value>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT * FROM chunks WHERE id=?")?;
        let row = stmt.query_row([id], row_to_json);
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
        // Stored vectors are L2-normalised here so the search inner loop can use
        // a plain dot product instead of recomputing norms on every comparison.
        if cache_cell.borrow().is_none() {
            let sql = format!("SELECT chunk_id, embedding FROM {table}");
            let mut stmt = self.conn.prepare(&sql)?;
            let raw: Vec<(String, Vec<u8>)> = stmt
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut entries: Vec<(String, Vec<f32>)> = Vec::with_capacity(raw.len());
            for (id, blob) in raw {
                // Fail-closed: a persisted embedding must be a whole number of
                // f32 values (4 bytes each). A structurally corrupt blob aborts
                // the load rather than silently yielding a truncated vector.
                if blob.is_empty() || blob.len() % 4 != 0 {
                    return Err(crate::errors::InnateError::Other(format!(
                        "corrupt embedding for chunk {id} in {table}: {} bytes (not a non-zero multiple of 4)",
                        blob.len()
                    )));
                }
                let mut v = unpack_embedding(&blob);
                l2_normalize(&mut v);
                entries.push((id, v));
            }
            *cache_cell.borrow_mut() = Some(entries);
        }

        let cache = cache_cell.borrow();
        let entries = cache.as_ref().unwrap();

        // Normalise the query once; cached vectors are already unit-length, so
        // cosine similarity reduces to a dot product over each entry.
        let mut q = query.to_vec();
        l2_normalize(&mut q);

        // Score by (index, similarity) without cloning ids; partial-sort the top
        // `limit` to the front (O(N) select), then clone ids for the winners only.
        // Only score vectors whose dimension matches the query. A mismatch means
        // a stale embed_version vector or a (4-byte-aligned) corruption — either
        // way it must not contribute a truncated/garbage dot product.
        let mut scored: Vec<(usize, f32)> = entries
            .iter()
            .enumerate()
            .filter(|(_, (_, v))| v.len() == q.len())
            .map(|(i, (_, v))| (i, dot_product(&q, v)))
            .collect();
        if scored.len() > limit {
            scored.select_nth_unstable_by(limit - 1, |a, b| {
                b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
            });
            scored.truncate(limit);
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored
            .into_iter()
            .map(|(i, sim)| (entries[i].0.clone(), sim))
            .collect())
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
        for row in rows {
            let row = row?;
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

    pub fn get_deps(&self, chunk_id: &str) -> Result<Vec<DepEdge>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT dst, kind, dst_lib FROM deps WHERE src=?")?;
        let rows = stmt.query_map([chunk_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Batch variant of `get_deps`: fetch outgoing edges for many sources in one
    /// query. Returns `src` → `[(dst, kind, dst_lib)]`. Sources with no edges are
    /// simply absent from the map.
    pub fn get_deps_batch(&self, srcs: &[&str]) -> Result<HashMap<String, Vec<DepEdge>>> {
        if srcs.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = srcs.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("SELECT src, dst, kind, dst_lib FROM deps WHERE src IN ({placeholders})");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(srcs.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut map: HashMap<String, Vec<(String, String, Option<String>)>> = HashMap::new();
        for row in rows {
            let (src, dst, kind, lib) = row?;
            map.entry(src).or_default().push((dst, kind, lib));
        }
        Ok(map)
    }

    pub fn get_reverse_deps(&self, chunk_id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT src FROM deps WHERE dst=?")?;
        let rows = stmt.query_map([chunk_id], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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
