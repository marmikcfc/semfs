# RCA: semfs+sqlite codex run (clean seed, case 289) timed out — load-induced search timeout (NOT recall) → agent fell back to brute-force exploration

> **Verdict (see CORRECTION at bottom):** the initial "poor local-search recall" diagnosis was
> WRONG. Instrumented repro shows the exact queries return 72–84 hits standalone. The empties
> during the run were a **load-induced search timeout** (daemon CPU-saturated → 25s `SEARCH_DEADLINE`
> bail → empty). The sections below are kept as the investigation trail; the CORRECTION is authoritative.

- **Date:** 2026-06-04 (run: 2026-06-03)
- **Severity:** the run produced no result (timeout, 0 tokens, grade skipped). Blocks filling the semfs+sqlite metric on the clean seed.
- **Component:** `semfs` search under CPU contention (25s deadline bail to empty); daemon CPU cost (L5 rerank + background L7).
- **Status:** root-caused (corrected); **core fix IMPLEMENTED 2026-06-04** — search
  no longer fails closed on the deadline (returns best-effort RRF hits) + rerank
  candidate cap. Tested (266 green). CPU-thread cap / L7 throttle / bench-timeout /
  under-load EC2 re-run deferred. See `tickets/search-deadline-fails-closed-to-empty/`.

## Symptom

`semfs-codex`, case 289, `SEMFS_CONTAINER_TAG=chanpin-extract-test` (the clean OCR'd 5,773-chunk
seed): **status=timeout, totalTokens=0, durationMs≈2,000,000** (hit the 2000 s agent wall-clock),
`returned_paths_exist=False (skipped_due_to_status:timeout)`. The mount succeeded and the seed was
correct (5,773 chunks, uncontaminated); the *agent* never finished.

The prior run on the **old** `workspace-bench-chanpin` cache (12,320 chunks) **completed**
(62,667 tokens, passed) on the same task — so the timeout is specific to this run, not the harness.

## Investigation (codex trace: `output/SEMFSCodex--*/289/raw/codex_stdout.jsonl`)

In **one turn**, codex executed **94 shell commands**:
- **6 were `semfs grep`** — and their outputs were the problem:

| grep query | output bytes | useful? |
|---|---:|---|
| "store best-selling product data file top product title…" | **0** | empty |
| "best-selling product data store product title transaction amount…" | **0** | empty |
| (same, retried) | 1,482 | no answer file |
| "best-selling product sales data title transaction amount…" | **0** | empty |
| **"热销 商品 标题 成交金额 转化率 店铺"** (Chinese) | **0** | empty |
| "store best-selling product data file top product title…" | 117,344 | 117 KB flood (matched the many 403 pages), no precise hit |

- **The other 88 commands were manual filesystem exploration** — `ls -la`, `cat`, `ls -R source_files`,
  repeated `python os.walk`/`os.listdir`. Many commands appear **duplicated back-to-back** (retries).

So: codex tried semantic search first, **got mostly empty results** (4/6 greps returned 0 bytes,
including a native-language Chinese query), abandoned semfs, and **degraded into brute-force
directory walking** that never located + processed the buried answer file
(`desktop/fashion_ecommerce/product_data/top10_product_status_table.xlsx`) within 2000 s → killed.

## Root cause

**Local semfs search had poor recall on this seed** — reasonable English *and* Chinese queries for
the task returned **empty** result sets (or a 117 KB undifferentiated flood). With search not
surfacing the answer files, the search-reliant agent fell back to exhaustive manual exploration and
ran out of wall-clock. The timeout is a *downstream* effect of the **retrieval-quality defect**.

Why empty results (hypotheses, not yet confirmed):
1. **Rerank over-pruning** — the L5 rerank may discard all candidates below a threshold, returning
   nothing rather than best-effort top-k.
2. **FTS tokenization** — the daemon log is flooded with `Unicode mismatch … "fi"/"fl" ligature`
   warnings, suggesting the fulltext tokenizer mishandles ligatures/CJK, hurting lexical recall.
3. **Vector recall** — query-embedding vs the local index may be miscalibrated for these queries
   (the old cloud-pulled cache, which had transcription-sibling text, matched better → it completed).

## Contributing factor

The semfs daemon ran at **~300–357 % CPU (≈3.5 of 4 vCPUs) for the full ~33 min** serving the
rerank-heavy searches. On the shared 4-vCPU box this **slowed codex's own shell commands** (the
duplicated commands are consistent with slowness/retries) and burned wall-clock — so even the
brute-force fallback was slow. Per-query rerank cost is high.

