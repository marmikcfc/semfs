# Case 289 — Retrieval Investigation (consolidated)

**Status:** active · **Opened:** 2026-06-06 · **Owner:** Marmik + Claude
**Case:** Workspace-Bench 289 "best-selling product" · chanpin persona · codex GPT-5.4
**Baselines:** plain codex **143,837** · local best (e5, no rewrite) **82,653 (−43%)** · cloud **18,144 (−87%, 4 calls)**

## Why this ticket exists (clarified intent)
The goal is NOT "embedder/backend is the token lever" — it has been *proven not to be* (all embedders/
backends land ~82–135K; the token driver is ranking trust + codex exploration). The matrix is run as a
**consistency probe**:

1. **Is the failure the SAME across every embedder × backend?** If yes → the bug is in the shared
   pipeline (RRF / rerank / L6-L7 / return / agent loop), not the embedder. If a config behaves
   DIFFERENTLY → that delta localizes the cause.
2. **Where exactly does each config's answer land, L1→L7?** Per-config layer tables expose whether the
   dilution point moves.
3. **Cloud embedding models** are on the table — strong multilingual alignment (OpenAI/Cohere/Voyage)
   could fix the cross-lingual recall miss (F1) without query rewrite. To be tested.
4. **100% embedding coverage** is a hard sub-goal: every file must embed; fix any failure.

## Root cause so far (proven)
Two compounding causes; see `artifacts/2026-06-06-cross-lingual-recall-miss-case289.md` +
`artifacts/case289_deep_analysis.html` for full evidence.

- **Retrieval:** English query vs 100%-Chinese answer file → answer ranks #417 (EN) vs #1 (ZH) in pure
  vector; BM25 dead for EN→ZH. Fixed for *recall* by `SEMFS_REWRITE` (translate-rewrite).
- **Ranking trust (the token driver):** even when retrieval finds it, two bugs make local's #1
  untrustworthy/unstable, so codex doesn't trust the top hit and brute-explores (62KB os.walk + pandas) →
  17–19 tool calls vs cloud's 3–4.

## Points of failure (severity)
| id | layer | failure | sev |
|---|---|---|---|
| F1 | L1 dense | cross-lingual gap (e5 EN→ZH weak; #417) | critical |
| F2 | L1 dense | e5 `query:`/`passage:` prefixes NOT applied (`embed/local.rs:95`) | high |
| F3 | L1 lexical | BM25/unicode61 no CJK segmentation — verbatim-run match only | high |
| F4 | RRF | single-lane dilution: cross-lingual answer out-voted → #7/#8 | high |
| F5 | L6/L7 | **multiplicative boost on NEGATIVE rerank scores INVERTS** (`rank.rs:193,206`) → demotes best hit | critical |
| F6 | L6 | `access_count` bumped every search (`sqlite_vec.rs:1094`) feeds salience → non-deterministic order | critical |
| F7 | return | whole-doc return floods context on large-doc corpus | high |
| F8 | agent | codex compensates with 62KB os.walk + inspections → 17–19 calls | critical |
| F9 | L0 extract | 29 files failed to embed — xlsx mis-detected as `format=Pdf` | med |
| F10 | L0 extract | 3 HTML-garbage files indexed (403 pages saved as .xlsx) | low |
| F11 | grader | existence-only grading masks F1–F8 | context |

## P0 FIXES (the actual token levers)
**P0-a — Fix F5/F6 (ranking trust).** This is the keystone: rerank already puts the answer #1; L6/L7 then
demote it via the negative-score multiplication bug, and access_count drift makes it non-deterministic.
- Sign-correct the nudge: `score *= factor` → `score *= (factor if score>=0 else 1/factor)` in BOTH
  `apply_salience` and `apply_comention_boost` (so a boost always raises rank, both signs).
- Add **`SEMFS_SALIENCE=off`** (and `SEMFS_COMENTION=off`) to disable the post-rerank boosts entirely —
  the A/B switch Marmik asked for: if the sign-fix doesn't fully stabilize, run with salience OFF and
  compare. OFF ⇒ deterministic (rerank order preserved).

**P0-b — Populate local `profile.md`.** Kills F8's 62KB os.walk by giving codex the tree + a topic summary
up front (currently empty; it's cloud-`/v4/profile`-backed and that container has no memories).

## P1 / P2
- P1: keep `SEMFS_REWRITE` (shipped); add e5 prefixes (F2); test cloud embedding models (F1).
- P2: fix xlsx→Pdf mis-detection for 100% coverage (F9); `SEMFS_RETURN_MODE=snippet` (shipped, pair w/ P0).

## Open questions Marmik raised
**Q: Where is the knowledge graph stored?**
A: In the **`edges` table of the local SQLite cache** (`~/.semfs/<tag>.db`). L7 graph extraction
(`graph_llm` + `graph_queue` in `SqliteVecStore`) runs an LLM per file to extract entities and writes
`from_path → to_path` (file↔entity / file↔file) edges. `apply_comention_boost` reads it
(`SELECT to_path FROM edges WHERE from_path=?`). So the KG is local, per-container, backend-specific
(SQLite today; pgvector has no code/graph lane — single-embedder).

**Q: Would Leiden community detection help?**
A: Possibly for *thematic / multi-hop* retrieval (GraphRAG-style: detect communities → boost docs in the
same community as a strong hit, or attach community summaries to profile.md). BUT it is **not a P0 fix for
case 289**: the answer is a single terse file with few entity links; the failure is the multiplicative-
salience bug + cross-lingual recall, not graph structure. Leiden also presupposes a dense, correct edge
graph — which we haven't validated. Park it as a P3 experiment (E14 below), revisit after P0/P1 land.

## Sub-tickets
- **`tickets/ls-kg-semantic-readdir/`** — "ls → KG" semantic-orientation exploration (Marmik's idea). The
  data shows the first-move (grep-first vs os.walk-first) is THE remaining lever; this sub-ticket works out
  how to make the agent's reflexive move return semantic relevance. 3 POSIX-clean approaches (A: profile.md
  Leiden digest, B: annotated `ls`, C: map-header on first grep ⭐), projected tool-call traces, and
  `ls_kg_exploration.html`. Cross-ref: E24/E25 here.

## Verification gate (definition of done)
For the *best* config: answer at stable FINAL #1 (RANKDUMP), codex ≤ ~6 tool calls, tokens materially
below 82,653 and ideally approaching cloud's 18,144; 100% files embedded; per-config L1→L7 + time + token
table filled. See `EXPERIMENTS.md` for the full matrix and status.
