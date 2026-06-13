use serde_json::{json, Value};
use std::time::Duration;

use crate::embedding::EmbeddingProvider;
use crate::errors::{InnateError, Result};
use crate::refine::{DistillProvenance, DistilledChunk, Distiller};
use crate::settings::{EmbeddingConfig, LlmConfig};

// ---------------------------------------------------------------------------
// Prompt for distillation
// ---------------------------------------------------------------------------

const DISTILL_PROMPT_VERSION: &str = "2";

fn safe_prompt_field(value: Option<&str>) -> String {
    let value = value.unwrap_or("");
    let (cleaned, action) = crate::utils::sanitize(value);
    match action {
        crate::utils::SanitizeAction::Discard => "[removed unsafe content]".to_string(),
        _ => cleaned,
    }
}

fn build_distill_prompt(log: &Value) -> String {
    let query = safe_prompt_field(log.get("query").and_then(Value::as_str));
    let output = safe_prompt_field(log.get("output").and_then(Value::as_str));
    let output_summary = safe_prompt_field(log.get("output_summary").and_then(Value::as_str));
    let nomination = safe_prompt_field(log.get("nomination").and_then(Value::as_str));
    let outcome = safe_prompt_field(log.get("outcome").and_then(Value::as_str));

    let mut context_parts = vec![];
    if !query.is_empty() {
        context_parts.push(format!("Query: {query}"));
    }
    if !nomination.is_empty() {
        context_parts.push(format!("Nominated insight: {nomination}"));
    }
    if !output_summary.is_empty() {
        context_parts.push(format!("Summary: {output_summary}"));
    }
    if !output.is_empty() {
        let truncated: String = output.chars().take(1500).collect();
        context_parts.push(format!("Output (truncated): {truncated}"));
    }
    if !outcome.is_empty() {
        context_parts.push(format!("Outcome: {outcome}"));
    }

    let context = context_parts.join("\n");

    format!(
        r#"You are a knowledge distillation assistant. Given an agent interaction log, \
extract zero or more independent reusable procedural principles.

Agent interaction:
{context}

Output a JSON array. Each item has:
{{
  "content": "<principle; when it applies; what to avoid>",
  "trigger_desc": "<2-6 word canonical phrase>",
  "anti_trigger_desc": "<when NOT to apply this, or null>"
}}
Return [] if nothing is worth keeping.

Rules:
- content must be self-contained and actionable for a future agent reading cold
- trigger_desc must match the vocabulary a future agent would use in a search query
- Never store conversation text verbatim; always distil to reusable principle form
- If outcome is "fail", focus on what to avoid
- Keep principles independent; do not combine unrelated lessons"#
    )
}

fn build_distill_prompt_with_related(log: &Value, logs: &[Value]) -> String {
    let mut prompt = build_distill_prompt(log);
    let log_id = log.get("id").and_then(Value::as_str).unwrap_or("");
    let context_key = log.get("context_key").and_then(Value::as_str);
    let related: Vec<String> = logs
        .iter()
        .filter(|other| other.get("id").and_then(Value::as_str).unwrap_or("") != log_id)
        .filter(|other| {
            context_key.is_some() && other.get("context_key").and_then(Value::as_str) == context_key
        })
        .take(4)
        .map(|other| {
            let query = safe_prompt_field(other.get("query").and_then(Value::as_str));
            let summary = safe_prompt_field(other.get("output_summary").and_then(Value::as_str));
            let outcome = safe_prompt_field(other.get("outcome").and_then(Value::as_str));
            format!("- Query: {query}; outcome: {outcome}; summary: {summary}")
        })
        .collect();
    if !related.is_empty() {
        prompt.push_str(
            "\n\nRelated recent interactions (use only to identify repeated patterns or conflicts):\n",
        );
        prompt.push_str(&related.join("\n"));
    }
    prompt
}

// ---------------------------------------------------------------------------
// Shared HTTP transport with retry/backoff
// ---------------------------------------------------------------------------

