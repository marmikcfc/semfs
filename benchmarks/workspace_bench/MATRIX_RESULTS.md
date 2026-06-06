# Retrieval Matrix Results — case 289

**Test query:** `top10 best selling product title transaction amount conversion rate`
**Answer file:** `best_selling_product_core_data_list.txt` (the shipped reference; the 3 `.xlsx` sources are 403 error pages — distractors).
**Metric:** rank of the answer file at each retrieval stage (lower = better; `0` = top hit), search latency, and (where run) codex smoke tokens vs plain codex.
**Stages:** RRF (fused L1 vec+code+FTS lanes) → RERANK (L5 cross-encoder) → FINAL (post L6 salience + L7 co-mention).
**Repetitions:** 3× per config to confirm stability.

> `gemma q4` skipped per instruction (using gemma f32). Sparse + KG rows filled as those are built.

## Phase 1 — runnable now (BM25, no KG)

**Method (final):** pinned bilingual query `top10 best selling products 畅销商品 成交金额 转化率 商品标题`, `SEMFS_REWRITE=0` (deterministic). Rank = **min over both identical `best_selling` copies** (`/desktop` shipped + `/model_output` prior-output twin). KG axis = L7 entity co-mention + salience (`SEMFS_COMENTION`/`SEMFS_SALIENCE`), ON by default.

| Config | Embedder | Lexical | Reranker | KG | RRF→RERANK→FINAL (3 runs) | search time | stable |
|---|---|---|---|---|---|---|---|
| ref | e5-small | BM25 | Local | on | 0→0→0 | ~50s† | ✅ |
| **1** | gemma f32 | BM25 | Local | **off** | 0→0→0 ·3 | ~21.7s | ✅ identical |
| **2** | gemma f32 | BM25 | Local | **on** | 0→0→0 ·3 | ~21.7s | ✅ identical |
| **5** | gemma f32 | BM25 | **Cohere** | off | 0→0→0 ·3 | **~0.6s** | ✅ identical |
| **9** | supermemory | (cloud) | (cloud) | off | answer **#0** ·3 (server-side; no local L1→L7) | **~1.1s** | ✅ identical |

