# RCA — case 289 token blowup is a CROSS-LINGUAL L1 RECALL MISS (2026-06-06)

## Problem (precise)
- **What:** semfs+codex on Workspace-Bench case 289 ("best-selling product") burns 82–134K tokens
  vs cloud's 18,144. Every cheap lever (embedder swap, backend swap, payload cap, grep-header
  guidance) failed to move it.
- **Where:** the local retrieval pipeline for the verbatim LOCAL query
  `best-selling product data file title transaction amount conversion rate`.
- **Seed:** `~/.semfs/chanpin-e5-nosum.db` (e5-small, 5,493 chunks). Probe via mounted daemon
  (`--no-sync --no-push`, `SEMFS_DEBUG_RANKING=1`), RANKDUMP in the daemon log.

## The decisive evidence
1. The answer file is in the seed as **two single-chunk copies** (1 chunk each, 468 chars):
   - `/desktop/fashion_ecommerce/product_data/best_selling_product_core_data_list.txt`
   - `/model_output/best_selling_product_core_data_list.txt`
2. For the verbatim query it appears in **ZERO RANKDUMP lines** — absent from the vec top-80,
   the fts top-80, the code top-80, the RRF top-57, and the RERANK top-50. It never becomes a
   candidate. So RRF/rerank/fusion are IRRELEVANT here — recall fails at L1.
3. **The answer content is 100% Chinese**, the query is 100% English:
   - `成交金额` = "transaction amount", `转化率` = "conversion rate", titles `2015春季新款…`.
   - EN terms transaction/conversion/best/selling/title/amount/product → **all absent** from the file.

