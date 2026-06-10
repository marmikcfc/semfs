//! fastembed-backed local embedder using the **registry** API.
//!
//! Models come from fastembed-rs's built-in `EmbeddingModel` registry (auto-
//! downloaded + cached on first use); fastembed applies each model's correct
//! pooling/normalization internally, so there is no hand-rolled pooling here.
//! (Bring-your-own-ONNX is intentionally deferred — see the project goal.)

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Context;
use fastembed::{
    EmbeddingModel, InitOptions, InitOptionsUserDefined, OutputKey, QuantizationMode,
    TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};

use super::Embedder;

/// fastembed registry revision folded into the embedder identity so a fastembed
/// upgrade that re-bundles a model's ONNX/tokenizer under the SAME `model_code`
/// invalidates existing caches and forces a reindex (the registry exposes no
/// per-model checksum). EXACT version incl. patch — a 5.13.x→5.13.y rebundle must
/// also invalidate. Enforced by `fastembed_rev_tracks_dependency` against the
/// lockfile. (Full artifact-hash fingerprinting is deferred with the BYO-ONNX work.)
const FASTEMBED_REV: &str = "fe5.13.4";

/// Cap the embed batch passed to ONNX. fastembed defaults to 256; a 256-wide
/// batch through the 768-d code model retains a multi-GB ONNX CPU arena (ort
/// grows the arena to the largest tensor it ever sees and never shrinks),
/// OOM-killing a full-container warm. Bounding the batch caps that high-water
/// mark. See `tickets/solve-oom-issue/` (OOM #2).
const EMBED_BATCH_SIZE: usize = 16;
/// Cap the per-sequence token length. Chunks are word-windows (`max_words=200`),
/// but `split_whitespace` on CJK text (no spaces) can make one chunk thousands
/// of tokens — and attention memory is QUADRATIC in sequence length, so a long
/// sequence dominates the arena regardless of batch. Bounding it (respecting the
/// model's own max via fastembed's `.min(model_max)`) keeps the forward pass
/// bounded. 2048 covers ~200-word English/code chunks fully; only pathological
/// long CJK chunks truncate — an acceptable, bounded recall trade vs. OOM.
const EMBED_MAX_LENGTH: usize = 1024;

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
        let identity = format!("fastembed:{FASTEMBED_REV}:{}:{}", info.model_code, dims);

        let mut opts = InitOptions::new(model)
            .with_show_download_progress(false)
            // Bound sequence length so a long (CJK) chunk can't blow up the
            // quadratic attention arena. fastembed clamps to the model's own max.
            .with_max_length(EMBED_MAX_LENGTH);
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

    /// Bring-your-own-ONNX embedder for a model the fastembed registry doesn't
    /// expose — notably a **Q4-quantized** EmbeddingGemma (registry `gemma` is
    /// fp32 only; fastembed has no Q4 mode). Loads `<base>.onnx` + its external
    /// weights `<base>.onnx_data` + the gemma tokenizer files from `dir`.
    ///
    /// `dims` is the model's vector width (768 for EmbeddingGemma-300M).
    /// `identity_tag` is folded into a `byo:<tag>:<dims>` identity that is
    /// DISTINCT from the registry `fastembed:…` identity, so a reader never
    /// mistakes a q4 seed for an fp32 one (their vectors differ → not searchable
    /// against each other). EmbeddingGemma emits a pooled `sentence_embedding`
    /// output, so we select it by name and apply NO extra pooling.
    pub fn from_onnx_dir(
        dir: &Path,
        dims: usize,
        model_base: &str,
        identity_tag: &str,
    ) -> anyhow::Result<Self> {
        let rd = |name: &str| -> anyhow::Result<Vec<u8>> {
            std::fs::read(dir.join(name))
                .with_context(|| format!("read {}", dir.join(name).display()))
        };
        let onnx = rd(&format!("{model_base}.onnx"))?;
        // The external-data filename MUST match the one referenced inside the
        // ONNX (`<base>.onnx_data` for the onnx-community gemma exports).
        let data_name = format!("{model_base}.onnx_data");
        let data = rd(&data_name)?;
        let tokenizer_files = TokenizerFiles {
            tokenizer_file: rd("tokenizer.json")?,
            config_file: rd("config.json")?,
            special_tokens_map_file: rd("special_tokens_map.json")?,
            tokenizer_config_file: rd("tokenizer_config.json")?,
        };
        let mut model = UserDefinedEmbeddingModel::new(onnx, tokenizer_files)
            .with_external_initializer(data_name, data)
            // q4 weights are statically quantized; `None` (no runtime dynamic
            // quant) is the right mode. Verified by the cosine sanity gate.
            .with_quantization(QuantizationMode::None);
        model.output_key = Some(OutputKey::ByName("sentence_embedding"));

        let te = TextEmbedding::try_new_from_user_defined(
            model,
            InitOptionsUserDefined::new().with_max_length(EMBED_MAX_LENGTH),
        )?;
        Ok(Self {
            model: Mutex::new(te),
            dims,
            identity: format!("byo:{identity_tag}:{dims}"),
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
        // Sub-batch HERE rather than relying on fastembed's `batch_size` arg.
        // fastembed runs ALL inputs in ONE ONNX pass when `batch_size` is `None`
        // (for dynamically-quantized models it forces `batch_size = texts.len()`
        // and outright REJECTS `Some(n < len)`), so a large file's chunk list
        // becomes a single giant batch → multi-GB ONNX arena → OOM on a full warm
        // (a 273-chunk transcription spiked ~7 GB). Splitting into fixed windows
        // and passing `None` per window (the only form dynamic-quant accepts)
        // bounds every ONNX pass to EMBED_BATCH_SIZE sequences. See
        // `tickets/solve-oom-issue/` (OOM #2).
        let mut out = Vec::with_capacity(texts.len());
        for window in texts.chunks(EMBED_BATCH_SIZE) {
            out.extend(model.embed(window, None)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    /// Enforce that FASTEMBED_REV tracks the EXACT pinned `fastembed` version (it's
    /// the cache-busting key folded into the embedder identity). Fails on any
    /// fastembed version bump until someone updates the const — which is exactly
    /// when re-bundled model artifacts must invalidate existing caches.
    #[test]
    fn fastembed_rev_tracks_dependency() {
        let lock = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock"))
            .expect("read workspace Cargo.lock");
        let ver = lock
            .split("name = \"fastembed\"")
            .nth(1)
            .and_then(|s| s.split("version = \"").nth(1))
            .and_then(|s| s.split('"').next())
            .expect("fastembed version in Cargo.lock");
        // EXACT match (incl. patch) — a patch-level rebundle must invalidate caches.
        assert_eq!(
            super::FASTEMBED_REV,
            format!("fe{ver}"),
            "fastembed is {ver}; bump FASTEMBED_REV to fe{ver} (forces cache reindex)"
        );
    }

    // Real-model behaviour (arctic-s embed → index → semantic search) is validated
    // live by the holistic e2e harness `crates/e2e/phase_local_l1_l5.sh`, which
    // downloads the registry models and exercises them through a real mount — so
    // there's no network/download test in the default `cargo test` here.
}
