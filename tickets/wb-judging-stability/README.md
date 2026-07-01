# WB-Lite judging stability — SEM-42

**Linear:** [SEM-42](https://linear.app/semfs/issue/SEM-42) (High) · **Related:** SEM-40 (PPR/map) · **Source:** Codex review of the variance analysis, 2026-06-27.

## Problem
WB-Lite run-to-run variance is **dominated by the judging pipeline, not the agent**. From the houqin map run (`../wblite-ppr-ab/artifacts/map_smoke_glm/`):
- **Truncation / evidence starvation** — 29/112 judge files cite truncated excerpts in *failing* rubrics → 20/57 rep-pairs → **~80% of total rep variance**. The judge false-fails what it can't see.
- **Synthetic zeros** — valid deliverable + `status=ok` but score 0 with no judge artifact (e.g. `358-ppr_on-r2`, ~40pp).
- **Execution-artifact zeros** — timeouts folded into accuracy as 0 (~16–27%).
- Effect this swamps: arm Δ +0.46pp, 95% CI [−2.6, +3.5]; per-cell SD ~8–10pp.

## Plan
- **P0:** remove judge input truncation (`benchmarks/e2b/run_judge.py` + prompt builder) → judge sees full deliverable; treat "cannot verify/truncated" as non-terminal (re-judge), not a fail.
- **P1:** never zero a judge-eligible cell — `unjudged` + retry; distinguish no-deliverable / judge-failed / timeout. Retry + hash-cache for idempotent re-judge; post-run audit of suspect zeros.
- **P2:** report accuracy raw vs excluding-artifact-zeros; judge-consistency probe.

## Done when (original premise — partly wrong)
"Drop per-cell SD toward <5pp." **This premise was wrong:** variance is agent-side (find-vs-miss), NOT judge truncation — see results.

## RESULTS (2026-06-27) — P0 was the whole lever
- **P0 (truncation 2000→100K chars in `agent_eval.py`):** +~10pp on EVERY arm (re-judged existing deliverables, GPU-free). houqin plain 17.7→28.8 · ppr_off 9.6→17.5 · ppr_on 9.2→19.8.
- **Variance unchanged** (6.7→6.5pp) → truncation was a systematic *bias*, not the variance source. Codex's "~80% of variance" was correlation.
- **P1 (synthetic-zeros): already solved** — 0 deliverable-but-unjudged cells after P0; `rejudge_loop` excludes unjudgeable cells + scores no-deliverable=0 correctly. No code change needed.
- **P2: clean reporting +0.5–2pp** (exclude timeouts/no-deliverable, ordering unchanged) + **judge determinism confirmed (6/6 reproducible)**.
- **Corrected houqin verdict:** `plain 28.8 > ppr_on 19.8 > ppr_off 17.5` — plain still wins, by MORE than the original. Map parked (+53% tokens).

## Status for next session
- **Code UNCOMMITTED:** `agent_eval.py` (P0), `rejudge_loop.py` (filter), `build_comparison.py` + `comparison.html`.
- Optional: persist P2 raw/clean + post-run judge audit (P1.7) in the dashboard.
- The real remaining noise is **agent-side variance → needs n≥5–10** (separate from judging).

See SEM-42 for the full writeup.
