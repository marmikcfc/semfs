# next_plaid — source-grounded reference specs (theirs + ours)

_Companion to `README.md`. These are the full, file:line-grounded maps produced by reading the actual code — the substrate the work breakdown and parity checklist stand on. Generated 2026-06-29 by deep-reading `lightonai/next-plaid` (cloned to scratchpad) and this repo._

---

# §THEIRS — NextPlaid / FastPlaid engine (100%-parity reference)

> Clone root (citations relative to it / match the GitHub tree `lightonai/next-plaid`). Workspace v1.6.0, edition 2021, Apache-2.0 (`Cargo.toml:5-12`). Members: `next-plaid`, `next-plaid-api`, `next-plaid-onnx`, `colgrep`. "FastPlaid" = the algorithm/heritage; "NextPlaid" = the `next-plaid` crate (the multi-vector DB). CPU-first PLAID; CUDA only for k-means/quantization (no custom CUDA index).

## 1. Crate / module map

| Crate | Purpose |
|---|---|
| `next-plaid/` | Core CPU PLAID engine: index build, mmap store, search, codec, k-means, MaxSim, update/delete, SQLite filter, FTS5 (`next-plaid/src/lib.rs:1-4`) |
| `next-plaid-onnx/` | ColBERT ONNX encoder (`Colbert`) → `[num_tokens,128]` f32 multi-vectors |
| `next-plaid-api/` | Axum HTTP server wrapping engine + encoder |
| `next-plaid-api/python-sdk/` | `next-plaid-client` Python SDK (httpx) |
| `next-plaid-onnx/python/` | `colbert_export` — HF→ONNX export + int8 quantize |

`next-plaid` modules (`src/lib.rs:15-29`): `index`(2054), `codec`(753), `kmeans`(556), `maxsim`(508), `search`(743), `mmap`(1889), `update`(1244), `delete`(560), `filtering`(3832), `text_search`(1846), `embeddings`(137), `cuda`(609), `utils`(339), `error`(66).

## 2. Index build (FastPlaid)

Entry: `create_index_with_kmeans_files` (`src/index.rs:927`) → `compute_kmeans` → `create_index_files` (`:551`); `MmapIndex::create_with_kmeans` (`:1392`).

**2.1 K-means** (`src/kmeans.rs`): lib `fastkmeans-rs 1.0.7` (`:17`). `K = 2^floor(log2(16·√(avg_tokens_per_sampled_doc · num_documents)))` (`:304-309`), clamped `K.min(total_sample_tokens)` (`:312`). Training sample `n_samples = min(1+16·√(120·N), N)` (`:273-276`), `ChaCha8Rng::seed_from_u64(seed)` shuffle (`:278-282`). `max_iters=4` (`:48,321`), `tol=1e-8` (`:64,323`), `max_points_per_centroid=256` (`:49,324`), chunk sizes 51200/10240 (`:67,325`). Centroids L2-normalized (`row/=max(‖row‖,1e-12)`, `:414-419`). **Clusters tokens, not docs** (`:290-301`). Backends CPU/CUDA/Metal w/ CPU fallback (`:331-412`).