/// Max total attempts (initial try + retries) for a single LLM/embedding call.
const HTTP_MAX_ATTEMPTS: u32 = 3;
/// Per-request socket timeout. Each retry gets a fresh timeout window.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// POST `body` as JSON to `url` with the given extra headers, retrying transient
/// failures (network/timeout errors, HTTP 429, and 5xx) with exponential backoff.
/// `Content-Type: application/json` is set automatically. `label` names the call
/// site in error messages (e.g. "LLM", "Anthropic", "Embedding").
fn post_json_retry(
    url: &str,
    headers: &[(&str, &str)],
    body: &Value,
    label: &str,
) -> Result<Value> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let mut req = ureq::post(url)
            .timeout(HTTP_TIMEOUT)
            .set("Content-Type", "application/json");
        for (k, v) in headers {
            req = req.set(k, v);
        }
        match req.send_json(body) {
            Ok(response) => {
                return response
                    .into_json()
                    .map_err(|e| InnateError::Other(format!("{label} response parse error: {e}")));
            }
            Err(err) => {
                let (retryable, retry_after) = match &err {
                    ureq::Error::Status(code, resp) => (
                        status_is_retryable(*code),
                        resp.header("retry-after")
                            .and_then(|s| s.trim().parse::<u64>().ok()),
                    ),
                    ureq::Error::Transport(_) => (true, None),
                };
                if retryable && attempt < HTTP_MAX_ATTEMPTS {
                    std::thread::sleep(backoff_delay(attempt, retry_after));
                    continue;
                }
                return Err(InnateError::Other(format!("{label} HTTP error: {err}")));
            }
        }
    }
}

/// Transient HTTP statuses worth retrying: rate limits and server-side errors.
fn status_is_retryable(code: u16) -> bool {
    code == 429 || (500..=599).contains(&code)
}

/// Backoff before the next attempt. Honors a server `Retry-After` (seconds, capped
/// at 30s) when present, otherwise exponential: 250ms, 500ms, 1s, ...
fn backoff_delay(attempt: u32, retry_after_secs: Option<u64>) -> Duration {
    if let Some(secs) = retry_after_secs {
        return Duration::from_secs(secs.min(30));
    }
    let shift = (attempt - 1).min(6);
    Duration::from_millis(250u64.saturating_mul(1 << shift))
}

// ---------------------------------------------------------------------------
// HTTP distiller — one type for both OpenAI-compatible endpoints (GPT, DeepSeek,
// local Ollama, ...) and the Anthropic Messages API. The request/response shape
// is selected per call from `config.provider`; everything else (distill loop,
// provenance, retry transport) is shared.
// ---------------------------------------------------------------------------

pub struct HttpDistiller {
    config: LlmConfig,
}

impl HttpDistiller {
    pub fn new(config: LlmConfig) -> Self {
        Self { config }
    }

    fn call(&self, prompt: &str) -> Result<String> {
        if self.config.provider == "anthropic" {
            self.call_anthropic(prompt)
        } else {
            self.call_openai(prompt)
        }
    }

    fn call_openai(&self, prompt: &str) -> Result<String> {
        let api_key = self
            .config
            .resolved_api_key()
            .ok_or_else(|| InnateError::Other("LLM API key not configured".into()))?;

        let base = self.config.resolved_base_url();
        let url = format!("{base}/chat/completions");

        let body = json!({
            "model": self.config.model_id,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": 800,
            "temperature": 0.2,
        });

        let auth = format!("Bearer {api_key}");
        let resp_json = post_json_retry(&url, &[("Authorization", &auth)], &body, "LLM")?;

        resp_json
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| InnateError::Other("unexpected LLM response shape".into()))
    }

    fn call_anthropic(&self, prompt: &str) -> Result<String> {
        let api_key = self
            .config
            .resolved_api_key()
            .ok_or_else(|| InnateError::Other("Anthropic API key not configured".into()))?;

        let base = self.config.resolved_base_url();
        let url = format!("{base}/v1/messages");

        let body = json!({
            "model": self.config.model_id,
            "max_tokens": 800,
            "messages": [{"role": "user", "content": prompt}],
        });

        let resp_json = post_json_retry(
            &url,
            &[("x-api-key", &api_key), ("anthropic-version", "2023-06-01")],
            &body,
            "Anthropic",
        )?;

        resp_json
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| InnateError::Other("unexpected Anthropic response shape".into()))
    }
}

