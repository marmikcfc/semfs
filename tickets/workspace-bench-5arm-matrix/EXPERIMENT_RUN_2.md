> <!-- STALE-BANNER --> ⚠️ **STALE (2026-06-25)** — superseded by later runs; findings folded into [/CURRENT_STATE.md](../../CURRENT_STATE.md). Kept for lineage.

# Experiment Run 2 — Modal-based benchmark series

_Started: 2026-06-12. Platform: Modal (replaces EC2 box). All experiments run via
`modal run benchmarks/modal/semfs_modal.py::…`. Results archived under
`tickets/workspace-bench-5arm-matrix/artifacts/run2/`._

---

## Infrastructure fixes applied (prereq for all runs)

### Fix 1 — Provider routing (401 unauth)
`run_case()` now imports `codex.py` harness and calls `harness.run()`. The harness
starts a local Python chat-adapter (OpenAI Responses API → OpenRouter REST). Model
name MUST contain `/` (e.g. `openai/gpt-5.4`); bare `gpt-*` bypasses adapter → 401.

### Fix 2 — Sandbox write gate (empty deliverables)
`CODEX_SANDBOX_MODE=danger-full-access` — without this, codex's `workspace-write`
sandbox gates writes on `.git` presence; `/tmp/workdir` has no git → all writes blocked.

### Fix 3 — WB prompt wrapping (no deliverables)
`_wrap_prompt_wb()` replicates `agent_runner.py::_wrap_prompt()`: Chinese workdir
anchor + output directory rule + English path-list tail. Without it the agent doesn't
know where to write or that it must output a path list.

### Fix 4 — Judge fastembed flooding (0/15 scores)
`_run_judge()` now copies ONLY `model_output/*.` to `judge_dir/output/` instead of
symlinking the full workdir. The semfs embedder writes ~898MB of ONNX blobs to
`.fastembed_cache/` in the workdir, which flooded the judge's 50-file collection cap.

---

## E-SMOKE: Modal smoke test (provider fix verification)

**Status:** PASSED (2026-06-12)
**Command:** `modal run benchmarks/modal/semfs_modal.py::e9w2_smoke`

| field | expected | actual |
|---|---|---|
| rc | 0 | **0** ✅ |
| calls | >0 | **11** ✅ |
| tokens | >0 | **143,734** ✅ |
| deliverables | non-empty | **best_selling_product_core_data_list.txt** ✅ |
| judge | non-zero | **4/15 (0.267)** ✅ |
| confidence_high_fired | — | false |

**Artifact:** `artifacts/run2/smoke/e9w2_smoke_v4.json`

---

## E9w2: Spread-normalized confidence + COMPLETE-FILE gate

**Status:** COMPLETED — kill condition met (2026-06-12)
**Config:** case=289, arm=nokg, render_mode=two-tier, ×3 reps

**Results:**
| run | calls | tokens | HIGH fired | judge score |
|---|---|---|---|---|
| m1 | 5 | 60,274 | **false** | 4/15 (0.267) |
| m2 | 6 | 49,536 | **false** | 4/15 (0.267) |
| m3 | 14 | 238,251 | **false** | 4/15 (0.267) |

**Kill condition verdict:** HIGH never fired in 3/3 runs. The agent finds data via
direct file reads (5–6 calls in 2/3 runs) without using `semfs grep`. Same bimodal
call pattern (5–6 vs 14) as EC2 E9w1. The confidence optimization path is stopped.

**Why HIGH doesn't fire:** Case 289 data is accessible via direct corpus reads. Agent
doesn't need to call `semfs grep` at all. The COMPLETE-FILE gate is working correctly
(it prevents HIGH on truncated excerpts), but there are no grep calls to evaluate.

**Decision:** Stop E9w2 signal optimization. Proceed to E8 (honest baseline).

**Artifact:** `artifacts/run2/e9w2/e9w2_results.jsonl`

---

## E8: Honest headline run (the quotable result)