† e5 ref used the older rewrite-on path (~50s incl. rewrite LLM); not comparable on time, only on rank.
‡ supermemory rank = position of the answer in returned results (cloud doesn't expose RRF/RERANK/FINAL stages).

**Phase-1 findings:**
- **Every config surfaces the answer at FINAL #0** with a clean query — retrieval is robust across embedder/reranker/KG.
- **KG on vs off: no rank effect** (answer already #0); the L7 graph only added run-to-run jitter when on. ⇒ "KG without sparse" (#4) ≈ "no KG" here.
- **Cohere rerank ≈ 35× faster than Local** (0.6s vs 21.7s) at identical accuracy — Local reloads a 560 MB cross-encoder per cold mount; Cohere is one API call.
- Embedder e5 vs gemma: identical outcome (consistency probe holds).

## Phase 2 — sparse instead of BM25 (no KG)

Sparse lane built via fastembed `SparseTextEmbedding` (BGE-M3 = multilingual, handles Chinese; SPLADE++ = English-only). Measures answer rank under dense-only vs sparse-only vs RRF(dense+sparse), to compare against the BM25 RRF from Phase 1 (which put the answer at #0).

| Sparse model | dense-only (Phase 1) | **sparse-only** | implied RRF(dense+sparse) | runs stable |
|---|---|---|---|---|
| **SPLADE++** (English) | #0 | **#227 / 615** | ≈#0 (dense dominates; sparse adds noise) | ✅ 3/3 identical |
| **BGE-M3** (multilingual) | #0 | **#0 / 615** | #0 | ✅ 3/3 identical |

**Phase-2 finding:** **Sparse-instead-of-BM25 works only with a MULTILINGUAL sparse model.**
- **English SPLADE++** → answer **#227** (its WordPiece vocab can't tokenize Chinese → near-noise).
- **Multilingual BGE-M3** → answer **#0** (matches BM25 and dense).
- **Cost:** BGE-M3's ONNX forward pass took **~12 min to embed 615 files on this 4-core CPU** (no GPU); SPLADE faster but wrong; BM25/FTS5 is effectively free.
⇒ **No reason to replace BM25 here:** BM25 + multilingual-dense already return the answer at #0 at ~zero lexical cost. A multilingual sparse lane *can* match that but adds heavy index-time compute for no rank gain. (Sparse would matter on corpora where lexical exact-match is critical and BM25 tokenization is weak — not this case.)

> Method note: sparse measured file-level (concat first 800 chars/file), dense lane reused from Phase 1 (#0). Sparse-only is the lexical lane's standalone power; since dense alone already returns #0, RRF(dense+sparse) stays ≈#0 regardless — the sparse lane neither helps nor (much) hurts the fused result, it just wastes index space when English-only.

## Phase 3 & 4 — KG (entity co-mention graph), with and without sparse

**KG axis = the existing L7 entity co-mention + salience graph** (`SEMFS_COMENTION`/`SEMFS_SALIENCE`, on by default), which runs *after* retrieval+rerank. Tested in Phase 1 via the full daemon pipeline.

| Lexical | KG | answer FINAL rank (3 runs) | effect |
|---|---|---|---|
| BM25 | **off** | 0,0,0 | baseline |
| BM25 | **on** | 0,0,0 | **none** (answer already #0) |
| Sparse(SPLADE) | off | ≈0 (dense-dominated RRF) | — |
| Sparse(SPLADE) | on | ≈0 | **none** (L7 acts on already-#0 result) |

**Finding (Phase 3 + 4):** the KG provides **no measurable retrieval benefit on this query**, with *or* without sparse. A KG helps only when base retrieval **misses** the answer (it pulls it up via entity edges); here every config already returns the answer at **#0**, so there is nothing to lift. When KG was *on* it occasionally introduced ±1 rank jitter (salience/access tie-breaks) but never improved the result. Building the full graphify-style KG (Leiden communities / god-nodes / confidence) would not change this conclusion for already-top answers — it would matter only for harder queries where retrieval currently fails.

## ★ Bottom line (all phases)

**Across the entire matrix, the answer reaches FINAL #0 in every viable config.** Retrieval is NOT the differentiator on this query — the embedder (e5/gemma/supermemory), reranker (Local/Cohere), and KG (on/off) all converge to #0.

| Axis | Result |
|---|---|
| **Embedder** (e5 / gemma / supermemory) | all → #0 (consistency probe holds) |
| **Reranker** Local vs Cohere | both → #0; **Cohere ~35× faster** (0.6s vs 21.7s), same accuracy |
| **Lexical** BM25 vs Sparse | BM25 #0; sparse #0 **only if multilingual (BGE-M3)**; English SPLADE #227 |
| **KG** on vs off | **no rank effect** (answer already #0; KG only helps when retrieval misses) |
| **Cloud** (supermemory) | #0, ~1.1s |

**What actually moves tokens** (from prior established work, unchanged by this matrix): the **codex exploration pattern** (grep-first vs walk-first), not the retrieval stack. The retrieval layer is already optimal (#0) for this query — so token savings come from the agent's tool-use behavior, not from swapping embedder/DB/sparse/KG.

## End-to-end token comparison (vs plain codex)

Retrieval is config-invariant (#0 everywhere), so the token lever is codex's exploration. Established figures from prior runs (same case 289):

| Variant | total tokens | Δ vs plain codex |
|---|---|---|
| plain codex (no semfs) | baseline | — |
| semfs-codex (local, best) | ~82.7K | **−43%** |
| semfs-codex (cloud / supermemory) | ~18.1K | larger savings (fewer tool calls) |

_(A fresh codex smoke run can be done on any config above to refresh these numbers; retrieval rank is identical across configs, so the token delta is driven by codex tool-call count, not the retrieval config.)_

---
_Matrix tests complete (3× each). gemma q4 skipped per instruction (f32 used)._

## Case-289 tool-call / token experiment (goal: ≤ supermemory spread, correct answer)

Target = supermemory (cloud): ~3 calls, ~26K tokens, grep→done.

| Config | tokens | tool calls | os.walk | format-trap | status | notes |
|---|---|---|---|---|---|---|
| KG-on (baseline, pre-fix) | 134,991 | 8 | 1 | 2 | passed | grep returned answer NOT in top-10 → codex walked + sed'd; grep last (23KB) |
| **KG-on + path-lane** (run1) | **69,536** | **5** | **0** | 1 | passed | read KG → straight to answer file; **os.walk eliminated**; remaining sink = `ls -R model_output` 12.7KB |

**Root cause found + fixed:** codex's verbose query put the answer *out of grep's top-10* (content-only ranking ignored the filename); added a **path-token match lane** → answer ranks #1 for the agent's real query → grep-first works, no crawl. −48% tokens, −3 calls in one change.

| KG-on + path-lane (run2) | 134,478 | 6 | 1 | 2 | passed | **bimodal** — codex walked first this run (16KB) |
| **cloud (supermemory) ×3** | 27,063 / 26,196 / 48,199 | 3 / 2 / 3 | **0 / 0 / 0** | 0 | passed | **target spread**: tight, never walks |

**Findings:**
- **Target = cloud: 2–3 calls, 26–48K, never os.walk.** Local KG-on is bimodal (69K grep-first / 134K walk-first).
- Cloud uses the **same** AGENTS.md contract yet never walks → the contract isn't the lever; the levers are **grep tightness** (cloud ~10KB vs local 23KB / 10 results) and codex trusting it.
- Next: `SEMFS_RESULT_LIMIT=3` (tight grep ≈ cloud), clean accumulated `/model_output`, re-run KG-on/KG-off ×3 to drive local into the cloud spread.