impl Distiller for HttpDistiller {
    fn distill(&self, log_entries: &[Value]) -> crate::errors::Result<Vec<DistilledChunk>> {
        distill_with(log_entries, |prompt| self.call(prompt))
    }

    fn distill_with_context(
        &self,
        primary: &Value,
        related_logs: &[Value],
    ) -> crate::errors::Result<Vec<DistilledChunk>> {
        distill_entry_with(primary, related_logs, |prompt| self.call(prompt))
    }

    fn provenance(&self) -> DistillProvenance {
        DistillProvenance {
            provider: Some(self.config.provider.clone()),
            model: Some(self.config.model_id.clone()),
            prompt_version: Some(DISTILL_PROMPT_VERSION.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Shared parse logic
// ---------------------------------------------------------------------------

fn distill_with(
    log_entries: &[Value],
    call: impl Fn(&str) -> Result<String> + Copy,
) -> Result<Vec<DistilledChunk>> {
    let mut out = Vec::new();
    for entry in log_entries {
        out.extend(distill_entry_with(entry, log_entries, call)?);
    }
    Ok(out)
}

fn distill_entry_with(
    entry: &Value,
    related_logs: &[Value],
    call: impl Fn(&str) -> Result<String>,
) -> Result<Vec<DistilledChunk>> {
    let log_id = entry["id"].as_str().unwrap_or("").to_string();
    let prompt = build_distill_prompt_with_related(entry, related_logs);
    let mut raw = call(&prompt)?;
    let mut parsed = parse_distill_response(&raw);
    if parsed.is_err() {
        raw = call(&format!(
            "{prompt}\n\nYour previous response was invalid. Return only a valid JSON array."
        ))?;
        parsed = parse_distill_response(&raw);
    }
    let items = parsed.map_err(|error| {
        InnateError::Other(format!("LLM distillation response invalid: {error}"))
    })?;
    let mut out = Vec::new();
    for parsed in items {
        let content = parsed
            .get("content")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let Some(content) = content else { continue };
        let trigger_desc = parsed
            .get("trigger_desc")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty());
        let anti_trigger_desc = parsed
            .get("anti_trigger_desc")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty() && s.to_lowercase() != "null");
        out.push(DistilledChunk {
            content: content.to_string(),
            trigger_desc,
            anti_trigger_desc,
            source_log_id: log_id.clone(),
            nomination: entry
                .get("nomination")
                .and_then(Value::as_str)
                .map(str::to_string),
        });
    }
    Ok(out)
}

fn parse_distill_response(raw: &str) -> std::result::Result<Vec<Value>, String> {
    let json_str = extract_json(raw);
    let parsed: Value = serde_json::from_str(json_str.trim()).map_err(|e| e.to_string())?;
    if parsed.get("skip").and_then(Value::as_bool) == Some(true) {
        return Ok(vec![]);
    }
    match parsed {
        Value::Array(items) => Ok(items),
        Value::Object(_) => Ok(vec![parsed]),
        _ => Err("expected a JSON object or array".to_string()),
    }
}

fn extract_json(text: &str) -> &str {
    // Strip markdown code fences if present: ```json ... ``` or ``` ... ```
    let stripped = text.trim();
    if let Some(inner) = stripped
        .strip_prefix("```json")
        .or_else(|| stripped.strip_prefix("```"))
    {
        if let Some(end) = inner.rfind("```") {
            return inner[..end].trim();
        }
    }
    if let (Some(start), Some(end)) = (stripped.find('['), stripped.rfind(']')) {
        return &stripped[start..=end];
    }
    // Backward-compatible object response.
    if let (Some(start), Some(end)) = (stripped.find('{'), stripped.rfind('}')) {
        return &stripped[start..=end];
    }
    stripped
}

// ---------------------------------------------------------------------------
// Build distiller from settings
// ---------------------------------------------------------------------------

pub fn build_distiller(config: &LlmConfig) -> std::sync::Arc<dyn Distiller + Send + Sync> {
    std::sync::Arc::new(HttpDistiller::new(config.clone()))
}

// ---------------------------------------------------------------------------
// LLM embedding provider (OpenAI-compatible /v1/embeddings)
// ---------------------------------------------------------------------------

pub struct LlmEmbeddingProvider {
    config: EmbeddingConfig,
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use std::cell::Cell;

    use serde_json::json;

    use std::time::Duration;

    use super::{
        backoff_delay, build_distill_prompt, distill_entry_with, distill_with,
        parse_distill_response, parse_embedding_response, status_is_retryable,
    };

    #[test]
    fn embedding_response_is_parsed_fail_closed() {
        // Happy path: correct dimension parses.
        let resp = json!({"data": [{"embedding": [0.1, 0.2, 0.3]}]});
        assert_eq!(parse_embedding_response(&resp, 3).unwrap(), vec![0.1f32, 0.2, 0.3]);

        // Wrong dimension is rejected, not silently accepted.
        assert!(parse_embedding_response(&resp, 4).is_err());

        // A non-numeric element fails the whole parse (no silent drop).
        let bad = json!({"data": [{"embedding": [0.1, "oops", 0.3]}]});
        assert!(parse_embedding_response(&bad, 3).is_err());

        // Missing embedding field is rejected.
        let shape = json!({"data": []});
        assert!(parse_embedding_response(&shape, 3).is_err());
    }

    #[test]
    fn only_rate_limit_and_5xx_are_retryable() {
        assert!(status_is_retryable(429));
        assert!(status_is_retryable(500));
        assert!(status_is_retryable(503));
        assert!(status_is_retryable(599));
        assert!(!status_is_retryable(400));
        assert!(!status_is_retryable(401));
        assert!(!status_is_retryable(404));
        assert!(!status_is_retryable(200));
    }

    #[test]
    fn backoff_is_exponential_and_honors_retry_after() {
        // Exponential schedule: 250ms, 500ms, 1s for attempts 1..3.
        assert_eq!(backoff_delay(1, None), Duration::from_millis(250));
        assert_eq!(backoff_delay(2, None), Duration::from_millis(500));
        assert_eq!(backoff_delay(3, None), Duration::from_millis(1000));
        // Retry-After overrides the schedule and is capped at 30s.
        assert_eq!(backoff_delay(1, Some(5)), Duration::from_secs(5));
        assert_eq!(backoff_delay(1, Some(120)), Duration::from_secs(30));
    }

    #[test]
    fn prompt_redacts_secrets_before_external_llm_call() {
        let prompt = build_distill_prompt(&json!({
            "query": "debug sk-12345678901234567890",
            "output_summary": "Authorization: Bearer secret-token-value"
        }));
        assert!(!prompt.contains("sk-12345678901234567890"));
        assert!(!prompt.contains("secret-token-value"));
        assert!(prompt.contains("[REDACTED]"));
    }

    #[test]
    fn malformed_response_is_retried_instead_of_silently_skipped() {
        let calls = Cell::new(0);
        let chunks = distill_with(&[json!({"id": "log-1", "query": "q"})], |_| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                Ok("not json".to_string())
            } else {
                Ok(r#"[{"content":"retry worked","trigger_desc":"retry"}]"#.to_string())
            }
        })
        .unwrap();
        assert_eq!(calls.get(), 2);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, "retry worked");
    }

