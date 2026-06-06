# Experiment Log — case 289 retrieval investigation

Legend: ✅ done · 🔄 in progress · ☐ planned/queued · 💡 possible (not yet scheduled)
Token numbers = E2E `semfs-codex` case 289 totalTokens. Baseline plain=143,837 · cloud=18,144.

## A. Completed experiments (E2E + probes)
| id | experiment | result | finding |
|---|---|---|---|
| ✅ E1 | plain codex | 143,837 tok | baseline |
| ✅ E2 | e5-small + sqlite + 50s timeout | **82,653 / 19 calls** | local best; "passes" via lucky `ls model_output/` (search FAILS) |
| ✅ E3 | Gemma-300M fp32 + sqlite | 87,216 / 18 | embedder not the lever |
| ✅ E4 | pglite + e5 | 89,928 / 17 | backend not the lever (≈ sqlite-e5) |
| ✅ E5 | cloud (Supermemory) | 18,144 / 4 | target; translates query + compact chunk returns + answer #1 |
| ✅ E6 | e5 + DOC_RETURN_CAP=4KB (no rewrite) | 370,799 | REFUTED — codex reads files when grep starved |
| ✅ E7 | Gemma + strengthened grep header | 133,756 | REFUTED — hints backfire |
| ✅ E8 | e5 + SEMFS_REWRITE (translate-rewrite) | 114,301 / 19 | retrieval FIXED (#417→#1) but tokens UP (bigger ZH-doc payloads) |
| ✅ E9 | e5 + rewrite + DOC_RETURN_CAP=6KB | 129,176 / 17 | REFUTED — cap→codex os.walk |
| ✅ E10 | e5 + rewrite + RESULT_LIMIT=4 | 135,670 | REFUTED — per-doc size dominates |
| ✅ E11 | e5 + rewrite + RETURN_MODE=snippet | 133,428 / 17 | grep payload shrank but codex os.walk anyway |
| ✅ E12 | cloud re-run (cloud3) trace capture | 26,595 / 3 cmds | confirmed shape: ls→grep(#1)→write; answer at clean #1 |
| ✅ P-RANK | RANKDUMP layer probe (e5, rewrite) | RRF #7/#8 → RERANK #1 → FINAL unstable | localizes dilution (RRF) + rescue (rerank) + demotion (L6/L7) |
| ✅ P-COV | coverage audit (fs_unindexed) | 595 indexed / **29 failed** | xlsx→Pdf mis-detect; OCR ran; 3 html-garbage |
| ✅ P-FTS | FTS/CJK empirical | 转化率/成交金额 match; 畅销/EN don't | BM25 verbatim-run only |

## B. In progress / doing now
| id | experiment | status |
|---|---|---|
| ✅ P0-a | Fix F5/F6 (salience/co-mention sign-inversion + `SEMFS_SALIENCE`/`SEMFS_COMENTION` off flags) | DONE — shipped to box binary (05:06) |
| ✅ P0-a-test | re-probe RANKDUMP with fix | **answer RRF#2→RERANK#0→FINAL#0** (was demoted pre-fix). Answer now stable FINAL #1/#2. |
| ✅ P0-a-ablate | `SEMFS_SALIENCE=off SEMFS_COMENTION=off` | also FINAL#0 — deterministic pure-rerank order; A/B confirms fix matches off |
| ✅ P0-a-e2e | E2E: rewrite + P0 ranking fix (salience on) | **112,592 / 11 cmds** — barely moved (vs rewrite-alone 114,301). Answer was FINAL#1 but codex STILL os.walk'd ×3 (empty profile.md) AND one grep returned **219KB whole-doc**. ⇒ P0-a necessary but NOT sufficient; token cost now = F7 (whole-doc payload) + F8 (os.walk). |
| ✅ P0-combo | rewrite + P0-a + RETURN_MODE=snippet (F7) | **97,778 / 8 cmds** — snippet shrank grep 219KB→30KB (saved ~15K), but codex STILL os.walk'd (#2, 18.9KB) + inspected files. Best rewrite-path so far, still > 82,653 baseline. |
| ✅ P0-b-v1 | local profile.md = directory map | BUILT but a STOPGAP. Marmik (correctly) flagged: a *semantic* FS needs a *semantic* overview, not a file tree. Design doc §B3 already says: back profile.md with a **Leiden community digest**, not a dir map. |
| 🐛 P0-b-bug | `warm_profile` gated behind `if !local_only` (`daemon_runtime.rs:404`) → **never ran on local mounts** → E2E profile.md was EMPTY (0 bytes at cmd #1). The 93.7K "p0all" had NO profile.md (the −4K was variance). | FIXED: `warm_profile(fetch_cloud)` always runs local-gen; cloud fetch only when !local_only. Verified profile.md=1834B on local mount. |
| ✅ P0-all2 | E2E: rewrite + P0-a + P0-b(active!) + snippet | **92,591 / 7 cmds** — profile.md ACTIVE (1834B) but codex **os.walk'd at cmd #1, BEFORE reading profile.md at cmd #4**. Dir-map profile.md = DEAD LEVER (agent walks first). Confirms doc thesis "can't instruct exploration away." Full P0 stack still ≫ 82K baseline & 35.7K May ideal. |

**KEY:** local runs CONSISTENTLY os.walk-first; cloud runs CONSISTENTLY ls+grep-first. Systematic, not variance.
The lever is codex's FIRST-MOVE choice (enumerate vs search) — driven by the mount/environment, not profile.md text.

## ⭐⭐ BREAKTHROUGH (cmplocal, full P0 stack) — 35,241 tokens (−75%!)
| run | tokens | cmds | first move | os.walk bytes |
|---|---:|---:|---|---:|
| p0all2 (same config) | 92,591 | 7 | **os.walk-first** | ~62K equiv |
| **cmplocal (same config)** | **35,241** | **4** | **`cat profile.md && grep`-first** | **1,053** |

SAME full-P0 config (rewrite+rank+profile+snippet), 2.6× token difference — **purely from codex's first move.**
When codex reads the (now-populated) profile.md + greps FIRST, it collapses to **cloud-level (matches May-27's 35,763
cloud ideal), −75% vs plain.** When it os.walk-first, 92K. **⇒ The first-move is THE lever, it's STOCHASTIC, and a
populated profile.md tilts codex toward grep-first (when it reads it).** This is the first local run to match cloud.
- Implication: reduce the variance / force grep-first → reliable −75%. Marmik's "ls-returns-KG / map-on-first-grep"
  idea (E24) directly targets this: ride the unavoidable first move.
- ⚠️ Harness finalization stalls on the post-run mount snapshot (codex completes + agent.json written, but
  snapshot_after_run hangs → narrative/archive skipped). archive_traces untested live; capture from case dir for now.

## B2. Earlier E2E (profile.md was EMPTY due to the bug — treat as no-profile)
| id | config | tokens | calls | note |
|---|---|---:|---:|---|
| ✅ p0fix | rewrite + P0-a rank | 112,592 | 11 | answer FINAL#1 but codex still os.walk×3 + 219KB grep |
| ✅ p0snip | rewrite + P0-a + snippet | 97,778 | 8 | snippet 219KB→30KB; still os.walk |
| ✅ p0all(bug) | + P0-b (but profile EMPTY) | 93,736 | 8 | profile.md=0B (bug); ~variance |

## B3. Design pivot: profile.md → graphify community digest (Marmik's ask)
- **Decision:** profile.md should be a **Leiden community digest** (god-node topic summary), per design doc §B3 — NOT a directory map. communities≈topics, god-nodes≈central concepts.
- **BLOCKER:** KG is too sparse — **only 21/595 files have edges**; 102 entity nodes, max degree 3; entities are office-supplies/people; **answer file has 0 entities**. A digest now = useless for case 289.
- **Prereq:** run **comprehensive L7 extraction** (LLM per file × 595) → rich KG → Leiden → digest. (E14 promoted.)
- **Caveat:** profile.md is a WEAK lever (cloud's was empty, still 4 calls; doc thesis = "lever is what the agent GETS BACK"). Keep dir-map as deterministic fallback when KG is sparse.

## B4. REGRESSION to investigate (high priority)
Design doc (2026-05-27) logged a **35,763-token (−75%)** semfs-codex run on case 289 (cat profile→1 grep→write,
3 calls). Current runs are 82–135K. **Same case, 9× worse.** What regressed? (cross-lingual without rewrite?
corpus/seed change? GPT-5.4 behavior? more files indexed → bigger crawl?) → **E21**.
| ☐ P0-b | populate local `profile.md` (tree + topic summary) → E2E (expect os.walk gone) | queued |

## C. Planned — the consistency matrix (Marmik's ask: same failure everywhere?)
Each cell: seed (verify 100% embed) → RANKDUMP L1→L7 → E2E (tokens + time). Run AFTER P0 so each gets its best shot.
| id | embedder | backend | notes / blockers |
|---|---|---|---|
| ☐ E-M1 | e5-small | sqlite | control (re-measure post-P0) |
| ☐ E-M2 | e5-small | pgvector | needs postgres+pgvector on box |
| ☐ E-M3 | e5-small | pgvector + HNSW | HNSW index after load |
| ☐ E-M4 | Gemma-300M int8 | sqlite | int8 ONNX (user-defined model); verify load |
| ☐ E-M5 | Gemma-300M int8 | pgvector | |
| ☐ E-M6 | Gemma-300M int8 | pgvector + HNSW | |
| ☐ E-M7 | Qwen3-0.6B (candle, last-token) | sqlite | `qwen3` feature build; ONNX was decoder dead-end |
| ☐ E-M8 | Qwen3-0.6B | pgvector | |
| ☐ E-M9 | Qwen3-0.6B | pgvector + HNSW | |
| ☐ E-M10 | BGE-M3 dense int8 | sqlite | user-defined ONNX |
| ☐ E-SPARSE | BGE-M3 **sparse lane** (replaces BM25) | sqlite | NEW LANE — big code; probed pure-sparse #8–11 |

## D. Cloud embedding models (Marmik considering)
| id | model | rationale |
|---|---|---|
| 💡 E-C1 | OpenAI text-embedding-3-large | strong multilingual; may fix F1 without rewrite |
| 💡 E-C2 | Cohere embed-multilingual-v3 | multilingual-first |
| 💡 E-C3 | Voyage voyage-3 / multilingual | strong retrieval benchmarks |
| 💡 E-C4 | Jina embeddings v3 (cloud) | multilingual + long-context |

## E. Coverage / extraction fixes (hard sub-goal: 100% embed)
| id | item | status |
|---|---|---|
| ☐ E-COV1 | Fix xlsx→Pdf mis-detection (sniffer) → re-seed → verify 0 in fs_unindexed | planned |
| ☐ E-COV2 | Handle the 5 PDF extraction fails (OCR fallback / pdf-extract) | planned |
| ☐ E-COV3 | Decide on 3 html-garbage files (exclude or re-hydrate) | planned |
| 💡 E-COV4 | URL-encoded-ZH filename handling | possible |

## F. Other possible experiments
| id | experiment | hypothesis |
|---|---|---|
| 💡 E13 | add e5 `query:`/`passage:` prefixes (F2) | lifts dense recall, may reduce rewrite reliance |
| 💡 E14 | Leiden community detection on `edges` KG → community boost / summaries | thematic/multi-hop; NOT P0 (see issue.md) |
| 💡 E15 | lane-weighted RRF (boost vec when other lanes cross-lingually dead) | counter F4 dilution |
| 💡 E16 | translate-rewrite to PURE target language (drop EN terms) | pure-ZH ranked #1 vs bilingual #4 in probe |
| 💡 E17 | rerank pool size / RERANK_CANDIDATES sweep | does answer survive into rerank reliably? |
| 💡 E18 | cohere/rerank-4-pro vs local jina | rerank quality (only matters once in pool) |
| 💡 E19 | disable access_count read-path bump entirely | full determinism (vs flag) |
| 💡 E20 | measure smoke-test wall-clock per config (mount + run) | the "time taken" column |
| ✅ E21 | **REGRESSION? — RESOLVED: NO regression.** The May-27 35.7K run used an ENGLISH query that only works if the backend translates EN→ZH (answer content is 100% Chinese) + profile from cloud `/v4/profile` ⇒ it was **CloudIndex (Supermemory)**. Local SQLite backend landed the SAME day (`51a99a5`, 2026-05-27) — it NEVER hit 35.7K. Cloud still achieves it (18–26K). The cross-lingual gap is a NEW-local-backend property, not a regression. | done |
| ☐ E22 | **graphify community-digest profile.md** (Leiden god-nodes) | the right B3 design; needs E14 (rich KG) first |
| ☐ E23 | comprehensive L7 extraction (all 595 files) → rich KG | prereq for E14/E22; currently 21/595 |
| ☐ E24 | **map-header on first grep** (ride the unavoidable move) — Marmik's "ls=KG" idea, POSIX-clean variant (c) | force grep-first / orient+answer in one call → reduce the 35K-vs-92K first-move variance. HIGH value. **→ full exploration in `tickets/ls-kg-semantic-readdir/` (3 approaches A/B/C + projected tool-call traces + ls_kg_exploration.html).** |
| ☐ E25 | repeat full-P0 ×5 to measure first-move variance distribution | how often grep-first (35K) vs walk-first (92K)? |
| 🐛 E26 | harness: post-run `snapshot_after_run` stalls on FUSE mount → finalization (narrative/archive) skipped | snapshot should exclude/timeout the mount, or unmount before snapshot |

## ⭐ CONTROLLED COMPARISON (cmplocal vs cmpcloud, both grep-first)
| metric | LOCAL (full P0) | CLOUD |
|---|---:|---:|
| tokens | **35,241** | **~26,598** |
| commands | 4 | 3 |
| os.walk | 1 (1KB) | 0 |
| grep | 1 | 1 |
| grep payload | 30,959 B | 10,741 B |
| first move | cat profile.md+grep | ls+profile→grep |

**Finding:** when local greps-first it's **competitive with cloud (35K vs 27K, ~1.3×)** — NOT 5×. Both 1 grep, grep-first.
Residual gap = (a) grep payload 31KB vs 11KB (local snippet still ~3× cloud's chunk return → tighten), (b) local did
a tiny extra os.walk + re-read the answer file (cloud's excerpt carried it). **Reliability is the real gap:** cloud
CONSISTENTLY grep-first (3–4 cmds); local BIMODAL (35K grep-first / 92K walk-first). → E24 (force grep-first) + tighten payload.
⚠️ Both runs: harness finalization stalled on mount snapshot (no agent.json/archive); recovered traces from case raw dir → /tmp/cmp{local,cloud}_trace/. E26 (fix snapshot stall) needed for archive_traces to work live.

## ⭐ THE case-289 lever (post-rewrite): the TRUST FIX (completeness annotation)
The 1M-token blowups (c1, g3) = codex **distrusting the grep excerpt → opening source files → format trap**
(20+ zipfile/xml/pandas cmds). Fix = per-hit **COMPLETE vs partial** signal on grep output (cloud has it
via `:1-10:` line ranges; local omits it). Changes (small, no re-seed): `SearchHit.complete` field;
`sqlite_vec.rs` computes it (snippet: file has 1 chunk; whole-doc: not truncated); `grep.rs` prints
`# <path> — COMPLETE (full file)` vs `# <path> — excerpt (file has N parts; open it for the rest)`.
Environmental FACT, not an imperative (the "USE IT AND STOP" escalation backfired +53% and was reverted).
Makes `os.walk` harmless (we do NOT suppress it) and preserves accuracy (agent opens file only for partial).
The KG is NOT this lever — see `tickets/ls-kg-semantic-readdir/` Scope Correction.

## Infra fixes
- ✅ **Trace loss FIXED** — `run_workspace_bench.sh` now has `archive_traces()`: after each run it copies every
  case's `agent.json` + `codex_stdout.jsonl` + `chat_adapter_log.jsonl` + `codex_invocation.json` into
  `_telemetry/<RUN_STAMP>-.../traces/<label>/`. Per-run traces now PERSIST (were overwritten before). Shipped to box.
- 🔄 **Controlled cloud-vs-local comparison** (cmplocal full-P0 vs cmpcloud) running with archiving — measuring
  tool calls, first-move, tokens, payload per run. Resolves whether "cloud greps-first / local walks-first" is
  real or stochastic.

## Notes
- (was) E2E command traces overwritten each run — now archived per-run (see Infra fixes).
- All seeds at `~/.semfs/<tag>.db` on EC2 (KEEP INTACT). New configs → new tags.
- 29-file embed failure must be fixed before the matrix is "fair" (some relevant files missing).
