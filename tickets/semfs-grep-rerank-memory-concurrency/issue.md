# Tech debt: `semfs grep` rerank activation spikes RAM → caps mount concurrency

Status: OPEN (tech debt) · Filed 2026-06-13 · Found during the E2B WB-PM matrix (8GB sandbox)
Related: tickets/workspace-bench-5arm-matrix/E2B_EXPERIMENT_LEDGER.md · rcas/2026-06-01-semfs-prewarm-oom-import-collection.md

## Symptom
On a real `semfs mount` of the chanpin seed (gemma-q4), **each `semfs grep` transiently
spikes the daemon RSS from a ~1.75 GB baseline to ~6.85 GB** (free memory → ~120 MB on an
8 GB box), then releases when the grep returns. Two concurrent greps would peak at ~13 GB →
**OOM**. This forces the benchmark to run **serially (concurrency=1)** on an 8 GB E2B sandbox
— roughly 6–8× slower than the planned conc=2+.

## Where the memory actually goes (grounded in the source, not the index)
The user's intuition was "free the in-memory index → more concurrency." Investigation shows
**it is NOT the index** that spikes:

- **Baseline ~1.75 GB** = the gemma-q4 embedder + the cross-encoder reranker model + the
  FS/index, **loaded ONCE at daemon start** (`daemon_runtime.rs:48 build_embedder`; L5 reranker
  built once). Kept resident so every grep is fast (no per-query model reload). *This* is the
  "in-memory" load — and keeping models resident is the right latency default; it's shared
  across greps and is not the concurrency limiter.
- **The per-grep SPIKE is the cross-encoder reranker's ATTENTION ACTIVATION.** Per
  `rerank/local.rs`: fastembed builds one `batch_size × max_length` input tensor per batch;
  attention memory is **O(batch × seq²)**. Knobs today: `RERANK_BATCH_SIZE=8`,
  `RERANK_MAX_LENGTH=1024`. The code comment records the history: the *default* batch (all ~50
  candidates × a 1024 window) **"blew past 15 GB → OOM-killed the daemon"**; batch=8 bounds it —
  but at batch 8 / seq 1024 it still peaks ~6.85 GB while scoring ~50 candidates per query.
- The vector index (`vchunks`, a vec0/SQLite virtual table) is read via SQL, not held as a giant
  RAM blob — so "lazy-load the index" would barely move the baseline and would NOT fix the spike.

## Why it caps concurrency
The resident models (baseline) are shared fine across greps. But **each grep's rerank allocates
its own transient activation tensor (~5 GB peak)**. N concurrent greps ⇒ N × activation ⇒ OOM.
So the limiter is *per-rerank activation × concurrency* — which is exactly why freeing/shrinking
that peak (or capping how many reranks run at once) would restore concurrency.

## Desired behaviour / options (in rough priority)
1. **Global rerank concurrency semaphore in the daemon.** Cap to K simultaneous rerank passes
   regardless of how many greps are in flight (queue the rest). Lets many greps run concurrently
   while only K pay the activation peak. K sized to RAM (K=1 on 8 GB; higher on bigger boxes).
   This is the cleanest concurrency win without changing result quality.
2. **Expose the rerank knobs as env** (`SEMFS_RERANK_BATCH_SIZE`, `SEMFS_RERANK_MAX_LENGTH`) —
   they're compile-time `const` today. Smaller batch / shorter window (seq² → quadratic savings)
   shrinks the peak on constrained mounts, at a latency/precision cost.
3. **Stream/chunk the rerank** so peak activation is independent of candidate count.
4. **Profile to confirm** the ~5 GB is rerank activation vs candidate-text vs vec read — add a
   memory-by-stage log to the L1→RRF→L5 pipeline (cheap, removes guesswork).
5. **Quantized/smaller cross-encoder for memory-constrained mounts** — the int8 variant load
   path exists (`rerank/local.rs`); verify it cuts *activation*, not just weights (it may not, if
   inference upcasts).

## Impact if fixed
- Run the WB PM matrix at **conc>1 on 8 GB** (the current ~6–8 h serial run → ~1–2 h).
- Removes the OOM that also derails `SEARCH_ONLY=off` real-mount runs on small boxes.

## Acceptance criteria
- [ ] A daemon-level cap on concurrent reranks (semaphore), default sized so peak RSS stays under
      a configurable ceiling.
- [ ] Rerank batch/seq overridable via env.
- [ ] Memory-by-stage log proving where the spike comes from.
- [ ] A/B: ≥2 concurrent `semfs grep` on an 8 GB mount without OOM, at equal result quality.

## Refs
- `crates/semfs-core/src/rerank/local.rs` (RERANK_BATCH_SIZE/MAX_LENGTH; the 15 GB-OOM history)
- `crates/semfs-core/src/backend/rank.rs`, `backend/sqlite_vec.rs` (rerank wiring / candidate cap)
- `crates/semfs/src/cmd/daemon_runtime.rs:48,99,110` (embedder + reranker built once at start)
- E2B evidence (RSS trace 1.75 GB baseline → 6.85 GB spike, free → 120 MB):
  tickets/workspace-bench-5arm-matrix/E2B_EXPERIMENT_LEDGER.md