    #[test]
    fn parser_accepts_multiple_distilled_chunks() {
        let parsed = parse_distill_response(
            r#"[{"content":"one"},{"content":"two","anti_trigger_desc":"never"}]"#,
        )
        .unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn nomination_is_distilled_instead_of_bypassing_the_model() {
        let prompt_seen = Cell::new(false);
        let entry = json!({
            "id": "log-1",
            "query": "original query",
            "nomination": "raw agent nomination",
            "output_summary": "summary",
            "outcome": "ok"
        });
        let chunks = distill_entry_with(&entry, std::slice::from_ref(&entry), |prompt| {
            prompt_seen.set(prompt.contains("raw agent nomination"));
            Ok(
                r#"[{"content":"generalized principle","trigger_desc":"generalize","anti_trigger_desc":null}]"#
                    .to_string(),
            )
        })
        .unwrap();

        assert!(prompt_seen.get());
        assert_eq!(chunks[0].content, "generalized principle");
        assert_eq!(
            chunks[0].nomination.as_deref(),
            Some("raw agent nomination")
        );
    }
}

impl LlmEmbeddingProvider {
    pub fn new(config: EmbeddingConfig) -> Self {
        Self { config }
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let api_key = self
            .config
            .resolved_api_key()
            .ok_or_else(|| InnateError::Other("Embedding API key not configured".into()))?;

        let base = self.config.resolved_base_url();
        let url = format!("{base}/embeddings");

        let body = json!({
            "input": text,
            "model": self.config.model_id,
        });

        let auth = format!("Bearer {api_key}");
        let resp_json = post_json_retry(
            &url,
            &[("Authorization", &auth)],
            &body,
            "Embedding",
        )?;

        parse_embedding_response(&resp_json, self.config.dim)
    }
}

/// Parse an OpenAI-compatible embedding response, fail-closed.
///
/// Every element must be numeric (bad entries are not silently dropped) and the
/// resulting length must equal `expected_dim`, so a malformed or wrong-dimension
/// vector never reaches cosine similarity.
fn parse_embedding_response(resp_json: &Value, expected_dim: usize) -> Result<Vec<f32>> {
    let embedding = resp_json
        .pointer("/data/0/embedding")
        .and_then(Value::as_array)
        .ok_or_else(|| InnateError::Other("unexpected embedding response shape".into()))?;
    let vec: Vec<f32> = embedding
        .iter()
        .map(|v| {
            v.as_f64().map(|x| x as f32).ok_or_else(|| {
                InnateError::Other("embedding response contains a non-numeric element".into())
            })
        })
        .collect::<Result<Vec<f32>>>()?;
    if vec.len() != expected_dim {
        return Err(InnateError::Other(format!(
            "embedding dimension mismatch: provider returned {}, expected {expected_dim} (check embedding.dim)",
            vec.len(),
        )));
    }
    Ok(vec)
}

impl EmbeddingProvider for LlmEmbeddingProvider {
    fn model_name(&self) -> &'static str {
        "llm-embedding"
    }

