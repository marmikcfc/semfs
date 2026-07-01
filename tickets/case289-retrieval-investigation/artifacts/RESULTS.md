# Case 289 token optimization — RESULTS (2026-06-06)

Goal: minimize codex+semfs tokens vs plain codex (143,837) on Workspace-Bench case 289
("best-selling product", chanpin persona, codex GPT-5.4). Cloud target: 18,144.

## TL;DR
- **Root cause of poor local retrieval = cross-lingual gap** (English query vs 100%-Chinese answer file).
  Proven; fixed with an L4 translate-rewrite (`SEMFS_REWRITE=1`): answer rank **#417 → #1**.
- **But fixing retrieval did not cut tokens.** The local↔cloud token gap is **codex's exploration count
  (17–19 tool calls local vs 4 cloud)**, not embedder/backend/fusion/return-format.
- **Best local = 82,653 (−43%)** (pre-existing e5+50s-timeout). Every config I tried on top is worse.
  Embedder×backend permutations are moot for tokens (all share the codex-exploration regime).

## A. End-to-end token table (case 289, codex GPT-5.4)
| config | tokens | vs plain | tool calls | time (ms) | status |
|---|---:|---:|---:|---:|:--:|
| cloud (Supermemory) | **18,144** | −87% | 4 | ~168k* | ✅ target |
| **e5-small sqlite, 50s timeout** | **82,653** | **−43%** | 19 | ~80k | ✅ best local |
| Gemma-300M fp32 sqlite | 87,216 | −39% | 18 | ~80k | ✅ |
| pglite + e5 | 89,928 | −37% | 17 | — | ✅ backend parity |
| e5 + `SEMFS_REWRITE` | 114,301 | −21% | 19 | ~94k | ⚠️ retrieval fixed, tokens up |
| e5 + rewrite + cap 6KB | 129,176 | −10% | 17 | — | ❌ |
| e5 + rewrite + snippet | 133,428 | −7% | 17 | ~? | ❌ |
| Gemma + strong grep-header | 133,756 | −7% | 15 | — | ❌ |
| e5 + rewrite + limit 4 | 135,670 | −6% | — | — | ❌ |
| plain codex | 143,837 | — | — | ~? | baseline |
| e5 + cap 4KB (no rewrite) | 370,799 | +158% | 36 | ~291k | ❌ codex reads files |

\* cloud durationMs dominated by slow cloud-container FUSE mount, not search.

## B. L1→L7 stage progression of the answer file (verbatim LOCAL query, e5 seed)
Answer = `best_selling_product_core_data_list.txt` (468 chars, 100% Chinese, 1 chunk; 2 copies in seed).
Measured via mounted-daemon RANKDUMP (`SEMFS_DEBUG_RANKING=1`). Rank = position of the answer file.

| layer | what it does | English query (no rewrite) | rewrite (EN+ZH terms) |
|---|---|---|---|
| **L1 dense (vec KNN, top-80)** | e5 cosine over `vchunks` | **#417 full-corpus → NOT in top-80 pool** | in pool |
| **L1 lexical (FTS5/BM25, top-80)** | exact-token match | **dead** (0 EN terms exist in the ZH file) | ZH tokens present → matches |
| **L1 code (KNN, top-80)** | code-lane | N/A (.txt) | N/A |
| **L4 query rewrite** | LLM expand/translate | (off) | appends `销售额 转化率 畅销产品数据文件标题` |
| **RRF fusion** | lane-additive 1/(K+rank) | **absent** (not in top-57) | **#7 / #9** (both copies) |
| **L5 cross-encoder rerank** | jina rerank top-50 | **absent** (not in top-50) | **#1 (rank 0) / #2** ⭐ |
| **L6 salience** | recency/access ×factor | — | reorders (demotes answer below 1 distractor) |
| **L7 entity-graph** | indexing-side edges | n/a at query time | n/a |
| **phase-2 / final return** | revalidate + return top-N | **not in returned 50** (codex finds via `ls`) | **final #2 / #3** |

Pure-vector full-corpus rank by query language (gemma_corpus, 592 files): EN-LOCAL **#417**, EN-CLOUD #384,
ZH **#2**, ZH2 **#1**, bilingual #4. → translating the query swings #417→#1.

## C. Why fixing retrieval didn't help tokens (the real constraint)
- cloud = **4** tool calls; every local config = **17–19**. Tokens ≈ calls × payload.
- The rewrite fixed RANK but codex still made 19 calls; successful ZH retrieval returns *larger* docs.
- Payload knobs backfire: cap/limit/snippet shrink grep output, but codex compensates with a 62KB
  `os.walk` of the mounted corpus, which re-replays across turns.
- ⇒ The binding constraint is codex's exploration on the local mount, not semfs retrieval/return tuning.

## D. Shipped changes (code)
- `crates/semfs-core/src/llm.rs` — `rewrite_query` prompt now emits target-language terms (multilingual).
- `crates/semfs/src/cmd/grep.rs` — `SEMFS_REWRITE` env auto-enables rewrite (codex calls plain `grep`).
- `crates/semfs-core/src/backend/sqlite_vec.rs` — `SEMFS_RETURN_MODE=snippet` returns the matched
  chunk(s) instead of the whole document (cloud-style compact returns).
Keep `SEMFS_REWRITE` on for correctness: local search now *finds* the answer instead of relying on the
predictable-output-path `ls` fallback (which is what makes the 82,653 baseline "pass" despite a failed search).

## E. Recommended next lever (design discussion, not a knob)
Close the call-count gap (19→4): understand why the cloud mount yields 4 calls (fewer browsable files?
more authoritative single hit?) and replicate that. A tree-walk-discouraging mount hint is plausible but
risky (grep-header variant was 133,756 — hints can backfire). This is codex-behavior / mount-presentation
work, distinct from the retrieval/return tuning explored here.
