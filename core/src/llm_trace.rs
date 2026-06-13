//! LLM / embedding HTTP call tracing.
//!
//! Every chat (distill) and embedding request flows through `llm::post_json_retry`,
//! which calls [`record`] once per call with the final outcome. Traces are appended
//! as JSONL to `~/.innate/logs/llm_trace.log` (size-capped with single-file rotation)
//! so any process — MCP, CLI, evolve — contributes automatically, failures included.
//!
//! Design notes:
//! - **Never logs the API key.** Only the request *body* is captured; the
//!   `Authorization` header is not passed in and is never recorded.
//! - Request/response bodies are truncated to keep the log bounded.
//! - Disable entirely with `INNATE_LLM_TRACE=0`.

use std::io::Write;
use std::time::Duration;

use serde_json::{json, Value};

use crate::errors::{InnateError, Result};

/// Max bytes for request/response preview each.
const PREVIEW_CAP: usize = 4000;
/// Rotate the trace file once it exceeds this size (5 MiB).
const ROTATE_BYTES: u64 = 5 * 1024 * 1024;

/// Whether tracing is enabled (default on; `INNATE_LLM_TRACE=0` turns it off).
fn enabled() -> bool {
    !matches!(
        std::env::var("INNATE_LLM_TRACE").ok().as_deref(),
        Some("0") | Some("false") | Some("off")
    )
}

/// Record one LLM/embedding call outcome. Best-effort: tracing failures never
/// affect the caller (the LLM call result is returned regardless).
///
/// * `label`  — call-site kind from `post_json_retry` ("LLM" / "Anthropic" / "Embedding").
/// * `url`    — full endpoint URL (only the host is stored).
/// * `request`— the JSON body sent (contains the prompt/input; no API key).
/// * `outcome`— the final `Result` after retries.
/// * `attempts` / `elapsed` — retry count and total wall time.
pub fn record(
    label: &str,
    url: &str,
    request: &Value,
    outcome: &Result<Value>,
    attempts: u32,
    elapsed: Duration,
) {
    if !enabled() {
        return;
    }
    let entry = build_entry(label, url, request, outcome, attempts, elapsed);
    // Serialize to a single line; swallow any error (diagnostics must not break calls).
    if let Ok(line) = serde_json::to_string(&entry) {
        let _ = append_line(&line);
    }
}

fn build_entry(
    label: &str,
    url: &str,
    request: &Value,
    outcome: &Result<Value>,
    attempts: u32,
    elapsed: Duration,
) -> Value {
    let kind = match label {
        "Embedding" => "embedding",
        _ => "chat",
    };
    let model = request.get("model").and_then(Value::as_str).unwrap_or("");
    let (status, error, response_preview, usage) = match outcome {
        Ok(resp) => (
            "ok".to_string(),
            Value::Null,
            json!(truncate(&resp.to_string())),
            resp.get("usage").cloned().unwrap_or(Value::Null),
        ),
        Err(e) => {
            let msg = e.to_string();
            (classify_error(&msg), json!(msg), Value::Null, Value::Null)
        }
    };

    json!({
        "ts": crate::utils::utc_now_iso(),
        "kind": kind,
        "label": label,
        "model": model,
        "host": host_of(url),
        "status": status,
        "attempts": attempts,
        "latency_ms": elapsed.as_millis() as u64,
        "token_usage": usage,
        "error": error,
        "request_preview": truncate(&request.to_string()),
        "response_preview": response_preview,
    })
}

/// Coarse status bucket derived from the error message produced by `post_json_retry`
/// (e.g. `"Embedding HTTP error: <ureq Error>"`). Buckets: `http_4xx`, `http_5xx`,
/// `rate_limited`, `transport`, `error`.
fn classify_error(msg: &str) -> String {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("status: 429") || lower.contains("429 ") {
        "rate_limited".into()
    } else if let Some(code) = extract_status_code(&lower) {
        if (500..=599).contains(&code) {
            "http_5xx".into()
        } else if (400..=499).contains(&code) {
            "http_4xx".into()
        } else {
            "error".into()
        }
    } else if lower.contains("transport") || lower.contains("connection") || lower.contains("tls") {
        "transport".into()
    } else {
        "error".into()
    }
}

