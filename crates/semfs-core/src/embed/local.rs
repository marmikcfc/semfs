//! fastembed-backed local embedder — real semantic quality, fully offline once
//! the model files are present.

use std::path::Path;
use std::sync::Mutex;

use fastembed::{
    InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};

use super::Embedder;

/// Standard BERT/MiniLM special tokens, used when a model dir ships the ONNX +
/// tokenizer but omits `special_tokens_map.json` (e.g. the transformers.js cache).
const DEFAULT_SPECIAL_TOKENS_MAP: &str = r#"{"unk_token":"[UNK]","sep_token":"[SEP]","pad_token":"[PAD]","cls_token":"[CLS]","mask_token":"[MASK]"}"#;

/// A local ONNX embedder loaded from a directory of model files. Uses fastembed's
/// "bring your own model" path so we can point at an already-downloaded model
/// (no network), which also satisfies the offline/air-gapped requirement.
pub struct LocalEmbedder {
    // fastembed's `embed` takes `&mut self`; the `Embedder` trait is `&self`, so
    // we guard the model behind a Mutex (embedding is sequential anyway).
    model: Mutex<TextEmbedding>,
    dims: usize,
}

impl LocalEmbedder {
    /// Load from a directory containing `onnx/model.onnx`, `tokenizer.json`,
    /// `config.json`, `tokenizer_config.json`. `special_tokens_map.json` is read
    /// if present, else a BERT/MiniLM default is supplied. Mean pooling (the
    /// sentence-transformers default for all-MiniLM / BGE).
    pub fn from_dir(dir: &Path, dims: usize) -> anyhow::Result<Self> {
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
        let model = UserDefinedEmbeddingModel::new(read("onnx/model.onnx")?, tokenizer_files)
            .with_pooling(Pooling::Mean);
        let model = TextEmbedding::try_new_from_user_defined(model, InitOptionsUserDefined::default())?;
        Ok(Self {
            model: Mutex::new(model),
            dims,
        })
    }
}

impl std::fmt::Debug for LocalEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalEmbedder")
            .field("dims", &self.dims)
            .finish_non_exhaustive()
    }
}

impl Embedder for LocalEmbedder {
    fn dimensions(&self) -> usize {
        self.dims
    }

    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let docs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let mut model = self
            .model
            .lock()
            .map_err(|_| anyhow::anyhow!("embedder mutex poisoned"))?;
        model.embed(docs, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::cosine;

    /// Path to the all-MiniLM-L6-v2 ONNX the TS tests already downloaded.
    fn minilm_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../bash/node_modules/@huggingface/transformers/.cache/Xenova/all-MiniLM-L6-v2")
    }

    /// Proves the REAL model loaded: semantically-close-but-lexically-disjoint
    /// text scores higher than unrelated text — something HashEmbedder can't do.
    /// Skips cleanly if the model files aren't present.
    #[test]
    fn local_embedder_captures_semantic_closeness() {
        let dir = minilm_dir();
        if !dir.join("onnx/model.onnx").exists() {
            eprintln!("skipping: model not present at {dir:?}");
            return;
        }
        let e = LocalEmbedder::from_dir(&dir, 384).unwrap();
        let v = |s: &str| e.embed(&[s.to_string()]).unwrap().pop().unwrap();
        let anchor = v("user authentication and login");
        let close = v("verifying a person's credentials to sign in");
        let far = v("a recipe for banana bread with walnuts");
        assert_eq!(anchor.len(), 384);
        assert!(
            cosine(&anchor, &close) > cosine(&anchor, &far),
            "real model must rank the synonym phrase closer than the unrelated one"
        );
    }
}
