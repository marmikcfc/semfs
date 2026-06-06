# RCA: RRF chunk-mass bias — high-chunk code/JSON files dominate content queries; embed-only made it fatal

- **Date:** 2026-06-04
- **Severity:** high — the dominant cause of local search ranking the answer file far below Supermemory
  on content queries; directly sank the summary experiments.
- **Component:** `semfs-core::backend::rank::rrf_bump` (RRF fusion) + the three retrieval lanes in
  `backend::sqlite_vec::search_blocking` (text/`vchunks`, code/`vchunks_code`, fts/`ffts`).
- **Status:** root cause confirmed; **fix #1 (max/best-rank aggregation) IMPLEMENTED 2026-06-04**
  (unit-tested, both backends); live re-seed measurement pending. Fixes #2/#3 not pursued. See
  `tickets/rrf-chunk-mass-and-lane-fusion/`.

## Symptom

On Workspace-Bench case 289 (`"best-selling product"`), across every local index variant the answer
file (`6-product-sales-analysis-dashboard.xlsx`) ranked well below Supermemory's #1. The
**coverage-summary + embed-only** seed was the worst: the dashboard fell to **RRF #72–84**, *outside*
the 50-candidate rerank window, so the cross-encoder never even scored it — it was dropped from the
returned top-10 entirely.

## Investigation — full experiment arc (dashboard = the answer file)

Two ranking stages: **RRF** (rank-fusion of the 3 lanes → candidate pool) gates **cross-encoder**
(jina-reranker-v2-base-multilingual int8, top-50 → final). Best of the dashboard's 3 duplicate copies:

| # | Config | Embedder / Extraction / Summary | RRF rank | Cross-encoder rank | In top-10? |
|---|---|---|---:|---:|:---:|
| — | Supermemory `/v4` (target) | server hybrid + server rerank | — | **#1** | ✅ |
| 1 | arctic, single chunk *(hist.)* | arctic-s / flattened / none | #19 | #11 | ❌ |
| 2 | e5, single chunk *(old binary)* | e5 / flattened / none | #17 | #4 | ✅ |
| 3 | e5, Knob A top-3 *(hist.)* | e5 / flattened / none | #17 | #6 | ✅ |
| 4 | e5 flattened, no summary | e5 / flattened / none | #17 | #8 | ✅ |
| 5 | **e5 per-sheet, no summary** ⭐ best | e5 / per-sheet / none | **#14** | **#5** | ✅ |
| 6 | e5 per-sheet, descriptive summary (append) | e5 / per-sheet / append | #17 | #11 | ❌ |
| 7 | e5 per-sheet, coverage summary (embed-only) | e5 / per-sheet / embed-only | **#72** | not reranked (RRF>50) → ~#72 | ❌ |

(Rows 4–7 measured on the same binary/session — directly comparable. Rows 1–3 are earlier binaries.)

Decomposition established along the way:
- **Per-sheet extraction helped** (flattened #8 → per-sheet #5): splitting multi-sheet workbooks into
  per-sheet units reshuffled competitors favourably; best config measured.
- **Summaries hurt** (per-sheet no-summary #5 → descriptive #11 → coverage embed-only #72): summaries
  make tables findable *in the abstract* but not *distinguishable* (every spreadsheet gets a generic
  "product/sales" summary), and embed-only additionally stripped the spreadsheet's chunk mass.

## Root cause

`rrf_bump` adds an RRF contribution **per matching chunk**, summed per file:

```
acc.entry(fp).score += 1.0 / (RRF_K + rank)   // called once PER retrieved chunk, in every lane
```

So a file's fused score scales with **how many of its chunks matched** — RRF became
**chunk-count-weighted, not rank-weighted.** Consequences:

1. **High-chunk files win.** Code files (`gen_ecommerce_matrix.py`, `gen_value_matrix.py`, …) and
   `package-lock.json` have dozens of chunks and match "product" via the **code lane** (Jina code
   embedder over identifiers). Dozens of chunk-votes → large RRF mass. The RRF top-15 for
   `"best-selling product"` was almost entirely `.py` / `.js` / `package-lock.json` — *not* sales data.
2. **Embed-only made it fatal.** Collapsing each spreadsheet to a single summary chunk (corpus chunks
   5,776 → 2,997) removed the dashboard's competing chunk mass, so it sank to RRF #72 — below 71
   high-chunk code/JSON files — and never reached the reranker.

The cross-encoder is the right arbiter (a `.py` file isn't relevant to "best-selling product" and
would score low) — but it only sees the **top-50 RRF**, and RRF mass had already crowded the pool with
code before precision ranking could run.

## Research — how practitioners fix this

The literature converges on this being the classic **long-document / many-chunk bias**:

- **Aggregate by MAX, not SUM.** Studies on chunk→document aggregation find sum/mean "drop severely as
  more segments are added," while **max aggregation stays stable** and avoids length/count bias
  (Adapting Learned Sparse Retrieval for Long Documents, arXiv:2305.18494; Rethinking Chunk Size for
  Long-Document Retrieval, arXiv:2505.21700). Late-interaction work reports the same "monotonic bias
  favoring longer chunks regardless of true relevance." Canonical RRF fuses **ranked lists** (one entry
  per item per list); each chunk should "vote for its parent document" with the **best** rank kept, not
  a per-chunk sum (AI21 multi-scale RRF).
- **Weighted RRF** — per-lane weights to down-weight the code lane (OpenSearch, Weaviate, Qdrant).
- **Query-intent routing / dynamic weights** — detect code patterns/identifiers/quoted strings → lean
  keyword/code; natural language → lean vector; route/gate lanes by query type (Milvus routing; Ailog
  query routing; MODE arXiv:2509.00100; RAGRouter arXiv:2505.23052).
- **Per-retriever quotas → union → cross-encoder rerank** — take top-N from each lane independently,
  union, let the reranker arbitrate; rerank 50–75 is the sweet spot (ZeroEntropy reranker guide; TDS
  cross-encoders & reranking).
- **Score normalization / DBSF** — normalize each source's distribution before *score* fusion (Qdrant
  DBSF) — less applicable to us (we are rank-based, not score-based).

