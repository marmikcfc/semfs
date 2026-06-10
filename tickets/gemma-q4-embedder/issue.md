# Q4-quantized gemma embedder for seeding (bring-your-own-ONNX)

**Status:** PROPOSED → implementing · Opened 2026-06-08
**Goal:** seed the full chanpin workspace with **gemma EmbeddingGemma-300M Q4 ONNX** so a
complete local seed is fast + memory-light enough to finish reliably on the 16 GB box.

## Why
- gemma **fp32** (the only path semfs supports today) is memory-heavy (~5–7 GB per daemon)
  and CPU-slow (~2–3 h for the full 1,452-file `chanpin_standard`); under any daemon
  concurrency it OOMs/corrupts (see `rcas/2026-06-08-partial-seed-indexing.md`).
- **Q4** (~4-bit weights, `model_q4.onnx` ≈ a few hundred MB vs fp32 ~1.2 GB) is much
  smaller/faster → a complete gemma seed becomes viable.
- **fastembed has NO Q4 mode** (`QuantizationMode` = None/Static/Dynamic only) and its
  registry `gemma` is pinned to **fp32** `onnx/model.onnx`. So Q4 requires fastembed's
  **`UserDefinedEmbeddingModel`** (bring-your-own-ONNX) path — which `embed/local.rs`
  explicitly DEFERRED ("Bring-your-own-ONNX is intentionally deferred").

## Source (confirmed present in HF repo `onnx-community/embeddinggemma-300m-ONNX`)
`onnx/model_q4.onnx` (+ `model_q4.onnx_data`), also `model_q4f16.onnx`, `model_no_gather_q4.onnx`.
Tokenizer: `tokenizer.json`, `config.json`, `special_tokens_map.json`, `tokenizer_config.json`.

## Design
1. **Download** the q4 artifacts to a cache dir (extend `/tmp/dl_quant.sh`):
   `model_q4.onnx`, `model_q4.onnx_data`, + the 4 tokenizer files.
2. **`embed/local.rs` — `LocalEmbedder::from_onnx_dir(dir, dims)`** using fastembed BYO:
   ```rust
   let mut m = UserDefinedEmbeddingModel::new(read(model_q4.onnx), TokenizerFiles{..4 files..})
       .with_external_initializer("model_q4.onnx_data".into(), read(..._data))
       .with_quantization(QuantizationMode::None);   // q4 weights are static; validate via gate
   m.output_key = Some(OutputKey::ByName("sentence_embedding")); // gemma pooled output
   // pooling = None (model emits the sentence embedding directly)
   let te = TextEmbedding::try_new_from_user_defined(m, InitOptionsUserDefined::new().with_max_length(EMBED_MAX_LENGTH))?;
   ```
   `dims = 768`. **identity = `byo:gemma-q4-onnx:768`** (distinct from `fastembed:…gemma…` so a
   q4 seed never gets read as an fp32 seed — same cache-busting discipline as `FASTEMBED_REV`).
3. **`resolve.rs`** — route `SEMFS_EMBED_MODEL=gemma-q4` (or `SEMFS_EMBED_ONNX_DIR=<path>`) to
   `from_onnx_dir`. Keep registry path for everything else.
4. **Sanity gate (MANDATORY before seeding 1,452 files):** embed a known triplet and assert
   cosine(`"reset my password"`, `"forgot password email link"`) ≫ cosine(vs `"bananas potassium"`).
   Wrong pooling/quant/output_key → garbage embeddings; the gate catches it.
5. **Re-seed** the canonical workspace `chanpin_standard` (1,452) with the q4 embedder via the
   wait-for-completion procedure (`benchmarks/workspace_bench/seed_complete.sh`,
   `EMBED=gemma-q4`), ONE daemon, no mid-run kills. Then rebuild KG over the complete db.

## Tasks
- [ ] T1: download q4 onnx + tokenizer to a stable cache dir on the box.
- [ ] T2: `from_onnx_dir` BYO loader in `local.rs` (+ distinct identity). TDD/sanity test.
- [ ] T3: `resolve.rs` route `gemma-q4`/`SEMFS_EMBED_ONNX_DIR`.
- [ ] T4: sanity gate (cosine triplet) — gate seeding on it.
- [ ] T5: build + deploy; clean up all gemma seeds (delete chanpin-gemma*.db).
- [ ] T6: full re-seed `chanpin_standard` with gemma-q4 (wait-for-completion) → verify coverage.
- [ ] T7: rebuild KG over the complete q4 db + materialize projection + verify.

## Risks / open questions
- **Quantization quality:** q4 may degrade retrieval vs fp32. Embedder is NOT the token lever
  (Gemma≈e5), so acceptable for a usable seed; note any rank regression.
- **QuantizationMode** for a statically-quantized q4 model: try `None` first; if embeddings are
  off, try `Static`. Sanity gate decides.
- **Which q4 file:** `model_q4.onnx` (pure q4) vs `model_q4f16.onnx` (q4 weights, fp16 activations
  — often better quality). Start with `model_q4.onnx`; fall back to q4f16 if the gate is weak.
- **Identity/cache mixing:** the q4 identity must differ from fp32 so `is_searchable` rejects a
  mismatched reader (different vectors). Encoded in `identity`.
- **External-initializer filename** must EXACTLY match the name referenced inside `model_q4.onnx`
  (`model_q4.onnx_data`).

## Refs
- fastembed 5.13.4 BYO API: `UserDefinedEmbeddingModel`, `InitOptionsUserDefined`,
  `TextEmbedding::try_new_from_user_defined`, `OutputKey::ByName`, `QuantizationMode`.
- `crates/semfs/src/cmd/resolve.rs` (registry mapping), `crates/semfs-core/src/embed/local.rs`
  (the deferred BYO note), `benchmarks/workspace_bench/seed_complete.sh`,
  `rcas/2026-06-08-partial-seed-indexing.md`, `benchmarks/workspace_bench/seed-coverage.md`.