**Status:** COMPLETED (2026-06-12) — 30/30 cells returned
**Command:** `modal run benchmarks/modal/semfs_modal.py::run_e8 --reps 3 2>&1 | tee artifacts/run2/e8/e8_raw.log`
**Cases:** 289, 175, 95, 15, 44
**Arms:** plain (baseline), nokg (two-tier render, leanhint3-class seed, v4.1 hint)
**Reps:** 3 per cell = 30 total cells
**Render mode:** two-tier for both arms

**Pre-registered condition:** ≥3-of-5 cases: semfs mean_tokens < plain AND
accuracy ≥ plain−1 → "semfs delivers" headline. <3-of-5 → declare wrong arena
and execute O8 via E11.

**VERDICT: PRE-REGISTERED CONDITION FAILED — 2/5 wins (need ≥3)**

**Results table:**
| case | arm | reps | accuracy (mean) | tokens (mean) | verdict |
|---|---|---|---|---|---|
| 289 | plain | 3 | 0.178 | 107,734 | |
| 289 | nokg | 3 | 0.311 | 390,759 | ACC_ONLY (3.6× tokens) |
| 175 | plain | 3 | 0.083 | 118,844 | |
| 175 | nokg | 3 | 0.305 | 110,407 | **WIN** ✓ |
| 95 | plain | 3 | 0.000 | 118,164 | |
| 95 | nokg | 3 | 0.000 | 303,311 | ACC_ONLY (2.6× tokens, both 0) |
| 15 | plain | 3 | 0.062 | 342,694 | ceiling |
| 15 | nokg | 3 | 0.062 | 493,085 | ACC_ONLY (1.4× tokens) |
| 44 | plain | 3 | 0.000 | 102,584 | ceiling |
| 44 | nokg | 3 | 0.021 | 92,466 | **WIN** ✓ |

**Arm definitions:**
- `plain`: raw corpus copy, no AGENTS.md injection, no `.semfs` marker, no SEMFS env vars
- `nokg`: corpus + AGENTS.md hint from seed + `.semfs` marker + semfs grep available

**Analysis:**
- nokg accuracy ≥ plain in 4/5 cases — semfs improves accuracy reliably
- nokg tokens < plain in only 2/5 cases — semfs does NOT save tokens on broad workspace tasks
- Token bloat on case 289 (3.6×): agent makes many grep calls returning large blobs
- Token bloat on case 95 (2.6×): both arms score 0 — case 95 is fundamentally hard for this agent/corpus combo
- Case 175 WIN: narrower task where grep surfaces the target file and agent completes quickly
- Case 44 WIN: easy task where grep helps and output is compact

**Conclusion:** Wrong arena for the token hypothesis. Broad workspace tasks (full 1452-file corpus) can be solved by direct file access without semfs. semfs grep is helpful for accuracy but adds token overhead in tasks where the agent explores broadly. E11 (discovery-stressed, needle-in-200-files) is the right arena for the semfs value proposition.

**Artifacts:** `artifacts/run2/e8/e8_results.jsonl` + 30 individual JSONs

---

## E11: Discovery-stressed + cross-lingual cases

**Status:** COMPLETED (2026-06-12) — 12/12 cells returned
**Command:** `modal run benchmarks/modal/semfs_modal.py::run_e11 --reps 3 2>&1 | tee artifacts/run2/e11/e11_final_raw.log`
**Cases:** e11-001 (product Q4-2023 return rate), e11-002 (region H1-2024 growth)
**Corpus:** `e11_discovery_corpus` — 200 product reports + 200 region summaries (400 files total)
**Arms:** plain + nokg (e11_seed.db built via Modal SEMFS_INDEX_ONLY indexer, 400 files, 4.4 MB)
**Reps:** 3 per cell

**Infrastructure fixes applied for E11:**
- EC2 box unreachable → `SEMFS_INDEX_ONLY=1` env var added to `daemon_runtime.rs`: skips FUSE mount
  after indexing completes, writing the seed DB cleanly without FUSE
- `modal volume put` path bug: remote paths are volume-relative (no `/data/` prefix); corpus and
  metadata re-uploaded to `corpus/e11_discovery_corpus/` and `wb/evaluation/tasks_local/`
