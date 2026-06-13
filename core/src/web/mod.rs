//! `innate web` — local read + governance web UI for the knowledge base.
//!
//! Design: the **fifth access module** (alongside MCP / CLI / SDK / Daemon). Unlike
//! the daemon (§九), this module DOES hold a live read-write `KnowledgeBase`, because
//! governance actions (approve/archive/invalidate/restore) mutate. It is therefore
//! security-sensitive: bound to localhost by default and gated by a one-time token
//! for every mutating endpoint.
//!
//! Synchronous stack only (`tiny_http` + the std accept loop) to match the rest of
//! the core; no async runtime is introduced.

use crate::KnowledgeBase;

mod api;
mod assets;

#[cfg(test)]
mod tests;

/// Whether a bind address is loopback-only (the trusted single-user case). Used
/// to decide both whether `--allow-remote` is required and whether read
/// endpoints must also present the auth token (they must, when non-loopback).
pub fn is_loopback(bind: &str) -> bool {
    match bind.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        // Hostnames: only the well-known localhost aliases are treated as
        // loopback; anything else is assumed network-reachable (fail safe).
        Err(_) => matches!(bind, "localhost" | "localhost.localdomain"),
    }
}

/// Start the web server and block on the accept loop until the process is killed.
///
/// * `bind` / `port` — listen address; defaults to `127.0.0.1:8788` via the CLI.
/// * `require_token` — when true (default), every governance (POST) endpoint requires
///   the printed token in the `X-Innate-Token` header. `--no-token` disables it.
pub fn serve(kb: KnowledgeBase, bind: &str, port: u16, require_token: bool) -> anyhow::Result<()> {
    let token = if require_token {
        Some(crate::utils::gen_uuid())
    } else {
        None
    };

    let addr = format!("{bind}:{port}");
    let server = tiny_http::Server::http(addr.as_str())
        .map_err(|e| anyhow::anyhow!("failed to bind {addr}: {e}"))?;

    // Token rides in the URL *fragment* (`#token=`), never the query string: the
    // fragment is not sent to the server and not written to access logs/Referer.
    // The frontend reads it, strips it from the address bar, and replays it via
    // the `X-Innate-Token` header.
    let url = match &token {
        Some(t) => format!("http://{bind}:{port}/#token={t}"),
        None => format!("http://{bind}:{port}/"),
    };
    eprintln!("innate web listening on http://{bind}:{port}");
    eprintln!("open: {url}");
    if token.is_none() {
        eprintln!("WARNING: --no-token set; governance endpoints are unauthenticated.");
    }

    // A local single-user viewer: a single-threaded accept loop is sufficient and
    // keeps the lifetime of the read-write KnowledgeBase trivially sound (no shared
    // mutable state across threads). Governance methods take &self.
    let ctx = api::Ctx {
        kb,
        token,
        bind: bind.to_string(),
        port,
    };
    for request in server.incoming_requests() {
        api::handle(&ctx, request);
    }
    Ok(())
}
