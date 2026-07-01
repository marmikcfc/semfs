# PPR A/B — hidden-KG Personalized PageRank vs 1-hop neighbour prior

_Status: **RUNNING** (resumed 2026-06-25). Living doc. Companion to `../../CURRENT_STATE.md` and Linear team `SemFS`._

## Question

Does replacing the hidden-KG **1-hop neighbour boost** with a **multi-hop Personalized PageRank (PPR)**
diffusion over the file↔entity graph improve agent accuracy (and at what token cost), on WB-Lite, across
4 personas? Both arms are otherwise the **identical `hiddenkg_l7` config** — the ONLY difference is the
graph-prior algorithm, so any delta is attributable to PPR.

## Arms (identical except `SEMFS_KG_PPR`)

| arm | graph prior | flag |
|---|---|---|
| `ppr_off` | 1-hop bounded neighbour boost (control) | `SEMFS_KG_PPR=off` |
| `ppr_on` | in-memory Personalized PageRank diffusion (treatment) | `SEMFS_KG_PPR=on` |

Both arms: KG **off-surface**, `SEMFS_COMENTION=on`, `SEMFS_HIDDEN_KG=on`, retrieval-pool injection off.
PPR (`crates/semfs-core/src/backend/hidden_kg.rs`): undirected bipartite adjacency from `edges`
(`from_path`=file, `to_path`=entity), seeded at matched entities, power-iterated
`r = restart·seed + (1−restart)·Â·r`, max-normalised per candidate file, capped at `PPR_CAP=0.12`.
Env: `SEMFS_PPR_RESTART=0.5`, `SEMFS_PPR_ITERS=30`. 12/12 Rust tests.

## Setup

- **Corpus / personas:** WB-Lite, 4 personas — chanpin (PM, 10 cases), kaifa (backend-dev, 11),
  houqin (logistics, 30), yunying (ops, 31). **82 cases × 3 reps × 2 arms = 492 cells.**
- **Seeds:** per-persona `<persona>-gemma-q4.db` (full FUSE tree + uniform Gemma KG), baked into E2B
  templates `semfs-mount-{persona}`. See `CURRENT_STATE.md` → Seeds.
- **Agent:** codex on self-hosted **GLM-5.1-NVFP4** (Modal vLLM, 4×B200, behind a litellm proxy).
- **Env:** **E2B real-FUSE** (hard rule: all semfs benchmarks run on E2B, never Modal). Binary:
  `benchmarks/e2b/assets/semfs-fixed` (x86_64, PPR compiled in).
- **Knobs** (`benchmarks/e2b/knobs/ppr_ab.json`): adaptive-K (pool 10), input-compress (gpt-4.1-nano)
  + output-compress + dedup (W5) + turnbrake; instruction-less flagless-grep prompt.
- **Judging:** Seed-2.0-Lite via OpenRouter (GPU-free) — inline per cell + a live re-judger
  (`rejudge_loop.py`) that also scores **no-deliverable cells as 0/total** (honest failures).

## Harness — the queue rewrite (2026-06-24)

The first run used **one `run_matrix` invocation per (persona, arm, rep)** = 24 invocations, each paying a
~10-min sandbox boot + an end-of-invocation long-tail barrier (one slow straggler blocked the rest), and
`run_cell` re-mounted on **every** cell.

The queue harness (`run_ppr_ab_queue.sh` + `run_matrix.py --reps` / `worker_batch`) replaces that with
**ONE invocation per persona**: a global queue of all (arm, rep, case), persistent workers that **boot
once** and **re-mount only on arm change** (arm-ordered queue → ≤1 remount/worker), no per-arm barrier.
Plus a **network-resilience deadline wrapper** (`_with_deadline`) so a dead socket fails fast instead of
wedging the run ~20 min. Net: 24→4 boots, no long-tail barrier, no per-cell remount → ~1.5–2× faster on
the big personas. Resume-safe (done cells skip; auto-backfills abandoned cells).

**Monitoring:** `python3 tickets/wblite-ppr-ab/mon.py` (snapshot) · web dashboard
`benchmarks/e2b/dashboard.py` (http://127.0.0.1:8765 — 3-way headline, per-arm summary, per-persona
3-way, persona×arm grid, per-case bifurcation).

## Status (as of 2026-06-25)

- **229 / 492 cells done** (chanpin 62 complete, kaifa 67 complete, houqin 100/180 partial, yunying 0/186).
- Everything that ran **is judged** (graded deliverables + no-deliverable cells scored 0/total).
- Paused once to stop the GPU; **resumed** on the queue harness (done cells skip).

## Preliminary findings — PERSONA-DEPENDENT, NOT a verdict

Per-persona 3-way (plain baseline n=3 vs ppr_off vs ppr_on), accuracy:

| persona | plain | ppr_off | ppr_on | read |
|---|---|---|---|---|
| chanpin | 5% | 7% | **11%** | PPR wins (+6 pp) |
| kaifa | 18% | 19% | 18% | neutral |
| houqin (partial) | **17%** | ~10% | ~10% | plain currently leads — but incomplete + fail-weighted |
| yunying | 20% | — | — | pending |

**The aggregate hides the persona split.** The early chanpin+kaifa read favoured PPR; once houqin entered
(where plain currently leads) and no-deliverable cells were scored as fails (7 of 8 silent failures were
ppr_off), the matched-set aggregate flipped toward plain (plain 15% ≥ ppr_on 13% > ppr_off 11%).

**Caveats (why this is not a verdict):**
1. houqin/yunying incomplete; the arms are **unmatched in n** (arm-ordered queue ran ppr_off ahead of
   ppr_on, so the pause left ppr_off further along).
2. houqin ppr_off ~10% is dragged by several no-deliverable 0-scores on a partial denominator.
3. A clean call needs houqin + yunying complete with matched n.

## Known issues / abandoned cells

- **No-deliverable cells** (timeouts + silent no-output, e.g. case 386 `.pptx`) scored 0/total as honest
  fails. See `abandoned_cells.md` for the original chanpin gaps (auto-backfilled by the queue resume).
- **Plain-baseline contamination** (fixed in the dashboard loader): the shared plain dir
  (`../workspace-bench-5arm-matrix/artifacts/e2b_runs`) had old experimental rep labels
  (`rP1p`/`ra1p1`/`rdiag`/…) polluting the baseline; restricted to clean r1/r2/r3.

## Resume / re-run

```bash
# resume (deploys GLM cold ~5-10min, warms, resumes; done cells skip):
bash tickets/wblite-ppr-ab/run_ppr_ab_queue.sh
# resume with GLM already warm:
WB_SKIP_GLM_DEPLOY=1 bash tickets/wblite-ppr-ab/run_ppr_ab_queue.sh
# single persona:
WB_PERSONAS=houqin bash tickets/wblite-ppr-ab/run_ppr_ab_queue.sh
```

Artifacts: `tickets/wblite-ppr-ab/artifacts/e2b_runs/` (results.jsonl, judged.jsonl, per-cell dirs).
GPU is fenced — the run auto-stops `glm51-nvfp4-vllm` on exit. Track tokens, **not $** (per metric directive).
