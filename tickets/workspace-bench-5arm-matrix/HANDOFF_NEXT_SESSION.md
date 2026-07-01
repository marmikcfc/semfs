> <!-- STALE-BANNER --> ⚠️ **HISTORICAL HANDOFF (2026-06-25)** — point-in-time session handoff; its blockers/next-steps are closed. Current state → [/CURRENT_STATE.md](../../CURRENT_STATE.md).

# Handoff — Next Session

## Current State

- The E2B template rebuild is **done** and the live `semfs-baked-v2` alias was updated on Modal.
- The template now bakes:
  - `/opt/corpus.tgz`
  - `/opt/chanpin-gemma-q4.db`
  - `/opt/chanpin-clean.db`
  - `/opt/chanpin-leanhint3.db`
- The Modal-side build path lives in [benchmarks/modal/semfs_modal.py](../../benchmarks/modal/semfs_modal.py).
- The E2B harness was updated in [benchmarks/e2b/run_matrix.py](../../benchmarks/e2b/run_matrix.py) and [benchmarks/e2b/cell_driver.py](../../benchmarks/e2b/cell_driver.py) to support:
  - `plain`
  - `best`
  - `hiddenkg`
  - arm-specific seed selection
  - `SEMFS_SEARCH_ONLY=off`
  - preflight seed checks

## What Was Verified

- Fresh E2B sandbox preflight now gets past seed inventory.
- The new template contents are present in the sandbox.
- The rebuild path no longer uses local staging disk.

## Current Blocker

- Preflight fails later on the mount check:
  - `surface contamination persists for arm=best; rebuild or replace the seed`
- This means the remaining task is **seed surface cleanliness**, not template plumbing.
- The next investigation should inspect exactly what `best` still exposes on mount:
  - `/kg`
  - root hint files like `AGENTS.md` / `CLAUDE.md`
  - any other surfaced derived artifacts

## Next Task

1. Inspect the `best` mount surface and identify the offending artifact.
2. Decide whether to:
   - rebuild `chanpin-leanhint3.db` as a surface-clean seed, or
   - relax the contamination check if the artifact is harmless and not agent-visible in practice.
3. Rerun the same preflight until it passes:
   ```bash
   python3 benchmarks/e2b/run_matrix.py --preflight --arms best,hiddenkg --knobs benchmarks/e2b/knobs/best_exp0002.json
   ```

## Experiments To Run

After preflight passes:

1. Cheap validation
   - cases: `53,171`
   - arms: `plain,best,hiddenkg`
   - reps: `n=1`

2. Real experiment
   - same arms
   - increase reps only after the validation run is clean

3. If the proxy arm still looks ambiguous
   - compare `best` vs `hiddenkg` vs `plain`
   - decide whether current `SEMFS_COMENTION=on` is enough to justify the hidden-KG proxy, or whether product work is needed for a true hidden KG mode

## Important Files

- [CURRENT_STATE.md](../../CURRENT_STATE.md)
- [HIDDEN_KG_EXPERIMENT_PLAN.md](HIDDEN_KG_EXPERIMENT_PLAN.md)
- [benchmarks/modal/semfs_modal.py](../../benchmarks/modal/semfs_modal.py)
- [benchmarks/e2b/run_matrix.py](../../benchmarks/e2b/run_matrix.py)
- [benchmarks/e2b/cell_driver.py](../../benchmarks/e2b/cell_driver.py)

