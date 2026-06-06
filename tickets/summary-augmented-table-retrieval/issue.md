# Feature: LLM table summaries for retrieval — make numeric/tabular data findable

- **Type:** Feature / search quality (proposed fix for the tabular-retrieval root cause)
- **Status:** IN PROGRESS — table-level (per-sheet) version BUILT at the extract layer
  (2026-06-04). Mechanism unit- + live-tested; the decisive **seed/ranking E2E is still
  pending** (see "Implementation status" at the bottom).
- **Created:** 2026-06-04
- **Component:** `semfs-core::extract` (spreadsheet/CSV), `backend::chunk` + `backend::sqlite_vec`
  (index/store/search), `backend::rank` (rerank input), `llm` (gpt-4.1-mini summarizer)
- **Branch context:** `feat/backend-agnostic-store`
- **Depends on / built atop:**
  - `tickets/local-ranking-precision-vs-supermemory/` — established the root cause this fixes.
  - The Knob B work (whole-doc return): the daemon already separates *rank-input* from
    *returned-content* (`sqlite_vec.rs` search → `memory` field), which this generalizes.

## Problem this solves (measured, case 289)

On Workspace-Bench case 289 (`"best-selling product"`), the answer file
(`6-product-sales-analysis-dashboard…xlsx`) ranks locally at RERANK #4–6 vs Supermemory's #1,
and codex over-searches (28 searches vs 1) → semfs+sqlite cost MORE tokens than no semfs.

Root cause (proven — see the ranking ticket + DB chunk counts):

1. The dashboard has **4 chunks**; **3 are number tables** (`1978 1335 1154…`). Embedding bare numbers
   is semantically empty, so those chunks are **never retrieved** — only the 1 title/summary chunk
   enters the pool (confirmed: N=1 and N=3 gave byte-identical rerank scores).
2. The cross-encoder can't judge numeric tables, so it ranks **taobao commerce *prose*** above the
   dashboard.
3. "Best-selling" = highest total — an inference a text model can't make from cells.

So the retrievable signal for a data table is far weaker than its actual relevance. Knob B (whole-doc
return) mitigates *once the file is in the top-k*, but doesn't fix *getting it there*.

## Proposal

For tabular/numeric content, **embed an LLM-generated summary instead of raw cells**, and return the
summary **with** the raw table:

1. **Chunk per table** — preserve table units (Excel: per-sheet to start; CSV: whole file; multi-table
   sheets later). We currently flatten sheets into one blob — stop flattening.
2. **Summary per table** via **gpt-4.1-mini** — a natural-language description: what the table is,
   entities/columns, units, key figures/totals, AND temporal terminology (**quarterly / yearly /
   monthly**, fiscal periods) inferred from the columns so paraphrased queries still hit.
3. **Embed the summary** (text lane, e5) — this is the retrieval key. Rich prose → matches queries
   like "best-selling product" that raw numbers never could.
4. **Rerank on the summary** — short prose fits the reranker's 1024-token window comfortably (no OOM,
   no truncation; strictly better than feeding raw number chunks).
5. **Retrieve → feed summary + table together** to the agent.

### Critical design rule: summary FINDS, table ANSWERS
gpt-4.1-mini will sometimes hallucinate a total or name the wrong "best-seller." The summary is a
**signpost for retrieval/rerank only**; the agent must compute the answer from the **returned raw
table (ground truth)**. The proposal's "feed summary and table together" already enforces this — make
it explicit.

## Why it should work (maps to all three failure modes)

| Root cause | Fix |
|---|---|
| number-table chunks not retrieved | embed a rich summary → table becomes findable |
| reranker can't judge numbers | rerank the summary prose; fits the 1024 window |
| query-understanding gap ("best-selling" = max total) | summary carries the concept + terminology |

It is the standard **"synthetic text for retrieval"** RAG pattern for tabular data, and plausibly part
of why Supermemory ranks the dashboard #1. The competition becomes **summary-vs-summary** (dashboard's
"product sales totals, best-selling" vs taobao's "campaign requirements") — which the dashboard should
win.

## Costs / risks / open questions (where the real work is)

1. **Index-time LLM cost & latency.** ~1 call per table × ~1,165 Office/PDF files (many multi-sheet) =
   potentially thousands of gpt-4.1-mini calls per seed. Infra exists (OCR is already key-gated +
   `spawn_blocking`). Mitigate: **cache by content hash** (re-summarize only on change); **only
   summarize tabular/numeric files** (prose already embeds fine).
2. **Table detection.** Excel sheets are a natural unit (easy first cut). Multi-table-per-sheet (the
   dashboard had a data block + a summary block) needs blank-row/header heuristics — defer.
3. **Storage + re-seed.** New shape: store table text *and* its summary, embed the summary. Schema/
   index change → a **fresh seed** (like the e5 swap).
4. **Determinism.** LLM summaries vary run-to-run → **cache** them so re-seeds are reproducible.
5. **Hallucination** → mitigated by returning the raw table (see design rule above).

## Cheaper variant to evaluate first

**One summary per file (or per sheet) as an extra "summary chunk,"** embedded alongside the existing
raw chunks, combined with Knob B whole-doc return. Skips table-boundary detection, ~1 call/file, and
likely captures most of the benefit: the file becomes retrievable via its summary; Knob B returns the
full data. If this alone moves the dashboard to #1, the hard part (per-table detection) is unnecessary.

