# RUN MANIFEST — read this BEFORE analyzing any run in this ticket

Purpose: tell whoever runs the **analyze-benchmark-results** skill *which* runs exist,
*what config* produced each, and *what is confounded* — so scores aren't read at face
value. Several runs here look like retrieval results but are actually infra/corpus/seed
artifacts. The cardinal rule still applies: **mine BOTH cloud and local actual outputs
(`codex_stdout.jsonl`), don't infer from scores.**

Agent = codex / GPT-5.4. Judge = Seed-2.0-Lite rubrics. `cached_input=0` on every run
(single-turn) ⇒ **tokens are driven by turn count + per-call output size**, so a lower
token number is NOT automatically "more efficient" — check whether the agent did less.

---

## Where things live
| Path | What |
|---|---|
| `artifacts/run5arm/<case>_<arm>/` | The 5×5 matrix (cases 15/44/95/175/289 × arms plain/nokg/gfs_off/gfs_on/cloud), 2026-06-10 |
| `e3_summary_ab/e_sum2.jsonl` | E3 summary-vs-raw A/B metrics (5 lines) |
| `e3_summary_ab/runs/<label>/` | Surviving E3 traces: `44_raw`, `289_raw`, `44_sum_dualstore` |
| `e3_summary_ab/corpus_403_stubs.txt` | The 3 corpus files that are 403 stubs |
| `RESULTS.md`, `ANALYSIS.md`, `HANDOFF.md` | Prior write-ups of the 5-arm matrix |
| `TOKEN_ECONOMY.md` | First-principles cost equation + five-whys (incl. the codex-clip finding) |
| `RESEARCH_NOTES_EXTERNAL.md` | How the field optimizes each component (PwC grep paper, Manus, SWE-grep, …) |
| `OPINIONS.md` | Calibrated opinions O1–O8 with credences + falsifiers |
| `EXPERIMENTS_NEXT.md` | E6–E14 specs (predictions, kill conditions, decision tree) |
| `constraint-based-creativity.md` | Ideation record behind E6–E14 (incl. rejected ideas + why) |
| `../../rcas/2026-06-11-wb-5arm-infra-failure-not-retrieval.md` | RCA: "semfs loses" was 4 infra bugs |
| `../../rcas/2026-06-11-summary-seed-drops-extracted-md.md` | RCA: summary seed discarded the raw table; dual-store fix |

