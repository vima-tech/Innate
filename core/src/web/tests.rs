//! Unit tests for the web router. These exercise `route()` directly (no socket),
//! covering the read endpoints, governance auth (token + same-origin), and the
//! `list_chunks` storage projection.

use std::collections::HashMap;

use serde_json::Value;
use tempfile::NamedTempFile;
use tiny_http::Method;

use super::api::{route, Ctx};
use crate::KnowledgeBase;

fn ctx_with(token: Option<&str>) -> (Ctx, NamedTempFile) {
    let f = NamedTempFile::new().unwrap();
    let kb = KnowledgeBase::open(f.path()).unwrap();
    // Seed two chunks so list/detail have something to return.
    kb.add(
        "alpha content",
        "note",
        Some("when alpha"),
        None,
        "manual",
        Some("skill_a"),
    )
    .unwrap();
    kb.add(
        "beta content",
        "note",
        Some("when beta"),
        None,
        "manual",
        Some("skill_b"),
    )
    .unwrap();
    let ctx = Ctx {
        kb,
        token: token.map(String::from),
        bind: "127.0.0.1".into(),
        port: 8788,
    };
    (ctx, f)
}

fn hdr(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn lists_chunks() {
    let (ctx, _f) = ctx_with(Some("secret"));
    let r = route(&ctx, &Method::Get, "/api/chunks", "limit=10", &hdr(&[]), "");
    assert_eq!(r.status, 200);
    let v: Value = serde_json::from_str(&r.body).unwrap();
    let chunks = v["chunks"].as_array().unwrap();
    assert_eq!(chunks.len(), 2);
    // Projection includes a truncated preview, not the raw row only.
    assert!(chunks[0].get("content_preview").is_some());
}

#[test]
fn list_state_filter_matches_nothing_for_bogus_state() {
    let (ctx, _f) = ctx_with(None);
    let r = route(
        &ctx,
        &Method::Get,
        "/api/chunks",
        "state=does-not-exist",
        &hdr(&[]),
        "",
    );
    assert_eq!(r.status, 200);
    let v: Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(v["chunks"].as_array().unwrap().len(), 0);
}

#[test]
fn inspect_is_open() {
    let (ctx, _f) = ctx_with(Some("secret"));
    let r = route(&ctx, &Method::Get, "/api/inspect", "", &hdr(&[]), "");
    assert_eq!(r.status, 200);
}

#[test]
fn governance_queue_is_open() {
    let (ctx, _f) = ctx_with(None);
    let r = route(&ctx, &Method::Get, "/api/governance", "", &hdr(&[]), "");
    assert_eq!(r.status, 200);
    let v: Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(v["state"], "pending");
    assert!(v["proposals"].is_array());
}

#[test]
fn llm_traces_endpoint_returns_array() {
    let (ctx, _f) = ctx_with(None);
    let r = route(
        &ctx,
        &Method::Get,
        "/api/llm-traces",
        "limit=5",
        &hdr(&[]),
        "",
    );
    assert_eq!(r.status, 200);
    let v: Value = serde_json::from_str(&r.body).unwrap();
    // Shape is stable regardless of whether any calls have been traced yet.
    assert!(v["traces"].is_array());
    assert_eq!(v["limit"], 5);
}

#[test]
fn metrics_trend_endpoint_returns_snapshots_array() {
    let (ctx, _f) = ctx_with(None);
    let r = route(
        &ctx,
        &Method::Get,
        "/api/metrics/trend",
        "limit=10",
        &hdr(&[]),
        "",
    );
    assert_eq!(r.status, 200);
    let v: Value = serde_json::from_str(&r.body).unwrap();
    // Stable shape even before any snapshot is written.
    assert!(v["snapshots"].is_array());
    assert_eq!(v["limit"], 10);
}

#[test]
fn inspect_endpoint_carries_observability_blocks() {
    let (ctx, _f) = ctx_with(None);
    let r = route(&ctx, &Method::Get, "/api/inspect", "", &hdr(&[]), "");
    assert_eq!(r.status, 200);
    let v: Value = serde_json::from_str(&r.body).unwrap();
    assert!(v.get("observability").is_some());
    assert!(v["operational"].get("daemon").is_some());
}

#[test]
fn serves_index_html() {
    let (ctx, _f) = ctx_with(None);
    let r = route(&ctx, &Method::Get, "/", "", &hdr(&[]), "");
    assert_eq!(r.status, 200);
    assert!(r.body.contains("<title>Innate"));
}

#[test]
fn governance_rejected_without_token() {
    let (ctx, _f) = ctx_with(Some("secret"));
    let id = first_chunk_id(&ctx);
    let path = format!("/api/chunk/{id}/approve");
    let r = route(&ctx, &Method::Post, &path, "", &hdr(&[]), "{}");
    assert_eq!(r.status, 403);
}

#[test]
fn governance_rejected_cross_origin() {
    let (ctx, _f) = ctx_with(Some("secret"));
    let id = first_chunk_id(&ctx);
    let path = format!("/api/chunk/{id}/approve");
    let h = hdr(&[
        ("origin", "http://evil.example.com"),
        ("x-innate-token", "secret"),
    ]);
    let r = route(&ctx, &Method::Post, &path, "", &h, "{}");
    assert_eq!(r.status, 403);
}

#[test]
fn governance_approve_with_token_succeeds() {
    let (ctx, _f) = ctx_with(Some("secret"));
    let id = first_chunk_id(&ctx);
    let path = format!("/api/chunk/{id}/approve");
    let h = hdr(&[
        ("origin", "http://127.0.0.1:8788"),
        ("x-innate-token", "secret"),
    ]);
    let r = route(&ctx, &Method::Post, &path, "", &h, "{}");
    assert_eq!(r.status, 200, "body: {}", r.body);
    let v: Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(v["ok"], true);
}

#[test]
fn archive_requires_reason() {
    let (ctx, _f) = ctx_with(Some("secret"));
    let id = first_chunk_id(&ctx);
    let path = format!("/api/chunk/{id}/archive");
    let h = hdr(&[("x-innate-token", "secret")]);
    let r = route(&ctx, &Method::Post, &path, "", &h, "{}");
    assert_eq!(r.status, 400);
}

#[test]
fn is_loopback_classifies_addresses() {
    assert!(super::is_loopback("127.0.0.1"));
    assert!(super::is_loopback("::1"));
    assert!(super::is_loopback("localhost"));
    assert!(!super::is_loopback("0.0.0.0"));
    assert!(!super::is_loopback("192.168.1.10"));
}

#[test]
fn loopback_reads_need_no_token() {
    // Default loopback bind: browsing endpoints stay open for the local user.
    let (ctx, _f) = ctx_with(Some("secret"));
    let r = route(&ctx, &Method::Get, "/api/chunks", "", &hdr(&[]), "");
    assert_eq!(r.status, 200);
}

#[test]
fn non_loopback_reads_require_token() {
    // Network-exposed bind: a read without the token must be refused so a LAN
    // client cannot dump the knowledge base.
    let (mut ctx, _f) = ctx_with(Some("secret"));
    ctx.bind = "0.0.0.0".into();
    let denied = route(&ctx, &Method::Get, "/api/chunks", "", &hdr(&[]), "");
    assert_eq!(
        denied.status, 403,
        "remote read without token must be denied"
    );

    let h = hdr(&[("x-innate-token", "secret")]);
    let ok = route(&ctx, &Method::Get, "/api/chunks", "", &h, "");
    assert_eq!(ok.status, 200, "remote read with token must succeed");
}

#[test]
fn non_loopback_static_assets_stay_public() {
    // The HTML/JS must load without a token so the page can then supply it.
    let (mut ctx, _f) = ctx_with(Some("secret"));
    ctx.bind = "0.0.0.0".into();
    let r = route(&ctx, &Method::Get, "/", "", &hdr(&[]), "");
    assert_eq!(r.status, 200);
}

#[test]
fn no_token_mode_allows_governance() {
    let (ctx, _f) = ctx_with(None);
    let id = first_chunk_id(&ctx);
    let path = format!("/api/chunk/{id}/approve");
    let r = route(&ctx, &Method::Post, &path, "", &hdr(&[]), "{}");
    assert_eq!(r.status, 200, "body: {}", r.body);
}

#[test]
fn governance_queue_is_open_and_lists_proposals() {
    let (ctx, _f) = ctx_with(None);
    let chunk_id = first_chunk_id(&ctx);
    let now = crate::utils::utc_now_iso();
    // Seed a pending proposal directly (the feedback path is exercised elsewhere).
    ctx.kb
        .storage
        .upsert_governance_proposal(
            &crate::utils::gen_uuid(),
            &chunk_id,
            "review_applicability",
            "Weighted negative feedback",
            3,
            2.5,
            2,
            &now,
        )
        .unwrap();

    // Read endpoint requires no token.
    let r = route(&ctx, &Method::Get, "/api/governance", "", &hdr(&[]), "");
    assert_eq!(r.status, 200, "body: {}", r.body);
    let v: Value = serde_json::from_str(&r.body).unwrap();
    let proposals = v["proposals"].as_array().unwrap();
    assert_eq!(proposals.len(), 1);
    let p = &proposals[0];
    assert_eq!(p["chunk_id"], chunk_id);
    assert_eq!(p["actor_count"], 2);
    // Joined chunk projection is present so the reviewer sees what was flagged.
    assert!(p.get("content_preview").is_some());
    assert!(p.get("skill_name").is_some());
}

#[test]
fn governance_queue_empty_by_default() {
    let (ctx, _f) = ctx_with(None);
    let r = route(&ctx, &Method::Get, "/api/governance", "", &hdr(&[]), "");
    assert_eq!(r.status, 200);
    let v: Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(v["proposals"].as_array().unwrap().len(), 0);
}

fn first_chunk_id(ctx: &Ctx) -> String {
    let rows = ctx.kb.storage.list_chunks(None, None, 1, 0).unwrap();
    rows[0]["id"].as_str().unwrap().to_string()
}