**2.2 Product quantization — `ResidualCodec`** (`src/codec.rs:107-123`): fields `nbits, centroids, avg_residual, bucket_cutoffs, bucket_weights, byte_reversed_bits_map, bucket_weight_indices_lookup`.
- nbits must divide 8 → {1,2,4,8}; **default 4** (`:161-166`; `index.rs:91`).
- `codes = argmax(emb @ centroidsᵀ)` (`:260-343`); `residual = emb − centroid[code]` (`index.rs:17-40`); `packed_dim = dim·nbits/8` (`:366`).
- quantize residual: `bucket = count(c in bucket_cutoffs : val>c)`, pack nbits **MSB-first** (`:384-396`).
- **codebook:** `bucket_cutoffs` = quantiles at `i/n_options`, i∈[1,n_options) → n_options−1 cutoffs; `bucket_weights` = quantiles at `(i+0.5)/n_options`, i∈[0,n_options) → n_options recon values (`index.rs:260-270`); `n_options=1<<nbits`.
- `avg_residual` = mean|residual| per dim (`:255-258`); `cluster_threshold` = quantile(token residual L2 norms, 0.75) (`:253`).
- held-out for codec: `min(0.05·total_emb, 50000)` tokens (`:195-212`).
- **decompress:** `out = centroid[code] + bucket_weights[bucket]`, then **L2-norm each row** (`codec.rs:443-467`). Quantiles numpy-linear-interp (`utils.rs:94-149`).
- artifacts: `centroids.npy, bucket_cutoffs.npy, bucket_weights.npy, avg_residual.npy, cluster_threshold.npy`; nbits in `metadata.json`.

**2.3 IVF** (`src/index.rs`): `code_to_docs: BTreeMap<centroid_id, Vec<doc_id>>` (`:479-487`); posting lists = **deduped, sorted DOC-ids** per centroid (one per doc, NOT per embedding) (`:491-499`). `ivf.npy` flat i64 (`:501-504`); `ivf_lengths.npy` i32[K] (`:505-508`); offsets = prefix-sum at load (`:1089-1094`). `get_candidates` = union of postings, sort+dedup (`:1142-1156`).

**2.4 On-disk + mmap:** `metadata.json` (`Metadata` `:106-127`), `plan.json`, `centroids.npy` (**mmap**), bucket/avg/threshold npy (RAM), `ivf*.npy` (RAM), per-chunk `{c}.codes/residuals.npy` merged → `merged_codes.npy`(i64, **mmap**)/`merged_residuals.npy`(u8, **mmap**), `metadata.db` (SQLite). NPY parse `mmap.rs:659-717`; zero-copy views `:917-955`. Merge-on-load w/ `padding_rows = max_doclen−last_doclen` (`index.rs:1112-1118`), manifest-cached, exclusive `flock` (`mmap.rs:25-57`). `memmap2 0.9`. `MmapIndex` fields `index.rs:995-1016`.

## 3. Search (PLAID multi-stage)

Entry `MmapIndex::search` → `search_one_mmap` (`index.rs:1258`; `search.rs:327`). Query = `[n_query_tokens, 128]`, MASK-expanded to `query_length=48` (`next-plaid-onnx/src/lib.rs:628-639`).

| Stage | What | Param | Cite |
|---|---|---|---|
| 0 | `Q_cs = query·centroidsᵀ` `[n_qtok,K]` | — | `search.rs:345` |
| 1 IVF probe | top `n_ivf_probe` centroids/qtok, union | **8** | `:388-414` |
| 2 prune | keep centroid iff `max_q Q_cs ≥ threshold` | **0.4** | `:416-425` |
| 3 candidates | union of IVF postings | — | `:431` |
| 4 approx | `Σ_q max_{code∈doc} Q_cs[q,code]` (no decompress) | — | `:305-324,447` |
| 5 cutoff | keep top `n_full_scores` | **4096** | `:459-465` |
| 6 decompress N | `max(n_full_scores/4, top_k)` (=1024) | — | `:468` |
| 7 decompress | `codec.decompress`, chunk `DECOMPRESS_CHUNK_SIZE=128` | — | `:481-493`; `index.rs:1159` |
| 8 exact | `maxsim_score(query, doc_emb)` | — | `:88,488` |
| 9 final | sort desc, return `top_k` | **10** | `:496-515` |

Batched path `>centroid_batch_size=100000` (`:521-640`). Subset/metadata pre-filter intersects candidates + scales `n_ivf_probe` (`:350-437`).

**MaxSim** (`maxsim.rs:270-294`): `score(Q,D)=Σ_i max_j (Q[i]·D[j])` — `scores=query·Dᵀ`, per-row simd_max, sum; scalar for `q·d<256` (`:298-315`). AVX2 (`:79-149`)/NEON (`:152-213`)/scalar. BLAS GEMM when feature on. **Search always CPU** (`search.rs:84-87`).