- Metadata bug: `product_report_042.txt` contains Widget-Sigma (not Widget-Omega);
  `region_summary_073.txt` contains North America (not Southeast Asia). Both fixed in metadata.json
- `_prep_workdir` AGENTS.md extraction: seeds built with SEMFS_INDEX_ONLY have no baked AGENTS.md.
  Added null-check + fallback discovery hint in the Python heredoc

**Case details:**
- `e11-001`: Find highest Q4-2023 return_rate in 200 product reports. Needle: `product_report_042.txt`,
  Widget-Sigma (variant 42), 8.73%. Task in Chinese, files in English.
- `e11-002`: Find fastest H1-2024 growth region in 200 region summaries. Needle: `region_summary_073.txt`,
  North America (territory 73), 34.6%. Task in Chinese, files in English.

**Results table:**
| case | arm | reps | accuracy (mean) | tokens (mean) | verdict |
|---|---|---|---|---|---|
| e11-001 | plain | 3 | 0.611 | 70,187 | |
| e11-001 | nokg | 3 | 0.556 | **50,467** | **WIN** (−28% tokens, acc within tol) |
| e11-002 | plain | 3 | 0.722 | 59,718 | |
| e11-002 | nokg | 3 | **0.778** | 59,880 | ACC_ONLY (+162 tok, +8% acc) |

**E11 verdict: 1/2 cases WIN** (need both for "semfs delivers in discovery arena" headline)

**Analysis:**
- Both arms find the correct needle in 11/12 cells (e11-002 nokg r2 returned wrong region in prior run)
- Final run: all 12 deliverables correct — the needle IS findable by both arms
- nokg arm used 28% fewer tokens on e11-001 (WIN), but essentially tied on e11-002 (+162 tokens, +0.3%)
- **Root cause of non-discrimination:** the E11 corpus files are tiny (18 lines × ~100 chars ≈ 1 KB each).
  Scanning all 200 product reports costs only ~70K tokens. semfs grep adds overhead without
  sufficient discovery benefit when files are this small.
- **Corpus design flaw:** rubric scores cap at 0.833 (5/6) because agents include "(variant 42)"
  in the product name (correct verbatim from the file) while the rubric expected a bare "Widget-Sigma".
  The process rubric (R6: "demonstrates search use") also varies, explaining the 0.5–0.833 range.

**Conclusion:** E11 confirms the "wrong arena" pattern from E8. For semfs to show token efficiency
gains, the corpus must have large enough files that linear scanning is prohibitively expensive.
200 × 1KB files = trivially scannable. Meaningful discovery stress requires files of ≥10KB or
≥1000 files. A future E12 should redesign with padded files or a larger corpus.

**Cross-lingual hypothesis result:** Both arms found the needle using direct file access. The nokg
arm's semfs grep (with SEMFS_REWRITE=1 for Chinese→English rewrite) did not demonstrate a clear
efficiency advantage because the scan cost was already low. The cross-lingual rewrite mechanism
was not the bottleneck.

**Artifacts:** `artifacts/run2/e11/e11_final_raw.log` + prior runs in same directory

---

## E15: Tri-store compression (gated, last)

**Status:** SKIPPED — E8 pre-registered condition failed (2/5 wins < 3/5 required)
**Decision:** E8 condition NOT met → wrong arena for the token hypothesis. E15 would
optimize delivery for a use case that isn't the primary semfs value proposition.
E11 (discovery-stressed) is the right arena to establish semfs value first.

---

## Artifact archive

```
tickets/workspace-bench-5arm-matrix/artifacts/run2/
  smoke/        ← e9w2_smoke_v4.json (passing smoke)
  e9w2/         ← e9w2_results.jsonl (3 reps, kill condition met)
  e8/           ← e8_results.jsonl + 30 individual JSONs (5 cases × 2 arms × 3 reps) + e8_raw.log
  e11/          ← e11_results.jsonl (12 cells, final run) + e11_final_raw.log + seed build logs
```
