//! Observability suite (design doc §7 acceptance): operation_runs instrumentation,
//! error_kind classification, daemon-health degradation, inspect() blocks, and
//! metric_snapshots trend deltas.

use super::*;
use crate::storage::metrics::{aggregate_ops, classify_error, OpRunRow};

#[test]
fn classify_error_maps_to_closed_vocabulary() {
    use crate::errors::InnateError as E;
    assert_eq!(
        classify_error(&E::EmbeddingUnavailable("status 400 Arrearage overdue".into())),
        "embedding_arrearage"
    );
    assert_eq!(
        classify_error(&E::EmbeddingUnavailable("connection reset".into())),
        "embedding_unavailable"
    );
    assert_eq!(classify_error(&E::ChunkNotFound("x".into())), "chunk_not_found");
    assert_eq!(classify_error(&E::InvalidState("x".into())), "invalid_state");
    assert_eq!(
        classify_error(&E::Other("request timeout after 30s".into())),
        "llm_timeout"
    );
    assert_eq!(
        classify_error(&E::Other("No such file or directory".into())),
        "spawn_failed"
    );
}

#[test]
fn aggregate_ops_computes_percentiles_status_and_error_top() {
    let rows = vec![
        row("recall", "ok", None, 10),
        row("recall", "ok", None, 20),
        row("recall", "ok", None, 30),
        row("recall", "error", Some("embedding_arrearage"), 5),
        row("evolve", "timeout", Some("llm_timeout"), 100),
    ];
    let agg = aggregate_ops(&rows);
    let recall = &agg["by_op"]["recall"];
    assert_eq!(recall["count"], 4);
    assert_eq!(recall["ok"], 3);
    assert_eq!(recall["error"], 1);
    // p95 nearest-rank over [5,10,20,30] → top value.
    assert_eq!(recall["p95_ms"], 30);
    assert_eq!(agg["by_op"]["evolve"]["timeout"], 1);
    // error_kind_top includes both failure kinds.
    let top = agg["error_kind_top"].as_array().unwrap();
    assert!(top.iter().any(|e| e["error_kind"] == "embedding_arrearage"));
    assert!(top.iter().any(|e| e["error_kind"] == "llm_timeout"));
    // performance is also broken down by source / agent / context (design §5.3).
    assert!(agg["by_source"]["cli"]["count"].as_i64().unwrap() >= 4);
    assert!(agg.get("by_agent").is_some());
    assert!(agg.get("by_context").is_some());
}

#[test]
fn measure_persists_ok_and_error_runs_with_classified_kind() {
    let (kb, _f) = tmp_kb();
    // ok path
    let ok: Result<i64> = kb.measure("distill", Some("cli"), None, || Ok(7));
    assert_eq!(ok.unwrap(), 7);
    // error path — manufactured arrearage failure must surface as a classified error row.
    let err: Result<()> = kb.measure("embed", None, None, || {
        Err(InnateError::EmbeddingUnavailable("Arrearage: overdue payment".into()))
    });
    assert!(err.is_err());

    let runs = kb.storage.operation_runs_since("").unwrap();
    let distill = runs.iter().find(|r| r.op == "distill").unwrap();
    assert_eq!(distill.status, "ok");
    assert!(distill.error_kind.is_none());
    let embed = runs.iter().find(|r| r.op == "embed").unwrap();
    assert_eq!(embed.status, "error");
    assert_eq!(embed.error_kind.as_deref(), Some("embedding_arrearage"));
}

#[test]
fn daemon_health_degrades_gracefully_when_state_db_missing() {
    let now = crate::utils::utc_now_iso();
    let h = crate::daemon::health(
        std::path::Path::new("/no/such/daemon_state.sqlite"),
        std::path::Path::new("/no/such/daemon.pid"),
        &now,
    );
    assert_eq!(h["state"], "never_run");
}