## 4. NextPlaid additions

**4.1 Incremental add** (`index.rs:1431-1591`): rebuild (`N≤start_from_scratch=999`), buffer (`new<buffer_size=100`, assign to existing centroids), centroid-expansion (`≥buffer_size`, k-means on outliers, append centroids). Outlier = `min L2² to centroid > cluster_threshold²` (`update.rs:422-608`). IVF read-modify-rewrite in RAM, sort+dedup (`:1000-1080`). New doc-ids sequential from `old_num_documents`.

**4.2 Delete** (`delete.rs:43`): **HARD compaction, no tombstone** — rewrite affected chunks dropping rows (`:120-181`), patch IVF + **densely renumber surviving doc-ids** `new=old−(#deleted<old)` (`:187-237`); SQLite `_subset_` re-sequenced identically (`filtering.rs:1704-1759`). ⚠️ delete uses non-atomic `File::create` (torn reads possible).

**4.3 Concurrency:** `atomic_write_file` (temp+rename+fsync) for add/build (`utils.rs:16-60`); exclusive flock for merge; SQLite cached read-only conns + WAL writer (`filtering.rs:657-731`); API per-index `ArcSwap<MmapIndex>` (lock-free reads, atomic swap) + per-index `tokio::Mutex` for update/delete (`api/src/state.rs:24-46`).

**4.4 SQLite metadata + WHERE** (`filtering.rs`): `rusqlite 0.38` + `fancy-regex` REGEXP UDF. Schema v2 thin/fat split (`METADATA` indexed cols incl `file,name,qualified_name,line,language,unit_type,complexity,...` + `_subset_`/`_content_id_`; `METADATA_CONTENT` large TEXT) (`:69-91`). `where_condition` validates clause → `SELECT _subset_ FROM METADATA WHERE {cond}` w/ `?` params → `Vec<i64>` passed as search subset (`:1880-1923`). Operators `= != <> < <= > >= LIKE REGEXP BETWEEN IN IS NULL AND OR NOT ()`; values only via `?`; injection guard rejects `; -- /* */` + DDL/DML (`:146-181,363-561`).

**4.5 ONNX encoder** (`next-plaid-onnx`): `ort =2.0.0-rc.11` (`load-dynamic`, CPU default) (`Cargo.toml:35`). Session `GraphOptimizationLevel::Level3`; EP order CUDA→TensorRT→CoreML→DirectML→MIGraphX→CPU (`lib.rs:323-397`). Load `model.onnx`/`model_int8.onnx` + `tokenizer.json` + `onnx_config.json` (`:985-991`). Prefix token inserted at **position 1** (after CLS, PyLate parity). **Projection + per-token L2 inside the ONNX graph** (`python/export.py:100-108`); Rust does row-select/skiplist (`:2154-2246`). Query → MASK-expand to `query_length`, attn=1, `[48,128]`; doc → skiplist punctuation removal, `[valid,128]` (`:2064-2078,2214-2242`). Batch CPU 32/GPU 64. `with_quantized(true)` → `model_int8.onnx` (ONNX **weight** quant via `quantize_dynamic QInt8`) — **distinct from PQ**; output embeddings stay f32, no int8 vector storage. `hierarchy.rs` = scipy-compatible Ward agglomerative for optional `pool_factor` doc-token pooling (CLS protected).

## 5. Params / defaults

