# Workspace-Bench 5-arm matrix — semfs vs baseline (codex/GPT-5.4)

**Status:** ✅ COMPLETE (2026-06-10, **25/25 runs**). This ticket is the canonical home for the
run — methodology, results, artifacts, and the doc index for future runs.

## TL;DR verdict

**The hypothesis — "semfs reduces tokens while maintaining accuracy" — is NOT supported by this
run, for any configuration.** `plain` (no semfs) wins on *both* axes: **46% accuracy @ 89K mean
tokens**. The only competitive semfs arm is `cloud` (token-neutral @ 93K but ~60% of plain's
accuracy); it *won* the high-ceiling case 95 (12/12 vs plain 11/12) but scored 0/12 on the other
synthesis case (175), so its edge is **coverage-dependent, not reliable**. Local raw-chunk semfs
(nokg/gfs_off/gfs_on) is decisively worse — 3–14% accuracy at 2–5× tokens, plus a 45-min timeout.
**The lever is index quality (summaries vs raw chunks), not the KG/by-topic knobs.** Top
follow-ups: a **local-+-summaries** arm and a **`SEARCH_ONLY=off`** arm.

```
FINAL AGGREGATE (Σpassed/Σtotal across 5 cases)
arm       accuracy        mean tokens   timeouts
plain     33/71 = 46%      89,270        0      ← best on BOTH axes
cloud     19/71 = 27%      93,443        0      ← token-neutral, won case 95 only
gfs_off    7/71 = 10%     177,711        0
gfs_on    10/71 = 14%     471,217        0      ← worst tokens
nokg       2/71 =  3%     247,300        1
```

---

## 1. Hypothesis

> **semfs reduces an agent's token usage while maintaining or improving accuracy** — by
> serving semantic retrieval + extracted text as an ordinary folder, so the agent stops
> crawling (`os.walk`/`find`) and stops shelling out to parse binaries (the "format trap").

## 2. Design — the 5-arm capability ladder

Each arm adds exactly one capability over the previous. **The task prompt is byte-identical
across all arms** (honest-A/B): semfs's help comes only through the product (mount + injected
`AGENTS.md` + `grep` shadow), never the prompt. Full architecture + diagrams:
[`../../benchmarks/workspace_bench/BENCH_ARCHITECTURE.md`](../../benchmarks/workspace_bench/BENCH_ARCHITECTURE.md).

| arm | storage | KG `/kg/` | graph-fs `/by-topic/` | isolates |
|---|---|---|---|---|
| `plain` | real disk (no semfs) | — | — | baseline ceiling |
| `nokg` | local sqlite | off | off | search + grep-inline alone |
| `gfs_off` | local sqlite | **on** | off | marginal value of the KG |
| `gfs_on` | local sqlite | on | **on** | marginal value of `/by-topic/` |
| `cloud` | Supermemory cloud | off | off | local-index vs cloud-index |

**Cases (5):** `15, 44, 95, 175, 289` × 5 arms = **25 runs**.

## 3. Configuration

- **Agent:** codex / `openai/gpt-5.4` via OpenRouter "ripbench" responses API.
- **Local seed:** `chanpin-gemma-q4.db` (gemma-q4 BYO-ONNX embedder, 768d; JINA reranker).
  Fresh copy → `chanpin-matrix` tag per run (contamination impossible; canonical never written).
- **Cloud container:** `workspace-bench-chanpin` (Supermemory `/v4/search`, server-side
  embeddings **with `.extracted.md` summaries indexed**, ~74% coverage). Verified searchable.
- **Common semfs knobs:** `GREP_INLINE=on`, `RETURN_MODE=snippet`, `RESULT_LIMIT=8`,
  `SEARCH_ONLY=on`, `REWRITE=1`, `NO_PUSH=1`, `NO_SYNC=1`. Full list:
  [`../../benchmarks/workspace_bench/KNOBS.md`](../../benchmarks/workspace_bench/KNOBS.md).
- **Judge:** Seed-2.0-Lite via OpenRouter (`agent_eval.py`).
- **Box:** EC2 `m7i.xlarge` (4 vCPU / 16 GB), `ap-south-1`, `13.201.35.159`.
- **`SKIP_PREPARE=1`** for all semfs arms (the FUSE mount shadows the workdir, so the ~6-min
  `copytree` is waste — saves it). `plain` preps the workdir each case.

