//! `StubEmbedder` — a deterministic, dependency-free embedder used ONLY as a test
//! double. It feature-hashes tokens into `dims` signed buckets, so it has no real
//! semantics, but it always loads and is reproducible — exactly what the backend
//! store tests need to exercise seed/search/rank without downloading a real model.
//!
//! It is gated `#[cfg(test)]` (see `embed/mod.rs`) so it never reaches the shipped
//! crate or public API — it is not a `SEMFS_EMBED_BACKEND` option. Production code
//! must use a real embedder (`LocalEmbedder`/`OpenAiEmbedder`); cloud storage uses
//! none. See tickets/remove-hash-embedder.

use super::Embedder;

/// Deterministic feature-hash embedder, for tests only. Hashes tokens into `dims`
/// signed buckets and L2-normalizes — captures lexical overlap, no semantics.
#[derive(Debug, Clone)]
pub struct StubEmbedder {
    dims: usize,
}

impl StubEmbedder {
    pub fn new(dims: usize) -> Self {
        Self { dims }
    }
}

impl Default for StubEmbedder {
    fn default() -> Self {
        Self::new(384)
    }
}

impl Embedder for StubEmbedder {
    fn dimensions(&self) -> usize {
        self.dims
    }

    fn identity(&self) -> String {
        format!("stub:{}", self.dims)
    }

    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| embed_one(t, self.dims)).collect())
    }
}

/// Hash each lowercased alphanumeric token into a signed bucket, then L2-normalize.
fn embed_one(text: &str, dims: usize) -> Vec<f32> {
    let mut v = vec![0f32; dims];
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        if raw.is_empty() {
            continue;
        }
        let h = fnv1a(raw.to_lowercase().as_bytes());
        let idx = (h % dims as u64) as usize;
        let sign = if (h >> 33) & 1 == 0 { 1.0 } else { -1.0 };
        v[idx] += sign;
    }
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

/// FNV-1a 64-bit — small, fast, dependency-free, deterministic across runs.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::cosine;

    #[test]
    fn dimensions_match_output_width() {
        let e = StubEmbedder::new(64);
        let out = e.embed(&["hello world".to_string()]).unwrap();
        assert_eq!(out[0].len(), 64);
        assert_eq!(e.dimensions(), 64);
    }

    #[test]
    fn deterministic_same_text_same_vector() {
        let e = StubEmbedder::default();
        let a = e.embed(&["authentication and login".to_string()]).unwrap();
        let b = e.embed(&["authentication and login".to_string()]).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn lexical_overlap_scores_higher_than_disjoint() {
        let e = StubEmbedder::default();
        let v = |s: &str| e.embed(&[s.to_string()]).unwrap().pop().unwrap();
        let base = v("user login and credential check");
        let overlap = v("login user credential");
        let disjoint = v("banana sunset orchestra");
        assert!(
            cosine(&base, &overlap) > cosine(&base, &disjoint),
            "shared tokens must score higher than unrelated text"
        );
    }
}
