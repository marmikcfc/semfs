> <!-- STALE-BANNER --> ⚠️ **STALE (2026-06-25)** — superseded by later runs; findings folded into [/CURRENT_STATE.md](../../CURRENT_STATE.md). Kept for lineage.

# semfs × Workspace-Bench — Experiments Report

_Synthesis as of 2026-06-18. Covers the evo optimization, the Phase-1 SELECT matrix, the
kg-quality + sufficiency improvements, the Modal GLM-5.1 infra, and the full 4-arm Modal run.
Companion to `CURRENT_STATE.md`, the per-cell traces in `artifacts/e2b_runs/`, and `.evo/project.md`._

Every claim is tagged **[verified]** (artifact-grounded), **[hypothesis]** (plausible, unconfirmed),
or **[refuted]**. Per-case numbers at n≤3 are coin-flips, not verdicts.

---

## 0. The question

Does semfs (semantic-filesystem retrieval) let a coding agent do **Workspace-Bench** product-management
tasks with **higher accuracy AND lower tokens than plain** (ripgrep over the raw file tree)? Test model:
`z-ai/glm-5.1`. Corpus: `chanpin` (1452 PM docs). Judge: `bytedance-seed/seed-2.0-lite` (rubric pass-rate).
**Hard rule:** all benchmark runs on **E2B real-FUSE**, never Modal-for-measurement.

---

## 1. Experiment inventory (timeline)

| # | Experiment | Cases · n · backend | Headline |
|---|---|---|---|
| 1 | **evo `/optimize`** | 53+171 · n=3 · OpenRouter | Found the **prompt/turnbrake lever**; compress+dedup+prompt won (0.444 vs plain 0.272, both axes) |
| 2 | **Phase-1 SELECT** (5 arms) | 15/44/53/95/175 · n=2 · OpenRouter | `compress+dedup+oc` topped present-only (0.26) — but missing-cell-inflated; **case-53-driven** |
| 3 | **kg-quality** (Leiden+kNN) | structural (offline) | Singletons **38.2% → 3.1%** [verified, deterministic]; overshot to a 135-file bucket |
| 4 | **sufficiency-resurfacing** | implemented + ran in #6 | Anti-dedup stop-signal; binary rebuilt so it actually executes |
| 5 | **Modal GLM-5.1 infra** | n/a | Self-hosted 754B endpoint + `semfs-baked-v2` (<60s boot) + rebuilt binary |
| 6 | **Full-4 Modal run** | 15/44/53/95/175 · n=2 · Modal | **Timeout-corrupted**; one robust signal: **KG-on hurts** (over-exploration) |

---

## 2. Metrics

> **Naming note:** every semfs arm's knob includes the **turnbrake prompt** — so the short labels
> below all implicitly carry "+prompt". Explicitly: `prompt+oc` = prompt+output-compression;
> `compress+dedup` = prompt+compress+dedup; **`compress+dedup+oc` = prompt + output-compression +
> `SEMFS_GREP_COMPRESS` + `SEMFS_DEDUP_WINDOW=5`** (knob: `compress_dedup_oc.json`). The "+prompt" is
> dropped from the labels only because it's common to all semfs arms.

### 2.1 evo run — cases 53+171, n=3, OpenRouter glm-5.1 (where the lever was found)

| arm | accuracy | tokens |
|---|---|---|
| plain (baseline) | 0.272 | 242K |
| prompt-only (`exp_0007`) | 0.349 | 143K |
| **compress+dedup+prompt (`exp_0002`)** ← evo winner | **0.444** | 173K |

**[verified]** The no-prompt ablation exploded to **0.24 / 878K** — the prompt is what stops the
re-search loop AND forces verbatim transcription (~2× accuracy). The prompt is the load-bearing lever.

### 2.2 Phase-1 SELECT — cases 15/44/53/95/175, n=2, OpenRouter, OLD binary+seed

Per-case accuracy (mean of reps):

| case | plain | prompt | prompt+oc | compress+dedup | compress+dedup+oc |
|---|---|---|---|---|---|
| 15 | 6% | 6% | 6% | 6% | 6% |
| 44 | 6% | 9% | 9% | 6% | 12% |
| **53** | 5% | **50%** | **91%** | 18% | **73%** |
| 95 | 0% | 0% | 0% | 0% | · (missing) |
| 175 | 4% | 0% | 4% | 0% | 12% |

Per-arm aggregate:

| arm | acc (present-only) | acc (fair, impute-0/10) | mean_tok | cells |
|---|---|---|---|---|
| plain | 0.042 | 0.042 | 805K | 10/10 |
| prompt-only | 0.131 | 0.131 | 1,488K | 10/10 |
| prompt+oc | **0.221** | **0.221** | 1,156K | 10/10 |
| compress+dedup+prompt | 0.068 | 0.061 | 894K | 9/10 |
| compress+dedup+oc ← "best" | 0.260 | 0.208 | 388K | 8/10 |