## Decisive test (cheap to define)

Re-seed (or seed a subset) with summaries, then for `"best-selling product"`:
- **Does the dashboard's summary retrieve + rerank #1?** (vs current #4–6)
- Then the E2E: codex case 289 search count → toward 1, tokens → toward Supermemory's ~35k.

## Relationship to other tickets
- `local-ranking-precision-vs-supermemory/` — the investigation that established the root cause; this is
  the proposed fix for its #3/#5 hypotheses (embedder/query-understanding on tabular data).
- Builds on the Knob B whole-doc-return work (rank-input vs returned-content already separated).
- `local-document-extractors/` — the extractor layer this extends (per-table extraction for Excel/CSV).

## Implementation status (2026-06-04) — table-level version built

Built the **per-sheet** ("table-level") variant entirely at the **extract layer**, so it is
backend-agnostic (both sqlite_vec and pgvector get it for free via `index()`):

- `extract/spreadsheet.rs` — **stopped flattening**: `extract_sheets() -> Vec<Sheet{name,text}>`
  (per-sheet is the table unit). Removed the now-dead `extract_spreadsheet`.
- `extract/summary.rs` (new) — per sheet: `gpt-4.1-mini` summary (prompt elicits
  entities/columns/units/key figures **and** temporal granularity: daily/weekly/monthly/
  quarterly/yearly/fiscal), temp 0, max 256 tok, 16 KiB input head. **Weaves** `summary` ahead
  of the raw cells so the summary becomes the embedded retrieval key + reranker input while the
  raw table is returned verbatim (Knob B whole-doc) for ground-truth answers.
- **Content-hash cache** (blake3, `<os-cache>/semfs/summaries/`, keyed on text+model+
  prompt-version) → cheap, reproducible re-seeds. Verified to short-circuit the network.
- **Key-gated** like OCR: no `OPENROUTER_API_KEY` ⇒ falls back to flattened raw cells, so the
  offline path never regresses. `extract_text` Xlsx/Xls arm routed through it on the blocking
  pool under a 180s budget.
- Tests: 13 new (weave, cache_key, cache round-trip, build_content fallback/weave/empty, key-
  gate, warm-cache-avoids-network, head_bytes boundary) + 2 **live** (`summarize_one`,
  `extract_text` weave) that ran green against the real API. Full crate suite green; clippy clean.

**Design decisions (intentional deviations from the pure proposal):**
- Embeds the summary **alongside** (not strictly instead of) raw cells. Reason: "embed only the
  summary" needs decoupling embed-source from stored-text in the shared `Cache::index` trait —
  a large/risky diff for a benefit (saving *local* e5 compute) that doesn't change ranking. The
  summary chunk is what matches semantic queries either way; raw number chunks stay unretrieved.
  Future refinement if needed.
- **CSV deferred** — CSV is valid UTF-8, so it flows the direct-text path (`file.rs`) and never
  reaches `extract_text`; wiring it is a separate change.
- **Multi-table-per-sheet deferred** (per the proposal).

**STILL PENDING — the decisive test** (§"Decisive test" above): re-seed with summaries and
confirm the dashboard retrieves+reranks #1 for `"best-selling product"` (vs #4–6), then the
codex case-289 E2E (search count → ~1, tokens → ~Supermemory's 35k). The mechanism is proven;
the *ranking outcome* is not yet measured.

## Status of related work (context, 2026-06-04)
- Knob B (whole-doc return, top-10 cap) — built, tested, deployed on the chanpin-e5 daemon. UNCOMMITTED.
- Knob A (top-N chunks to reranker) — tested, does NOT help (answer file has 1 retrieved chunk; reranker
  capped at 1024 tokens). Locked at N=1; kept the rerank batch-size OOM fix. UNCOMMITTED.

## OUTCOME (2026-06-04) — summaries INVALIDATED for this query; per-sheet extraction RETAINED

Clean, same-binary A/B on `"best-selling product"` (dashboard cross-encoder rank, best of 3 copies):

| Config | RRF | Cross-encoder | In top-10? |
|---|---:|---:|:---:|
| e5 flattened, no summary | #17 | #8 | ✅ |
| **e5 per-sheet, no summary** ⭐ | #14 | **#5** | ✅ |
| e5 per-sheet, descriptive summary (append) | #17 | #11 | ❌ |
| e5 per-sheet, coverage summary (embed-only) | #72 | not reranked → ~#72 | ❌ |

- **Per-sheet extraction helped** (#8 → #5) — keep it as the indexing default.
- **Every summary variant hurt** — descriptive-append (#11) and coverage+embed-only (#72). Summaries
  make tables findable in the abstract but not distinguishable; embed-only additionally stripped the
  spreadsheet's chunk mass, dropping it below high-chunk code/JSON files in RRF.
- **The real blocker is the retrieval stage**, not summaries: per-chunk RRF summation makes
  chunk-count = RRF mass, so code files (`gen_*.py`, `package-lock.json`) dominate content queries.
  → root cause + fix moved to `rcas/2026-06-04-rrf-chunk-mass-bias-code-lane-pollution.md` and
  `tickets/rrf-chunk-mass-and-lane-fusion/issue.md`.

**Decision:** drop summary weaving/embed-only (off by default); keep per-sheet extraction; pursue the
RRF chunk-mass + code-lane fix next.
