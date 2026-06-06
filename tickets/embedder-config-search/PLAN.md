# Master Execution Plan — embedder/quantization/backend config search (case 289)

- **Created:** 2026-06-05
- **Owner goal:** find the semfs config that gets codex closest to the cloud baseline (18,144 tok / 4 calls / answer #1) on Workspace-Bench case 289, then generalize.
- **Hard constraint:** ALL existing seeds stay intact on the EC2 instance — never overwrite
  `~/.semfs/chanpin-e5-nosum.db`, `~/.semfs/chanpin-gemma.db`, `~/.semfs/workspace-bench-chanpin.db`.
  Every new config → a NEW tag (`chanpin-<config>.db`).

## Where we are (baselines, E2E codex case 289)

| config | tokens | tool events | answer rank | embed time (5,777 chunks) | status |
|---|---:|---:|:--:|---:|:--:|
| plain codex | 143,837 | — | — | — | passed |
| e5-small sqlite, 50s | 82,653 | 19 | MISS→brute-force | ~12 min | passed |
| **Gemma-300M fp32 sqlite** | **87,216** | 18 | #2–10 | ~82 min | passed |
| cloud (Supermemory) | 18,144 | 4 | #1 | n/a | passed |

**Key learning (empirical):** swapping the embedder (e5→Gemma) **fixed vector recall but did NOT reduce
agent tokens** (87K ≈ 83K). The binding constraints are now downstream:
1. **Ranking** — answer must land **#1** (cloud) not #2–10; otherwise codex doesn't trust one hit and brute-forces.
2. **Whole-doc payload** — the first `grep` dumps ~120 KB (10 whole docs) that re-replays in context.
3. **403-HTML xlsx** — corrupt files burn ~5 dead-end calls.
So the "best config" is an embedder **AND** a ranking/payload story, not the embedder alone.

## Models / backends to evaluate

| axis | options |
|---|---|
| text embedder | e5-small (384d, baseline), Gemma-300M fp32 (768d, have), **Gemma-300M fp16**, **Qwen3-Embedding-0.6B int8** (1024d) |
| storage backend | sqlite (baseline), **pglite** (best sqlite config only) |
| reranker | local jina (default), cohere/rerank-4-pro (override shipped) |
| knobs | RESULT_LIMIT, DOC_RETURN_CAP (payload), RRF lane weighting |

Quantized targets (HF `onnx-community`):
- `embeddinggemma-300m-ONNX/onnx/model_fp16.onnx` (~600 MB; ~2× faster than fp32, ~same ranking)
- `Qwen3-Embedding-0.6B-ONNX/onnx/model_int8.onnx` (~300–400 MB; stronger model, last-token pooling — verify the ONNX exports a pooled `sentence_embedding`)

## Stages (gated — don't advance on a fail)

### Stage 0 — docs + tickets (this commit)
- [x] Master plan (this file)
- [ ] Update `benchmarks/workspace_bench/cloud_env_state.md` with the e5-50s / Gemma / cloud results + learnings
- [ ] Ticket: `find-best-config` (the matrix + run methodology)
- [ ] Ticket: `files-not-embedded` (extraction-failure audit)

### Stage 1 — code: user-defined ONNX embedder
fastembed's registry hardcodes `model.onnx` (fp32); quantized variants need the user-defined ONNX path.
- Add `LocalEmbedder::from_user_defined_onnx(onnx_path, tokenizer_dir, output_key, dims)` (mirror the
  reranker's `try_new_from_user_defined`).
- Map new `SEMFS_EMBED_MODEL` values: `gemma-fp16`, `qwen3-int8` → download + load the specific .onnx.
- **Verify** each ONNX graph outputs a pooled `sentence_embedding` (Gemma: yes; Qwen3: confirm — if it
  emits token states needing last-token pooling, fastembed can't (Cls/Mean only) → fall back to the
  candle `Qwen3TextEmbedding` path per `tickets/embedder-upgrade-gemma-qwen3`).

### Stage 2 — standalone full-corpus validation (cheap, read-only, no E2E, no new seed)
Per new embedder: embed all 5,777 chunks (read-only off an existing seed copy in /tmp), KNN-rank the
verbatim queries. Record **embed time** + **answer rank**. Gate: answer in top-10 + embed time sane.

### Stage 3 — seed (new tag, surgery from chunks — extraction is embedder-independent)
Copy an existing seed → `chanpin-<config>.db`, swap the text vector lane to the new model (re-embed
chunks; reuse fs tree + chunks + ffts). Verify dims + stamp. Existing seeds untouched.

### Stage 4 — grep gate (full pipeline) per config
Mount the new seed, grep verbatim queries, record answer rank through vec+FTS+RRF+rerank.

### Stage 5 — E2E codex per promising config
`semfs-codex` case 289, `--no-push --no-sync`, default knobs (fair). Record tokens/calls/answer/time.

### Stage 6 — pglite with the BEST sqlite config
Seed pglite (new tag) with the winning embedder; E2E; compare to its sqlite twin (backend-parity check).

### Stage 7 — synthesis
Pick the config minimizing E2E tokens (toward 18K). Note which levers (embedder/ranking/payload/403)
each config still leaves on the table.

## Execution order (this run)
1. Stage 0 docs+tickets → 2. Stage 1 code → 3. Stage 2 validate **qwen3-int8** + **gemma-fp16** →
4. (gate) Stage 3–5 for whichever passes → 5. Stage 6 pglite(best) → 6. Stage 7 synthesis.
Qwen3-int8 is the higher-upside (stronger model → maybe #1); gemma-fp16 is mainly a speed test
(fp16≈fp32 ranking, so likely ≈87K but faster).