## Why each lane misses
- **Lexical (FTS5/BM25):** dead. None of the English query tokens exist in the Chinese file →
  BM25 score 0 → file never enters the lexical pool. (True for ANY embedder — it's the lexical lane.)
- **Dense (e5-small):** must carry the entire EN→ZH bridge alone, on a terse numeric table — and
  fails. Full-corpus file-level vec rank of the answer (gemma_corpus, e5, 592 files):
  - EN-CLOUD query → **#384**, EN-LOCAL query → **#417** (hopeless; never enters k=80 pool)
  - ZH (Chinese) query → **#2**, ZH2 (Chinese) → **#1** ⭐, BILINGUAL (EN+ZH) → **#4**
  - i.e. translating the query alone swings the answer **#417 → #1** (~415 positions). Pure-ZH
    beats bilingual (mixing EN back in pulls the embedding toward EN distractors). **Cross-lingual
    translation is definitively the lever; e5's EN→ZH alignment is the weak link, ZH→ZH is excellent.**
  - Corpus is 55% Chinese (3029/5493 chunks >20 Han chars) — a structurally bilingual corpus.
- **Code lane:** N/A (a .txt, not code).

## Why prior levers all failed (now explained)
- **Embedder swap (e5/gemma/qwen/bge):** all multilingual, all still face a dead lexical lane + a
  hard EN→ZH dense match on a terse table → none reliably surfaces the answer. Consistent ~82–90K.
- **Backend swap (sqlite/pglite/pgvector):** identical retrieval logic → identical miss.
- **Payload cap:** made it WORSE (codex falls back to reading files when grep can't answer).
- **Grep-header guidance:** WORSE (+53%) — instructions can't help when the search never returns
  the answer; codex just explores more.
- **Cloud wins (18K, 4 calls):** Supermemory expands/translates the query (or uses a much stronger
  multilingual model), so the answer comes back at #1 → codex stops after ~4 calls.

## Root cause (stopping criteria met: actionable, controllable, fundamental, evidenced, non-blame)
**The token blowup is driven by an L1 recall miss caused by a cross-lingual gap: an English query
against Chinese content, with a lexical lane that is structurally dead cross-lingually and a dense
lane too weak (at e5-small/300M scale) to bridge EN→ZH on a terse table.** codex compensates by
exhaustively exploring the filesystem (15–19 calls), which is the token cost.

## Counter-analysis
- *Could it be fusion burying a retrieved answer?* No — RANKDUMP shows it never enters any pool.
- *Chunk truncation diluting the embedding?* No — single 468-char chunk, no truncation.
- *Wrong file indexed (403/garbage)?* No — content is the correct bilingual product table.

## The lever (cross-lingual recall), in priority order
1. **C5 — Translate/expand query to TARGET language (Chinese) before search.** Rewrite emits
   `成交金额 转化率 畅销产品 …` → revives BOTH lanes: BM25 gets live ZH tokens, dense gets a
   same-language match. Cheapest, no re-seed. **Highest leverage.**
2. **Summary-augmented extraction** (see memory `summary-augmented-table-retrieval`): weave an
   English NL summary of each table at extract time so the stored passage literally contains
   "best selling products … transaction amount … conversion rate" → both lanes match EN queries.
   Needs re-seed; backend-agnostic.
3. **Learned-sparse lexical lane (BGE-M3 sparse / SPLADE, C1):** replaces token-exact BM25 with a
   semantic lexical lane that can fire cross-lingually — structural, higher effort.

## ADDENDUM (2026-06-06 deep dive) — additional points of failure found
Full layer-wise probe + cloud/local trace comparison (see `tickets/embedder-config-search/case289_deep_analysis.html`):
- **F5 (critical, NEW BUG):** L6 salience + L7 co-mention **multiply** `similarity` (`rank.rs:193,206`), but
  cross-encoder rerank scores are **NEGATIVE** (−0.72). ×1.05 / ×salience(0.85–1.5) on a negative number
  makes the BEST hit *more negative → demoted*; stale hits (factor<1) get promoted. So rerank puts the
  answer at #1, then L6/L7 can knock it down to #2–3 behind a distractor.
- **F6 (critical, NEW):** `access_count` is bumped on EVERY search (`sqlite_vec.rs:1094`) and feeds salience
  → identical query returns DIFFERENT top-N order across runs (observed: answer #1 one run, #2/#3 the next).
  Non-deterministic ranking.
- **This is the precise cloud↔local link:** cloud grep returns the answer at a clean #1 → codex writes &
  stops (3–4 calls, 18–26K tok). Local's #1 is untrustworthy/unstable (F4 RRF dilution → rerank #1 → F5/F6
  demotion) → codex doesn't trust it → 62KB `os.walk` + pandas inspections (17–19 calls, 82–135K tok).
  So the "codex exploration is the lever" conclusion resolves to: **fix the ranking (F5/F6) so #1 is
  trustworthy & stable → codex stops exploring.**
- **F2 (high):** e5 prefixes (`query:`/`passage:`) NOT applied (`embed/local.rs:95` calls `embed(..,None)`)
  — e5 was trained to require them; degrades dense recall, esp. cross-lingual.
- **F9 (med):** 29 files in `fs_unindexed` FAILED to embed — dominated by **xlsx mis-detected as
  `format=Pdf`** → extraction fails (e.g. `annual_results_core_data_summary.xlsx`). 595 indexed / 29 failed.
- **F3 detail:** BM25/unicode61 has no CJK segmentation → matches only verbatim runs; confirmed `转化率`/
  `成交金额` (verbatim in answer) match, `畅销` (filename-only) and EN terms do not.
- Fix priority: **P0 = fix F5/F6 (ranking) + populate local profile.md (kills os.walk)**; P1 = keep rewrite,
  add e5 prefixes; P2 = xlsx→Pdf extractor fix, snippet returns.

## Verification plan
Apply C5 (translate-rewrite), re-probe with the mounted-daemon RANKDUMP: confirm the answer enters
the pool and ranks #1–3 post-rerank locally (zero codex tokens). Only then spend one E2E to confirm
the token drop toward cloud's 18,144.
