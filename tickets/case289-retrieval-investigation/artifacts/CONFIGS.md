# Configurations to test — prioritized by the actual lever (case 289)

Ordering reflects the empirical finding: **the embedder is NOT the binding constraint** (e5/Gemma/Qwen3
all ≈83–87K E2E). The token win toward cloud's **18,144** lives in **ranking/fusion, the lexical lane,
payload, and data quality** — so those are P0; embedder/backend swaps are P1.

Baselines: plain 143,837 · e5-sqlite 82,653 · Gemma-sqlite 87,216 · **cloud 18,144 (#1, 4 calls)**.

## The axes
| axis | current | options to test |
|---|---|---|
| **lexical lane** | FTS5 **BM25** (exact-token) | **learned sparse: SPLADE / BGE-M3-sparse**, none |
| **fusion** | RRF, lane-additive sum | lane-weighted (boost vec), breadth-normalized, max-pool |
| **payload** | whole-doc, `DOC_RETURN_CAP=64KB`, `RESULT_LIMIT=10` (chunks) | 8KB cap / snippet-only / dedup-to-files / limit=20 |
| **data quality** | 403-HTML xlsx indexed as garbage; 1MB index cap | re-hydrate/exclude 403 files; raise/smarter cap |
| **query** | raw (codex-phrased); `--rewrite` in-language | translate-rewrite (EN→ZH terms); query-shortening |
| **dense embedder** | e5-small (384) / Gemma-300M (768) | BGE-M3 (1024, multilingual), Qwen3-0.6B (candle), arctic |
| **storage** | sqlite-vec | pglite (in progress), pgvector |
| **reranker** | local jina | cohere/rerank-4-pro, none |

## P0 — the real levers (embedder-/backend-agnostic; do these first)

| # | config | hypothesis | effort | builds on |
|---|---|---|---|---|
| **C1** | **Replace BM25 lane with learned sparse (BGE-M3-sparse or SPLADE)** | gives the cross-lingual answer a **2nd lane** + semantic lexical match EN→ZH → lifts it in lane-additive RRF | high (new sparse lane) | the root-cause finding |
| **C2** | **Lane-weighted / breadth-normalized RRF** | stop multi-lane distractors out-voting a strong single-lane vec hit; weight vec↑ when other lanes are cross-lingually dead | med | `rrf-chunk-mass-and-lane-fusion` |
| **C3** | **Whole-doc payload cap** (`DOC_RETURN_CAP` 64KB→8KB or snippet-return) | the first grep dumps ~120KB that re-replays in context; cap → big token cut even at same calls | low | gemma/e5 E2E traces |
| **C4** | **403-HTML re-hydrate/exclude** (`top10/apparel/problem_product .xlsx`) | removes the ~5 dead-end agent calls chasing corrupt files | low–med | `extraction-coverage-audit` |
| **C5** | **Translate-rewrite** (`--rewrite` emits target-language terms) | gives the answer a lexical (BM25/sparse) match → 2nd lane without re-seed | low | rewrite analysis |
| **C6** | **RESULT_LIMIT as distinct files / =20** | answer sits at chunk-#10 boundary; multi-chunk distractors eat the 10 chunk slots | low | gemma grep diag (#10) |

## P1 — embedder / backend (lower leverage per findings)

| # | config | note |
|---|---|---|
| C7 | **BGE-M3 int8 dense+sparse** (`onnx-community` int8) | one model for C1 + a strong multilingual dense lane; CLS pooling (fastembed-supported for dense); int8 for CPU speed. **Best single bet** — dense lane (≈ gemma/qwen quality, multilingual) + the learned-sparse lane that replaces dead BM25 (C1). Dense testable via user-defined ONNX now; sparse needs the new lane. |
| C8 | Qwen3-0.6B via **candle** (correct last-token) | proper test of the strong model (ONNX int8 was a decoder dead-end, ranked #14); slow on CPU |
| C9 | Gemma-300M **fp16** | speed only (≈fp32 ranking); needs external-data ONNX loading |
| C10 | **pglite + e5** | backend parity (IN PROGRESS) — expect ≈ sqlite-e5 82.7K |
| C11 | pgvector (external pg) | scale/concurrency; same ranking as sqlite/pglite |

## P2 — reranker (only after recall/fusion fixed)
| # | config | note |
|---|---|---|
| C12 | cohere/rerank-4-pro | useless until the answer is in the rerank pool at a good fusion rank; revisit after C1/C2 |

## Recommended sequence
1. **C3 + C4 + C6** (cheap, no re-seed) — quick token cuts on the existing Gemma/e5 seeds.
2. **C2** (lane-weighted RRF) — code, no re-seed; directly attacks the #10 fusion seat.
3. **C1 / C7** (sparse lane via BGE-M3) — the highest-upside structural fix; needs a new lane + re-seed.
4. **C5** translate-rewrite — cheap complement.
5. **C10/C11** backend parity; **C8** Qwen3-candle; **C12** rerank — last.

## Method (per config, fair vs baselines)
New tag; verify embedder/lane stamp; grep-gate verbatim cloud+local queries; E2E `semfs-codex` 289
(`--no-push --no-sync`, default knobs unless the config IS a knob); log tokens/calls/answer into
`cloud_env_state.md`. **Keep all existing seeds intact.**

## Verbatim queries
- CLOUD: `best-selling product data file top10 product title transaction amount conversion rate`
- LOCAL: `best-selling product data file title transaction amount conversion rate`

## MASTER E2E RESULTS TABLE (codex case 289) — goal: fill every testable row
| id | config | E2E tokens | tool events | answer | status | notes |
|---|---|---:|---:|:--:|:--:|---|
| — | plain codex | 143,837 | — | — | ✅ | baseline |
| — | e5-small sqlite 50s | 82,653 | 19 | passed | ✅ | baseline |
| — | Gemma-300M fp32 sqlite | 87,216 | 18 | passed | ✅ | no win vs e5 |
| — | cloud (Supermemory) | 18,144 | 4 | passed | ✅ | target |
| C10 | pglite + e5 | **89,928** | 17 | passed | ✅ | **backend parity** w/ sqlite-e5 (82,653) — storage backend is NOT a token lever |
| C6 | e5/Gemma + RESULT_LIMIT=20 | — | — | — | ☐ | no code; on existing seed |
| C3 | + DOC_RETURN_CAP=4KB | **370,799** | 36 | passed | ❌ REFUTED | capping whole-doc return → codex READS files instead → 4.5× WORSE. Whole-doc return is load-bearing (replaces reads). Keep 64KB. Token cost = call count (ranking), NOT payload. |
| C13 | Gemma + strengthened grep header ("USE IT AND STOP, never read whole files") | **133,756** | 15 | passed | ❌ REFUTED | header backfired: 133K vs Gemma baseline 87K (+53%). Fewer tool events (15 vs 18) but FAR more tokens → the "never read whole files / use top result" instruction did NOT curb token use; if anything codex pulled bigger payloads or longer reasoning. Prompt-side nudging is NOT the lever. Confirms: the lever is RANK QUALITY (get answer to confident #1), not instructions. |

## ⭐ ROOT CAUSE FOUND (2026-06-06) — it's a CROSS-LINGUAL L1 RECALL MISS, not embedder/backend/fusion
Verbatim probe (mounted e5 daemon + RANKDUMP): the answer file **never enters any lane's top-80 pool**.
Content is 100% **Chinese** (`成交金额`=transaction amount, `转化率`=conversion rate); query is 100% English →
BM25 lexical lane is **dead** (0 EN terms in file) and e5's EN→ZH dense match is too weak.
Full-corpus e5 vec rank of answer (gemma_corpus, /592 files):

| query language | answer rank |
|---|---:|
| English (EN-LOCAL) | **#417** |
| English (EN-CLOUD) | #384 |
| Chinese (ZH2) | **#1** ⭐ |
| Chinese (ZH) | #2 |
| Bilingual EN+ZH | #4 |

→ **Translating the query swings #417→#1.** Cross-lingual translation IS the lever. Cloud wins (18K/4 calls)
because Supermemory expands/translates. RCA: `rcas/2026-06-06-cross-lingual-recall-miss-case289.md`.
**This supersedes the embedder/backend permutation matrix** — those rows all share a dead lexical lane +
EN→ZH dense gap, so they cannot move the needle. The real fixes: C5 (translate-rewrite, target-language) and
index-side bilingual/summary augmentation.
| C5 | **e5 + SEMFS_REWRITE=1** (translate-rewrite, target-language ZH terms) | **114,301** | 5 grep calls | passed | ⚠️ MIXED | rewrite FIRED (nano appended `销售额 转化率 畅销产品数据文件标题`); answer went absent→**rerank #1 / final top-3** (proven via RANKDUMP probe). codex even adopted ZH queries itself. promptTokens=112,412 (whole-doc payload), completion=1,889. NOT a token win alone because the answer being retrieved means 5 grep calls each dump ~22K whole-doc payload. **But it makes payload-cap SAFE** (answer no longer needs file-reads) → unlocks C3+C5 combo. |
| C5+C3 | **e5 + REWRITE + DOC_RETURN_CAP=6KB** | **129,176** | 19 trace | passed | ❌ REFUTED | cap made it WORSE than rewrite-alone (129K vs 114K). Smaller payload → less info/call → codex searched MORE (19 events vs 5). Byte-capping whole-docs backfires in BOTH regimes (with/without rewrite). |
| C6 | **e5 + REWRITE + RESULT_LIMIT=4** | **135,670** | 9 cmds | passed | ❌ REFUTED | even worse. Each grep STILL returned ~75KB because the 4 top docs are LARGE Chinese files (answer .txt is 468B but its top-4 neighbors are big dashboards/reports). RESULT_LIMIT doesn't help — **per-doc size dominates, not count**. Confirms the wall: WHOLE-DOC return of large docs. |
| — | **CONCLUSION** | — | — | — | 🧱 | All payload knobs (cap, limit) fail. The architecture returns WHOLE DOCS; cloud returns matching CHUNKS. Reaching cloud's 18K needs **chunk/snippet-return** (return the matching passage + small context, not the whole doc), combined with rewrite (puts the right chunk at #1). That's the real lever. |
| C4 | seed w/o 403-xlsx | — | — | — | ☐ | re-seed |
| C2 | lane-weighted RRF | — | — | — | ☐ | code |
| C7 | BGE-M3 int8 dense | — | — | — | ☐ | user-defined ONNX |
| C1/C7s | BGE-M3 sparse lane (replaces BM25) | — | — | — | ☐ | new lane (big) |
| C8 | Qwen3-0.6B candle | — | — | — | ☐ | qwen3 feature (big) |
| C9 | Gemma fp16 | — | — | — | ☐ | external-data ONNX |
| C11 | pgvector | — | — | — | ☐ | external pg |
| C12 | + cohere/rerank-4-pro | — | — | — | ☐ | after C1/C2 |

**Scope note:** several rows (C1 sparse lane, C2 fusion, C8 candle, C9 external-data) require real code,
not just a re-run — so this table fills over multiple sessions. Cheap/no-code rows (C6, pglite) go first.