    fn content_dim(&self) -> usize {
        self.config.dim
    }

    fn trigger_dim(&self) -> usize {
        self.config.dim
    }

    fn embed_content(&self, text: &str) -> Result<Vec<f32>> {
        self.embed(text)
    }

    fn embed_trigger(&self, text: &str) -> Result<Vec<f32>> {
        self.embed(text)
    }

    /// Content and trigger share the same model and dimension here, so a single
    /// HTTP request serves both spaces — half the round trips per recall.
    fn embed_both(&self, text: &str) -> Result<(Vec<f32>, Vec<f32>)> {
        let v = self.embed(text)?;
        Ok((v.clone(), v))
    }
}

// ---------------------------------------------------------------------------
// Connection test helpers (used by install)
// ---------------------------------------------------------------------------

/// Test the LLM config by sending a minimal request. Returns Ok(model_response) or Err.
pub fn test_llm(config: &LlmConfig) -> Result<String> {
    let distiller = build_distiller(config);
    let dummy_log = json!({
        "id": "test",
        "query": "connection test",
        "output_summary": "test",
        "outcome": "ok"
    });
    // We don't care about the result — just that the call succeeds.
    distiller.distill(&[dummy_log])?;
    Ok(format!("OK — model: {}", config.model_id))
}

/// Test the embedding config. Returns Ok(dim) or Err.
pub fn test_embedding(config: &EmbeddingConfig) -> Result<usize> {
    let provider = LlmEmbeddingProvider::new(config.clone());
    let vec = provider.embed("connection test")?;
    Ok(vec.len())
}