### Sources
- https://arxiv.org/pdf/2305.18494 — Adapting Learned Sparse Retrieval for Long Documents (max vs sum)
- https://arxiv.org/html/2505.21700v2 — Rethinking Chunk Size for Long-Document Retrieval
- https://www.ai21.com/blog/query-dependent-chunking/ — multi-scale RRF, chunk-votes-for-parent
- https://opensearch.org/blog/introducing-reciprocal-rank-fusion-hybrid-search/ — RRF / weighted RRF
- https://qdrant.tech/documentation/search/hybrid-queries/ — hybrid fusion & DBSF
- https://weaviate.io/blog/hybrid-search-explained — hybrid search & weighting
- https://milvus.io/blog/build-smarter-rag-routing-hybrid-retrieval.md — routing + hybrid retrieval
- https://app.ailog.fr/en/blog/guides/query-routing-rag — query routing
- https://arxiv.org/pdf/2509.00100 — MODE: Mixture of Document Experts
- https://arxiv.org/pdf/2505.23052 — RAGRouter
- https://zeroentropy.dev/articles/ultimate-guide-to-choosing-the-best-reranking-model-in-2025/ — candidate-set sizing
- https://towardsdatascience.com/advanced-rag-retrieval-cross-encoders-reranking/ — two-stage rerank
- https://supermemory.ai/blog/hybrid-search-guide/ — Supermemory's own hybrid-search writeup (our #1 baseline)

## Recommended fix (detail in the ticket)

1. **Max / best-rank aggregation** — count each file **once per lane** (its best chunk's rank), not
   once per chunk. Root-cause fix; ~10 lines; literature-blessed. **✅ IMPLEMENTED 2026-06-04.**
2. **Query-intent code-lane gate** — suppress/down-weight the code lane on natural-language queries.
   **Not pursued** (descoped per owner: ship #1 alone first).
3. **(Optional) per-lane candidate quotas → union → cross-encoder rerank** — guarantee the content lane
   a share of the 50 rerank slots regardless of code mass. **Not pursued.**

Measure against config #5 (per-sheet, no summary; current best RRF #14 / cross-encoder #5): does the
dashboard's RRF rank rise and the cross-encoder pull it toward #1?

## Fix #1 — implementation (2026-06-04)

`backend::rank` now fuses **per lane (best rank)** instead of per chunk:
- Added a `Lane` enum (`Text`/`Code`/`Fts`). `rrf_bump` takes a `lane` and keeps the **min** rank per
  `(file, lane)`; `FileAcc.best_rank: [Option<usize>; 3]` replaces the running `score` field, and
  `FileAcc::score()` derives the fused score as `Σ_lanes 1/(RRF_K + best_rank)` — one vote per lane.
- `chunks` still collects **every** matched chunk → reranker input (Knob A) unchanged.
- Both backends route their lanes through it: `sqlite_vec` (text/code/fts), `pgvector` (text/fts) —
  shared `rank.rs`, so they can't drift.
- Tests: replaced the old per-chunk-sum test with `rrf_bump_is_count_invariant_within_a_lane`
  (50-chunk code file @best-rank-5 now scores below a 1-chunk file @rank-0; all 50 chunks still
  collected) and `rrf_bump_keeps_best_rank_per_lane`. `cargo test -p semfs-core --lib backend::` →
  63 passed; clippy clean on `default` + `pg`.

**STILL PENDING — the decisive measurement:** re-seed and confirm on config #5 for `"best-selling
product"` that the RRF top is sales/business data (not `gen_*.py`/`package-lock.json`), the dashboard's
RRF rank rises comfortably inside the 50-window, and the cross-encoder pulls it toward #1. Mechanism is
unit-proven; the *ranking outcome* is not yet measured against a live index.

## Related
- `tickets/rrf-chunk-mass-and-lane-fusion/issue.md` — implementation ticket.
- `tickets/local-ranking-precision-vs-supermemory/` — the parent investigation.
- `tickets/summary-augmented-table-retrieval/` — the summary experiments that surfaced this (invalidated;
  per-sheet extraction retained).
