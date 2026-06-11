#[cfg(test)]
mod tests {
    use tempfile::NamedTempFile;
    use crate::kb::KnowledgeBase;

    fn tmp_kb() -> (KnowledgeBase, NamedTempFile) {
        let f = NamedTempFile::new().unwrap();
        let kb = KnowledgeBase::open(f.path()).unwrap();
        (kb, f)
    }

    #[test]
    fn add_and_recall() {
        let (kb, _f) = tmp_kb();
        let id = kb.add(
            "Always validate user input at system boundaries",
            "note",
            Some("input validation"),
            None,
            "manual",
            None,
        ).unwrap();
        assert!(!id.is_empty());

        let result = kb.recall("validate input", 6000, false, false, None, "sdk", "false", false, "off").unwrap();
        assert!(!result.trace_id.is_empty());
        // DummyEmbeddingProvider is hash-based, so the chunk may or may not score highly
        // — just verify the recall didn't panic and returned a trace_id.
    }

    #[test]
    fn spark_and_promote() {
        let (kb, _f) = tmp_kb();
        let sid = kb.spark("Use HNSW index for recall scalability", None, None).unwrap();
        assert!(!sid.is_empty());

        let nid = kb.promote_spark(&sid, "note").unwrap();
        assert!(!nid.is_empty());

        let chunk = kb.storage.get_chunk(&nid).unwrap().unwrap();
        assert_eq!(chunk["origin"].as_str().unwrap(), "captured");
        assert_eq!(chunk["state"].as_str().unwrap(), "active");
    }

    #[test]
    fn record_state_machine() {
        let (kb, _f) = tmp_kb();
        let trace_id = crate::utils::gen_uuid();
        // Direct record without prior recall (fresh insert path)
        kb.record(
            &trace_id, Some("test query"), None, Some("summary"), Some("ok"),
            None, None, None, None, 0, "cli",
        ).unwrap();
        let log = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
        assert_eq!(log["distill_state"].as_str().unwrap(), "new");
        // Second call must not downgrade
        kb.record(
            &trace_id, None, None, None, Some("ok"),
            None, None, None, None, 0, "cli",
        ).unwrap();
        let log2 = kb.storage.get_episodic_log(&trace_id).unwrap().unwrap();
        assert_eq!(log2["distill_state"].as_str().unwrap(), "new");
    }

    #[test]
    fn invalidate_cascade() {
        let (kb, _f) = tmp_kb();
        let id = kb.add("sensitive content", "note", None, None, "manual", None).unwrap();
        kb.invalidate(&id, "test").unwrap();
        let chunk = kb.storage.get_chunk(&id).unwrap().unwrap();
        assert_eq!(chunk["state"].as_str().unwrap(), "archived");
        assert_eq!(chunk["confidence"].as_f64().unwrap(), 0.0);
        let h = chunk["content_hash"].as_str().unwrap();
        assert!(kb.storage.is_hash_invalidated(h).unwrap());
    }

    #[test]
    fn inspect_returns_counts() {
        let (kb, _f) = tmp_kb();
        kb.add("test chunk", "note", None, None, "manual", None).unwrap();
        let info = kb.inspect().unwrap();
        let active = info["chunks"]["active"].as_i64().unwrap_or(0);
        assert!(active >= 1);
    }

    #[test]
    fn evolve_smoke() {
        let (kb, _f) = tmp_kb();
        let result = kb.evolve("manual").unwrap();
        assert!(result["distilled"].is_number());
    }
}