## 4. Results (11/25 so far)

| case | arm | score | tokens | tools | wall | notes |
|---|---|---|---|---|---|---|
| **15** | plain | **6/16** | 184,569 | 8 | 428s | production/xlsx-authoring task |
| (ceil ~6) | nokg | 1/16 | 164,343 | 12 | 122s | |
| | gfs_off | 1/16 | 204,456 | 6 | 76s | |
| | gfs_on | 3/16 | **666,493** | 12 | 605s | token blowup |
| | cloud | 1/16 | 141,691 | 5 | 90s | |
| **44** | plain | 2/16 | 58,268 | 6 | 463s | low-ceiling case |
| (ceil ~2) | nokg | 1/16 | 81,128 | 8 | 102s | |
| | gfs_off | 2/16 | 196,609 | 6 | 217s | |
| | gfs_on | 1/16 | **498,461** | **20** | 734s | token blowup |
| | cloud | 2/16 | 86,967 | 6 | 75s | ties plain, fastest |
| **95** | plain | **11/12** | 86,288 | 14 | 380s | high-ceiling; file-enumeration+synthesis task |
| (ceil 12) | nokg | **0/12** | 700,997 | 23 | 1421s | flailed 24min; SEARCH_ONLY hid the files |
| | gfs_off | **0/12** | 225,938 | 11 | 74s | |
| | gfs_on | **0/12** | 550,865 | 16 | 368s | |
| | **cloud** | **12/12** | 141,301 | **6** | 99s | **BEATS plain** — summaries win |
| **175** | plain | **8/12** | 38,221 | 5 | 394s | synthesis task |
| (ceil 8) | nokg | 0/12 | 290,035 | 15 | 382s | |
| | gfs_off | 0/12 | 112,197 | 12 | 508s | |
| | gfs_on | 0/12 | 276,197 | 16 | 145s | |
| | cloud | 0/12 | 49,693 | ? | — | cloud ALSO fails this synthesis case |
| **289** | plain | **6/15** | 79,007 | 7 | 391s | QA case |
| (ceil ≥6) | nokg | 0/15 | TIMEOUT | 17 | 2024s | hit 45-min cap |
| | gfs_off | 4/15 | 149,359 | 13 | 144s | |
| | gfs_on | 6/15 | 364,070 | 24 | 388s | ties plain acc, 4.6× tokens |
| | cloud | 4/15 | 46,778 | 2 | 29s | fewest tokens/calls, lower acc |

Raw: [`artifacts/pm_results_5arm.jsonl`](artifacts/pm_results_5arm.jsonl).

## 5. Findings so far

1. **No semfs arm has beaten `plain` on accuracy in any completed case** (15: 6 vs best-semfs 3;
   44: tie at 2). The retrieval/QA cases (95, 175, 289) are the real test — case 15/44 are
   production/low-ceiling, semfs's worst fit.
2. **`gfs_on` (the `/by-topic/` overlay) is the consistent token loser** — 666K & 498K, the two
   worst runs (12–20 tool calls). The browsable overlay *invites* crawling with no accuracy
   payoff. Strong negative signal for graph-fs as an agent-facing surface.
3. **`cloud` is the best-behaved semfs variant** — accuracy ties plain (case 44), low tokens
   (87–142K), fastest (75–90s, no FUSE latency + summaries). Suggests the local seed's weakness
   is the *form* of retrieved content (raw chunks) vs cloud's `.extracted.md` summaries.
4. **`KG` (gfs_off) barely moves the needle** vs `nokg` — the knowledge graph isn't the lever.
5. **Earlier standalone validations** (pre-matrix, same config): `15/plain` 6/16@86K,
   `15/gfs_on` 1/16@171K, `15/cloud` 4/16@122K — consistent ordering, different absolute tokens.
