# xafs PPR A/B — get xafs ready to test `ppr_off` / `ppr_on` / `ppr_map`

_Created 2026-06-29. Folder `tickets/wb-xafs-ppr-ab/`. Linear: SEM-47 (SemFS). Companion to
`../wblite-ppr-ab/EXPERIMENT.md` (the WB-Lite PPR A/B this extends to the xafs corpus)._

> One sentence: stand up the **xafs** corpus (19,170 files, English, zero code) as a benchmark
> persona and run the three hidden-KG graph-prior arms — `ppr_off` (1-hop control), `ppr_on`
> (Personalized PageRank), `ppr_map` (PPR + cached workspace map) — head-to-head on **E2B**, then
> judge with Seed-2.0-Lite. The seed is ready; this ticket is the **plumbing** (stage → wire →
> smoke → run → judge).

---

## What "the test" is (arm definitions — from `benchmarks/e2b/run_matrix.py`)

All three arms are **MOUNT_ARMS + SURFACE_OFF_ARMS**: a live semfs FUSE daemon with the `/kg/`
surface hidden; the agent uses `semfs grep`, and the hidden KG re-ranks the hits. They are
**identical to `hiddenkg_l7`** (hidden KG + co-mention, no surface) except for the graph-prior
algorithm:

| arm | `SEMFS_KG_PPR` | extra | isolates |
|---|---|---|---|
| `ppr_off` | `off` | 1-hop bounded neighbour boost (control) | the baseline prior |
| `ppr_on`  | `on`  | in-memory PPR diffusion (`SEMFS_PPR_RESTART=0.5`, `SEMFS_PPR_ITERS=30`) | 1-hop → multi-hop |
| `ppr_map` | `on`  | + cached `workspace_map.txt` injected into the prompt (built once/sandbox by `semfs_map.py`, read by `cell_driver` as `WB_WORKSPACE_MAP`) | map-vs-no-map vs `ppr_on` |

Shared arm env: `SEMFS_KG=off`, `SEMFS_COMENTION=on`, `SEMFS_HIDDEN_KG=on`,
`SEMFS_HIDDEN_KG_RETRIEVAL=off`, surface off.

---

## Confirmed seed state — `xafs-gemma-q4.db` (Modal `semfs-bench-data:/seeds/`, inspected 2026-06-29)

`SEMFS_SEED_ONLY=1 modal run benchmarks/modal/semfs_modal.py::inspect_seed_tables --seed xafs-gemma-q4.db`

| table | count | gates | ✅ |
|---|---|---|---|
| chunks / vchunks_rowids | 615,950 / 615,950 | search | ✅ (`vchunks` errors only b/c inspect container lacks `vec0`) |
| graph_entity / graph_relation | 57,585 / 134,616 | **hidden-KG prior (PPR substrate)** | ✅ |
| graph_community / graph_god_node | 18,284 / 312 | community surface | ✅ |
| fs_dentry / fs_inode / fs_data | 23,394 / 23,395 / **226,804** | **mountable FUSE tree** | ✅ |
| push_queue | 18,284 | cloud push | ✅ |
| distinct_files / chunks_per_file | 19,170 / 32.13 | corpus size | ✅ |
| dup_groups / code_lane_corrupt_stamp | 0 / false | contamination / corruption | ✅ clean |
| fs_config | `byo:gemma-q4-onnx:768` | embedder match (the E2B mount must use gemma-q4) | ✅ |

**The seed satisfies every requirement of the three arms** (mountable + hidden KG + clean).

> ⚠️ **Doc-correction:** the `2026-06-25` gotcha in `../wb-xafs-seeds-e2b-ready/README.md`
> ("xafs search-only, `fs_data=0`, `push_queue=0`, not mountable") and the "78 coarse communities"
> caveat are **STALE** — the live seed is materialized (`fs_data=226,804`, 23K-node tree,
> 18,284 communities). The `--phase fs` materialize was evidently run after that note.

---

## Why this is plumbing, not seed work — the 3 real blockers

1. **Seed is Modal-only.** It must reach E2B (the HARD RULE: semfs benchmarks run on E2B real-FUSE,
   never Modal — `memory/all-benchmark-tests-on-e2b.md`). xafs seed is ~GBs (615K chunks) → **runtime-pull**,
   not baked (same call as `research`). No `semfs-mount-xafs` template exists yet.
