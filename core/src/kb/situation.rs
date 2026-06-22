//! Situation — the signal bundle that triggers intuition.
//!
//! Replaces the bare `query` as the unit of resonance. The key design constraint
//! (PRD §3.2 / Spec §2) is the **double-path split**:
//!
//! - **Resonance path** ([`Situation::embed_text`]): the *rich* situation joined into one
//!   string and embedded. Fine-grained is fine — it is a continuous cosine similarity that
//!   naturally tolerates fragments.
//! - **Calibration path** ([`Situation::context_key`]): the situation *coarsened* into a stable
//!   signature before hashing into a `context_key`. This MUST be coarse (stage + error_class +
//!   file_type, never the raw error text), otherwise every slightly-different situation becomes a
//!   new bucket, `chunk_context_stats` never accumulates ≥5 evidence, and calibration collapses
//!   to ~0 (`evidence_weight = min(evidence/5, 1)`).
//!
//! A pure-`query` situation degrades exactly to the legacy
//! `content_hash(normalize_query(query))` behaviour, so `recall()` stays zero-regression.

use crate::utils::content_hash;

use super::normalize_query;

/// The signal bundle that drives intuition. Borrowed and `Default`-able.
#[derive(Debug, Clone, Default)]
pub struct Situation<'a> {
    /// Explicit question. May be empty/absent for ambient appraisal.
    pub query: Option<&'a str>,
    /// Current or most-recent error text.
    pub last_error: Option<&'a str>,
    /// The last few actions taken (commands, edits, steps).
    pub recent_actions: &'a [String],
    /// Task stage (e.g. "merge", "implement", "review").
    pub stage: Option<&'a str>,
    /// File type / path summary in scope (e.g. "src/foo.tsx").
    pub file_context: Option<&'a str>,
}

impl<'a> Situation<'a> {
    /// Construct from a bare query — the legacy code path. Used by `recall` to keep
    /// existing callers' behaviour identical (degrades to `normalize_query`).
    pub fn from_query(query: &'a str) -> Self {
        Situation {
            query: Some(query),
            ..Default::default()
        }
    }

    /// True when only `query` carries signal — every other field empty. In this case both
    /// `embed_text` and `context_key` degrade to the legacy query-only behaviour.
    fn is_query_only(&self) -> bool {
        self.last_error.map(str::trim).unwrap_or("").is_empty()
            && self.recent_actions.iter().all(|a| a.trim().is_empty())
            && self.stage.map(str::trim).unwrap_or("").is_empty()
            && self.file_context.map(str::trim).unwrap_or("").is_empty()
    }

    /// **Resonance path.** Join the rich situation into one embed string. Labelled segments
    /// keep the embedder from blurring distinct signals together; empty fields are dropped.
    ///
    /// A query-only situation returns the query verbatim, so the embedding is byte-identical
    /// to the legacy `embed_both(query)` path (zero regression for `recall`).
    pub fn embed_text(&self) -> String {
        let query = self.query.map(str::trim).unwrap_or("");
        if self.is_query_only() {
            return query.to_string();
        }
        let mut parts: Vec<String> = Vec::new();
        if !query.is_empty() {
            parts.push(format!("[query] {query}"));
        }
        if let Some(err) = self.last_error.map(str::trim).filter(|s| !s.is_empty()) {
            parts.push(format!("[error] {err}"));
        }
        let actions: Vec<&str> = self
            .recent_actions
            .iter()
            .map(|a| a.trim())
            .filter(|a| !a.is_empty())
            .collect();
        if !actions.is_empty() {
            parts.push(format!("[actions] {}", actions.join(" ; ")));
        }
        if let Some(stage) = self.stage.map(str::trim).filter(|s| !s.is_empty()) {
            parts.push(format!("[stage] {stage}"));
        }
        if let Some(files) = self.file_context.map(str::trim).filter(|s| !s.is_empty()) {
            parts.push(format!("[files] {files}"));
        }
        parts.join("\n")
    }

    /// **Calibration path.** Hash the coarse signature into a stable `context_key`.
    ///
    /// `coarse_keys` selects which dimensions enter the signature (default
    /// `stage,error_class,file_type`). A query-only situation degrades to the legacy
    /// `content_hash(normalize_query(query))` so read/write buckets match historical data.
    pub fn context_key(&self, coarse_keys: &str) -> String {
        if self.is_query_only() {
            return content_hash(&normalize_query(self.query.unwrap_or("")));
        }
        content_hash(&self.coarse_signature(coarse_keys))
    }

    /// Build the coarse signature string, e.g. `stage=merge|err=TypeError|file=tsx`.
    /// Only the dimensions named in `coarse_keys` are included, in a fixed order, so the
    /// key is stable across runs. Never includes raw error text — only the error *class*.
    pub fn coarse_signature(&self, coarse_keys: &str) -> String {
        let keys: Vec<&str> = coarse_keys
            .split(',')
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .collect();
        let mut parts: Vec<String> = Vec::new();
        for key in keys {
            match key {
                "stage" => parts.push(format!(
                    "stage={}",
                    self.stage.map(str::trim).unwrap_or("").to_lowercase()
                )),
                "error_class" => parts.push(format!("err={}", self.error_class())),
                "file_type" => parts.push(format!("file={}", self.file_type())),
                // Unknown dimension: ignore rather than poison the signature.
                _ => {}
            }
        }
        parts.join("|")
    }

