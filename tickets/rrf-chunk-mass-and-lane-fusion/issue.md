# Feature: fix RRF chunk-mass bias + tame the code lane on content queries

- **Type:** Feature / search ranking (the dominant retrieval-stage lever for content queries)
- **Status:** **IN PROGRESS** — fix #1 (max/best-rank aggregation) **BUILT + unit-tested 2026-06-04**,
  both backends; live re-seed measurement pending. Fixes #2 (code-lane gate) and #3 (per-lane quotas)
  **descoped** (owner: ship #1 alone first). Root cause confirmed in
  `rcas/2026-06-04-rrf-chunk-mass-bias-code-lane-pollution.md`.
- **Created:** 2026-06-04
- **Component:** `semfs-core::backend::rank` (`rrf_bump`, `FileAcc`, `to_hits`) + the retrieval lanes in
  `backend::sqlite_vec::search_blocking` (and the mirror in `backend::pgvector`).
- **Branch context:** `feat/backend-agnostic-store`

## Problem (see the RCA for full evidence)

`rrf_bump` sums an RRF contribution **per matching chunk**, so a file's fused score scales with its
**chunk count**, not its best rank. High-chunk code/JSON files (`gen_*.py`, `package-lock.json`) match
"product" via the code lane and flood the RRF top-50 on content queries, burying the answer file. On
the coverage-summary + embed-only seed the dashboard fell to **RRF #72**, outside the rerank window, so
the cross-encoder never scored it. This is the textbook **long-document / many-chunk bias**; the
literature's fix is **max/best-rank aggregation** plus **query-aware lane weighting**.

## Scope (ordered; ship + measure incrementally)

### 1. Max / best-rank RRF aggregation  — root-cause fix, do first  ✅ DONE 2026-06-04
Count each file **once per lane** using its **best (lowest) chunk rank**, not a per-chunk sum.
- In each lane's retrieval loop, track the best rank seen per filepath *for that lane*; bump RRF once
  per (file, lane) with that rank. Equivalent to the canonical "each chunk votes for its parent doc,
  keep the best vote."
- `FileAcc` already collects `chunks: Vec<(rank, text)>`; extend per-lane best-rank tracking (or pass a
  lane id to `rrf_bump` and dedup per (file, lane)).
- Keep the top-N-chunks-for-rerank behaviour (`RERANK_CHUNKS_PER_FILE`) unchanged.
- **Apply to both** `sqlite_vec` and `pgvector` so ranking can't drift (shared `rank.rs`).

### 2. Query-intent code-lane gate — "tame the code lane"  — DESCOPED (not pursued)
On natural-language/content queries, suppress or down-weight the code lane; keep it full-strength for
code-ish queries.
- Cheap heuristic first: treat a query as "code-ish" only if it contains code signals (identifiers with
  `_`/`::`/camelCase, file extensions, quoted symbols, bracket/operator tokens). Otherwise apply a
  code-lane weight `< 1` (or skip the lane) in fusion.
- Make the weight/threshold a const (env-overridable for sweeps), defaulting to a modest down-weight,
  not a hard skip (a content query can still legitimately want a code file).

### 3. (Optional) Per-lane candidate quotas → union → rerank  — DESCOPED (not pursued)
If code still leaks after 1+2: take top-N **per lane** independently, union, and let the cross-encoder
arbitrate the merged set (rerank 50–75). Guarantees the content lane a fixed share of the rerank slots
regardless of code mass. Bigger change; only if needed.

## Out of scope (decided)
- Score normalization / DBSF — we are rank-based (RRF), not score-based; not applicable.
- Summaries — invalidated for this query (see `tickets/summary-augmented-table-retrieval/`); per-sheet
  extraction is retained as the indexing default.

## Test / success criteria
Measure on **config #5** (per-sheet, no summary — current best: RRF #14 / cross-encoder #5) for
`"best-selling product"`:
- **After #1:** the RRF top should be sales/business data, not `gen_*.py`/`package-lock.json`; the
  dashboard's **RRF rank rises** (target: into the top-~15 → comfortably inside the 50-window) and the
  cross-encoder pulls it up (target: top-3, toward Supermemory's #1).
- **After #2:** code files largely absent from the content-query top-10.
- Guard against regressions: a code query (e.g. "function that retries the search") must still surface
  the right code file (code lane not over-suppressed).
- Add unit tests: `rrf_bump`/aggregation counts a multi-chunk file once per lane (best rank); code-ish
  vs content query classification.

## Why it matters
Confirmed three ways (Knob A, baselines, summary seeds) that the **retrieval stage** — not the
embedder, reranker model, or summaries — is what buries the answer file on content queries via code
chunk-mass. This is the highest-leverage remaining fix toward matching Supermemory's #1, and unlike the
summary experiments it has strong literature support and is cheap (#1 is ~10 lines).

## Implementation status (2026-06-04) — fix #1 built

`backend::rank` fuses **per lane (best rank)**, not per chunk:
- `Lane` enum (`Text`/`Code`/`Fts`); `rrf_bump(.., lane)` keeps the **min** rank per `(file, lane)`.
- `FileAcc.best_rank: [Option<usize>; 3]` replaces the running `score` field; `FileAcc::score()`
  derives `Σ_lanes 1/(RRF_K + best_rank)` — one vote per lane, count-invariant.
- `chunks` still collects every matched chunk → reranker input (`RERANK_CHUNKS_PER_FILE`) unchanged.
- Routed through both backends: `sqlite_vec` (text/code/fts) + `pgvector` (text/fts), shared `rank.rs`.
- Tests: `rrf_bump_is_count_invariant_within_a_lane` (50-chunk code file @best-rank-5 < 1-chunk file
  @rank-0; all 50 chunks still collected for rerank) + `rrf_bump_keeps_best_rank_per_lane`.
  `cargo test -p semfs-core --lib backend::` → 63 passed; clippy clean on `default` + `pg`.

**STILL PENDING — the decisive measurement** (success criteria above): re-seed config #5, confirm the
RRF top is sales/business data, the dashboard's RRF rank rises inside the 50-window, and the
cross-encoder pulls it toward #1. Mechanism unit-proven; live ranking outcome not yet measured.

## References
- `rcas/2026-06-04-rrf-chunk-mass-bias-code-lane-pollution.md` — root cause, full experiment table,
  research citations (max-vs-sum aggregation, weighted RRF, query routing, per-lane quotas).
- `tickets/local-ranking-precision-vs-supermemory/` — parent investigation.
