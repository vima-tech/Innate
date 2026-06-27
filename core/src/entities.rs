//! Deterministic entity extraction for associative (spreading-activation) recall.
//!
//! SAG-inspired: a chunk's high-signal tokens (error codes, CLI flags, paths,
//! code symbols) become index entities in `chunk_entities`. Two chunks that share
//! an entity are associatively linked even when their prose is dissimilar, which
//! lets `recall` spread activation across that link — the ACT-R associative term
//! (`S_ji`) that base-level activation alone cannot supply.
//!
//! Deterministic on purpose — no LLM, no `regex` dependency (manual char
//! scanning) — so it runs on the no-LLM capture hot path *and* in migration
//! backfill. Mirrors the project constraint that capture never depends on the LLM
//! (see `ResilientDistiller`).
//!
//! Deliberately ignores plain lowercase words: those are already recoverable via
//! the lexical/BM25 channel, and entities must stay **discriminative** so the
//! ACT-R fan term keeps promiscuous tokens from dominating the spread.

use std::collections::HashSet;

/// One extracted entity: the normalized join key plus a coarse type tag. The tag
/// is diagnostic only — recall keys on `entity`, never on `etype`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedEntity {
    pub entity: String,
    pub etype: &'static str,
}

/// Cap per chunk so a pathological blob can't bloat the index.
const MAX_ENTITIES_PER_CHUNK: usize = 64;
const MIN_ENTITY_LEN: usize = 2;

/// Punctuation trimmed from both ends of a token. Excludes `-` `/` `_` `:` so
/// flags (`--release`), paths (`core/src/x.rs`), snake_case and `::` paths keep
/// their structure; `.` is included so trailing sentence dots drop while internal
/// dots (`file.rs`) survive (trim is end-anchored, not internal).
const TRIM: &[char] = &[
    '"', '\'', '`', ',', ';', '.', '(', ')', '[', ']', '{', '}', '?', '!', '<', '>', '*', '|',
    '=', '@', '#', '%', '\\',
];