`IndexConfig` (`index.rs:43-102`): `nbits=4, batch_size=50000, seed=Some(42), kmeans_niters=4, max_points_per_centroid=256, n_samples_kmeans=None, start_from_scratch=999, force_cpu=false, fts_tokenizer=Unicode61`.
`SearchParameters` (`search.rs:58-68`): `batch_size=2000, n_full_scores=4096, top_k=10, n_ivf_probe=8, centroid_batch_size=100000, centroid_score_threshold=Some(0.4)`.
`UpdateConfig` (`update.rs:95-108`): `buffer_size=100, start_from_scratch=999` (+ k-means defaults).
`ColbertConfig` (`next-plaid-onnx/src/lib.rs:616-666`): `query_prefix="[Q] ", document_prefix="[D] ", query_length=48, document_length=300, do_query_expansion=true, embedding_dim=128, uses_token_type_ids=true, mask_token_id=103, pad_token_id=0, do_lower_case=false`.
Env: `NEXT_PLAID_FORCE_GPU/CPU`, `NEXT_PLAID_MAX_NEAREST_CENTROID_MEMORY_MB` (1GB), `INDEX_DEFAULT_START_FROM_SCRATCH` (999), `OPENBLAS_NUM_THREADS=1`; API `CONCURRENCY_LIMIT=100, MAX_BATCH_TEXTS=64, MAX_BATCH_DOCUMENTS=300`.

## 6. Public API

**Rust:** `MmapIndex::{load, create_with_kmeans, update_or_create, search(query,&params,subset), search_batch, update, update_with_metadata, delete, reconstruct, get_candidates, get_document_embeddings}` (`index.rs` lines per §6.1 of source notes). `QueryResult{query_id, passage_ids:Vec<i64>, scores:Vec<f32>}` (`search.rs:73-80`). `next-plaid-onnx::Colbert::{new, encode_queries, encode_documents(_raw), with_quantized/parallel}`.

**HTTP (axum):** `POST /indices`, `…/documents`, `…/update[_with_encoding]`, `PUT …/config`, `DELETE …`, `POST …/search[/filtered][_with_encoding]`, `/encode`, `/rerank[_with_encoding]`, `…/metadata/*`. `SearchRequest` (`models.rs:226-262`): `queries, params{top_k,n_ivf_probe,n_full_scores,centroid_score_threshold}, subset, text_query, alpha(0.75), fusion("relative_score"|"rrf"), filter_condition, filter_parameters`. `/rerank` = pure MaxSim over caller docs, no index (`handlers/rerank.rs:57-94`).

**Python `next-plaid-client`:** `NextPlaidClient(base_url, timeout, headers)`; `create_index/add/search/keyword_search/delete/encode/rerank/...`. `IndexConfig(nbits=4, batch_size=50000, start_from_scratch=999, ...)`, `SearchParams(top_k=10, n_ivf_probe=8, n_full_scores=4096, centroid_score_threshold=0.4)`. Embeddings as base64 LE-f32 + `[rows,cols]`.

## 7. Build / deploy

Workspace `[profile.release] lto=true, codegen-units=1, opt-level=3`; cargo-dist (darwin aarch64/x86, linux x86-gnu, windows msvc). `next-plaid` features: `default=[]` (pure ndarray), `accelerate`/`openblas`/`mkl` (BLAS), `metal_gpu`, `cuda`/`cuda-13` (mutually exclusive). Deps: `ndarray 0.16, rayon 1.10, memmap2 0.9, ndarray-npy 0.9.1, fastkmeans-rs 1.0.7, rusqlite 0.38(bundled,functions), fancy-regex 0.13, fs2 0.4`. **`ort` only in `next-plaid-onnx`.** `build.rs` links CUDA stubs only under `feature=cuda` (kernels JIT via cudarc NVRTC). Docker: cargo-chef multi-stage; `runtime-cpu` (debian-slim, mkl/openblas) + `runtime-cuda` (`nvidia/cuda:12.4.1`); **ONNX Runtime v1.23.0** via `ORT_DYLIB_PATH`; `:8080`, healthcheck `/health`; entrypoint downloads HF model files.

**Parity caveats:** (1) delete renumbers doc-ids densely (no stable ids); (2) IVF stores doc-ids not embedding-ids; (3) PQ-nbits ≠ int8-ONNX-weights; (4) projection+L2 baked in ONNX graph; (5) `do_query_expansion` Rust default true but persisted `onnx_config.json` may be false — verify per model; (6) delete path non-atomic.

