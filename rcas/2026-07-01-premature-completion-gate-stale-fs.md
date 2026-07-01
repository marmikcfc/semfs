# RCA — premature completion gate (`entity>0` + stale fs) baked a half-built seed → bogus cells

**Date:** 2026-07-01
**Component:** `finish_dp013.sh` orchestrator (xAFS per-dp ppr completion) + `gen_xafs_dashboard.py`
**Status:** Fixed (gate hardened; dashboard filter added as defence-in-depth)
**Severity:** Medium — produced 3 invalid cells and a *false* "52/52 complete" signal.

## Symptom
`finish_dp013.sh` announced `DP013_PPR_DONE — MATRIX 52/52`, but the three dp_013 cells were garbage:
`tokens=0 calls=None correct=None` with nonsense answers. The matrix was **not** actually complete.

## Root cause (two compounding issues)
**1. Weak completion gate.** The script waited for the dp_013 seed to be "ready" using:
```bash
if [ "$ent" -gt 0 ] && [ "$fsd" -gt 0 ]; then break; fi   # entity>0 AND fs_data>0
```
Both conditions were satisfied *while the KG was still building*:
- `fs_data` was **stale** — dp_013 had `fs_data=123,068` left over from an earlier interrupted
  `phase=all` run (fs had been materialised before that run died).
- `graph_entity` crossed 0 the instant the new GPU KG build wrote its first batch (3,591 of an
  eventual ~40K entities).

So the gate fired mid-KG-build, `finish_dp013` baked a seed with a **partial KG + stale fs/communities**,
and ran the agent against it → the cells errored/produced garbage.

**2. Underlying flakiness.** The CPU `build_corpus_seed` orchestrator runs on preemptible Modal workers
and was preempted mid-KG (the GPU vLLM is fast + persistent, but the *caller* is not). `build_kg` resumes
from its checkpoint, but the seed spends a long window in a valid-looking-but-incomplete state — exactly
what a `>0` gate mistakes for "done."

## Fix
1. **Gate on KG *stability*, not presence** (`finish_dp013_v2.sh`): poll until the entity count is stable
   across reads **and** ≥15K (sanity for a 9,988-file corpus) **and** communities>0 (finalize ran on the
   full KG) **and** fs_data>100K — or the build prints its final `"out_seed"` completion dict. Abort-guard
   before baking if the seed still looks incomplete.
2. **Defence-in-depth in the dashboard** (`gen_xafs_dashboard.py`): an `is_real(r)` filter counts a cell
   only if `status != "error"` AND `tokens > 0`. The bogus 0-token cells are dropped → shown as pending,
   re-runnable, and never pollute accuracy. (This also caught an earlier miscount: error records were
   written with lowercase `status="error"` while the filter checked uppercase `"ERROR"`.)

## Lessons
- "Exists / >0" is not "complete." For an incrementally-built artifact, gate on **stability + a sanity
  threshold + a positive completion signal**, never on the mere presence of rows.
- **Stale state from a prior interrupted run is a trap** — a leftover `fs_data` made a partial seed look
  finished. Verify the field that reflects *this* run's progress (the thing being actively written), not a
  field that could be carried over.
- Make the downstream consumer (dashboard) robust to bad upstream records, so a gate bug degrades to
  "pending" not "silently wrong."

## Related
`rcas/2026-06-20-sqlite-corruption-incremental-commit-modal-preemption.md` (preemption + incremental
commit). GPU: gemma-4-31b-nvfp4 vLLM redeployed `GEMMA_MIN=1` (persistent). SEM-47, `tickets/wb-xafs-ppr-ab/`.
