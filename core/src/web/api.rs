//! HTTP routing for `innate web`.
//!
//! `route()` is pure (no IO): it takes the parsed request and returns a `Resp`,
//! so it can be unit-tested without a socket. `handle()` does the tiny_http IO
//! and delegates to `route()`.

use std::collections::HashMap;
use std::io::Read;

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response};

use super::assets;
use crate::KnowledgeBase;

/// Shared, immutable-for-the-loop server context.
pub(crate) struct Ctx {
    pub kb: KnowledgeBase,
    pub token: Option<String>,
    pub bind: String,
    pub port: u16,
}

/// A fully-resolved response, independent of the transport.
pub(crate) struct Resp {
    pub status: u16,
    content_type: &'static str,
    pub body: String,
}

fn json_resp(status: u16, v: Value) -> Resp {
    Resp {
        status,
        content_type: "application/json; charset=utf-8",
        body: v.to_string(),
    }
}

fn err(status: u16, msg: &str) -> Resp {
    json_resp(status, json!({ "error": msg }))
}

fn asset(content_type: &'static str, body: &str) -> Resp {
    Resp {
        status: 200,
        content_type,
        body: body.to_string(),
    }
}

// ── tiny_http glue ──────────────────────────────────────────────────────────

pub(crate) fn handle(ctx: &Ctx, mut request: Request) {
    let method = request.method().clone();
    let raw_url = request.url().to_string();
    let (path, query) = split_url(&raw_url);

    // Lower-cased header map for case-insensitive lookups.
    let mut headers: HashMap<String, String> = HashMap::new();
    for h in request.headers() {
        headers.insert(
            h.field.as_str().as_str().to_ascii_lowercase(),
            h.value.as_str().to_string(),
        );
    }

    // Read the body (governance endpoints carry a small JSON payload).
    let mut body = String::new();
    let _ = request.as_reader().take(64 * 1024).read_to_string(&mut body);

    let resp = route(ctx, &method, path, query, &headers, &body);

    let header = Header::from_bytes(&b"Content-Type"[..], resp.content_type.as_bytes())
        .expect("static content-type header is valid");
    let response = Response::from_string(resp.body)
        .with_status_code(resp.status)
        .with_header(header);
    let _ = request.respond(response);
}

// ── pure router ─────────────────────────────────────────────────────────────

pub(crate) fn route(
    ctx: &Ctx,
    method: &Method,
    path: &str,
    query: &str,
    headers: &HashMap<String, String>,
    body: &str,
) -> Resp {
    let segs: Vec<&str> = path.trim_matches('/').split('/').collect();

    // When bound to a non-loopback address the server is network-reachable, so
    // read endpoints must also present the token — otherwise any LAN client can
    // dump the whole knowledge base. On loopback (trusted single user) reads
    // stay open so the local UI needs no token for browsing. Static assets are
    // always public so the page can load and then supply the token via header.
    let is_api = segs.first() == Some(&"api");
    if is_api && !super::is_loopback(&ctx.bind) && !token_ok(ctx, headers) {
        return err(403, "missing or invalid token");
    }

    match (method, segs.as_slice()) {
        // Static assets.
        (Method::Get, [""]) | (Method::Get, ["index.html"]) => {
            asset("text/html; charset=utf-8", assets::INDEX_HTML)
        }
        (Method::Get, ["app.js"]) => asset("application/javascript; charset=utf-8", assets::APP_JS),
        (Method::Get, ["style.css"]) => asset("text/css; charset=utf-8", assets::STYLE_CSS),

        // Read endpoints (token required only on non-loopback binds, see above).
        (Method::Get, ["api", "inspect"]) => match ctx.kb.inspect() {
            Ok(v) => json_resp(200, v),
            Err(e) => err(500, &e.to_string()),
        },
        (Method::Get, ["api", "chunks"]) => list_chunks(ctx, query),
        (Method::Get, ["api", "governance"]) => list_governance(ctx, query),
        (Method::Get, ["api", "llm-traces"]) => list_llm_traces(query),
        (Method::Get, ["api", "chunk", id]) => match ctx.kb.inspect_id(id) {
            Ok(v) => json_resp(200, v),
            Err(e) => err(404, &e.to_string()),
        },

        // Governance endpoints (token + same-origin required).
        (Method::Post, ["api", "chunk", id, action]) => {
            governance(ctx, headers, id, action, body)
        }

        _ => err(404, "not found"),
    }
}