Each `<run>/output/raw/codex_stdout.jsonl` holds the agent's tool calls + outputs. For the
**cloud** arm, the `semfs grep` outputs in that file ARE Supermemory's response (rewritten
query + ranked filepaths + verbatim chunks). The raw API scores/candidate set are NOT
archived (the daemon log isn't copied) — you have the rendered ranked output, not the scores.

---

## Arm configs
| Arm | Backend / seed | Notes |
|---|---|---|
| `plain` | none — agent reads the real persona workdir files | the baseline; no semfs |
| `nokg` | local semfs, `chanpin-clean.db` (gemma-q4), KG off | search + grep-inline |
| `gfs_off` | local + baked `/kg/` files | `/kg/` is static in the seed |
| `gfs_on` | local + `/by-topic/` graph-fs | |
| `cloud` | Supermemory server-side, container `workspace-bench-chanpin` | `SEMFS_STORAGE_BACKEND=cloud` |

## Seeds
- **`chanpin-clean.db`** — gemma-q4, raw-cell chunks + `.extracted.md` table siblings. The
  RAW representation. The one to trust for "can the agent read the table" cases.
- **`chanpin-sum.db`** — summary seed, 199/201 xlsx summarized. **summary-ONLY for ~196
  files (raw table discarded — agent cannot read those tables)**, EXCEPT case-44's 3
  dev-task files which were rebuilt with the **dual-store** fix (summary embedded + raw
  table in `.extracted.md`). Do NOT run other xlsx cases on this seed expecting valid
  answers — only case 44 is dual-store-valid here.
- **`workspace-bench-chanpin.db`** — the cloud arm's container (server-side search).

## Binaries
- 5-arm matrix + the first E3 A/B: pre-dualstore binary (has the `grep` render-cap +
  RRF chunk-mass fixes).
- `44_sum_dualstore`: **new dual-store binary** (md5 `1f4cf280…`), summary embedded but
  `.extracted.md` = raw table. See the summary RCA.

---

## Run-by-run: config + what's confounded

### 5-arm matrix (`artifacts/run5arm/`, 2026-06-10)
Headline (per RESULTS.md): plain ~46%@89K beats all semfs arms on both axes; cloud
~27%@93K (token-neutral); local raw-chunk semfs 3–14% @ 2–5× tokens.
**Caveats before trusting any local-semfs cell:**
- ⚠️ **Infra confound** — the "local semfs loses 3–14%" headline is substantially the 4
  infra bugs (disk-ENOSPC `malformed db`, contamination sidecars, fastembed mount-hang,
  vec0 corruption). See the infra RCA. **For each local-semfs cell, open
  `codex_stdout.jsonl` and check for `database disk image is malformed`, 50s search
  timeouts, or "falling back to cloud search → 0 results" BEFORE reading its score as a
  retrieval result.** A clean, health-gated re-run of `289/nokg` went 0/15 → plain's score.
- ⚠️ **Case 289 — corpus data bug**: all 3 of 289's source files
  (`top10_product_status_table.xlsx`, `apparel_product_shooting_sheet.xlsx`,
  `problem_product_tracking.xlsx`) are **403 Forbidden HTML stubs on disk** (321 B each).
  The data was never acquired. So 289 is **partially unanswerable for ALL arms incl. plain
  & cloud** — its ~4/15 is the corpus ceiling, NOT a retrieval comparison. (Only 3/1098
  corpus files are stubs — localized to 289.)
- ⚠️ **Case 95 — txt-driven**: task reads `description_9/12/15/22.txt`. The summary lever
  is Excel-only ⇒ irrelevant to 95.
- ⚠️ **Cases 15 & 44 — low rubric ceilings**: very demanding spec rubrics (exact
  `Chart.js 4.4.0`, named modules, exact counts), so every arm caps low. Keep for
  completeness; exclude from headline decisions.

### E3 summary-vs-raw A/B (`e3_summary_ab/`)
All on the `nokg` arm, `SEARCH_ONLY=off`. Metrics in `e_sum2.jsonl` (one line per cell):

| line `label` | seed | passed | tokens | trace | verdict |
|---|---|---|---|---|---|
| `44` `summaries` | chanpin-sum (summary-only, **no `.extracted.md`**) | 0/16 | 16K | **OVERWRITTEN** | ❌ INVALID — agent "files not found" (couldn't read tables). Not a retrieval result. |
| `44` `raw` | chanpin-clean | 4/16 | 130K | `runs/44_raw/` | ✅ valid baseline (low ceiling) |
| `289` `summaries` | chanpin-sum (summary-only) | 4/15 | 100K | **OVERWRITTEN** | ⚠️ confounded — 403 answer file (both arms) + summary-only seed |
| `289` `raw` | chanpin-clean | 4/15 | 175K | `runs/289_raw/` | ⚠️ capped by the 403 source data |
| `44` `summaries-dualstore` | chanpin-sum, 3 files rebuilt **dual-store** | 2/16 | 125K | `runs/44_sum_dualstore/` | ✅ VALID summary test |

**Artifact-overwrite caveat:** sum and raw of a case shared the box dir `<case>_nokg`
(`RUNLABEL` tagged only the JSONL line, not the dir), so the later run's TRACE overwrote
the earlier. The `…summaries` traces for 44 and 289 are gone — **only their JSONL metrics
survive**. `44_sum_dualstore` ran in a separate dir, so its trace is intact.

**The one valid summary read (case 44):** dual-store summary **2/16** vs raw **4/16**,
token-neutral. Both pass {generated, 120 tasks}; the 2-pt gap is two tech-stack rubrics —
n=1 dashboard-construction noise on a low-ceiling case; **both arms read identical raw
tables**. Structural reason summaries can't help 44: the task **names** its 3 source files,
so the agent never needs semantic retrieval. Summaries only help when the agent must
*search* for the right data file among many — no clean WB case offers that (44 names files,
289 = 403 data, 95 = txt, 15 = missing file + low ceiling, 175 = csv/unconfirmed coverage).

---

## Cross-cutting traps (the things that make analysis wrong)
1. **Don't read a `status:"failed"` / tiny-token / few-tool-call run as a hard task** — it's
   usually the agent giving up early (seed/infra), e.g. 44-sum 16K/3-calls/0-of-16.
2. **Rule out infra before retrieval** — `malformed db`, search timeout, "falling back to
   cloud → 0 results" in the trace ⇒ infra bucket, re-run clean before concluding.
3. **Rule out corpus** — if the answer file is a 403 stub (289) or named in the task (44),
   retrieval/summary levers can't move the score; the ceiling is structural.
4. **Token diffs ≠ efficiency** under `cached_input=0` — a lower token count can mean the
   agent read less (often because it couldn't read the file), not that it was leaner.
5. **chanpin-sum is summary-only except case 44** — its other xlsx have no readable table;
   any new run on it for a non-44 xlsx case will fail to *answer* regardless of retrieval.
6. **Cloud scores aren't archived** — you have the rendered Supermemory grep output, not
   the numeric similarity scores; for score-level ranking analysis, archive the daemon log
   (`…/semfs/logs/<tag>.log`) on the box first.

## How to use this with the analyze skill
1. Read this manifest + the two RCAs.
2. For the case you're analyzing, open BOTH the cloud trace (`run5arm/<case>_cloud/…/codex_stdout.jsonl`)
   and the local trace; diff the actual `semfs grep` outputs (what each surfaced, in what
   order), not the scores.
3. Classify each loss into the bucket (infra / corpus / retrieval / ranking / delivery /
   hint / synthesis) before proposing a lever.