/// Extract high-signal entities from a chunk's content and optional trigger desc.
pub fn extract_entities(content: &str, trigger_desc: Option<&str>) -> Vec<ExtractedEntity> {
    let mut raw: Vec<(String, &'static str)> = Vec::new();
    for text in [Some(content), trigger_desc].into_iter().flatten() {
        collect_backtick_spans(text, &mut raw);
        for tok in text.split_whitespace() {
            if let Some(cl) = classify(tok) {
                raw.push(cl);
            }
        }
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<ExtractedEntity> = Vec::new();
    for (entity, etype) in raw {
        if entity.len() >= MIN_ENTITY_LEN && seen.insert(entity.clone()) {
            out.push(ExtractedEntity { entity, etype });
            if out.len() >= MAX_ENTITIES_PER_CHUNK {
                break;
            }
        }
    }
    out
}

/// Backtick-delimited spans are author-marked code, so their tokens clear a lower
/// bar: any identifier-like piece becomes a `symbol` even if it would otherwise
/// look like a plain word (e.g. `` `recall` ``).
fn collect_backtick_spans(text: &str, raw: &mut Vec<(String, &'static str)>) {
    let mut in_span = false;
    let mut buf = String::new();
    for ch in text.chars() {
        if ch == '`' {
            if in_span {
                for piece in buf.split_whitespace() {
                    if let Some(cl) = classify(piece) {
                        raw.push(cl);
                    } else if let Some(sym) = as_identifier(piece) {
                        raw.push((sym, "symbol"));
                    }
                }
                buf.clear();
            }
            in_span = !in_span;
        } else if in_span {
            buf.push(ch);
        }
    }
}

/// Classify a whitespace-delimited token into a high-signal entity, or `None` for
/// plain prose. Normalization lowercases for join stability — both the stored
/// chunk text and the recall query run through this same function.
fn classify(tok: &str) -> Option<(String, &'static str)> {
    // Flags keep their leading dashes (TRIM excludes '-'); detect before trimming.
    let pre = tok.trim_matches(TRIM);
    if pre.starts_with("--") && pre.len() >= 4 && pre[2..].chars().all(is_ident_char) {
        return Some((pre.to_lowercase(), "flag"));
    }
    let t = pre;
    if t.len() < MIN_ENTITY_LEN {
        return None;
    }

    // Error code: short alpha prefix + >=3 digits, nothing else (E0277, GH1234).
    if let Some((alpha, digits)) = split_alpha_digits(t) {
        if (1..=4).contains(&alpha.len()) && digits.len() >= 3 {
            return Some((t.to_lowercase(), "error"));
        }
    }

    // Rust-style path / namespaced symbol.
    if t.contains("::") && t.chars().all(is_ident_char) {
        return Some((t.to_lowercase(), "path"));
    }
    // Filesystem path: a slash plus a dotted file component.
    if t.contains('/') && t.contains('.') && t.chars().all(is_ident_char) {
        return Some((t.trim_end_matches('/').to_lowercase(), "path"));
    }

    // snake_case / kebab identifier.
    if (t.contains('_') || t.contains('-')) && is_identifier_token(t) {
        return Some((t.to_lowercase(), "symbol"));
    }
    // CamelCase / mixedCase identifier.
    if is_camel_case(t) {
        return Some((t.to_lowercase(), "symbol"));
    }
    None
}

/// Lower bar used inside backtick spans: accept any identifier-like token.
fn as_identifier(tok: &str) -> Option<String> {
    let t = tok.trim_matches(TRIM);
    if t.len() >= MIN_ENTITY_LEN && is_identifier_token(t) {
        Some(t.to_lowercase())
    } else {
        None
    }
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '.' | '/')
}

/// All chars are identifier chars and at least one is alphabetic (excludes bare
/// numbers / version strings like `1.2.3`).
fn is_identifier_token(t: &str) -> bool {
    t.chars().all(is_ident_char) && t.chars().any(|c| c.is_alphabetic())
}

/// Split a token into a leading alphabetic run and a trailing all-digit run,
/// rejecting anything with other characters in between.
fn split_alpha_digits(t: &str) -> Option<(&str, &str)> {
    let split = t.find(|c: char| c.is_ascii_digit())?;
    let (alpha, digits) = t.split_at(split);
    if !alpha.is_empty()
        && alpha.chars().all(|c| c.is_ascii_alphabetic())
        && !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
    {
        Some((alpha, digits))
    } else {
        None
    }
}

/// Has an internal lower→upper transition (mixedCase / CamelCase) and is otherwise
/// alphanumeric — captures `recallCandidates`, `KnowledgeBase`, `BM25`.
fn is_camel_case(t: &str) -> bool {
    if !t.chars().all(|c| c.is_alphanumeric()) {
        return false;
    }
    let bytes: Vec<char> = t.chars().collect();
    bytes
        .windows(2)
        .any(|w| w[0].is_lowercase() && w[1].is_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ents(content: &str) -> Vec<String> {
        extract_entities(content, None)
            .into_iter()
            .map(|e| e.entity)
            .collect()
    }

    #[test]
    fn extracts_error_codes() {
        assert!(ents("hit error E0277 while building").contains(&"e0277".to_string()));
        // Bare numbers and version strings are not entities.
        assert!(!ents("bumped to 1.2.3 in 2026").contains(&"1.2.3".to_string()));
    }

    #[test]
    fn extracts_flags_and_paths() {
        let e = ents("run cargo build --release and edit core/src/kb/recall.rs");
        assert!(e.contains(&"--release".to_string()));
        assert!(e.contains(&"core/src/kb/recall.rs".to_string()));
    }

    #[test]
    fn extracts_symbols_not_plain_words() {
        let e = ents("call KnowledgeBase::recall via the snake_case helper get_deps");
        assert!(e.contains(&"knowledgebase::recall".to_string()));
        assert!(e.contains(&"snake_case".to_string()));
        assert!(e.contains(&"get_deps".to_string()));
        // plain prose is left to the lexical channel
        assert!(!e.contains(&"call".to_string()));
        assert!(!e.contains(&"via".to_string()));
    }

    #[test]
    fn backtick_spans_lower_the_bar() {
        let e = ents("the `recall` method matters");
        assert!(e.contains(&"recall".to_string()));
        // outside backticks, the same lowercase word is ignored
        assert!(!ents("recall the method").contains(&"recall".to_string()));
    }

    #[test]
    fn dedups_and_caps() {
        let e = extract_entities("E0277 E0277 E0277", None);
        assert_eq!(e.iter().filter(|x| x.entity == "e0277").count(), 1);
    }
}