**[verified] The "best" label is confounded.** `compress+dedup+oc` topped *present-only* (0.26) and
*tokens* (388K) — but only on **8/10 cells**; it dropped case 95 (0% for everyone) and the expensive
95-cells, inflating both numbers. On the **fair denominator, `prompt+oc` (0.221) actually beats it
(0.208).** And the entire spread is **case-53-driven** (every other case is a floor) — and 53 is an
**evo-trained** case.

### 2.3 Full-4 Modal run — cases 15/44/53/95/175, n=2, Modal GLM-5.1, NEW binary + dense+decontam seed

Per-cell status (the story is the timeouts):

| arm | judged | timeouts | lost cells |
|---|---|---|---|
| plain | 8/10 | 3 | 44/r2, 95/r2 |
| best (compress+dedup+oc) | 9/10 | 1 | 95/r2 |
| sufficiency | 9/10 | 2 | 15/r2 |
| **KG-on** | **5/10** | **5** | 44/r1, 44/r2, 95/r2, 175/r1(net), 175/r2 |

Per-arm aggregate:

| arm | acc (present) | acc (fair, impute-0) | mean_tok | cells |
|---|---|---|---|---|
| sufficiency | 0.130 | **0.117** | 537K | 9 |
| plain | 0.112 | 0.089 | 186K | 8 |
| best (compress+dedup+oc) | 0.055 | 0.049 | 517K | 9 |
| KG-on | 0.025 | 0.013 | 218K | 5 |

**[verified] This run does NOT yield a clean accuracy winner** — uneven denominators (5–9 cells),
widespread timeouts, low N. See §4.5.

---

## 3. Robust findings (what we stand behind)

### 3.1 The transcription/stop PROMPT is the only proven positive lever **[verified]**
The `WB_TURNBRAKE` prompt — "the grep results ARE the file contents, stop searching, transcribe values
verbatim" — is what makes semfs beat plain in the evo run (prompt-only 0.349 vs plain 0.272 on both axes;
no-prompt ablation 0.24/878K). It is **content-agnostic** (no case-specific text), so it plausibly
generalizes — but our *evidence* for it is overfit (§4.1–4.2).

### 3.2 kg-quality: singletons solved structurally **[verified, exact]**
Full multi-level **Leiden + embedding-kNN edges** (commit `0106b2e`, TDD) cut the chanpin KG from
**173 communities / 66 singletons (38.2%)** to **32 / 1 (3.1%)** — deterministic, so this is the exact
number, not noise. ~35% of files that had a *zero* "related-files" pointer now sit in a real cluster.
**Caveat:** overshot to a 135-file bucket (21% of corpus; target was <60) — coherent (compliance theme),
not a junk-drawer, but coarse as a retrieval pointer. `RESOLUTION=1.0` is the lever.

### 3.3 KG-on (the dense overlay) HURTS end-to-end **[verified]**
In the full-4 run, KG-on lost **6/10 cells**, five to timeout. Smoking gun: **case 44 timed out on *both*
KG-on reps and on *no other arm*.** The agent reads the KG overlay (32 communities, some huge), over-explores
it, and runs out of clock. This confirms the §3.2 caveat: the dense KG as configured **drives over-exploration**,
the opposite of helping. Fix candidates: higher `RESOLUTION` (smaller communities) or layer sufficiency on top.

### 3.4 Over-exploration is the #1 unsolved token sink **[verified]**
Even with the turnbrake prompt, the agent re-runs `semfs grep` **50–190× per task** (the smoke alone: 52
calls). This is what `sufficiency-resurfacing` targets (re-surface the seen set + a "you have it, stop"
verdict, instead of dedup-stripping which made it *worse*).

---

## 4. Confounds & caveats (why no clean verdict yet)

### 4.1 Case-53 dominance **[verified]**
In Phase-1, the *entire* arm ranking is decided by **case 53** (spread 5%→91%); cases 15/44/175 are
near-floor for all arms and 95 is 0% for all. So "which arm wins" ≈ "which arm wins on case 53."

### 4.2 No held-out data — the 10-case overfit **[verified]**
**WB-Lite has exactly 10 cases** {15,44,45,53,55,95,171,175,386,388} and we've used **all of them** for
tuning and "validation." evo trained the prompt on **53 + 171**; 53 reappears in Phase-1 SELECT and 171 in
the planned Phase-2 "VALIDATE." The train/validate split is **fictional** — there is no untouched data.
A real generalization test needs fresh WB cases drawn from outside the 10.

### 4.3 Missing-cell artifacts **[verified]**
Present-only means flatter arms that lost cells (they drop their failures). Always read the **impute-0**
or common-cases column. This is why Phase-1's `compress+dedup+oc` "best" evaporates on a fair denominator.

