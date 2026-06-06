# Embedder upgrade — fix cross-lingual recall (EmbeddingGemma-300M, then Qwen3-0.6B)

- **Type:** Investigation → feature (gated, staged)
- **Status:** OPEN — starting with EmbeddingGemma-300M
- **Created:** 2026-06-05
- **Component:** `semfs-core` embed seam (`embed/local.rs`, `cmd/resolve.rs`) + re-seed
- **Branch:** `feat/backend-agnostic-store`

## Why

Workspace-Bench case 289: the answer file ranks **#1 in cloud** but **never enters the local top-10**.
Proven root cause = **cross-lingual retrieval recall** (English query ↔ Chinese content); the local
`multilingual-e5-small` embedder doesn't bridge EN→ZH well enough to retrieve the answer into the
candidate pool. Reranker swaps (incl. `cohere/rerank-4-pro`) did **not** help — the answer isn't in the
pool, so rerank can't recover it. The lever is the **embedder**.

Full evidence: `tickets/explore-agent-search-behavior/retrieval-investigation.html` +
`rcas/2026-06-05-agent-search-token-blowup-turn-multiplication.md`.

### Proof the embedder is the lever (local seed, current e5)
| query | answer rank |
|---|---|
| EN ("best-selling product data file …conversion rate") | **MISS** |
| EN+ZH / pure ZH | **#1** |
| EN + `--rewrite` (in-language expand) | MISS |
| EN, reranker = cohere/rerank-4-pro | MISS |

## Candidate embedders (both supported by fastembed 5.13.4)

| model | backend | dims | pooling | semfs path |
|---|---|---|---|---|
| **EmbeddingGemma-300M** | **ONNX registry** (`EmbeddingModel::EmbeddingGemma300M`, `onnx-community/embeddinggemma-300m-ONNX`) | 768 | Mean | reuse `LocalEmbedder::from_registry` (tiny change) |
| **Qwen3-Embedding-0.6B** | **candle** (`Qwen3TextEmbedding::from_hf`, `qwen3` feature) — NOT ONNX (ONNX path can't do its last-token pooling) | 1024 | last-token | new `Embedder` impl + enable `qwen3` feature (pulls candle) |

Note (F4): neither model's full strength is reachable without **query/passage-aware prompts**
(Gemma: `task: search result | query:` / `title: none | text:`; Qwen3: `Instruct: …\nQuery:`). semfs's
single `embed()` can't yet distinguish query vs doc. A first pass can run degraded (no prompt); the
real win likely needs the prompt-aware refactor.

## Plan (staged, gated — do NOT skip gates)

### Phase 1 — EmbeddingGemma-300M (ONNX, easiest first)
1. **Download + setup** EmbeddingGemma-300M via fastembed (registry pull).
2. **Tiny standalone test** (no re-index): embed the **exact codex queries** + the answer chunk +
   current top distractors; rank by cosine. Test both *with* and *without* Gemma's retrieval prompts.
   - **Gate:** answer chunk ranks at/near #1 for the exact query → proceed. Else → skip to Phase 2.
3. **Full seeding** — re-index the chanpin corpus with Gemma (768-dim, new embedder identity).
4. **Simple test** — `semfs grep` the exact codex queries against the Gemma seed; check answer rank.
5. **End-to-end** — run semfs-codex case 289 (against a seed COPY; `--no-push --no-sync`); compare
   tokens / tool calls / answer rank vs the e5-50s and cloud baselines.

### Phase 2 — Qwen3-Embedding-0.6B (candle) — ONLY if Gemma fails the gates
1. **Tiny standalone test** first (Python or a candle probe) on the exact queries.
2. If it ranks the answer → integrate via the `qwen3` candle feature + new `Embedder` impl, re-seed,
   E2E. ⚠️ 0.6B on CPU (4 vCPU, no GPU) will be slow to index + per-query.

## Decision criteria
- **Pass (standalone):** the exact cloud query and exact local query rank the answer file in the top few.
- **Pass (E2E):** case 289 with the new embedder finds the answer via search (not brute-force),
  fewer tool calls / lower tokens than e5-50s (82.7K), trending toward cloud (18.1K).

## Exact queries (verbatim — test on these)
**CLOUD:** `best-selling product data file top10 product title transaction amount conversion rate`
**LOCAL:**
1. `best-selling product data file title transaction amount conversion rate`
2. `best-selling product data file`
3. `top10 product title transaction amount conversion rate`
4. `best selling product`

Answer file: `desktop/fashion_ecommerce/product_data/best_selling_product_core_data_list.txt`
(also acceptable: `desktop/financial-data/6-product-sales-analysis-dashboard(...).xlsx`).

## Guardrails
- **Never touch the canonical seed** `~/.semfs/chanpin-e5-nosum.db` — re-seed into a NEW tag
  (`chanpin-gemma`), run E2E against a copy, `--no-push --no-sync`.
- Cloud container `workspace-bench-chanpin` is read-only baseline (`--no-push`).
- EC2 box: `m7i.xlarge` 4 vCPU / 15 GiB, no swap — watch memory during re-seed.

## Related
- `tickets/explore-agent-search-behavior/` (investigation + HTML tracker)
- `tickets/rrf-chunk-mass-and-lane-fusion/` (chunk-mass; complementary)
- `tickets/search-throughput-readpath-isolation/` (throughput)
- `benchmarks/workspace_bench/cloud_env_state.md` (env + baselines)
