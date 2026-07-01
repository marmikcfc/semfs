# Ticket: Parallelize L7 (entity-graph extraction) — decouple from the index write path

- **Status:** IMPLEMENTED & VERIFIED (2026-06-02) — **both backends** (SQLite + pgvector/pglite).

## RESOLUTION (2026-06-02)

L7 decoupled from the synchronous index/flush path into a background, concurrency-bounded worker —
exactly the design below, with the queue living inside the indexer (no `SqliteFile` plumbing).

**What landed:**
- `crates/semfs-core/src/cache/graph_queue.rs` (new) — `GraphQueue` (ino-keyed, deduped) + `run_graph_worker`
  (`Semaphore(L7_CONCURRENCY=8)` + `JoinSet`, mirrors the hydration worker).
- `LocalIndexer` trait — defaulted `graph_queue()` + `index_graph()` (no-op for backends without an extractor).
- `SqliteVecStore` — `index()` writes vectors only and **enqueues** the file; `index_graph()` reads content,
  runs `extract_entities` in `spawn_blocking` (so N blocking LLM calls overlap), writes `edges`. Split
  `drop_file_vectors`/`drop_file_edges` so a vector re-index preserves edges until the worker re-derives them.
- `daemon_runtime` — spawns `run_graph_worker` (clones the indexer before `with_indexer`).

**Verified on EC2** (full chanpin warm with `OPENROUTER_API_KEY` set → `entity-graph extraction enabled (L7)`):
- Indexing is **NOT gated** by L7 — ran at full embed speed (chunk 343 at ~50 s, vs ~1 chunk/s with the
  old inline L7).
