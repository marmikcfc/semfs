# Find the best semfs config for case 289 (embedder × quantization × backend × reranker)

- **Type:** Investigation / benchmark
- **Status:** OPEN — executing per `PLAN.md` (in this folder)
- **Created:** 2026-06-05

## Goal
Find the config that minimizes E2E codex tokens/calls on Workspace-Bench case 289, approaching the
cloud baseline (**18,144 tok / 4 calls / answer #1**), then generalize. Execution stages + gating are
in **`PLAN.md`**.

## Why this exists
Single-axis swaps haven't won at the agent level:
- e5→Gemma embedder fixed *vector recall* (#405→#3-7) but E2E tokens were flat (82.7K→87.2K).
- The binding constraints are downstream: **ranking (answer must hit #1), whole-doc payload size, and
  403-HTML data corruption** — not the embedder alone.
So "best config" = the right **combination**, found by a gated matrix sweep, not guesswork.

## Matrix
- **embedder:** e5-small · Gemma-300M fp32 (have) · **Gemma-300M fp16** · **Qwen3-Embedding-0.6B int8**
- **backend:** sqlite (baseline) · **pglite** (best sqlite config only)
- **reranker:** local jina · cohere/rerank-4-pro (`SEMFS_RERANK_MODEL` override, shipped)
- **knobs:** `SEMFS_RESULT_LIMIT`, `DOC_RETURN_CAP` (payload), RRF lane weighting (`rrf-chunk-mass-and-lane-fusion`)

## Run methodology (per config — fair vs baselines)
1. Seed into a **new tag** (`chanpin-<config>`); existing seeds untouched.
2. **Verify** the seed is the intended model (vchunks dim + `text_embed_model` stamp + chunk count).
3. **Grep gate** — verbatim cloud + local queries → answer rank (full pipeline).
4. **E2E** — `semfs-codex` case 289, `SEMFS_CONTAINER_TAG=<tag>`, `SEMFS_EMBED_MODEL=<model>`,
   `SEMFS_NO_PUSH=1 SEMFS_NO_SYNC=1`, default knobs; clear stale `output/SEMFSCodex--*/289`; record
   tokens / tool events / answer / duration.
5. Log into the results table in `cloud_env_state.md`.

## Verbatim test queries (must use exactly)
- CLOUD: `best-selling product data file top10 product title transaction amount conversion rate`
- LOCAL: `best-selling product data file title transaction amount conversion rate`

## Results
| config | backend | reranker | grep rank (cloud/local) | E2E tokens | events | embed time | notes |
|---|---|---|---|---:|---:|---:|---|
| e5-small | sqlite | jina | MISS / MISS | 82,653 | 19 | ~12m | baseline |
| Gemma-300M fp32 | sqlite | jina | #2 / #10 | 87,216 | 18 | ~82m | no win vs e5 |
| Gemma-300M fp16 | sqlite | jina | ≈ fp32 (not run) | — | — | — | **dead end** — see findings |
| Qwen3-0.6B int8 (ONNX) | — | — | answer #14/19 (subset) | — | — | — | **dead end** — see findings |
| <best sqlite> | pglite | — | — | — | — | — | deprioritized (backend-agnostic ranking) |

## Findings (2026-06-05) — quantized embedders are NOT the lever

Both requested quantized ONNX variants are **integration dead-ends** *and* show no ranking win:

**Gemma-300M fp16** (`onnx-community/embeddinggemma-300m-ONNX/model_fp16.onnx`):
- Exports a pooled `sentence_embedding` (good), but has **external weights** (`model_fp16.onnx_data`,
  617 MB). fastembed's user-defined embedder API takes ONNX **bytes**, which can't resolve external
  data → not loadable via the simple path (would need raw `ort` with a file path + external-data).
- fp16 is a *precision* change, not a *model* change → **ranking ≈ fp32** (which already gave NO E2E
  win: 87K ≈ e5's 83K). So fp16's only benefit is ~2× embed speed — irrelevant to the token goal.
  → Not worth integrating.

**Qwen3-Embedding-0.6B int8** (`onnx-community/Qwen3-Embedding-0.6B-ONNX/model_int8.onnx`):
- It's a **decoder/text-generation export** with KV-cache I/O (`position_ids` + 28× `past_key_values`)
  and outputs `last_hidden_state` (no pooled output). Needs **last-token pooling** — which fastembed
  can't do (Cls/Mean only). I ran a best-effort raw-`onnxruntime` probe (empty KV-cache + position_ids +
  last-token pool + query instruction, both right & left padding).
- Result on the competitor subset (19 files): answer `.txt` ranks **#14–15** (vs Gemma **#1–3** on the
  same subset). The richer dashboard ranks #2–3 in both, but the **bare ZH list `.txt` ranks poorly**
  under Qwen3-int8. Caveat: custom decoder inference + int8 quant — the *proper* test is the candle
  `Qwen3TextEmbedding` path (`tickets/embedder-upgrade-gemma-qwen3`), slow on CPU. But integration
  complexity + no observed win make Qwen3-int8-ONNX unattractive.

**Conclusion — pivot.** Three embedders now tested (e5, Gemma-fp32, Qwen3-int8-ONNX): **none beats the
others at the agent level**, because the embedder was never the binding constraint. The token win
(toward cloud's 18K) lives in:
1. **Ranking** — get the answer to **#1** (RRF lane-weighting / `rrf-chunk-mass-and-lane-fusion`); the
   answer `.txt` is a sparse list that fusion seats at #2–10. (The dashboard ranks #2–3 across ALL
   embedders — it may be the better retrieval target.)
2. **Whole-doc payload cap** (the ~120 KB first-grep dump).
3. **403-HTML ingestion fix** (`tickets/extraction-coverage-audit`).
**Recommend deprioritizing the embedder/quantization/pglite axes** until ranking+payload land — those
are backend-/embedder-agnostic and are where cloud's advantage actually comes from.

## Constraints
- KEEP all existing seeds intact (`chanpin-e5-nosum`, `chanpin-gemma`, `workspace-bench-chanpin`).
- Watch EC2 memory (15 GiB, no swap) during seeds; OOM-guard.

## Related
- `PLAN.md` (this folder) · `tickets/embedder-upgrade-gemma-qwen3/` · `tickets/explore-agent-search-behavior/`
- `tickets/rrf-chunk-mass-and-lane-fusion/` (ranking lever) · `tickets/ranking-debug-observability/`
- `tickets/extraction-coverage-audit/` (why some files weren't embedded)