2. **xafs cases are a different benchmark + a different judge format.** xafs has **13 cases**
   (`dp_001`–`dp_013`) from the **`supermemory/xAFS`** HF dataset (file-based `snapshot_download`,
   NOT Parquet) — fetch script already exists: `benchmarks/modal/xafs_wb_embed.py::phase1_xafs`
   (`download_xafs()`), which lays out `/data/corpus/xafs/dp_XXX/` + `tasks.json`. Each case is
   `question.json = {question, answer, …}` + `data/**` — a **single gold-answer QA** format.
   ⚠️ This is **not** the WB-Lite multi-rubric `metadata.json` that `run_matrix.py`/`run_judge.py`
   expect, so the WB rubric judge is **not directly reusable**: xafs needs an **answer-correctness**
   judge (LLM-as-judge comparing the agent's final answer to the gold `answer`). No xafs judge exists
   in the repo today — it must be adapted.
3. **Harness wiring.** `run_matrix.py` is WB-Lite-wired (chanpin case IDs hard-coded in `CASES_FULL`).
   Need a persona path: `WB_PERSONA=xafs`, xafs case list, `WB_E2B_SEED_DEFAULT=<xafs seed>`,
   `WB_SEARCH_ONLY` decision (the seed HAS an fs tree → can run full-tree, not search-only).

---

## Build progress (2026-06-29)

| piece | status | evidence |
|---|---|---|
| Seed validated (search) | ✅ | Phase 0 `grep_seed` → rc=0, ranked hits (dp_010 Nova, dp_011 Citizen Sentinel) |
| Cases enumerated | ✅ | `supermemory/xAFS` → 13 workspaces, **110 questions** (single 33 / multi 51 / format 26) |
| Judge model route | ✅ | OpenRouter serves `google/gemini-3.1-pro-preview` (verified via models API) |
| **`run_judge_xafs.py`** | ✅ **DONE** | 9/9 unit tests (`benchmarks/e2b/tests/test_run_judge_xafs.py`) + **live smoke**: real Gemini judge scored "2034 dollars"≡"$2,034" CORRECT, "$3,500" INCORRECT, tokens recorded |
| Seed → E2B (bake) | ✅ DONE | `semfs-mount-xafs` baked (5.9 GB seed + 220 MB corpus, `tasks.json` excluded; build 1m44s, fits the measured 11 GB sandbox). `bake_e2b_persona.py` extended w/ `corpus_path`/`corpus_arcname`/`exclude_names`. |
| **FUSE smoke (Nemotron :free)** | ✅ **PASS** | `smoke_xafs_nemotron.py`: boot → mount (dp_001..dp_010 tree) → grep 13.9 KB → Nemotron `nvidia/nemotron-3-ultra-550b-a55b:free` → **"$2,034"** → Gemini judge **correct=True**. Validates mount + PPR retrieval + agent + judge end-to-end. |
| Answer-capture (run_matrix) | ◑ PLANNED | candidate_answer = agent's final answer; emit cells JSONL `{dp,qid,arm,rep,candidate_answer,agent_tokens}` for the 3-arm 110-Q run. |

> **GOTCHA (smoke debug):** the baked seed lands in **root-owned `/opt`**; semfs opens it **RW**
> (access-tracking + WAL/journal sidecars in the seed's dir) and on failure **silently falls back to
> the cloud backend → 401 → 0 results** (no loud error). Fix: **move** (instant rename, no copy) the
> seed to user-owned `~/.semfs/` before mount/grep. Env must match `SEMFS_ENV` (`SEARCH_ONLY=off` +
> `SEMFS_RESULT_LIMIT`/`GREP_*_CAP`); mount needs `--startup-timeout 180` for the 5.9 GB seed.

**Judge usage:** `python3 run_judge_xafs.py --cells <run>.jsonl --cases-dir <xafs> --out judged.jsonl`
(model overridable via `XAFS_JUDGE_MODEL`; resume-safe; prints overall + per-arm accuracy & tokens-per-correct).

## Steps to results

**Phase 0 — validate the seed supports the arms (cheap, Modal-side) [START HERE]**
- [ ] `grep_seed` smoke on `xafs-gemma-q4.db` with `SEMFS_HIDDEN_KG=on SEMFS_KG_PPR=on` → confirm
      `semfs grep` returns ranked hits and the PPR prior re-ranks (sanity gate before any GB staging).
  → verify: non-empty ranked hits + ranking-debug shows the hidden-KG/PPR boost firing.

**Phase 1 — stage the seed to E2B**
- [ ] Export `xafs-gemma-q4.db` from the Modal volume → Drive `semfs/experiments/` (large binary;
      link from this Linear issue). WAL-checkpoint before export.
- [ ] Runtime-pull path into the E2B box / mount template (mirror the `research` runtime-pull, since
      both are too big to bake). `--startup-timeout` raised (large materialized seed → daemon
      `configuring_api` >30s; houqin needed this).
  → verify: seed file present in sandbox at `WB_E2B_SEED_DEFAULT` path; `seed_exists` passes.

**Phase 2 — stage the xafs cases + build the Supermemory-faithful answer-judge**

_Judging is locked to **Supermemory's own method** (their dataset card + SMFS writeup):_
- _**Contract:** the agent emits a **final answer (text)** = `candidate_answer`. No deliverable files
  (xAFS agents don't write files — they answer). → capture the agent's final transcript answer._
- _**Judge:** LLM-as-judge, **semantic match** (paraphrase- & format-tolerant), scoring
  `(prompt, gold_answer, candidate_answer)`. Supermemory used **Gemini 3.1 Pro Preview @ temp 0**;
  match it (else justify the substitute). NOT exact-match, NOT the WB rubric judge._
- _**Metric:** **tokens per correct answer** (their headline) + accuracy — the semfs thesis exactly._
- _`question.json` schema (confirmed against the live dataset): **an ARRAY of question objects**
  per workspace (dp_001 has 9: q01–q09), each `{id, family (single_hop|multi_hop|format_spanning),
  prompt, gold_file_ids[], gold_answer}`. No embedded judging fields → external judge._
- _**Unit = workspace × question**, NOT 13. "13 cases" = 13 workspaces; total QA items = dozens
  (~9/workspace). A cell = (dp_XXX, qNN, arm, rep). Enumerate all questions, not just 13._

- [ ] Fetch the 13 cases from `supermemory/xAFS` (scripted: `xafs_wb_embed.py::phase1_xafs`).
      Likely already on the Modal volume at `/data/corpus/xafs/dp_XXX/`; pull `question.json` to
      confirm the schema above.
- [ ] Adapt `run_matrix.py` so the cell captures the agent's **final answer** (not deliverable
      collection) per case, recording tokens.
- [ ] Write `run_judge_xafs.py`: LLM semantic-match grader over `(prompt, gold_answer, candidate_answer)`
      → correct/incorrect, judge = Gemini 3.1 Pro Preview (temp 0). Report tokens-per-correct.
  → verify: grader scores a known-correct + a known-wrong answer right on one case; tokens recorded.

**Phase 3 — wire + FUSE smoke**
- [ ] `WB_PERSONA=xafs`, xafs case list, `WB_E2B_SEED_DEFAULT=<xafs seed>`, embedder=gemma-q4.
      Decide `WB_SEARCH_ONLY` (default off — the seed has a real fs tree).
- [ ] One-case FUSE smoke: mount → `semfs grep` → hidden-KG/PPR re-rank → deliverable written.
  → verify: one cell of one arm produces a non-empty deliverable + the mount serves the tree.

**Phase 4 — run the 3-arm matrix on E2B**
- [ ] `run_matrix.py` queue mode, arms `ppr_off,ppr_on,ppr_map`, n≥2, all 13 cases.
  → verify: `results.jsonl` + per-cell deliverables land for all 13×3×n cells.

**Phase 5 — judge + analyze**
- [ ] `run_judge_xafs.py` (Gemini 3.1 Pro Preview, temp 0, semantic match) over all cells — NOT the WB rubric judge.
- [ ] Final table: **accuracy AND tokens** for `ppr_off` vs `ppr_on` vs `ppr_map`; mine actual
      responses (analyze-benchmark-results discipline).
  → verify: every cell has a `rubrics_judge--seed-2.0-lite-judge.json`; report raw + artifact-zero-excluded.

---

## RESULTS v2 — CROSSOVER: KG arms win at scale; `ppr_off` overall winner (2026-07-01, FINAL 49/52)

Agent = **codex / gpt-5.4-mini**, judge = **Gemini 3.1 Pro**. Full 4-arm × 13-persona matrix (q01 per
persona = 13 of 110 Q). **49/52 cells — dp_013 (9,988 files) DEFERRED** (GPU stopped for cost; `build_kg`
doesn't resume incrementally + preemptible caller made it flaky, and the result is already decisive).

> **The v1 headline "plain wins" was a SMALL-WORKSPACE ARTIFACT.** It only tested vector-search (no KG)
> on the first few cells. With the KG/PPR arms built and the big workspaces run, the picture inverts.

### The matrix (✓/✗ + agent tokens)

| dp | files | plain | ppr_on | ppr_map | ppr_off |
|---|---|---|---|---|---|
| dp_001 | 5 | ✓92K | ✓50K | ✓36K | ✓50K |
| dp_002 | 10 | ✓69K | ✓75K | ✓45K | ✓83K |
| dp_003 | 20 | ✓103K | ✓175K | ✓165K | ✓68K |
| dp_004 | 30 | ✓98K | ✓141K | ✗327K | ✓86K |
| dp_005 | 50 | ✗101K | ✗678K | ✗2378K | ✗900K |
| dp_006 | 100 | ✓86K | ✓46K | ✓847K | ✓62K |
| dp_007 | 200 | ✓129K | ✓117K | ✓185K | ✓128K |
| dp_008 | 299 | ✓149K | ✓82K | ✓121K | ✓80K |
| dp_009 | 480 | ✗180K | ✗228K | ✗168K | ✗185K |
| dp_010 | 991 | ✗268K | ✗298K | ✗495K | ✗518K |
| **dp_011** | **1998** | **✗279K** | **✗101K** | **✓71K** | **✓82K** |
| **dp_012** | **4998** | **✗410K** | **✗312K** | **✓81K** | **✓88K** |
| dp_013 | 9988 | ✗261K | · | · | · |
| **totals (common 12)** | | **7/12 · 319K/✓** | **7/12 · 330K/✓** | **8/12 · 616K/✓** | **9/12 · 259K/✓** |

### Findings v2
1. **`ppr_off` is the overall winner** — 9/12 correct, best tokens-per-correct (259K). Hidden KG + 1-hop
   neighbours beats everything on accuracy AND efficiency.
2. **Crossover at scale** — on big workspaces (dp_011, dp_012) plain ✗✗ and ppr_on ✗✗ (grep drowns in
   thousands of files, 300–400K tok, wrong), but **ppr_map ✓✓ and ppr_off ✓✓ — correct AND ~4× cheaper**
   (71–88K). The KG lets the agent jump straight to the answer file. This is semfs's real value case.
3. **PPR diffusion does NOT help** — `ppr_on` (7/12) ≈ plain, and LOSES to the 1-hop control `ppr_off`
   (9/12). The graph *prior* helps; multi-hop PageRank *diffusion* over it does not.
4. **`ppr_map` accurate but expensive** — 8/12 but 616K/correct (dp_005 2.38M + dp_006 847K blowups). The
   injected map helps navigation at scale but can send the agent chasing.
5. **Small workspaces** (dp_001–008): everyone mostly right, plain/ppr_off cheapest. **dp_005 pathological
   for ALL arms** (memo-noise + Zelle-vs-Venmo). dp_009/010 wrong for all (genuinely hard).

**Caveat:** q01-only slice (13 of 110 questions) — directional pattern, not full per-persona accuracy.

**Bugs caught + fixed (RCAs 2026-07-01):** (1) `semfs_map.py` budget capped only KG clusters, not the DIR
skeleton → a 3,306-dir map (~100K tok) crashed codex (0-call/30s) → fixed: cap dirs top-40, keep clusters;
(2) `finish_dp013` gate (`entity>0` + stale fs) fired mid-KG-build → bogus 0-token cells → dashboard
`is_real` (tokens>0) filter drops them, gate hardened to KG-stability.

**Infra:** gemma-4-31b-nvfp4 vLLM redeployed with `GEMMA_MIN=1` (persistent — the earlier `min_containers=0`
idle-died mid-build → empty KGs); KG + summaries now point at it (batched GPU ≫ serialized OpenRouter).

**Artifacts:** `_xafs_perdp_{plain,ppr_on,ppr_map,ppr_off}.json`; `xafs_dashboard.html` (live);
runner `run_xafs_perdp.py`; templates `plain-xafs-dp_001..013` (13 separate) + `semfs-ppr-xafs` (KG seeds).

---

## RESULTS v1 — SUPERSEDED (plain wins on small-workspace exact-lookups, 2026-06-30)

_Kept for history. The "plain wins" conclusion held only for the small-workspace vector-only slice; the
v2 full matrix above overturns it at scale. Agent = codex/gpt-5.4-mini, judge = Gemini 3.1 Pro._

### 1. Plain FS baseline reproduces SMFS — FS collapses at scale
Plain (raw `find`/`grep`/`cat`, per-dp scoped) on all 13: **8/13 correct, ~196 K tok/Q**. Small
workspaces (≤300 files) cheap + 8/8 right; **big workspaces (dp_009–013, 480–9,988 files) expensive
(175–595 K tok) AND wrong (0/5)** — exactly SMFS's "FS accuracy collapses at 10 K files."

### 2. Combined-seed confound — xAFS needs ONE index per workspace
One seed for all 13 → `semfs grep` searches all 19 K files/Q → blowups (487 K on a 50-file dp,
788 K on dp_006). Invalid FS-vs-semfs comparison. Fix = per-dp seeds (also tiny → solves disk +
one-arm-per-sandbox). Built + verified 9 (dp_001–009); template `semfs-perdp-xafs`.

### 3. Semantic search < literal grep on EXACT-LOOKUP tasks
Per-dp scoped vector-search semfs (vectors + summaries + fs, **NO KG**) vs plain, 6 cells:

| dp | files | semfs tok | ok | plain tok | ok |
|---|---|---|---|---|---|
| dp_001 | 5 | 37 K | ✓ | 53 K | ✓ |
| dp_002 | 10 | 74 K | ✓ | 78 K | ✓ |
| dp_003 | 20 | 149 K | ✓ | 61 K | ✓ |
| dp_004 | 30 | 97 K | ✓ | 54 K | ✓ |
| dp_005 | 50 | **554 K** | **✗** | 75 K | ✓ |
| dp_009 | 480 | 316 K | ✗ | 456 K | ✗ |
| **total** | | **1.23 M · 4/6** | | **0.78 M · 5/6** | |

**Plain wins: more accurate (5/6 vs 4/6) AND 58% fewer tokens.** Root cause (dp_005 grep
diagnostic): for "find the Zelle confirmation number," semantic search returns *conceptually related*
files (Venmo log, bills) and misses the *exact keyword* match; plain's `grep -r "Zelle"` finds it
instantly. **xAFS questions are mostly exact lookups (IDs, confirmation numbers, amounts) where
literal grep beats semantic retrieval.** semfs's −30% token efficiency held on some cells; accuracy
did not.

**Untested (deferred):** the KG/PPR prior (`ppr_off/on/map`) — entity links to the exact file —
needs the GPU gemma-vLLM KG build. Whether it recovers exact-lookup accuracy is open. dp_010–013
per-dp seeds also unbuilt (CPU+summary embed too slow; would need summaries-off or GPU).

**Artifacts:** `benchmarks/e2b/_xafs_slice_plain.json`, `_xafs_perdp_search.json`; runners
`run_xafs_slice.py` (plain/combined), `run_xafs_perdp.py` (per-dp scoped), `run_judge_xafs.py`;
templates `semfs-mount-xafs` (combined), `semfs-perdp-xafs` (9 per-dp).

---

## Carry-over caveats

- **Judge is the dominant noise source (SEM-42, Backlog).** ~80% of WB-Lite rep variance is the judge
  false-failing on **truncated excerpts** + **synthetic zeros** (valid deliverable scored 0, no
  artifact). xafs deliverables can be long → this bites here too. Land the SEM-42 truncation fix, or at
  minimum run the post-run judge audit (re-judge truncation/no-evidence zeros with full content) before
  trusting any verdict. n=13 cases is already low-power; judge noise on top makes a raw arm-Δ unreliable.
- **xafs = 0 code files, English** (PHASE0 of `next-plaid-late-interaction`). No code lane (confirmed:
  `vchunks_code` absent) — irrelevant to PPR (doc-only), but means the hidden KG is doc-entity-driven.
- **Don't re-run Leiden/KG on preemptible Modal** (houqin corruption — `rcas/2026-06-20-sqlite-…`). KG
  is already built; touch only the fs/export path.
- **Embedder must match** at mount: `byo:gemma-q4-onnx:768`. A mismatched embedder silently tanks recall.

## Pointers
- `../wblite-ppr-ab/EXPERIMENT.md` (the WB-Lite PPR A/B), `../wb-xafs-seeds-e2b-ready/README.md` (seed build + the now-stale gotcha)
- `../wb-judging-stability/README.md` / SEM-42 (judge truncation fix)
- `benchmarks/e2b/run_matrix.py` (arms, seed-source, ppr_map map gen), `benchmarks/e2b/run_judge.py` (Seed-2.0-Lite)
- `benchmarks/modal/semfs_modal.py` (`inspect_seed_tables`, `grep_seed`, `index_corpus --phase fs`, export)
- `memory/xafs-wb-embedding-setup.md`, `memory/all-benchmark-tests-on-e2b.md`
