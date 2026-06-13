use crate::errors::Result;
use crate::utils::{content_hash, pack_embedding};

/// Embedding provider trait — swap for real models at construction time.
pub trait EmbeddingProvider: Send + Sync {
    fn model_name(&self) -> &'static str {
        "custom"
    }
    fn content_dim(&self) -> usize;
    fn trigger_dim(&self) -> usize;
    fn embed_content(&self, text: &str) -> Result<Vec<f32>>;
    fn embed_trigger(&self, text: &str) -> Result<Vec<f32>>;

    /// Embed `text` for both the content and trigger spaces. Default issues two
    /// separate calls. Providers backed by a single shared model (e.g. a remote
    /// embedding endpoint) should override this to make one request and avoid the
    /// duplicate round trip on the recall hot path.
    fn embed_both(&self, text: &str) -> Result<(Vec<f32>, Vec<f32>)> {
        Ok((self.embed_content(text)?, self.embed_trigger(text)?))
    }
}

/// Hash-based deterministic embeddings — no model needed, good for tests.
pub struct DummyEmbeddingProvider {
    content_dim: usize,
    trigger_dim: usize,
}

impl DummyEmbeddingProvider {
    pub fn new(content_dim: usize, trigger_dim: usize) -> Self {
        Self {
            content_dim,
            trigger_dim,
        }
    }
}

impl Default for DummyEmbeddingProvider {
    fn default() -> Self {
        Self::new(1024, 256)
    }
}

impl EmbeddingProvider for DummyEmbeddingProvider {
    fn model_name(&self) -> &'static str {
        "DummyEmbeddingProvider"
    }

    fn content_dim(&self) -> usize {
        self.content_dim
    }
    fn trigger_dim(&self) -> usize {
        self.trigger_dim
    }

    fn embed_content(&self, text: &str) -> Result<Vec<f32>> {
        Ok(hash_to_vec(text, self.content_dim))
    }

    fn embed_trigger(&self, text: &str) -> Result<Vec<f32>> {
        Ok(hash_to_vec(text, self.trigger_dim))
    }
}

fn hash_to_vec(text: &str, dim: usize) -> Vec<f32> {
    let h = content_hash(text);
    let bytes = h.as_bytes();
    let mut v: Vec<f32> = (0..dim)
        .map(|i| {
            let b = bytes[i % bytes.len()] as f32;
            (b / 255.0) * 2.0 - 1.0
        })
        .collect();
    // L2-normalise
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

/// Serialise a provider's embedding as raw bytes for DB storage.
pub fn embed_to_bytes(
    provider: &dyn EmbeddingProvider,
    text: &str,
    trigger: bool,
) -> Result<Vec<u8>> {
    let vec = if trigger {
        provider.embed_trigger(text)?
    } else {
        provider.embed_content(text)?
    };
    Ok(pack_embedding(&vec))
}
