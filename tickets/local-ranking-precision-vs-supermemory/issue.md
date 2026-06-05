# Investigation: local sqlite ranking precision lags Supermemory → agent over-searches → no token savings

- **Type:** Investigation / search quality (the real blocker for local-backend token savings)
- **Status:** OPEN — evidence gathered (case 289), root direction identified, fix TBD
- **Created:** 2026-06-04
- **Component:** local search ranking (`semfs-core::backend::sqlite_vec` RRF + L5 rerank + L7/L6); `semfs grep` top-k
- **Branch context:** `feat/backend-agnostic-store`
- **Depends on / built atop:** the hang fix (`tickets/search-deadline-fails-closed-to-empty/`) and the
  reranker now wired into the sqlite daemon (`daemon_runtime.rs:99`). Those made local search *work*;
  this ticket is about making it *good enough to save tokens*.

## The finding

On Workspace-Bench case 289, semfs+sqlite (fixed build, reranker on) **passed but used 186,884
tokens — MORE than plain codex (143,837)**, while semfs+Supermemory used **35,763** (≈75% less).
A direct head-to-head of codex's `"best-selling product"` query shows the cause is **ranking
precision, not result verbosity**:

| | Supermemory (`/v4/search`) | local sqlite (reranked) |
|---|---:|---:|
| results returned (top-k) | **10** (server default) | **50** (`RERANK_CANDIDATES`) |
| total bytes | 505 KB | **115 KB** |
| per-result | full document | chunk (passage) |
| top-ranked hit | `…6-product-sales-analysis-dashboard.xlsx` (on-target) | `…qqbrand_usage_scenario_compliance_checklist.xlsx` (off-target) |
| searches codex issued (full run) | **1** | **28** |

**Key correction to an earlier assumption:** local is NOT more verbose per query — it returned
*fewer* bytes (115 KB vs 505 KB; Supermemory returns whole documents, one was 251 KB). The token
blow-up comes from codex issuing **28 searches on sqlite vs 1 on Supermemory.**

## Root direction

**Local ranking puts the wrong files on top.** Supermemory's server-side rerank surfaced the
product-sales-analysis dashboard first → codex found what it needed in one shot. The local
cross-encoder (now active) ranked brand-guidelines / compliance checklists above the sales data →
codex kept reformulating (28 queries) → 28× the search cost → no net savings.

So the dominant token lever for the local backend is **ranking precision** (right file at the top →
one search), not per-result size.

## Hypotheses to investigate (not yet confirmed)

1. **Cross-encoder model quality / inputs.** The local reranker scores the query against the
   *representative chunk* of each candidate file. If the rep-chunk is a poor summary of the file
   (e.g. a header row, a boilerplate paragraph), the cross-encoder mis-scores it. Compare what text
   each lane feeds the reranker vs what Supermemory reranks.
2. **Candidate pool (RRF top-50) quality.** The reranker only sees the top-50 RRF candidates. If the
   truly-relevant file isn't in the top-50 RRF (vector+FTS recall ordering), the reranker can't
   rescue it. Check whether the sales-dashboard file is even in the 50.
3. **Chunk granularity.** Local indexes chunks; the rep-chunk per file may not capture the file's
   gist. Supermemory ranks at document level (returns whole docs). Document-level signal may rank
   better than a single chunk.
4. **L7/L6 interaction.** The ×1.05 co-mention boost + salience could nudge off-target files up;
   verify they're not distorting the rerank order (esp. with the sparse ~740-edge graph).
5. **Missing query understanding.** Supermemory's `hybrid` mode may do query expansion/understanding
   server-side that the local lanes don't (L4 rewrite is opt-in/off here).

## Hypothesis ranking by potential (2026-06-04)

Ranked by likelihood of being the *dominant* cause, weighted by how cheaply each can be confirmed.
They're intertwined: #1/#2/#3 are one story — "does the right file get *into* the top-50 the
reranker sees, with coherent signal?" #1 is the symptom; #2 and #3 are the likely causes.