---

# §OURS — semfs retrieval/indexing architecture

> Repo `/Users/marmikpandya/semantic-filesystem`. Ignore `.evo/run_*/worktrees/` (stale). One-paragraph model: a persona seed (`<persona>-gemma-q4.db`) carries 3 layers — search index (`chunks`+`vchunks` vec0 + `ffts` BM25), FUSE tree (`fs_*`), KG (`edges`/`graph_*`). A FUSE daemon mounts it; the agent runs `semfs grep` → daemon IPC → `SqliteVecStore::search_blocking`: **5 lanes → RRF → KG additive prior (PPR) → cross-encoder rerank → comention/salience nudges → top-10**. **Every vector = one mean-pooled 768-d vector per ~200-word chunk. No per-token / multi-vector / ColBERT anything exists today.**

## 1. Crate layout

| Path | What | Key |
|---|---|---|
| `crates/semfs-core/` | the library — VFS, store, embed, extract, search, KG, daemon, sync | below |
| `crates/semfs/` | the CLI (`mount/grep/...`) | `cmd/grep.rs` (search entry), `cmd/resolve.rs` (embedder/reranker/backend selection), `cmd/daemon_runtime.rs`, `cmd/mount.rs` |
| `crates/e2e/`, `crates/spikes/` | shell E2E tests / throwaway POC | — |

`semfs-core/src/` (`lib.rs:22-35`): `backend/` (search engine + stores + KG), `embed/`, `extract/`, `cache/` (store schema + `Db`), `rerank/`, `daemon/`, `mount/`, `sync/`, `vfs/`, `api/`, `llm.rs`. Search = `backend/sqlite_vec.rs` (`SqliteVecStore`) + `backend/rank.rs` + `backend/hidden_kg.rs`.

## 2. Backend-agnostic store

Seam: `trait SemanticIndex { async fn search(query, filepath) -> Vec<SearchHit> }` (`backend/mod.rs:39`, **read-path only**; writes on concrete structs). `SearchHit{filepath, memory, chunk, similarity:f64, seen_at_turn}` (`:22-35`).

| Backend | Module | ctor | Vector storage |
|---|---|---|---|
| sqlite (default) | `sqlite_vec.rs:198` | `SqliteVecStore::new` | sqlite-vec `vec0` + fts5 |
| pgvector | `pgvector.rs:113` | `connect` | `vector(N)` column |
| pglite | `pgvector.rs:221` | `embedded` | same |
| cloud | `cloud.rs:34` | `CloudIndex` | none (Supermemory proxy) |

**Schema** (`cache/schema.sql` + runtime vec0 in `cache/db.rs:181-237`): `fs_inode/fs_dentry/fs_data`(raw bytes 4KB blocks)/`fs_symlink/fs_config`; **`chunks`**(`schema.sql:108` — `id PK, ino, filepath, ord, text, last_accessed_at, access_count`, one row per ~200-word chunk, the join hub); `edges/graph_entity/graph_relation/graph_community/graph_god_node` (KG); **`ffts`** (`USING fts5(text)`, rowid==chunks.id, BM25); `fs_unindexed`; **`vchunks`** (`db.rs:215` runtime, `USING vec0(embedding float[768])`, rowid==chunks.id, TEXT vectors); **`vchunks_code`** (`db.rs:224`, optional CODE lane).

