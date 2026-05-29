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
use hf_hub::{api::sync::ApiBuilder, Cache, Repo, RepoType};

use super::Reranker;

/// A local ONNX cross-encoder reranker backed by a fastembed registry model.
pub struct LocalReranker {
    // fastembed `rerank` borrows the model mutably; the trait is `&self`.
    model: Mutex<TextRerank>,
}

impl LocalReranker {
    /// Load a SPECIFIC ONNX variant of a fastembed registry reranker (e.g.
    /// `onnx/model_int8.onnx`) at a PINNED `revision` (commit SHA). The variant +
    /// tokenizer files are fetched from the model's HF repo via hf-hub (cached
    /// under fastembed's cache dir) and loaded through the user-defined path —
    /// this is how we use the int8 build, since the registry's own loader is
    /// pinned to the full-precision `onnx/model.onnx`. Pinning the revision keeps
    /// the assets reproducible across builds/machines (an HF HEAD update can't
    /// silently swap the model or its tokenizer underneath us).
    pub fn from_registry_onnx(
        model: RerankerModel,
        onnx_file: &str,
        revision: &str,
        cache_dir: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let cache = Cache::new(cache_dir.unwrap_or_else(|| PathBuf::from(fastembed::get_cache_dir())));
        let api = ApiBuilder::from_cache(cache)
            .with_progress(false)
            .build()
            .map_err(|e| anyhow::anyhow!("hf-hub api init: {e}"))?;
        // `RerankerModel`'s Display is its HF repo id (model_code); pin the revision.
        let repo = api.repo(Repo::with_revision(
            model.to_string(),
            RepoType::Model,
            revision.to_string(),
        ));

        let fetch = |file: &str| -> anyhow::Result<PathBuf> {
            repo.get(file)
                .map_err(|e| anyhow::anyhow!("fetch {file} from {model}: {e}"))
        };
        let read = |file: &str| -> anyhow::Result<Vec<u8>> { Ok(std::fs::read(fetch(file)?)?) };

        let onnx_path = fetch(onnx_file)?;
        // All four tokenizer files are required and hard-fail on any fetch/read
        // error: a swallowed `special_tokens_map.json` failure would silently
        // substitute generic BERT special tokens, producing wrong rerank scores
        // with no signal. On error the caller fails open to NO rerank instead.
        let tokenizer_files = TokenizerFiles {
            tokenizer_file: read("tokenizer.json")?,
            config_file: read("config.json")?,
            special_tokens_map_file: read("special_tokens_map.json")?,
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

// The int8 reranker is validated live (download + load + score discrimination)
// by the holistic e2e harness `crates/e2e/phase_local_l1_l5.sh`, which runs grep
// through a real mount with this reranker in the pipeline — so there's no
// network/download test in the default `cargo test` here.
