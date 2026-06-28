> <!-- STALE-BANNER --> ⚠️ **SUPERSEDED (2026-06-25)** — this hidden-KG design SHIPPED as code (`crates/semfs-core/src/backend/hidden_kg.rs`, `SEMFS_HIDDEN_KG` / `SEMFS_KG_PPR`). Kept for design history. Current → [/CURRENT_STATE.md](../../CURRENT_STATE.md) · [PPR EXPERIMENT.md](../wblite-ppr-ab/EXPERIMENT.md).

# Hidden-KG Experiment Plan

Goal: make the E2B harness clean enough to run the 3-arm experiment:

1. `plain`
2. `best_exp0002`
3. `best_exp0002 + hidden internal KG only`

## Current reality

The third arm does **not** exist as a full product mode yet.

What we can run now is a **proxy**:

- keep surfaced KG off
- keep graph-FS off
- keep current post-rerank co-mention on

This tests the present internal graph nudge (`SEMFS_COMENTION`), not the fuller hidden-routing KG design.

## Current blocker

The Modal-side `semfs-baked-v2` rebuild is now complete and the template includes:

- `/opt/corpus.tgz`
- `/opt/chanpin-gemma-q4.db`
- `/opt/chanpin-clean.db`
- `/opt/chanpin-leanhint3.db`

The current preflight now gets past seed inventory and fails later on:

- `surface contamination persists for arm=best; rebuild or replace the seed`

So the next task is not template plumbing. It is to inspect what `best` still exposes
on mount and decide whether that should be:

1. rebuilt out of `chanpin-leanhint3.db`, or
2. tolerated because it is benign and does not affect the experiment contract.

## Arm contract

### `plain`
- no semfs mount
- raw tree only

### `best`
- use `benchmarks/e2b/knobs/best_exp0002.json`
- seed:
  - default `WB_E2B_SEED_BEST=/opt/chanpin-leanhint3.db`
  - override per run with `WB_E2B_SEED_BEST` or `WB_E2B_SEED_DEFAULT`
- mount env:
  - `SEMFS_KG=off`
  - `SEMFS_COMENTION=off`
  - `SEMFS_GRAPH_FS=off`
  - `SEMFS_SEARCH_ONLY=off`
- hard-fail if surfaced KG artifacts remain after mount

### `hiddenkg`
- use `benchmarks/e2b/knobs/best_exp0002.json`
- seed:
  - default `WB_E2B_SEED_HIDDENKG=/opt/chanpin-clean.db`
  - override per run with `WB_E2B_SEED_HIDDENKG` or `WB_E2B_SEED_DEFAULT`
- mount env:
  - `SEMFS_KG=off`
  - `SEMFS_COMENTION=on`
  - `SEMFS_GRAPH_FS=off`
  - `SEMFS_SEARCH_ONLY=off`
- hard-fail if surfaced KG artifacts remain after mount

## Why the old harness was not clean

1. The daemon was mounted with shared env, not arm-specific env.
2. The harness still forced `SEARCH_ONLY=on`, while the runbook requires `off`.
3. The canonical seed may contain baked `/kg` and root hint files.
4. `SEMFS_COMENTION` is a separate switch, so `SEMFS_KG=off` alone is not a full KG-off control.

## Changes made

1. `run_matrix.py`
- added supported arm names `best` and `hiddenkg`
- made semfs remount per cell, per arm
- restored the runtime seed from an arm-specific seed source before each semfs mount
- switched mount default to `SEMFS_SEARCH_ONLY=off`
- added KG surface cleanup for surface-off arms
- added explicit seed inventory checks and seed-path overrides:
  - `WB_E2B_SEED_BEST`
  - `WB_E2B_SEED_HIDDENKG`
  - `WB_E2B_SEED_DEFAULT`
- added `--preflight` to inspect mount state and one grep before expensive runs
- made contamination a preflight error instead of a warning

2. `cell_driver.py`
- added `best` and `hiddenkg` arm handling
- stopped forcing `SEARCH_ONLY=on`
- made agent-side semfs env consistent with the mount contract

## What still remains if we want the true ideal hidden-KG arm

1. Separate `KG_INTERNAL` from `KG_SURFACE` in product code.
2. Let internal graph routing/priors exist without materializing `/kg` or root hint files.
3. Build the real query-to-community / entity-expansion path.

## Recommended run order

1. Preflight:
   - `python3 benchmarks/e2b/run_matrix.py --preflight --arms best,hiddenkg --knobs benchmarks/e2b/knobs/best_exp0002.json`
2. Cheap validation:
   - cases `53,171`
   - arms `plain,best,hiddenkg`
   - `n=1`
3. Real experiment:
   - same arms
   - increase reps only after the preflight and validation look clean

## External prerequisite still needed

The harness is now ready to enforce the right seed contract, and the template has been rebuilt.

What still may need another pass is the `best` seed surface cleanup.

If we decide to rebuild or replace the seed, use the same template path and, if needed,
point the harness at alternate in-sandbox paths with:

- `WB_E2B_SEED_BEST=...`
- `WB_E2B_SEED_HIDDENKG=...`

Modal is the right place to source or rebuild them if local disk is tight.