    /// Normalise `last_error` to a stable category — the command of the no-blow-up rule.
    /// Strategy (no original text leaks through):
    /// 1. A typed error name (`TypeError`, `NullPointerException`, …) → that name.
    /// 2. A Rust diagnostic code (`error[E0599]` / `E0277`) → the code.
    /// 3. A panic → `panic`.
    /// 4. Otherwise the first alphabetic token, lowercased.
    fn error_class(&self) -> String {
        let err = self.last_error.map(str::trim).unwrap_or("");
        if err.is_empty() {
            return String::new();
        }
        // Rust diagnostic code: E followed by digits (optionally inside error[...]).
        if let Some(code) = find_rust_error_code(err) {
            return code;
        }
        // Typed error name: an identifier ending in Error / Exception.
        if let Some(name) = err
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .find(|tok| tok.len() > 3 && (tok.ends_with("Error") || tok.ends_with("Exception")))
        {
            return name.to_string();
        }
        let low = err.to_lowercase();
        if low.contains("panic") {
            return "panic".to_string();
        }
        // Fallback: first alphabetic token, lowercased, truncated.
        low.split(|c: char| !c.is_alphabetic())
            .find(|t| !t.is_empty())
            .map(|t| t.chars().take(24).collect())
            .unwrap_or_default()
    }

    /// Extract a coarse file type from `file_context` — the extension of the last path token,
    /// or the bare token if no extension. Never the full path.
    fn file_type(&self) -> String {
        let ctx = self.file_context.map(str::trim).unwrap_or("");
        if ctx.is_empty() {
            return String::new();
        }
        // Take the first whitespace/comma-separated path token.
        let token = ctx
            .split(|c: char| c.is_whitespace() || c == ',')
            .find(|t| !t.is_empty())
            .unwrap_or("");
        match token.rsplit_once('.') {
            Some((_, ext)) if !ext.is_empty() && ext.len() <= 8 => ext.to_lowercase(),
            _ => token
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(token)
                .to_lowercase(),
        }
    }
}

/// Find a Rust-style error code `E<digits>` anywhere in the text. Returns e.g. `E0599`.
fn find_rust_error_code(err: &str) -> Option<String> {
    let bytes = err.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if (bytes[i] == b'E' || bytes[i] == b'e')
            && i + 1 < bytes.len()
            && bytes[i + 1].is_ascii_digit()
        {
            let start = i;
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            // At least 3 digits to look like a real diagnostic code (E0599), avoid "e5".
            if j - (start + 1) >= 3 {
                return Some(format!("E{}", &err[start + 1..j]));
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_only_degrades_to_legacy_key() {
        let s = Situation::from_query("How to fix the Merge?");
        let legacy = content_hash(&normalize_query("How to fix the Merge?"));
        assert_eq!(s.context_key("stage,error_class,file_type"), legacy);
        // embed_text is byte-identical to the bare query (after trim).
        assert_eq!(s.embed_text(), "How to fix the Merge?");
    }

    #[test]
    fn context_key_stable_across_differing_error_text() {
        // Same stage + error class + file type, different raw error message → same key.
        let a = Situation {
            stage: Some("merge"),
            last_error: Some("TypeError: cannot read property 'x' of undefined at line 42"),
            file_context: Some("src/components/Foo.tsx"),
            ..Default::default()
        };
        let b = Situation {
            stage: Some("merge"),
            last_error: Some("TypeError: undefined is not a function in handler"),
            file_context: Some("src/pages/Bar.tsx"),
            ..Default::default()
        };
        let keys = "stage,error_class,file_type";
        assert_eq!(a.context_key(keys), b.context_key(keys));
        assert_eq!(
            a.coarse_signature(keys),
            "stage=merge|err=TypeError|file=tsx"
        );
    }

    #[test]
    fn differing_class_yields_different_key() {
        let keys = "stage,error_class,file_type";
        let a = Situation {
            stage: Some("merge"),
            last_error: Some("TypeError: boom"),
            file_context: Some("a.tsx"),
            ..Default::default()
        };
        let b = Situation {
            stage: Some("merge"),
            last_error: Some("RangeError: boom"),
            file_context: Some("a.tsx"),
            ..Default::default()
        };
        assert_ne!(a.context_key(keys), b.context_key(keys));
    }

    #[test]
    fn rust_error_code_classified() {
        let s = Situation {
            stage: Some("build"),
            last_error: Some("error[E0599]: no method named `foo` found"),
            file_context: Some("src/lib.rs"),
            ..Default::default()
        };
        assert_eq!(s.coarse_signature("error_class"), "err=E0599");
    }

    #[test]
    fn embed_text_includes_all_nonempty_fields() {
        let actions = vec!["git merge".to_string(), "cargo test".to_string()];
        let s = Situation {
            query: Some("why did merge fail"),
            last_error: Some("conflict"),
            recent_actions: &actions,
            stage: Some("merge"),
            file_context: Some("Cargo.toml"),
        };
        let text = s.embed_text();
        assert!(text.contains("[query] why did merge fail"));
        assert!(text.contains("[error] conflict"));
        assert!(text.contains("git merge"));
        assert!(text.contains("[stage] merge"));
        assert!(text.contains("[files] Cargo.toml"));
    }
}