- `edges` **populated concurrently** by the worker (358→380→… while indexing continued).
- RSS bounded ~5.5 GB (Phase 1 / OOM #2 fix).
- Tests: 2 new (`index_enqueues_graph_work_when_extractor_present`, `reindex_preserves_edges_delete_clears_them`) +
  existing all pass (223 semfs-core + 43 semfs), clippy clean.

**pgvector/pglite ALSO decoupled (2026-06-02).** `PgVectorStore` now implements the same split:
`index()` writes chunks + enqueues; `index_graph()` reconstructs content from the file's stored chunk
`text` (no cache-DB handle on this backend), runs `extract_entities` in `spawn_blocking` (blocking ureq),
and writes `edges` via async sqlx; `with_graph_extractor` creates the `GraphQueue`; trait
`graph_queue()`/`index_graph()` implemented. `run_graph_worker` is backend-agnostic, so it drives both.

**Verified on EC2** (pglite backend, `SEMFS_STORAGE_BACKEND=pglite`, L7 on): mounted embedded pglite,
indexed with **CPU ~230%** (NOT the ~15% of inline-gated L7 → decoupled), **RSS bounded ~1.6 GB**, no L7
errors; `semfs grep "product roadmap planning"` returned **67 results** over the pglite store (full
read path incl. co-mention boost). Unit: `pg_index_enqueues_graph_work_when_extractor_present` +
existing pglite tests pass (231 total with `pg-local`).

---

### (original design below)
- **Status (at filing):** OPEN (design ready)
- **Created:** 2026-06-02
- **Branch:** `feat/backend-agnostic-store`
- **Component:** `semfs` daemon — local indexer (L7 entity graph) + a new background extraction worker
- **Depends on:** `tickets/solve-oom-issue/` (OOM #2 gates a *full* seed regardless of L7)
- **Goal:** build the L7 entity graph (`edges`) during seeding without the ~3-hour serialized stall — turn it into a ~5-min concurrent background job so the index can ship *with* the graph.

---

## Problem

L7 = the entity graph. At index time, `index()` calls `graph::extract_entities(llm, content)` — a **synchronous, blocking `ureq` LLM call** (model `openai/gpt-4.1-nano`) — once per file, and stores `file → entity` rows in the `edges` table. The co-mention boost (`rank.rs::apply_comention_boost`, ×1.05) reads those edges at search time.

Today this runs **inline on the FUSE flush path**, which the fuser session dispatches on a **single thread** (`rt.block_on(file.flush())`). So L7 calls are **fully serialized**: one ~1–3 s LLM round-trip at a time, CPU idle. Measured: **~1.5 chunks/s vs ~15 chunks/s** with L7 off → a full chanpin seed goes from ~15 min to **~2.5–3 h**.

L7 is **embarrassingly parallel** (per-file independent) and **network-IO-bound** (the CPU is idle during each call), so it parallelizes well even on the current 4-vCPU box. The only thing serializing it is the architecture.

## Model & cost (answers a recurring question)

- **Model:** `openai/gpt-4.1-nano` via OpenRouter — **hardcoded** in `LlmClient::openrouter()` (`crates/semfs-core/src/llm.rs:30`). The generic `LlmClient::new(key, base_url, model)` *can* take any model, but `resolve::build_llm` uses the hardcoded `openrouter()` path. Only the **API-key presence** is config-driven (gates whether L7 runs at all); the model string is not.
- **Cost estimate (chanpin, ~1,181 indexable text files, one call/file):**
  - Input ≈ file content + system prompt ≈ ~7k tokens/file (mixed EN/CJK) → ~8.3M input tokens.
  - Output (structured entity JSON) ≈ ~500 tokens/file → ~0.6M output tokens.
  - At ~\$0.10/1M in + ~\$0.40/1M out (gpt-4.1-nano, **verify current OpenRouter pricing**): **≈ \$1–3 per full warm.**
  - **2× if we build both sqlite *and* pglite indexes** (each backend stores its own `edges` → extraction runs per backend) → **~\$2–6 total**. A handful of large transcription files dominate per-call latency/tokens but the total stays small. Cost is **not a constraint**.
- **Recommended minor improvements:** (a) make the model **config-driven** (`SEMFS_LLM_MODEL` env, default `gpt-4.1-nano`); (b) consider extracting entities **once and reusing** across backends to avoid the 2× (see Open questions).

## Design: background, concurrency-bounded extraction pool

Mirror the existing **hydration worker** (`crates/semfs-core/src/cache/hydration.rs`: `Semaphore::new(HYDRATION_CONCURRENCY=4)` + `JoinSet` + `claim_next`/`complete`/`enqueue`).

**1. Split the indexer write into two phases.** The `LocalIndexer` trait gains a deferred graph method; backends (`SqliteVecStore`, `PgVectorStore`) implement both:
```
index(ino, filepath, content)        // SYNCHRONOUS on flush: chunk → embed → write chunks/vectors/fts.
                                      //   L7 extraction is REMOVED from here.
index_graph(ino, filepath, content)  // DEFERRED (async): extract_entities(llm) → write `edges`.
                                      //   no-op when no graph_extractor is attached.
has_graph_extractor() -> bool        // so CacheFs knows whether to enqueue.
```

**2. `CacheFs::flush()` enqueues instead of extracting inline.** After `index()` (vectors) succeeds, if `indexer.has_graph_extractor()`, push the `ino` onto a **graph scheduler** (a clone of the hydration scheduler: dedup pending, skip in-flight). flush returns immediately after the embed.

**3. New `run_graph_worker` background task** (started in `daemon_runtime::run`, like the hydration worker):
```
loop:
  acquire Semaphore(L7_CONCURRENCY)            // start ~8, env-tunable; IO-bound so can go higher
  ino = scheduler.claim_next()
  spawn:
     content = fs.read_all_content(ino)        // re-read from cache; queue holds inos, not bytes
     spawn_blocking(|| indexer.index_graph(ino, fp, &content))   // blocking ureq → N concurrent
     scheduler.complete(ino)
```
- `spawn_blocking` because the LLM client is blocking `ureq` (tokio blocking pool holds many concurrent calls); the `Semaphore` bounds real concurrency.
- Queue holds **inos, not content** → memory stays bounded (worker re-reads `fs_data`).
- `edges` writes are already **idempotent** (`DELETE FROM edges WHERE from_path=?` then insert) and **fail-open** (extraction error → warn, no edges for that file).

**4. Drain semantics for warms.** Expose graph-queue depth (or a `wait_idle`) so a pre-warm can **wait for L7 to drain** before declaring "done"/`ready`. Coverage check (`seedcheck.py`) extended to report `edges` files vs indexed files.

## Code touch points
- `crates/semfs-core/src/cache/mod.rs` — `LocalIndexer` trait: add `index_graph`, `has_graph_extractor`.
- `crates/semfs-core/src/backend/sqlite_vec.rs` — move the L7 block out of `index()` into `index_graph()`.
- `crates/semfs-core/src/backend/pgvector.rs` — same split for the pgvector backend.
- `crates/semfs-core/src/cache/file.rs` (`flush`) — enqueue graph work after vector index.
- `crates/semfs-core/src/cache/graph_sched.rs` (new) — scheduler (copy hydration scheduler).
- `crates/semfs-core/src/cache/graph_worker.rs` (new) — `run_graph_worker` (copy hydration worker).
- `crates/semfs/src/cmd/daemon_runtime.rs` — spawn `run_graph_worker`; wire shutdown/drain.
- `crates/semfs-core/src/llm.rs` (optional) — `SEMFS_LLM_MODEL` env override.

## Expected throughput
| | rate | full chanpin seed (~1,181 files) |
|---|---|---|
| Serialized (today) | ~0.5–1 file/s | ~2.5–3 h |
| Pool N=8 | ~4 files/s | **~5 min** |
| Pool N=16 | ~8 files/s (until rate-limited) | **~2–3 min** |

Because L7 now overlaps embedding, total seed time ≈ `max(embed, L7)` instead of `embed + L7`.

## Testing
- Unit: scheduler dedup/in-flight (copy hydration tests); `index_graph` writes edges + re-index/delete clears them (existing `reindex_and_delete_clear_a_files_edges` test moves to the new path).
- Integration: flush enqueues; worker drains; `edges` populated for all text files after drain; fail-open on a forced LLM error.
- E2E: a small mount with `OPENROUTER_API_KEY` set → after drain, co-mention boost fires; with key unset → no enqueue, no edges, search still works.
- Concurrency: N parallel files → bounded RSS, no DB-lock deadlock (edges write under the same `Immediate` tx behavior as chunks).

## Caveats
- **Eventual consistency:** edges fill in shortly *after* the vector index; a search in that window runs the ±5% co-mention boost with partial edges. Fine transiently; warms gate "done" on drain.
- **Does NOT fix OOM #2:** L7 leaving the flush path removes the *time* blocker, not the embed-arena *memory* blocker. A full seed still needs the OOM #2 fix.
- **Rate limits:** cap N (8–16) and add retry/backoff on 429s.

## Open questions
1. Extract entities **once per file content** and share across backends (sqlite + pglite) to avoid 2× cost/time? Would need a backend-neutral entity cache keyed by content hash.
2. Batch K files per LLM call (fewer round-trips) vs. concurrency (simpler, robust)? Start with concurrency.
3. Make `L7_CONCURRENCY` and the model env-tunable.

## Success criteria
- Full chanpin warm **with L7** completes in **~15–20 min** on the 4-vCPU box (≈ `max(embed, L7)`), no OOM.
- `edges` populated for ~all indexed text files (uniform, not the stale 110).
- Search co-mention boost verified to fire; L7-off path unchanged (no enqueue, no edges).