#[test]
fn inspect_carries_observability_and_operational_blocks() {
    let (kb, _f) = tmp_kb();
    let v = kb.inspect().unwrap();
    assert!(v.get("observability").is_some());
    assert!(v.get("operational").is_some());
    // daemon sub-block always present; ops omitted until operation_runs has data.
    assert!(v["operational"].get("daemon").is_some());
    // windows always present with the three horizons.
    let w = &v["observability"]["windows"];
    for k in ["1d", "7d", "30d"] {
        assert!(w.get(k).is_some(), "missing window {k}");
    }
}

#[test]
fn trends_block_reports_delta_against_week_old_snapshot() {
    let (kb, _f) = tmp_kb();
    let now = crate::utils::utc_now_iso();
    let week_old = {
        use chrono::{DateTime, Duration, Utc};
        (now.parse::<DateTime<Utc>>().unwrap() - Duration::days(8))
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
    };
    // Baseline ≥7d old, then a current snapshot with a higher debt ratio.
    kb.storage
        .insert_metric_snapshot(&week_old, r#"{"knowledge_debt_ratio":1.0}"#)
        .unwrap();
    kb.storage
        .insert_metric_snapshot(&now, r#"{"knowledge_debt_ratio":3.0}"#)
        .unwrap();
    let v = kb.inspect().unwrap();
    let trends = v.get("trends").expect("trends present with snapshots");
    let delta = trends["delta_vs_7d"]["knowledge_debt_ratio"].as_f64().unwrap();
    assert!((delta - 2.0).abs() < 1e-6, "expected +2.0 delta, got {delta}");
}

#[test]
fn recall_snapshot_is_schema_2_with_channels_scores_packing() {
    let (kb, _f) = tmp_kb();
    kb.add(
        "schema 2 recall snapshot content for build innate",
        "note",
        Some("build innate"),
        None,
        "manual",
        None,
    )
    .unwrap();
    let _ = kb
        .recall(RecallParams {
            query: "build innate",
            budget: 4000,
            trace: true,
            include_sparks: false,
            top: Some(5),
            source: "cli",
            expand_deps: "false",
            allow_trim: false,
            refine_mode: "off",
            min_score: None,
            session_only: false,
            rerank: false,
            lexical_only: false,
        })
        .unwrap();
    let snap: String = kb
        .storage
        .query_chunks("SELECT recall_snapshot AS s FROM episodic_log ORDER BY ts DESC LIMIT 1")
        .unwrap()
        .first()
        .and_then(|r| r.get("s"))
        .and_then(Value::as_str)
        .unwrap()
        .to_string();
    let v: Value = serde_json::from_str(&snap).unwrap();
    assert_eq!(v["schema"], 2);
    let r = &v["recall"];
    for k in ["content", "trigger", "lexical", "spread"] {
        assert!(r["channels"].get(k).is_some(), "missing channel {k}");
    }
    assert!(r["scores"].get("max").is_some());
    assert!(r["packing"].get("skipped_by_dep_depth").is_some());
}

#[test]
fn instrumentation_covers_embed_hook_recall_and_distill_ops() {
    let (kb, _f) = tmp_kb();
    kb.add(
        "hook recall op coverage content build",
        "note",
        Some("build"),
        None,
        "manual",
        None,
    )
    .unwrap();
    // hook-sourced recall → op='hook_recall' with a nested op='embed'.
    let _ = kb
        .recall(RecallParams {
            query: "build",
            source: "hook",
            trace: true,
            top: Some(3),
            budget: 4000,
            ..Default::default()
        })
        .unwrap();
    // evolve → op='distill'.
    let _ = kb.evolve("manual");
    let ops: std::collections::HashSet<String> = kb
        .storage
        .operation_runs_since("")
        .unwrap()
        .into_iter()
        .map(|r| r.op)
        .collect();
    assert!(ops.contains("hook_recall"), "ops={ops:?}");
    assert!(ops.contains("embed"), "ops={ops:?}");
    assert!(ops.contains("distill"), "ops={ops:?}");
}

#[test]
fn observability_has_dimension_rates_and_recall_pack_proxies() {
    let (kb, _f) = tmp_kb();
    let v = kb.inspect().unwrap();
    let obs = &v["observability"];
    assert!(obs["by_dimension"]["event_source"]["rates"].is_object());
    assert!(obs["by_dimension"].get("agent").is_some());
    assert!(obs["by_dimension"].get("context_key").is_some());
    let rp = &obs["recall_pack"];
    for k in [
        "zombie_chunks",
        "avg_retrieved",
        "avg_selected",
        "selected_unused_rate",
        "used_rank_mrr",
        "hook_silence_rate",
        "selected_unused_top",
        "selected_rank_distribution",
        "high_rank_unused",
        "low_rank_used",
    ] {
        assert!(rp.get(k).is_some(), "recall_pack missing {k}");
    }
    // rank distribution carries the documented buckets.
    for b in ["1", "2-3", "4-10", "11+"] {
        assert!(rp["selected_rank_distribution"].get(b).is_some(), "missing bucket {b}");
    }
    assert!(obs["lifecycle"]
        .get("governance_backlog_oldest_ts")
        .is_some());
    // Full §5.1 rate set present in both windows and per-dimension breakdowns.
    let w30 = &obs["windows"]["30d"];
    for k in [
        "usage_annotation_rate",
        "selected_to_used_rate",
        "feedback_coverage",
    ] {
        assert!(w30.get(k).is_some(), "window 30d missing {k}");
    }
}

#[test]
fn embed_op_is_recorded_with_source_across_paths() {
    let (kb, _f) = tmp_kb();
    // add() embeds content+trigger → one embed op with source='add'.
    kb.add("embed source coverage content", "note", Some("trig"), None, "manual", None)
        .unwrap();
    let runs = kb.storage.operation_runs_since("").unwrap();
    let embed = runs.iter().find(|r| r.op == "embed");
    assert!(embed.is_some(), "add() should record an embed op");
    // recall threads its source onto the recall op row.
    let _ = kb
        .recall(RecallParams {
            query: "trig",
            source: "mcp",
            trace: true,
            top: Some(3),
            budget: 4000,
            ..Default::default()
        })
        .unwrap();
    // operation_runs.source column is populated (no longer always NULL).
    let any_src: i64 = kb
        .storage
        .query_chunks("SELECT COUNT(*) AS n FROM operation_runs WHERE source IS NOT NULL")
        .unwrap()
        .first()
        .and_then(|r| r.get("n"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    assert!(any_src > 0, "operation_runs.source should be populated");
}

#[test]
fn agent_coverage_suggestion_fires_on_mcp_cli_null_agent() {
    let (kb, _f) = tmp_kb();
    let row = crate::storage::EpisodicLogRow {
        id: crate::utils::gen_uuid(),
        trace_id: crate::utils::gen_uuid(),
        lib_id: kb.storage.lib_id().unwrap(),
        ts: crate::utils::utc_now_iso(),
        event_source: "mcp".to_string(),
        agent: None,
        task_state: "recalled".to_string(),
        usage_state: "unknown".to_string(),
        distill_state: "discarded".to_string(),
        ..Default::default()
    };
    kb.storage.upsert_episodic_log(&row).unwrap();
    let v = kb.inspect().unwrap();
    let fired = v["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["action"].as_str().unwrap_or("").contains("INNATE_AGENT"));
    assert!(fired, "expected agent-attribution suggestion for mcp NULL agent");
}

fn row(op: &str, status: &str, error_kind: Option<&str>, duration_ms: i64) -> OpRunRow {
    OpRunRow {
        op: op.to_string(),
        status: status.to_string(),
        error_kind: error_kind.map(|s| s.to_string()),
        duration_ms,
        source: Some("cli".to_string()),
        agent: None,
        context: None,
    }
}