1. **(#2) RRF candidate-pool recall — HIGHEST, cheapest to check.** The reranker only ever sees the
   top-50 RRF candidates (`RERANK_CANDIDATES`). If the answer file isn't in those 50, no rerank can
   surface it — a hard ceiling. One-query check; its answer collapses the other four.
2. **(#3) Chunk vs document granularity — HIGH, and the likely CAUSE of #1.** Local ranks the
   representative *chunk* per file; Supermemory ranks whole *documents* (it returned the 251 KB
   dashboard as one result). A large relevant doc's signal is diluted across chunks → no single
   chunk scores high → it ranks low or falls out of the top-50. Biggest structural divergence.
3. **(#5) Query understanding / embedder language coverage — MEDIUM-HIGH.** Corpus is predominantly
   Chinese; the local text embedder is `snowflake-arctic-embed-s`, an **English-focused** model →
   likely weak vector recall on Chinese → wrong files surface in RRF (feeds #1). FTS catches exact
   Chinese tokens (so recall isn't zero) but doesn't rank well. Supermemory's `hybrid` mode + its
   embedder presumably handle Chinese far better. **Highest-leverage single fix if confirmed**
   (swap to a multilingual embedder, e.g. arctic-embed-m-v2.0).
4. **(#1) Cross-encoder reranker input — MEDIUM-LOW, mostly a symptom.** Matters only if the file is
   in the pool but ranked wrong. The model is likely fine; the variable is the rep-chunk it scores,
   which is the granularity problem (#2). Downstream, not an independent dominant cause.
5. **(#4) L7/L6 distortion — LOWEST.** Co-mention boost ×1.05 + salience ~0.85–1.5 over a sparse
   ~740-edge graph can't move a file from rank ~20 to #1. Negligible as a dominant cause.

### Decisive first experiment (splits the whole problem in ~one query)
For `"best-selling product"`, dump the **top-50 RRF candidates** and record the **rank of the
sales-dashboard file** (`/desktop/financial-data/6-product-sales-analysis-dashboard…xlsx`):
- **Not in the 50** → it's **recall** → chase #3 (multilingual embedder) + #2 (document-level scoring).
- **In the 50 but ranked low** → it's **rerank scoring** → chase #1.

Best current bet: the **#3 → #1 chain** (chunk granularity + an English embedder on a Chinese corpus
→ the right doc never reaches the reranker), with the **embedder language coverage (#3/#5)** as the
highest-leverage single fix.

## Complementary lever (smaller)

- **`grep` top-k cap.** Even with good ranking, returning 50 chunks is more than the agent needs.
  Cap `semfs grep` output to a small ranked top-k (~8–10, matching Supermemory's 10) with concise
  snippets. This trims per-search bytes but does NOT fix the over-searching (which is ranking).

## How to measure progress

- Re-run codex case 289 on the clean seed; success = **token count drops toward Supermemory's
  ~35k** AND **search count drops from ~28 toward a few**. Track both.
- Offline harness: for a fixed set of queries with known answer files, measure **rank-of-answer-file**
  (local vs Supermemory). Target: answer in the top ~3 locally, as Supermemory achieves.

## Why it matters

This is THE blocker for the local-backend value prop. The hang fix + reranker made local search
*functional*; this makes it *worth using*. Without it, semfs+sqlite costs more tokens than no semfs
at all (186k > 143k), so the local backend has negative ROI on this case.

## Evidence / related
- `rcas/2026-06-04-semfs-grep-hangs-post-search-under-load-no-token-savings.md` (the hang that masked this)
- `tickets/search-deadline-fails-closed-to-empty/` (hang + rerank-cap fixes — prerequisites, done)
- The §8 baseline: plain 143,837 (0 SM calls) · semfs+Supermemory 35,763 (1 search) · semfs+sqlite 186,884 (28 searches)
- Supermemory top-k = server default (10); semfs sends no `limit` in `SearchReq` (`api/dto.rs`).
