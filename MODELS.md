# Embedding & Reranking Models

How semfs chooses the models that power semantic search (L2 embed / L5 rerank),
what the defaults are, and how to change them — including what "custom model"
means today and its requirements.

> **TL;DR**
> - Defaults are **local [fastembed-rs](https://github.com/Anush008/fastembed-rs) registry models** — downloaded + cached on first use, fully offline afterward.
> - The **backend family** (local vs cloud) is an **env var** switch — no recompile.
> - The **specific model** is a compile-time constant — changing it is a one-line edit + rebuild.
> - **Arbitrary user-supplied ONNX (bring-your-own) is not wired yet** — deferred. Today "custom" means *a different fastembed registry model* or *a cloud model*.

---

## 1. Default models

| Role | Model | Dims | Vector lane | Source |
|------|-------|------|-------------|--------|
| **Text embed** | `Snowflake/snowflake-arctic-embed-s` | 384 | `vchunks` | fastembed registry (`EmbeddingModel::SnowflakeArcticEmbedS`) |
| **Code embed** | `jinaai/jina-embeddings-v2-base-code` | 768 | `vchunks_code` | fastembed registry (`EmbeddingModel::JinaEmbeddingsV2BaseCode`) |
| **Rerank** | `jinaai/jina-reranker-v2-base-multilingual` (`onnx/model_int8.onnx`) | — | — | HF repo, pinned commit `9cfeff2…`, loaded via fastembed user-defined path |

Files that pin these (edit here to change — see §4):
- `crates/semfs/src/cmd/resolve.rs` — `TEXT_EMBED_MODEL`, `CODE_EMBED_MODEL`, `RERANK_MODEL`, `RERANK_ONNX`, `RERANK_REV`.

**Routing:** a file is sent to the code lane (`jina-code`) when its extension is
code-like (`.rs`, `.py`, `.ts`, `.go`, …; see `is_code_path` in
`crates/semfs-core/src/backend/sqlite_vec.rs`); everything else (prose, config,
markdown) uses the text lane (`arctic-s`). Search queries both lanes + keyword
(FTS) and fuses with RRF.

**Where models are cached:** fastembed's cache dir (e.g. `./.fastembed_cache/`,
git-ignored). Download happens once, at daemon startup (embedders) and first
`grep` (reranker); afterward everything is offline.

---

## 2. The two-level selection model

There are **two independent dials**:

```
                         ┌─ SEMFS_EMBED_BACKEND ─┐        ┌─ which model ─┐
  embedding  ─────────►  │ local | openai |      │  ───►  │ const in      │
                         │ openrouter | hash     │        │ resolve.rs    │
                         └───────────────────────┘        └───────────────┘
   (env var, no rebuild)                          (compile-time, rebuild)
```

1. **Backend family** — runtime, via env vars. Pick local fastembed vs a cloud
   provider. No rebuild.
2. **Specific model** — compile-time `const` in `resolve.rs`. Changing *which*
   registry model or cloud model is used is a code edit + rebuild.

This split is why "switch to cloud" is easy but "use a different local model"
needs a recompile today.

---

## 3. Switching the backend family (no rebuild)

All via environment variables read by `ResolveEnv::from_env`
(`crates/semfs/src/cmd/resolve.rs`).

### Embedding — `SEMFS_EMBED_BACKEND`

| Value | Effect | Requires |
|-------|--------|----------|
| _(unset)_ / `local` | **Default.** fastembed registry models (arctic-s text, jina-code code) | nothing (downloads on first use) |
| `openai` | OpenAI `text-embedding-3-small` (1536d) | `OPENAI_API_KEY` |
| `openrouter` | OpenRouter `text-embedding-3-small` (1536d) | `OPENROUTER_API_KEY` |
| `hash` | Deterministic hash embedder (dev/testing only — no real semantics) | nothing |

### Reranking — `SEMFS_RERANK_BACKEND`

| Value | Effect | Requires |
|-------|--------|----------|
| _(unset)_ / `local` | **Default.** fastembed `jina-reranker-v2` (int8) | nothing (downloads on first use) |
| `cohere` | Cohere rerank via OpenRouter | `OPENROUTER_API_KEY` |
| `relace` | Relace reranker | `RELACE_API_KEY` |
| `none` | Skip reranking (RRF order stands) | nothing |

### Notes
- **Cloud embeddings still index locally.** The cloud provider produces the
  vectors; they're stored + searched in the *local* SQLite/pgvector index.
- **Fails loud:** selecting `openai` without `OPENAI_API_KEY` returns an explicit
  error, not a silent fallback.
- **Non-silent default:** if a cloud key is present but no `SEMFS_*_BACKEND` is
  set, the resolver logs an INFO noting it's defaulting to local and how to switch.
- **Cloud search fallback** is separate: when `grep` finds no usable local index
  it falls back to the Supermemory `CloudIndex` (requires an API key). That's a
  search backend, not an embed/rerank model.

#### Example

```bash
# Default — fully local, offline after first download.
semfs mount notes --path ./mnt

# Cloud embeddings (OpenAI) + local int8 reranker.
SEMFS_EMBED_BACKEND=openai OPENAI_API_KEY=sk-... semfs mount notes --path ./mnt

# Cloud embeddings + cloud reranker, both via OpenRouter.
SEMFS_EMBED_BACKEND=openrouter SEMFS_RERANK_BACKEND=cohere \
  OPENROUTER_API_KEY=sk-or-... semfs mount notes --path ./mnt
```

