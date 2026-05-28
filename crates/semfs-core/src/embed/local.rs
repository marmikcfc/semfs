//! fastembed-backed local embedder using the **registry** API.
//!
//! Models come from fastembed-rs's built-in `EmbeddingModel` registry (auto-
//! downloaded + cached on first use); fastembed applies each model's correct
//! pooling/normalization internally, so there is no hand-rolled pooling here.
//! (Bring-your-own-ONNX is intentionally deferred — see the project goal.)

use std::path::PathBuf;
use std::sync::Mutex;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

use super::Embedder;

/// A local ONNX embedder backed by a fastembed registry model.
pub struct LocalEmbedder {
    // fastembed's `embed` takes `&mut self`; the `Embedder` trait is `&self`, so
    // we guard the model behind a Mutex (embedding is sequential anyway).
    model: Mutex<TextEmbedding>,
    dims: usize,
    /// Stable identity: `fastembed:<model_code>:<dims>` — persisted with an index
    /// so a reader can detect a model swap (see SqliteVecStore::is_searchable).
    identity: String,
}

impl LocalEmbedder {
    /// Build from a fastembed registry model. Downloads + caches the ONNX on
    /// first use (`cache_dir` overrides fastembed's default cache location).
    /// The vector width is read from the registry, never guessed.
    pub fn from_registry(model: EmbeddingModel, cache_dir: Option<PathBuf>) -> anyhow::Result<Self> {
        let info = TextEmbedding::list_supported_models()
            .into_iter()
            .find(|m| m.model == model)
            .ok_or_else(|| anyhow::anyhow!("embedding model {model:?} not in fastembed registry"))?;
        let dims = info.dim;
        let identity = format!("fastembed:{}:{}", info.model_code, dims);

        let mut opts = InitOptions::new(model).with_show_download_progress(false);
        if let Some(dir) = cache_dir {
            opts = opts.with_cache_dir(dir);
        }
        let model = TextEmbedding::try_new(opts)?;
        Ok(Self {
            model: Mutex::new(model),
            dims,
            identity,
        })
    }
}

impl std::fmt::Debug for LocalEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalEmbedder")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl Embedder for LocalEmbedder {
    fn dimensions(&self) -> usize {
        self.dims
    }

    fn identity(&self) -> String {
        self.identity.clone()
    }

    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let mut model = self
            .model
            .lock()
            .map_err(|_| anyhow::anyhow!("embedder mutex poisoned"))?;
        model.embed(texts, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::cosine;

    /// Live (network) test: downloads the text model and checks semantic order.
    /// Gated behind RUN_FASTEMBED so the default `cargo test` stays offline/fast.
    #[test]
    fn registry_text_embedder_orders_by_meaning() {
        if std::env::var("RUN_FASTEMBED").is_err() {
            eprintln!("skipping: set RUN_FASTEMBED=1 to download + run the registry model");
            return;
        }
        let e = LocalEmbedder::from_registry(EmbeddingModel::SnowflakeArcticEmbedS, None).unwrap();
        assert_eq!(e.dimensions(), 384);
        let v = e
            .embed(&[
                "user login and credential verification".to_string(),
                "banana bread recipe".to_string(),
                "how does authentication work".to_string(),
            ])
            .unwrap();
        // The auth query is closer to the auth doc than to the recipe.
        assert!(cosine(&v[2], &v[0]) > cosine(&v[2], &v[1]));
    }
}
