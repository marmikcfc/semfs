//! fastembed-backed local cross-encoder reranker.
//!
//! The model still comes from fastembed-rs's registry catalogue, but we load a
//! chosen ONNX VARIANT (e.g. the int8 quantization) rather than the registry's
//! pinned full-precision `onnx/model.onnx`: fastembed's `RerankInitOptions` has
//! no model-file/quantization override, so we fetch the variant + tokenizer from
//! the model's HF repo via hf-hub (auto-downloaded + cached in the same cache as
//! fastembed) and load it through fastembed's user-defined reranking path.

use std::path::PathBuf;
use std::sync::Mutex;

use fastembed::{
    OnnxSource, RerankInitOptionsUserDefined, RerankerModel, TextRerank, TokenizerFiles,
    UserDefinedRerankingModel,
};
use hf_hub::{api::sync::ApiBuilder, Cache};

use super::Reranker;

/// Standard BERT/MiniLM special tokens, used when a model repo omits
/// `special_tokens_map.json`.
const DEFAULT_SPECIAL_TOKENS_MAP: &str = r#"{"unk_token":"[UNK]","sep_token":"[SEP]","pad_token":"[PAD]","cls_token":"[CLS]","mask_token":"[MASK]"}"#;

/// A local ONNX cross-encoder reranker backed by a fastembed registry model.
pub struct LocalReranker {
    // fastembed `rerank` borrows the model mutably; the trait is `&self`.
    model: Mutex<TextRerank>,
}

impl LocalReranker {
    /// Load a SPECIFIC ONNX variant of a fastembed registry reranker (e.g.
    /// `onnx/model_int8.onnx`). The variant + tokenizer files are fetched from the
    /// model's HF repo via hf-hub (cached under fastembed's cache dir) and loaded
    /// through the user-defined path — this is how we use the int8 build, since
    /// the registry's own loader is pinned to the full-precision `onnx/model.onnx`.
    pub fn from_registry_onnx(
        model: RerankerModel,
        onnx_file: &str,
        cache_dir: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let cache = Cache::new(cache_dir.unwrap_or_else(|| PathBuf::from(fastembed::get_cache_dir())));
        let api = ApiBuilder::from_cache(cache)
            .with_progress(false)
            .build()
            .map_err(|e| anyhow::anyhow!("hf-hub api init: {e}"))?;
        // `RerankerModel`'s Display is its HF repo id (model_code).
        let repo = api.model(model.to_string());

        let fetch = |file: &str| -> anyhow::Result<PathBuf> {
            repo.get(file)
                .map_err(|e| anyhow::anyhow!("fetch {file} from {model}: {e}"))
        };
        let read = |file: &str| -> anyhow::Result<Vec<u8>> { Ok(std::fs::read(fetch(file)?)?) };

        let onnx_path = fetch(onnx_file)?;
        let special_tokens_map_file = repo
            .get("special_tokens_map.json")
            .ok()
            .and_then(|p| std::fs::read(p).ok())
            .unwrap_or_else(|| DEFAULT_SPECIAL_TOKENS_MAP.as_bytes().to_vec());
        let tokenizer_files = TokenizerFiles {
            tokenizer_file: read("tokenizer.json")?,
            config_file: read("config.json")?,
            special_tokens_map_file,
            tokenizer_config_file: read("tokenizer_config.json")?,
        };

        let udm = UserDefinedRerankingModel::new(OnnxSource::File(onnx_path), tokenizer_files);
        let model = TextRerank::try_new_from_user_defined(udm, RerankInitOptionsUserDefined::default())
            .map_err(|e| anyhow::anyhow!("load reranker {onnx_file}: {e}"))?;
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

    /// Live (network) test: downloads the int8 reranker and checks it discriminates.
    /// Gated behind RUN_FASTEMBED so the default `cargo test` stays offline/fast.
    #[test]
    fn registry_int8_reranker_scores_relevant_above_irrelevant() {
        if std::env::var("RUN_FASTEMBED").is_err() {
            eprintln!("skipping: set RUN_FASTEMBED=1 to download + run the int8 reranker");
            return;
        }
        let r = LocalReranker::from_registry_onnx(
            RerankerModel::JINARerankerV2BaseMultiligual,
            "onnx/model_int8.onnx",
            None,
        )
        .unwrap();
        let docs = vec![
            "To reset your password, click 'forgot password' and follow the email link.".to_string(),
            "Bananas are a good source of potassium and dietary fiber.".to_string(),
        ];
        let scores = r.rerank("how do I reset my account password", &docs).unwrap();
        assert_eq!(scores.len(), 2);
        assert!(scores[0] > scores[1], "password doc must outscore banana doc");
    }
}