## Contrast with the completing run

Old-cache run (12,320 chunks, cloud-pulled): completed, 62,667 tokens, 18 searches. Its search
returned usable results (the cloud pipeline's text matched the queries). The clean seed's search
returned empties — pointing at the **local index's recall** (FTS tokenization + rerank), not coverage
(the answer files are indexed in both: 1 chunk each).

## Proposed fixes

1. **Search recall:** never return empty when candidates exist — guarantee a best-effort top-k from
   the vector lane even if rerank scores are low; fix FTS ligature/CJK tokenization (the `Unicode
   mismatch` flood). Add a test: a product-data query on chanpin must return the product files.
2. **Per-query latency:** profile the L5 rerank; cap candidates reranked / cache the query embed so a
   single agent doesn't peg the box for 30 min.
3. **Bench robustness:** raise the agent timeout (this run was at the 2000 s edge; the completing run
   was ~2,430 s total) so a slow-but-progressing agent isn't cut off.

## Reproduce

Offline-grep the clean seed db directly with the case-289 query and confirm it returns empty
(`semfs grep "…product title transaction amount conversion rate…"` against
`extract-test/cache/.../chanpin-extract-test.db`).

## Related
- `rcas/2026-06-03-extract-uncapped-utf8-text-path-node-modules-hang.md`
- `tickets/local-seed-coverage-gaps/`, `tickets/decouple-backends-from-supermemory/`
- Memory: "semfs helps Codex (−75%) but not Claude" — assumed the grep surfaces the files; here it didn't.

---

## CORRECTION (2026-06-04) — instrumented repro DISPROVES the recall hypothesis

Added per-stage logging to `search_blocking` (qvec_len / vec_n / code_n / fts_n / rrf_files /
reranked / final_hits) and **re-ran codex's EXACT empty queries** against the clean seed
(idle daemon):

| query | qvec_len | vec_n | code_n | fts_n | rrf_files | final_hits |
|---|---:|---:|---:|---:|---:|---:|
| "store best-selling product data file top product title transaction amount conversion rate" | 384 | 80 | 80 | 80 | 72 | **72** |
| "热销 商品 标题 成交金额 转化率 店铺" (Chinese) | 384 | 80 | 80 | 51 | 84 | **84** |

The queries that returned **0 bytes during the codex run** return **72 and 84 hits standalone**,
and the Chinese query's TOP hit is literally the answer file
(`/desktop/fashion_ecommerce/product_data/best_selling_product_core_data_list.txt`). **Recall,
FTS tokenization, and rerank are all fine** — the "poor recall" hypothesis above is WRONG.

### Actual root cause: load-induced search timeout, not recall

During the codex run the daemon sat at **~300–357 % CPU for the full ~33 min**. Under that
starvation a search's **25 s `SEARCH_DEADLINE`** (≈ the daemon's `SEARCH_TIMEOUT`) is exceeded —
the local query-embed alone, CPU-starved, can blow the deadline — so the search **bails early
(sqlite_vec.rs:772, before taking the connection) and returns empty/error**. Idle daemon → embed
is fast → full pipeline → 72–84 hits. So the empties are a **timing/contention failure**, not a
retrieval-quality failure.

What saturated the daemon for 33 min (not fully confirmed, no per-search log from that run since
the logging postdates it): most likely **background L7 entity-graph extraction** (OPENROUTER on)
processing the per-case-mounted files, **+ the per-query L5 rerank** (cross-encoder ONNX, multi-
threaded → ~350 %). A single agent's searches + background L7 saturate the 4-vCPU box → foreground
searches starve → deadline → empty → codex falls back to manual exploration → 2000 s timeout.

### Corrected fixes

1. **Never return empty on deadline-bail when retrieval has candidates.** The early bail at
   sqlite_vec.rs:772 returns empty/error; it should return best-effort RRF hits (the deadline path
   at the rerank stage already does this — make the pre-connection bail consistent). An agent must
   not see "0 results" for a query that *does* match.
2. **Bound search CPU so one agent can't starve itself:** cap/parallelism-limit the L5 rerank
   (thread count + candidate cap), cache the query embed; consider pausing/throttling background L7
   while foreground searches are in flight.
3. **Raise the bench agent timeout** (2000 s was at the edge) so a slow-but-progressing run isn't cut.

The original "recall/FTS" fixes (#1 in the first Proposed-fixes section) are NOT needed — recall
is fine. Keep this correction as the authoritative cause.