---

## 4. "Custom models" — the tiers

What you can do today depends on *how* custom you need to be. Tiers 1–3 are
supported; Tier 4 (arbitrary local ONNX) is deferred.

### Tier 1 — A different fastembed **registry** model (supported, rebuild)

Swap, e.g., `arctic-s` for `bge-small-en-v1.5`, or `jina-code` for another code
model, by editing the constant in `crates/semfs/src/cmd/resolve.rs`:

```rust
const TEXT_EMBED_MODEL: EmbeddingModel = EmbeddingModel::BGESmallENV15; // was SnowflakeArcticEmbedS
```

**Requirements:**
- Must be a variant of fastembed-rs's `EmbeddingModel` enum (≈44 models:
  all-MiniLM, all-mpnet, BGE en/zh/m3, GTE, Jina v2, multilingual-e5, mxbai,
  nomic, Snowflake-arctic, …) or `RerankerModel` enum (4: `bge-reranker-base`,
  `bge-reranker-v2-m3`, `jina-reranker-v1-turbo-en`,
  `jina-reranker-v2-base-multilingual`).
- **Dimensions are read from the registry automatically** — you do *not* set a
  dims env var; the vec0 table is created at the model's true width.
- **Pooling/normalization is handled by fastembed** per model — registry models
  "just work" (no manual pooling config).
- **Changing a model invalidates existing indexes** built with the old model
  (see §5). Start a fresh index.

### Tier 2 — A different **ONNX variant** of a registry model (supported, rebuild)

This is exactly how the default reranker uses the int8 build. To pick a different
quantization (e.g. fp16) edit `resolve.rs`:

```rust
const RERANK_ONNX: &str = "onnx/model_fp16.onnx"; // was onnx/model_int8.onnx
const RERANK_REV:  &str = "<commit-sha>";          // pin a reproducible revision
```

**Requirements:**
- The named file must exist in that model's Hugging Face repo (e.g.
  `jina-reranker-v2-base-multilingual` ships `model.onnx`, `model_int8.onnx`,
  `model_fp16.onnx`, `model_quantized.onnx`, …).
- Pin `RERANK_REV` to a commit SHA so the artifact is reproducible across
  machines/builds.
- (Embedders currently always load the registry's default ONNX; per-variant
  selection like this exists only for the reranker via `from_registry_onnx`.)

### Tier 3 — A different **cloud** model (supported; small code edit)

`OpenAiEmbedder::new(api_key, base_url, model, dims)` is a generic
OpenAI-compatible `/embeddings` client — it can point at any compatible endpoint.
Today the resolver hard-codes `text-embedding-3-small`/1536d for the `openai`
branch, so using a different cloud model is a small edit in `build_embedder`
(`resolve.rs`). 

**Requirements:**
- An OpenAI-compatible `/embeddings` endpoint (`base_url`), the model name, the
  output dimension, and an API key.

### Tier 4 — Arbitrary user-supplied ONNX, "bring your own" (NOT yet supported)

Loading a model from an arbitrary local directory is **deferred** — the previous
`from_dir` path was removed during the fastembed-registry alignment. The only
local loaders today are `from_registry` (embedder) and `from_registry_onnx`
(reranker), both registry-sourced.

When this is enabled, the expected requirements (from fastembed's
`try_new_from_user_defined` contract) will be:
- A model directory with `onnx/model.onnx`, `tokenizer.json`, `config.json`,
  `tokenizer_config.json` (and `special_tokens_map.json`).
- **Embedders:** a known pooling strategy (fastembed defaults to mean pooling;
  a CLS-pooling model loaded as mean would silently produce wrong vectors), and
  the correct output dimension declared.
- **Rerankers:** a cross-encoder (sequence-classification head emitting one
  relevance score per query–doc pair).

If you need this now, say so — it's a scoped addition (re-introduce a
`from_dir`/`from_path` constructor + a `SEMFS_*_MODEL_DIR` env knob), not a
redesign.

---

## 5. Model changes & the index (important)

Each local index records an **embedder identity** (model + dims + fastembed
revision) per lane. The system is strict about this to avoid silent relevance
corruption:

- A reader (`grep`) only uses the local index when its embedder identity matches
  the one that built it; otherwise it **falls back to cloud search**.
- A writer (daemon) **refuses** to open an index under a different/mismatched
  model rather than mixing two vector spaces.
- There is **no automatic re-embed** of existing files when you change a model.

**So: changing any local model requires a fresh index** — delete the cache DB /
re-mount a clean container. Reverting to the previous model makes the old index
valid again (changes are non-destructive). This is by design; see the per-lane
guards in `crates/semfs-core/src/backend/sqlite_vec.rs`.

---

## 6. Quick reference — env vars

| Var | Purpose | Values |
|-----|---------|--------|
| `SEMFS_EMBED_BACKEND` | embedding backend family | `local`(default) \| `openai` \| `openrouter` \| `hash` |
| `SEMFS_RERANK_BACKEND` | reranking backend family | `local`(default) \| `cohere` \| `relace` \| `none` |
| `OPENAI_API_KEY` | OpenAI embeddings | — |
| `OPENROUTER_API_KEY` | OpenRouter embeddings, Cohere rerank, LLM (L4/L7) | — |
| `RELACE_API_KEY` | Relace reranker | — |

There is intentionally **no** `SEMFS_EMBED_DIMS` and **no** model-name env var —
dims come from the registry, and the specific model is a compile-time constant
(§4).