**Multi-vector blockers (why it can't live in `vchunks` as-is):** (1) 1:1 rowid join `chunks.id==vchunks.rowid==ffts.rowid` (`sqlite_vec.rs:1001,625`); (2) invariant `chunk_n==text_n+code_n` **fails closed** (`:244`, probe `:509-511`); (3) vec0 `float[N]` = one fixed slot/row; (4) pgvector one-column-per-row. → need a **new child table** `(chunk_id, tok_ord) → embedding float[D]` + relax invariant + add MaxSim.

## 3. Embedding

`Embedder::embed(&[String]) -> Vec<Vec<f32>>` + `dimensions()` + `identity()` (`embed/mod.rs:28-43`), sync. Code-default TEXT=`MultilingualE5Small` 384-d, CODE=`JinaEmbeddingsV2BaseCode` 768-d (`resolve.rs:24-25`). **Benchmark forces `SEMFS_EMBED_MODEL=gemma-q4`** (`run_matrix.py:118`) → BYO Q4 EmbeddingGemma-300M 768-d, `LocalEmbedder::from_onnx_dir` selecting pooled `sentence_embedding` (`local.rs:123`). Remote = `OpenAiEmbedder` 1536-d (opt-in). **No NVFP4/vLLM embedder** (NVFP4 = KG extraction/serving only). **Single mean-pooled vector — no per-token output anywhere.** Embed sites: index `sqlite_vec.rs:601`, query `:936-952` (same embedder). **No query/passage prefix** applied (e5 asymmetric prefixes unused — latent lever).

## 4. Search / ranking

Path: `grep.rs:run(:850)` → `daemon_search` IPC (`:151,1015`) → daemon `ipc.rs:dispatch(:220, 120s timeout)` → `SqliteVecStore::search(:900)` → `search_blocking(:924-1676)`.

| Stage | line | what |
|---|---|---|
| query embed | `936-952` | text(+code) → qblob |
| L1 text KNN | `991-1019` | `vchunks MATCH k=80`, `rrf_bump(Text)` |
| L2 code KNN | `1027-1052` | `vchunks_code`, `Code` |
| L3 BM25 | `1055-1086` | `ffts MATCH`, `Fts` |
| L4 path-token | `1098-1176` | filename ≥2 tok, `Path` (`SEMFS_PATH_LANE=off`) |
| L5 integrity | `1186-1242` | force error-page sources |
| KG retrieval | `1244-1288` | `hidden_kg::query_kg_candidates` (`SEMFS_HIDDEN_KG_RETRIEVAL`) |
| KG prior | `1290-1319` | `query_kg_priors` → `rank::apply_file_priors` (`SEMFS_HIDDEN_KG`) |
| collapse | `1393` | `rank::to_hits` |
| **L5 rerank** | **`1423-1438`** | truncate 50, `rank::apply_reranker` (cross-encoder) |
| revalidate | `1461-1495` | drop ghosts |
| L7/L6 | `1502-1524` | comention ×1.05 / salience nudges |

**Score** (`rank.rs:80-87`): `FileAcc::score()=Σ_lanes 1/(60+best_rank) + prior.clamp(0,0.15)` (RRF_K=60). KG prior = bounded additive ≤0.15 (`apply_file_priors:109-115`), no alpha/beta blend. Cross-encoder **overwrites `similarity`** (`apply_reranker:197-198`) → prior only affects pre-rerank top-50 survival.

**Hidden-KG** (`backend/hidden_kg.rs`): `SEMFS_HIDDEN_KG→enabled(:67)`, `SEMFS_HIDDEN_KG_RETRIEVAL→retrieval_enabled(:71)`, `SEMFS_KG_PPR→ppr_enabled(:76)`. PPR `ppr_file_scores(:713-788)`: seeds=matched entity paths, restart `SEMFS_PPR_RESTART=0.5(:743)`, iters `SEMFS_PPR_ITERS=30(:744)`, `r=restart·seed+(1-restart)·Â·r(:751-768)`, bipartite file↔entity from `edges`, bails to 1-hop >400K edges, cap `PPR_CAP=0.12(:151)`. 1-hop (PPR off): `direct(.08)+neighbor(.04)(:155-168)` + community boost (.05).

**Rerank (L5)** (`rerank/mod.rs`): `trait Reranker::rerank(query, docs:&[String]) -> Vec<f32>(:19-23)`. Default = Jina-reranker-v2-base-multilingual int8 ONNX local cross-encoder (`resolve.rs:42-48`, `LocalReranker rerank/local.rs:51`); on by default; `choose_rerank(resolve.rs:129-135)` → `Local` unless `SEMFS_RERANK_BACKEND∈{none,cohere,relace}`; attached `with_reranker(sqlite_vec.rs:540)`.

**★ MaxSim insertion point** — `sqlite_vec.rs:1423-1438` (hits + query in scope, truncated to 50; `rep_chunk: HashMap<filepath,chunk_id>` available). Canonical seam `rank::apply_reranker(rank.rs:179-202)`: builds `docs` from `.chunk`, calls rerank, **replaces `similarity`**, re-sorts. Two wirings: **(1) drop-in `Reranker`** (smallest; but trait passes only `docs:&[String]` text → must token-embed query+docs at query time, no stored token vectors); **(2) new stage `rank::apply_maxsim`** (mirror `apply_comention_boost`/`apply_salience` at `rank.rs:206-247`) invoked at `:1435`, keyed by `rep_chunk` into a **new per-token vector table** (true precomputed ColBERT).

## 5. Extraction layer (our front-end substitute)

`extract::extract_text(filepath, bytes)` (`extract/mod.rs:82`), routes by **magic-byte sniff** (`:408`). docx `ooxml.rs:16`; pptx `ooxml.rs:24`; xlsx/xls `spreadsheet.rs:22`→`summary.rs:49` (`calamine` + per-sheet gpt-4.1-mini summary); pdf `pdf.rs:31`→pdftotext→OCR `ocr.rs:190`; jpg `ocr.rs:14`; doc/unreadable → `soffice_to_text(mod.rs:344)` (headless LibreOffice). Caps `MAX_EXTRACT_BYTES=1MiB(:25)`. **Extracted text → `chunks.text` at index (`sqlite_vec.rs:620-624`), recoverable via `Db::get_extracted_text(db.rs:855)` — the always-present plain-text source per file.** Format-trap fix: grep-inline (`SEMFS_GREP_INLINE=on` default) + optional `.extracted.md` sidecars (`SEMFS_EXTRACT_SIBLING` default OFF; written `db.rs:883`/`file.rs:339`). xlsx dual-store: chunk=summary, sidecar=raw table (`extract/mod.rs:223`).

## 6. Seed format

`<persona>-gemma-q4.db` = 3 layers (search index `chunks/vchunks/vchunks_code/ffts` · FUSE `fs_inode/fs_dentry/fs_data` · KG `edges/graph_*`). Build on Modal (Mac can't cross-compile fastembed/ONNX): `build_corpus_seed` in `benchmarks/modal/semfs_modal.py:947`, phase-gated — embed (`seed_dir`, `:1032`), kg (`build_kg` gemma-4-31B vLLM+AST+kNN, `:1047`), finalize (`materialize_kg` Leiden, `:1062`; `materialize_fs` POSIX tree, `:1084`). **fs_data trap:** no `materialize_fs` → mounts empty. Gate `semfs seed-verify`. In-repo: only `benchmarks/e2b/assets/chanpin-gemma-q4.db` (690MB); 6 personas on Modal vol `semfs-bench-data:/data/seeds/`, baked to E2B templates at `/opt/<name>.db` (`bake_e2b_persona.py:109`). KG uniform Gemma-4-31B-NVFP4, ~149K entities/~627K relations.

## 7. FUSE mount + benchmark wiring (exact `next_plaid` edits)

Arm definition lives in `benchmarks/e2b/run_matrix.py`: `SUPPORTED_ARMS(:58)`, `MOUNT_ARMS(:97)`, `SURFACE_OFF_ARMS(:98)`, `DEFAULT_SEED_SOURCES(:99-113)`, **`arm_mount_env(arm)(:116-186)`**, `arm_seed_source(:189)`/`mount_sig(:645-654)`. Base env: `SEMFS_EMBED_MODEL=gemma-q4, SEMFS_EMBED_ONNX_DIR=/home/user/gemma_q4, SEMFS_GRAPH_FS=off(:117-133)`. `ppr_*`: `SEMFS_HIDDEN_KG=on, SEMFS_COMENTION=on, SEMFS_HIDDEN_KG_RETRIEVAL=off`, differ only `SEMFS_KG_PPR=on|off(:154-163)`. Env flows: (1) mount/daemon `do_mount→sh("semfs mount ...", env=arm_mount_env(arm))(:202-204)`; `reset_runtime_seed` copies seed→`~/.semfs/chanpin.db(:343)`. (2) cell/agent `run_cell→cell_driver.py(:479-498)`, agent `grep` → PATH shim `/opt/semfs-shims/grep`. `ppr`-style arms need **no `cell_driver.py` branch**. Alt: `semfscodex.py` `_mount_semfs(:210-229)` mounts over `work_dir`. `ppr_map`: `semfs_map.py:build_map(:32-97)` → `WB_WORKSPACE_MAP` prompt prefix (`cell_driver.py:160-168`).

**Adding `next_plaid`** (daemon-side arm like PPR): (1) `+next_plaid` to `SUPPORTED_ARMS(:58)`; (2) `+next_plaid` to `MOUNT_ARMS(:97)`+`SURFACE_OFF_ARMS(:98)`; (3) opt `DEFAULT_SEED_SOURCES(:99-113)`; (4) `elif arm=="next_plaid":` in `arm_mount_env(:154-163)` — hidden-KG base + the new backend flag; (5) **add flag to `mount_sig(:645-654)`** (else shares `ppr_on` mount, silently misses flag); (6) Rust flag read `hidden_kg.rs:75` or `resolve.rs:129-135` + branch the stage at `sqlite_vec.rs:1435`. No `cell_driver.py` edit unless client-side grep format changes.

## 8. Have vs missing — multi-vector readiness

| Component | Status | Note |
|---|---|---|
| Extracted text per file | ✅ HAVE | `chunks.text`, `db.rs:855` — re-embed substrate |
| Chunking | ✅ HAVE | 200w/30 overlap (`chunk.rs:16-23`); ColBERT wants smaller windows |
| Single-vector index | ✅ HAVE | `vchunks` vec0 `float[768]` |
| Candidate+rerank seam | ✅ HAVE | `sqlite_vec.rs:1423-1438`; `Reranker` trait; `rep_chunk` chunk ids |
| Arm/env plumbing | ✅ HAVE | `truthy_env`, `RerankChoice`, `run_matrix.py` structs |
| **Per-token storage** | ❌ MISSING | vec0 1 vec/rowid; invariant fails closed → new `vchunks_tok(chunk_id,tok_ord,emb)` + pgvector sibling |
| **ColBERT model** | ❌ MISSING | embedder pooled only; need token-level ONNX output / real ColBERT (128-d) |
| **MaxSim scorer** | ❌ MISSING | RRF over ranks; need `rank::apply_maxsim` writing `similarity` |
| **Query per-token embed** | ❌ MISSING | query pooled (`:936`) |
| **arm + seed bake** | ❌ MISSING | string absent; precomputed index needs seed-build pop. |
| e5 query/passage prefixes | ⚠️ unused | orthogonal recall lever |

**Smallest viable (runtime MaxSim, no schema change):** `Reranker` impl token-embedding query+top-50-docs at query time, `SEMFS_RERANK_BACKEND=next_plaid`, exec at `:1435`. **Real (precomputed):** new `vchunks_tok` + relax invariant `:244` + `rank::apply_maxsim` keyed by `rep_chunk` at `:1435` + pgvector sibling + seed-build populate. Both converge on `sqlite_vec.rs:1423-1438` (scorer) + `run_matrix.py:58/97/98/154/645` (arm).