### 4.4 Backend non-replication **[verified]**
`compress+dedup+oc` scored **0.26 on OpenRouter** (Phase-1) but **0.055 on Modal** (full-4) — same config,
opposite result. **[hypothesis]** its per-hit `gpt-4.1-mini` compress calls (via OpenRouter, grep.rs:555)
get throttled under the 20-concurrent load. Not yet trace-confirmed.

### 4.5 Timeout corruption of the full-4 run **[verified]**
20 concurrent sandboxes → one 8×H200 (`max_inputs=32`) → **~21s/call** (vs ~7.5s solo). Over-explorers
(50–190 calls) then blow the 33-min cell timeout. Timeouts hit *every* arm (plain 3, best 1, suf 2, KG 5),
non-randomly, so the accuracy A/B is not interpretable. **Fix:** lower `--parallel` (faster calls) or raise
the timeout (cells run to completion). Rerun pending.

### 4.6 Inherited measurement issues **[verified, from prior work]**
- **WB judge filename lottery:** ~34/100 WB cases name the output file judge-side only → a correct
  deliverable under another name scores 0. Mitigated by injecting the expected filename as a prompt hint (all arms).
- **Token cost model:** codex caches 80–88%, so "total tokens" overstates real cost ~4–8×; fresh input is
  roughly constant. Compare *per-correct-answer*, not raw totals.

---

## 5. Infrastructure built (this session)

- **Modal GLM-5.1-FP8** (754B MoE) on 8×H200 via vLLM + a LiteLLM responses→chat proxy (Codex-compatible).
  ~21s/call at 20-concurrent, ~7.5s solo. ~$36/hr warm; 30–43 min cold boot. Runbook: `benchmarks/modal/GLM51_RUNBOOK.md`.
- **`semfs-baked-v2` E2B template** — bakes the plain corpus tarball + the **dense+decontaminated seed**
  (`from_template("semfs-baked")`). Boot verified: create **0.8s**, plain-data-ready **22s** (was ~minutes
  of 442MB upload), KG = 32 comms, 0 leak chunks.
- **`semfs-fixed` rebuilt** (Modal x86_64) — now has all four knobs: `SEMFS_SUFFICIENCY` ✅ + compress + dedup + KG.
- **Harness wiring** (`run_matrix.py` / `cell_driver.py`): `WB_MODAL_GLM=1` routes codex at the vLLM endpoint
  (harness chat-adapter auto-bridges responses→chat + drops the `multi_agent` tool — no LiteLLM/`--disable` needed);
  `WB_E2B_TEMPLATE` selector + baked-corpus fast path; `WB_PAR` parallelism knob.
- **Confirmed:** E2B handles **20 concurrent sandboxes** (4 vCPU / 8 GB each; parallelism is sandbox-level,
  one cell per sandbox, memory-bound not CPU-bound).
- **Run scripts:** `run_full4_modal.sh` (4-arm × 5-case × n=2, 20-parallel, raised 60/65-min timeouts).

---

## 6. Honest verdict

- **The single robust positive result** is the transcription/stop **prompt** (evo, both axes). Everything
  layered on top (oc, compress, dedup, KG, sufficiency) is **unproven or confounded** at current N.
- **kg-quality** solved the singleton problem *structurally* and decisively — but the resulting **KG overlay
  HURTS end-to-end** (over-exploration → timeouts). Net KG verdict so far: **negative**, pending resolution-tuning.
- **sufficiency** is implemented, the binary now executes it, and it *led* the full-4 fair denominator (0.117)
  — but that run is timeout-corrupted, so it's **promising, not proven**.
- **No clean accuracy winner exists from any single run** — low N, case-53-driven, missing-cell + timeout +
  backend confounds. The deepest issue is the **arena**: WB-Lite's 10 reused cases can't give a generalizable answer.

## 7. Next steps

1. **Clean Modal rerun** of the 4 arms — `WB_PAR=3` (12-concurrent → ~12–15s/call) and/or the raised timeout,
   to kill the timeout confound. Judge on the fair denominator across all 5 cases.
2. **Fresh held-out cases** — generate WB cases outside the 10 for a *real* generalization test of the prompt
   lever (the only way to separate "real lever" from "case-53 artifact").
3. **Fix KG-on before re-testing** — raise `RESOLUTION` (smaller communities) or layer sufficiency on the overlay.
4. **Trace-confirm** the `best`-arm compress throttling hypothesis (§4.4) from the full-4 `agent.json` + `semfs_logs`.

## 8. Where the data lives

All traces local in `tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs/` (1.0 GB, 358 cell dirs).
Full-4 cells (`*_rfr*`) have the complete stack incl. `semfs_logs/` + `sandbox_raw/`; Phase-1 cells
(`*_rp1*`) have `agent.json` + `ranking_this_cell.log` (heavy logs stripped); evo in `.evo/run_0000/worktrees/`.
