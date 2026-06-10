use chrono::Utc;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub fn utc_now_iso() -> String {
    // Matches Python: YYYY-MM-DDTHH:MM:SS.mmmZ (3-digit ms, dictionary-comparable)
    let now = Utc::now();
    now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

pub fn gen_uuid() -> String {
    Uuid::new_v4().to_string()
}

pub fn content_hash(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// Rough token estimate: 1 token ≈ 4 chars (matches Python heuristic).
pub fn estimate_tokens(text: &str) -> usize {
    (text.len() + 3) / 4
}

/// Default content sanitizer — strips NUL bytes; strips lines that look like
/// raw stack traces or raw diffs. Mirrors the Python `default_sanitize`.
pub fn default_sanitize(content: &str) -> Option<String> {
    let cleaned: String = content
        .lines()
        .filter(|l| {
            let t = l.trim();
            // Reject lines that are pure stack-trace noise
            !(t.starts_with("Traceback") || t.starts_with("File \"") || t.starts_with(">>>"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let cleaned = cleaned.replace('\0', "");
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() { None } else { Some(cleaned) }
}

/// Pack a Vec<f32> into bytes (little-endian f32 array).
pub fn pack_embedding(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Unpack bytes into Vec<f32>.
pub fn unpack_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

/// Cosine similarity between two equal-length slices. Returns 0.0 on zero norms.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na * nb) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ts_format() {
        let ts = utc_now_iso();
        assert!(ts.ends_with('Z'), "bad format: {ts}");
        assert_eq!(ts.len(), 24, "expected 24 chars: {ts}"); // 2024-01-15T08:30:00.000Z
    }

    #[test]
    fn cosine_identical() {
        let v = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn pack_roundtrip() {
        let v = vec![0.1_f32, 0.5, -0.3];
        assert_eq!(unpack_embedding(&pack_embedding(&v)), v);
    }
}