fn list_chunks(ctx: &Ctx, query: &str) -> Resp {
    let params = parse_query(query);
    let state = params.get("state").map(String::as_str).filter(|s| !s.is_empty());
    let origin = params
        .get("origin")
        .map(String::as_str)
        .filter(|s| !s.is_empty());
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50)
        .min(500);
    let offset = params
        .get("offset")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);

    match ctx.kb.storage.list_chunks(state, origin, limit, offset) {
        Ok(rows) => json_resp(200, json!({ "chunks": rows, "limit": limit, "offset": offset })),
        Err(e) => err(500, &e.to_string()),
    }
}

/// Read-only review queue: chunks the feedback loop has flagged for human
/// adjudication, strongest evidence first. This is what makes the human-review
/// backlog *visible and measurable* in the UI, rather than buried in inspect.
fn list_governance(ctx: &Ctx, query: &str) -> Resp {
    let params = parse_query(query);
    let state = params
        .get("state")
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("pending");
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(100)
        .min(500);
    match ctx.kb.storage.list_governance_proposals(state, limit) {
        Ok(rows) => json_resp(200, json!({ "proposals": rows, "state": state })),
        Err(e) => err(500, &e.to_string()),
    }
}

/// Recent LLM/embedding HTTP call traces (newest first) for agent debugging.
/// Reads `~/.innate/logs/llm_trace.log`; does not touch the knowledge db.
/// Optional filters: `kind` (chat|embedding), `status` (ok|http_4xx|rate_limited|…).
fn list_llm_traces(query: &str) -> Resp {
    let params = parse_query(query);
    let kind = params.get("kind").map(String::as_str).filter(|s| !s.is_empty());
    let status = params
        .get("status")
        .map(String::as_str)
        .filter(|s| !s.is_empty());
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(200)
        .min(2000);
    match crate::llm_trace::read_recent(limit, kind, status) {
        Ok(traces) => json_resp(200, json!({ "traces": traces, "limit": limit })),
        Err(e) => err(500, &e.to_string()),
    }
}

fn governance(
    ctx: &Ctx,
    headers: &HashMap<String, String>,
    id: &str,
    action: &str,
    body: &str,
) -> Resp {
    if !origin_ok(ctx, headers) {
        return err(403, "cross-origin request rejected");
    }
    if !token_ok(ctx, headers) {
        return err(403, "missing or invalid token");
    }

    let reason = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(String::from))
        .unwrap_or_default();

    let result = match action {
        "approve" => ctx.kb.approve(id),
        "restore" => ctx.kb.restore(id),
        "archive" => {
            if reason.trim().is_empty() {
                return err(400, "archive requires a non-empty reason");
            }
            ctx.kb.archive(id, &reason)
        }
        "invalidate" => {
            if reason.trim().is_empty() {
                return err(400, "invalidate requires a non-empty reason");
            }
            ctx.kb.invalidate(id, &reason)
        }
        _ => return err(404, "unknown governance action"),
    };

    match result {
        Ok(()) => json_resp(200, json!({ "ok": true, "id": id, "action": action })),
        Err(e) => err(400, &e.to_string()),
    }
}

// ── security helpers ────────────────────────────────────────────────────────

fn token_ok(ctx: &Ctx, headers: &HashMap<String, String>) -> bool {
    match &ctx.token {
        None => true, // --no-token mode
        Some(expected) => headers
            .get("x-innate-token")
            .is_some_and(|got| got == expected),
    }
}

/// CSRF defense: a browser always sends `Origin` on cross-site POSTs. Absent
/// `Origin` (non-browser clients like curl) is allowed but still needs the token.
fn origin_ok(ctx: &Ctx, headers: &HashMap<String, String>) -> bool {
    match headers.get("origin") {
        None => true,
        Some(o) => {
            let mut allowed = vec![
                format!("http://127.0.0.1:{}", ctx.port),
                format!("http://localhost:{}", ctx.port),
                format!("http://{}:{}", ctx.bind, ctx.port),
            ];
            // True same-origin: the `Host` header reflects the address the
            // browser actually connected to (e.g. the LAN IP behind a `0.0.0.0`
            // bind), so `http://<host>` is same-origin even though it differs
            // from the literal bind string. Still strictly equality-checked, so
            // a genuinely cross-site Origin is rejected.
            if let Some(host) = headers.get("host") {
                allowed.push(format!("http://{host}"));
                allowed.push(format!("https://{host}"));
            }
            allowed.iter().any(|a| a == o)
        }
    }
}

// ── parsing helpers ─────────────────────────────────────────────────────────

fn split_url(url: &str) -> (&str, &str) {
    match url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (url, ""),
    }
}

fn parse_query(query: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(url_decode(k), url_decode(v));
    }
    map
}

/// Minimal percent-decoder (handles `%XX` and `+`). Sufficient for the small set
/// of query params the viewer sends; avoids pulling in a urlencoding crate.
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                    continue;
                }
                out.push(b'%');
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