6. **🟢🔴 CASE 95 — the pivotal result: retrieval QUALITY is the whole game.**
   plain 11/12 @ 86K · **local semfs (nokg/gfs_off/gfs_on) all 0/12** (nokg burned **701K
   tokens / 24 min / 23 turns** for nothing) · **`cloud` 12/12 @ 141K in just 6 tool calls —
   BEATING plain.** Case 95 is a file-enumeration+synthesis task (read many specific
   `description_N.txt` files → report).
   - **First instinct (WRONG):** "`SEARCH_ONLY=on` hides the tree." But `cloud` also runs
     `SEARCH_ONLY=on` and scored 12/12 — so that alone can't be it.
   - **Corrected root cause: retrieval quality.** `cloud`'s container has `.extracted.md`
     *summaries* indexed → search surfaced the right content in 6 calls, no tree needed.
     `local`'s raw-chunk gemma-q4 index *failed* to surface the right files → the agent flailed,
     and `SEARCH_ONLY` removed its fallback. **Compound failure: poor local retrieval × no tree
     fallback.** Cloud avoids it via good retrieval.
   - **Implication (positive for semfs):** a semfs config (`cloud`, with summaries) **beat the
     plain baseline** on a hard case — higher accuracy *and* fewer tool calls. The make-or-break
     variable is **summaries-vs-raw-chunks in the index**, which is exactly the "local +
     summaries" arm (follow-up #1) — it would likely replicate cloud's win locally. The
     `SEARCH_ONLY=off` test (follow-up #0) is still worth running as the cheaper fallback fix for
     when local retrieval is weak.
7. **Even the QA case (289) didn't vindicate semfs.** plain 6/15 @ 79K; `gfs_on` *ties* (6/15)
   but at 364K (4.6×); `cloud` 4/15 @ 47K in 2 calls (efficient but lower acc); `nokg`
   **timed out** (45 min, 0/15). So on semfs's supposed best task type, the best it managed was
   an accuracy *tie* at far higher cost — no win.
8. **`cloud`'s case-95 win did NOT generalize.** Cloud scored 0/12 on case 175 (the other
   synthesis task) — same as local. So summaries help *only when the needed content is covered*
   (cloud's container is ~74% coverage); the win is coverage-dependent, not a reliable property.

## 6. Caveats (READ before quoting numbers)

- **n=1 per cell is noisy.** Same-arm reruns swing wildly (15/`gfs_on` 1/16@171K vs 3/16@666K;
  15/`cloud` 4/16 vs 1/16). codex exploration is stochastic; FUSE/API latency compounds it.
  **Trust the ordinal pattern across cases, not per-cell absolute numbers.** Quote a cell only
  with 2–3 repeats.
- **Structural rubrics are unwinnable** for the chanpin persona (expect `./data`, `./output_cc`,
  a `metadata.json` meta-task) — ceiling is well below 16/16 for *every* arm incl plain.
- **Cloud ≠ matched A/B** — different container/embeddings/coverage, and it has summaries the
  local seed lacks.
- **🔴 `nokg` arm is NOT cleanly "KG off."** `/kg/` (`KNOWLEDGE_GRAPH.md` etc.) is **baked into
  the canonical seed `chanpin-gemma-q4.db` as physical files**, so `SEMFS_KG=off` only stops
  *regeneration* — the baked-in files still appear in the mount. So `nokg` ≈ `gfs_off` in
  practice, and the **`nokg`-vs-`gfs_off` comparison (marginal value of KG) is contaminated**.
  Valid comparisons: `plain` vs any-semfs, local vs `cloud`, `gfs_off` vs `gfs_on` (both KG-on).
  To fix: `rm` the `/kg/` files from the matrix-tag copy before mounting `nokg` (via the daemon,
  not raw SQL), or build a KG-free canonical seed.

## 7. Artifacts (`artifacts/`)

Curated (traces + judge + diffs + timing + deliverables; the harness's duplicated
`telemetry/traces/` and huge workspace snapshots were excluded — they live on the box).

```
artifacts/
  pm_results_5arm.jsonl              # one JSON line per run (the results)
  run_case_fixed.sh                  # driver: one (case,arm) run
  run_pm_matrix_5arm.sh              # the matrix driver
  run5arm/<case>_<arm>/
    output/raw/codex_stdout.jsonl    # full codex trace (tool calls, usage)
    output/rubrics_judge--seed-2.0-lite-judge.json   # per-rubric scores
    output/agent.json                # tokens/turns/status
    output/output/*                  # the agent's DELIVERABLE (xlsx/csv/txt)
    output/raw/last_message.txt      # agent's final message
    telemetry/{timing_breakdown,diff_prepare,diff_run}.json
    run.log
```

Full (uncurated, incl. snapshots + per-case trace history) on the box:
`/srv/semfs-benchmark/matrix_artifacts/run5arm/` (~28–34M/run).

## 8. Reproduce

```bash
# on the box (see EC2_RUNBOOK_CURRENT.md for access)
/tmp/run_pm_matrix_5arm.sh                 # full 25-run matrix (serial)
# or a single cell:
MATRIX_RESULTS=/tmp/x.jsonl MATRIX_ART=/srv/.../x \
  /tmp/run_case_fixed.sh <case> <arm> <stamp> <skipprep>
```

## 9. Important docs for future agent runs

| doc | what it gives you |
|---|---|
| [`../../benchmarks/workspace_bench/BENCH_ARCHITECTURE.md`](../../benchmarks/workspace_bench/BENCH_ARCHITECTURE.md) | **harness architecture** — arm ladder, data flow, contamination model, honest-A/B, telemetry (diagrams) |
| [`../../benchmarks/workspace_bench/KNOBS.md`](../../benchmarks/workspace_bench/KNOBS.md) | **every env knob** (core + harness) with defaults + the two key ones explained |
| [`../../benchmarks/workspace_bench/EC2_RUNBOOK_CURRENT.md`](../../benchmarks/workspace_bench/EC2_RUNBOOK_CURRENT.md) | **how to run** — box access, PATH gotchas, mount cleanup, secrets |
| [`../../docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) | **core product** — backends, search pipeline L1–L7, **§6 agent-facing delivery (grep-inline, siblings, /kg/, /by-topic/, hint injection)** |
| [`../../benchmarks/workspace_bench/judge_pipeline.md`](../../benchmarks/workspace_bench/judge_pipeline.md) | judge setup + the baseUrl `/v1` gotcha |
| [`../../benchmarks/workspace_bench/seed-coverage.md`](../../benchmarks/workspace_bench/seed-coverage.md) | seed/cloud coverage per container + missing-file lists |
| [`../../benchmarks/workspace_bench/cloud_env_state.md`](../../benchmarks/workspace_bench/cloud_env_state.md) | cloud container state (`workspace-bench-chanpin` verified searchable) |
| [`../case289-retrieval-investigation/`](../case289-retrieval-investigation/) | prior deep retrieval investigation (token lever = codex exploration) |
| [`../format-trap-extraction-delivery/`](../format-trap-extraction-delivery/) | the grep-inline / `.extracted.md` format-trap work |
| [`../benchmark-adapter/`](../benchmark-adapter/) | plan for a multi-benchmark harness (WB → xAFS/terminal-bench/TheAgentCompany) |
| [`../../rcas/`](../../rcas/) | root-cause analyses (KG materialization race, partial seed, stale build, extraction coverage, agent FS isolation) |

## 10. Open follow-ups

> **2026-06-11 update:** the follow-ups below were executed as E1–E5 (see `RESULTS.md`).
> The next round is specified in **[`EXPERIMENTS_NEXT.md`](EXPERIMENTS_NEXT.md)** (E6–E14),
> grounded in [`TOKEN_ECONOMY.md`](TOKEN_ECONOMY.md), [`OPINIONS.md`](OPINIONS.md), and
> [`RESEARCH_NOTES_EXTERNAL.md`](RESEARCH_NOTES_EXTERNAL.md).

0. **🔴 `SEARCH_ONLY=off` arm (highest priority)** — case 95 showed `SEARCH_ONLY=on` hides the
   file tree and destroys file-enumeration tasks (0/12 vs plain 11/12). Re-run with
   `SEMFS_SEARCH_ONLY=off` so `ls`/`os.walk` see the real tree; semfs grep still available. This
   is the single most important config to test — it likely flips semfs from net-negative to
   net-positive on manipulation tasks.
1. **"local + summaries" arm** — cloud beat local semfs likely because of indexed
   `.extracted.md` summaries. Adding per-doc summaries to the local seed is the strongest next
   lever (the `summary-augmented-table-retrieval` direction).
2. **2–3 repeats per cell** — required for any quotable number (§6 variance).
3. **2-way parallelism** — feasible via lightweight symlinked `WB_ROOT` + per-worker tag +
   serialized cloud arm; deferred (setup ≈ runtime saved for a one-off; disk 93% full, ~90G
   reclaimable from unused `research_*`/`kaifa_*` personas). See BENCH_ARCHITECTURE §10.
4. **Drop or rethink `gfs_on`** — `/by-topic/` consistently costs tokens without accuracy gain.
