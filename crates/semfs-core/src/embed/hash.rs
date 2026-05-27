//! Deterministic, dependency-free embedder — the fail-open floor.

use super::Embedder;

/// Hashes tokens into `dims` signed buckets and L2-normalizes. Captures lexical
/// overlap only (no real semantics), but it always loads and is deterministic,
/// so search keeps working when no real model is available (Finding F3 floor).
#[derive(Debug, Clone)]
pub struct HashEmbedder {
    dims: usize,
}

impl HashEmbedder {
    pub fn new(dims: usize) -> Self {
        Self { dims }
    }
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self::new(384)
    }
}

impl Embedder for HashEmbedder {
    fn dimensions(&self) -> usize {
        self.dims
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
        let e = HashEmbedder::new(64);
        let out = e.embed(&["hello world".to_string()]).unwrap();
        assert_eq!(out[0].len(), 64);
        assert_eq!(e.dimensions(), 64);
    }

    #[test]
    fn deterministic_same_text_same_vector() {
        let e = HashEmbedder::default();
        let a = e.embed(&["authentication and login".to_string()]).unwrap();
        let b = e.embed(&["authentication and login".to_string()]).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn lexical_overlap_scores_higher_than_disjoint() {
        let e = HashEmbedder::default();
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
