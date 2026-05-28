//! fastembed-backed local cross-encoder reranker using the **registry** API.
//!
//! The model comes from fastembed-rs's built-in `RerankerModel` registry (auto-
//! downloaded + cached). Bring-your-own-ONNX is intentionally deferred.

use std::path::PathBuf;
use std::sync::Mutex;

use fastembed::{RerankInitOptions, RerankerModel, TextRerank};

use super::Reranker;

/// A local ONNX cross-encoder reranker backed by a fastembed registry model.
pub struct LocalReranker {
    // fastembed `rerank` borrows the model mutably; the trait is `&self`.
    model: Mutex<TextRerank>,
}

impl LocalReranker {
    /// Build from a fastembed registry reranker. Downloads + caches on first use.
    ///
    /// NOTE: the fastembed registry pins each reranker to a single ONNX file
    /// (e.g. `JINARerankerV2BaseMultiligual` → `onnx/model.onnx`, full precision);
    /// there is no registry knob to select an int8/quantized variant. Using a
    /// quantized ONNX would require the (deferred) bring-your-own-ONNX path.
    pub fn from_registry(model: RerankerModel, cache_dir: Option<PathBuf>) -> anyhow::Result<Self> {
        let mut opts = RerankInitOptions::new(model).with_show_download_progress(false);
        if let Some(dir) = cache_dir {
            opts = opts.with_cache_dir(dir);
        }
        let model = TextRerank::try_new(opts)?;
        Ok(Self {
            model: Mutex::new(model),
        })
    }
}

impl std::fmt::Debug for LocalReranker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalReranker").finish_non_exhaustive()
    }
}

impl Reranker for LocalReranker {
    fn rerank(&self, query: &str, docs: &[String]) -> anyhow::Result<Vec<f32>> {
        if docs.is_empty() {
            return Ok(vec![]);
        }
        let docs_ref: Vec<&str> = docs.iter().map(|s| s.as_str()).collect();
        let mut model = self
            .model
            .lock()
            .map_err(|_| anyhow::anyhow!("reranker mutex poisoned"))?;
        // return_documents=false: we only need scores, mapped back to input order.
        let results = model.rerank(query, docs_ref, false, None)?;
        let mut scores = vec![0f32; docs.len()];
        for r in results {
            if r.index < scores.len() {
                scores[r.index] = r.score;
            }
        }
        Ok(scores)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live (network) test: downloads the reranker and checks it discriminates.
    /// Gated behind RUN_FASTEMBED so the default `cargo test` stays offline/fast.
    #[test]
    fn registry_reranker_scores_relevant_above_irrelevant() {
        if std::env::var("RUN_FASTEMBED").is_err() {
            eprintln!("skipping: set RUN_FASTEMBED=1 to download + run the registry reranker");
            return;
        }
        let r =
            LocalReranker::from_registry(RerankerModel::JINARerankerV2BaseMultiligual, None).unwrap();
        let docs = vec![
            "To reset your password, click 'forgot password' and follow the email link.".to_string(),
            "Bananas are a good source of potassium and dietary fiber.".to_string(),
        ];
        let scores = r.rerank("how do I reset my account password", &docs).unwrap();
        assert_eq!(scores.len(), 2);
        assert!(scores[0] > scores[1], "password doc must outscore banana doc");
    }
}
