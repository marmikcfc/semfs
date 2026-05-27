//! fastembed-backed local cross-encoder reranker — offline, no download when
//! pointed at an already-present model directory.

use std::path::Path;
use std::sync::Mutex;

use fastembed::{
    RerankInitOptionsUserDefined, TextRerank, TokenizerFiles, UserDefinedRerankingModel,
};

use super::Reranker;

/// Standard BERT/MiniLM special tokens, used when a model dir omits
/// `special_tokens_map.json` (the transformers.js cache does).
const DEFAULT_SPECIAL_TOKENS_MAP: &str = r#"{"unk_token":"[UNK]","sep_token":"[SEP]","pad_token":"[PAD]","cls_token":"[CLS]","mask_token":"[MASK]"}"#;

/// A local ONNX cross-encoder reranker loaded from a directory of model files
/// (e.g. `ms-marco-MiniLM-L-6-v2`). Uses fastembed's user-defined path so it can
/// reuse an already-downloaded model with no network.
pub struct LocalReranker {
    // fastembed `rerank` borrows the model mutably; the trait is `&self`.
    model: Mutex<TextRerank>,
}

impl LocalReranker {
    /// Load from a directory with `onnx/model.onnx`, `tokenizer.json`,
    /// `config.json`, `tokenizer_config.json`. `special_tokens_map.json` is read
    /// if present, else a BERT/MiniLM default is supplied.
    pub fn from_dir(dir: &Path) -> anyhow::Result<Self> {
        let read = |rel: &str| -> anyhow::Result<Vec<u8>> {
            std::fs::read(dir.join(rel)).map_err(|e| anyhow::anyhow!("read {rel}: {e}"))
        };
        let special = std::fs::read(dir.join("special_tokens_map.json"))
            .unwrap_or_else(|_| DEFAULT_SPECIAL_TOKENS_MAP.as_bytes().to_vec());

        let tokenizer_files = TokenizerFiles {
            tokenizer_file: read("tokenizer.json")?,
            config_file: read("config.json")?,
            special_tokens_map_file: special,
            tokenizer_config_file: read("tokenizer_config.json")?,
        };
        let model = UserDefinedRerankingModel::new(read("onnx/model.onnx")?, tokenizer_files);
        let model =
            TextRerank::try_new_from_user_defined(model, RerankInitOptionsUserDefined::default())?;
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

    /// The ms-marco-MiniLM-L-6-v2 cross-encoder the TS tests already downloaded.
    fn ms_marco_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../bash/node_modules/@huggingface/transformers/.cache/Xenova/ms-marco-MiniLM-L-6-v2")
    }

    /// Proves the real cross-encoder loaded and discriminates: the on-topic doc
    /// scores higher than the unrelated one for the query. Skips if absent.
    #[test]
    fn local_reranker_scores_relevant_above_irrelevant() {
        let dir = ms_marco_dir();
        if !dir.join("onnx/model.onnx").exists() {
            eprintln!("skipping reranker test: model not present at {dir:?}");
            return;
        }
        let r = LocalReranker::from_dir(&dir).unwrap();
        let docs = vec![
            "To reset your password, click 'forgot password' and follow the email link.".to_string(),
            "Bananas are a good source of potassium and dietary fiber.".to_string(),
        ];
        let scores = r.rerank("how do I reset my account password", &docs).unwrap();
        assert_eq!(scores.len(), 2);
        assert!(
            scores[0] > scores[1],
            "password doc ({}) must outscore the banana doc ({})",
            scores[0],
            scores[1]
        );
    }
}