/// Pull the first 3-digit HTTP status (e.g. from "status: 404") out of a message.
fn extract_status_code(lower: &str) -> Option<u16> {
    let bytes = lower.as_bytes();
    for i in 0..bytes.len().saturating_sub(2) {
        if bytes[i].is_ascii_digit() && bytes[i + 1].is_ascii_digit() && bytes[i + 2].is_ascii_digit()
        {
            let three = &lower[i..i + 3];
            if let Ok(code) = three.parse::<u16>() {
                if (100..=599).contains(&code) {
                    return Some(code);
                }
            }
        }
    }
    None
}

fn host_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("")
        .to_string()
}

fn truncate(s: &str) -> String {
    if s.len() <= PREVIEW_CAP {
        return s.to_string();
    }
    // Truncate on a char boundary to keep valid UTF-8.
    let mut end = PREVIEW_CAP;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…[truncated {} bytes]", &s[..end], s.len() - end)
}

fn append_line(line: &str) -> std::io::Result<()> {
    let path = crate::paths::llm_trace_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    rotate_if_large(&path);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(f, "{line}")
}

/// Single-file rotation: when the log exceeds `ROTATE_BYTES`, move it to `.1`
/// (replacing any previous `.1`) so the active file stays bounded.
fn rotate_if_large(path: &std::path::Path) {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() >= ROTATE_BYTES {
            let rotated = path.with_extension("log.1");
            let _ = std::fs::rename(path, rotated);
        }
    }
}

/// Read the most recent trace entries (newest first) for the web viewer. Optional
/// exact-match filters on `kind` ("chat"/"embedding") and `status`. Read-only.
pub fn read_recent(limit: usize, kind: Option<&str>, status: Option<&str>) -> Result<Vec<Value>> {
    let path = crate::paths::llm_trace_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(InnateError::Other(format!("read llm_trace.log: {e}"))),
    };
    let mut out = Vec::new();
    for line in text.lines().rev() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue; // skip malformed lines rather than failing the whole view
        };
        if let Some(k) = kind {
            if v.get("kind").and_then(Value::as_str) != Some(k) {
                continue;
            }
        }
        if let Some(s) = status {
            if v.get("status").and_then(Value::as_str) != Some(s) {
                continue;
            }
        }
        out.push(v);
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_strips_scheme_and_path() {
        assert_eq!(host_of("https://dashscope.aliyuncs.com/v1/embeddings"), "dashscope.aliyuncs.com");
        assert_eq!(host_of("http://127.0.0.1:8788/x"), "127.0.0.1:8788");
    }

    #[test]
    fn truncate_respects_cap_and_utf8() {
        let s = "a".repeat(PREVIEW_CAP + 50);
        let t = truncate(&s);
        assert!(t.contains("truncated"));
        let multi = "好".repeat(PREVIEW_CAP); // 3 bytes each → exceeds cap
        let _ = truncate(&multi); // must not panic on char boundary
    }

    #[test]
    fn classifies_statuses() {
        assert_eq!(classify_error("LLM HTTP error: ... status: 404 ..."), "http_4xx");
        assert_eq!(classify_error("LLM HTTP error: ... status: 503 ..."), "http_5xx");
        assert_eq!(classify_error("Embedding HTTP error: Transport(...)"), "transport");
    }

    #[test]
    fn build_entry_redacts_to_host_and_records_outcome() {
        let req = json!({"model": "text-embedding-v4", "input": "secret prompt"});
        let ok: Result<Value> = Ok(json!({"data":[1], "usage":{"prompt_tokens":3}}));
        let e = build_entry("Embedding", "https://h.example.com/v1/embeddings", &req, &ok, 1, Duration::from_millis(42));
        assert_eq!(e["kind"], "embedding");
        assert_eq!(e["host"], "h.example.com");
        assert_eq!(e["status"], "ok");
        assert_eq!(e["model"], "text-embedding-v4");
        assert_eq!(e["latency_ms"], 42);
        assert_eq!(e["token_usage"]["prompt_tokens"], 3);
        // Request body (prompt) is captured; no api key is anywhere in the entry.
        assert!(e["request_preview"].as_str().unwrap().contains("secret prompt"));
        assert!(!e.to_string().contains("Authorization"));
    }
}
