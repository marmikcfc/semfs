# RCA: semfs-codex benchmark case `failed` — empty returnedPaths + deliverable lost on unmount

**Date:** 2026-05-25
**Host:** ubuntu@REDACTED_BENCH_HOST (EC2 ip-172-31-46-24)
**Benchmark:** Workspace-Bench smoke, `SEMFSCodex--GPT-5.4--Smoke-SEMFS`, case 100
**Run:** `_telemetry/vendored-fix-dfa-semfs-codex-owned2-20260525T163819Z-semfs-codex-smoke` (16:38Z)
**Symptom:** `status=failed` (graded), not `error`. Model ran fine (99,204 tokens, 154s, mount/unmount exit 0) but the single grading check failed.

> This is a **distinct** failure from `2026-05-25-semfs-bench-orphaned-fuse-mount.md`. That one was infra
> (`status=error`, mount failed over a stale FUSE entry). This one is **post-mount**: the mount
> succeeded, the model did the task, and the case still scored `failed`.

## Symptom (from run artifacts)

```
Status summary: {total:1, passed:0, failed:1, error:0, timeout:0}
Case 100: status=failed, prompt=92208 completion=6996 total=99204, 154395 ms
  check: returned_paths_exist → passed=FALSE, detail="Agent returned empty path list"
  returnedPaths: []   outputManifest: []   retrievalMethod: []
  lastAssistantMessage: "['model_output/onsite_hosting_execution_manual.doc']"
  semfs: mountExitCode=0 (16592ms), unmountExitCode=0 (33270ms)
```

The case has exactly one check (`returned_paths_exist`); it fired on an empty list.

## Root Cause — ONE cause (Layer 2). "Layer 1" is cosmetic.

> **Correction (verified against a passing run):** an earlier draft of this RCA called the empty
> `returnedPaths` the "proximate" cause. That is wrong. The most recent **plain** codex run
> (`vendored-fix-dfa-codex-20260525T162217Z`, 16:22) **PASSED** with `Returned paths: []` and
> `returned_paths_exist passed=True`. Identical empty `returnedPaths`, opposite outcome — so the
> empty list is **not** what fails the grade. The single variable that flips pass→fail is whether the
> deliverable file **physically exists in `work_dir` at grading time**.

### Why `returnedPaths` is irrelevant to the grade
The grade is driven by `output_paths` in `agent_runner.py` (`final_status = "passed" if output_paths
else "failed"`, L603). `output_paths` comes from `_collect_output_paths`, whose first method
**re-parses the path list out of `trace.lastText` itself** (L329) — independent of `run_res["paths"]` —
then gates every candidate on `os.path.isfile()` (L334). The `returnedPaths` field (L655) is only a
*report*, sourced from `run_res["paths"]`, which `agents/codex.py:731` hardcodes to `[]`. It is `[]` in
**both** the passing plain run and the failing semfs run. Fixing it would not change the grade.

### Layer 2 (the sole cause): deliverable doesn't survive unmount
Post-run, post-unmount state of the workdir
(`evaluation/filesys/houqin_workdir_Codex_GPT-5.4`):
- Workdir is **NOT a mountpoint**.
- `model_output/` **does not exist**; underlying dir is the bare fixture (`Desktop/`, `Documents/`, …).
- `diff_run.json`: `created=0, deleted=3838` — nothing created; the 3838 mounted files vanished.

The agent wrote `model_output/...` **into the semfs FUSE mount**. On unmount the mount tears down and the
workdir reverts to the bare host directory — the deliverable is gone from the local POSIX view. The
benchmark grades the host directory **after** unmount, so a correctly-extracted path would still fail
the existence check.

This is the live consequence of semfs's storage model: writes through the mount land in semfs's own store
(SQLite cache + cloud push queue), **not** in the underlying host directory. The benchmark assumes a
plain POSIX workspace where deliverables persist in the workdir; that assumption breaks under a FUSE
mount torn down before grading.

### Contributing: push queue never drains (carryover from prior RCA)
`semfs list` showed leftover daemons holding **QUEUE=3514 and QUEUE=3405** against this same workdir —
the `401` org-scoped push failure documented in the orphaned-mount RCA. Deliverables are stuck in the
local semfs cache, never pushed, never materialized to the graded directory.

## Evidence trail
- `run_narrative.md` / `run_narrative.json` (case 100): status, checks, returnedPaths, lastAssistantMessage.
- `executionSummary.tools`: 12 entries, all null → event-parser schema mismatch.
- `diff_run.json`: `created=0, deleted=3838`.
- Live `ls` of workdir post-unmount: no `model_output/`, not a mountpoint.
- `semfs list`: two stale daemons, QUEUE 3514 / 3405 on the workdir.

## Fix applied (Layer 2 only)
Stage-before-unmount / restore-after-unmount shim in `benchmarks/workspace_bench/semfscodex.py`:
- New `_stage_outputs_from_mount(...)`: while the mount is live, copy the run's deliverables OUT of the
  mount into `sandbox_dir/semfs_staged/<rel>`. Discovery mirrors `_collect_output_paths` — the union of
  paths parsed from `trace.lastText`, `result["paths"]`, the implied output subtree(s) (e.g.
  `model_output/`), and expected filenames. Only the implied subtrees are walked (never the whole
  ~3,800-file memory mount); bounded by `_MAX_STAGED_FILES`/`_MAX_STAGED_BYTES`.
- Restructured run/unmount: stage (in `try`, fail-open) → unmount (`finally`) → restore into the now-bare
  `work_dir` → set `result["paths"]` to the restored absolute paths.
- New `_restore_outputs_to_workdir(...)`: copies staged files back, rejects path escapes.
- New `_force_clear_mount(...)` + `_path_is_dead_or_mounted(...)`: **required wrinkle** — `semfs unmount`
  leaves an orphaned kernel FUSE entry (daemon gone, mount still registered → ENOTCONN; reproduces RCA
  #1's recurring bug, and `SEMFS_NO_SYNC=1` does *not* prevent it). The first fix attempt staged
  correctly but restore wrote into the *dead* mount and silently skipped → still `failed`. Fix:
  between unmount and restore, detect the orphan and `fusermount3 -u` (fallback `fusermount -u` /
  `umount`) before restoring. This also leaves the host with no orphaned mount after the run.
- Tests: `benchmarks/workspace_bench/test_semfscodex_staging.py` (extract/escape/roundtrip/no-op/clear).

## Verification (e2e on host, no stubs)
- `DATASET=smoke SKIP_PREPARE=1 SEMFS_NO_SYNC=1 …/run_workspace_bench.sh semfs-codex`
- First attempt (`layer2-fix-verify-…173327Z`-pre-force-clear): staged OK but restore hit ENOTCONN →
  still `failed` → led to the force-clear fix.
- Final run (`layer2-fix-v2-20260525T173327Z`): **`passed=1, failed=0`**; `returned_paths_exist`
  passed (count=1); `returnedPaths=['model_output/onsite_hosting_execution_manual.doc']`;
  `retrievalMethod` = all four (incl. `task_target_output_dir`); `outputManifest` = the .doc (15,278 B).
  Post-run: no orphaned mount, no active daemons.

**Layer 1 deliberately NOT changed.** `agents/codex.py:731` still returns `paths: []`; this is cosmetic
(plain codex passes with it empty). Populating it is optional trace-fidelity polish and would touch the
vendored tree, so it's out of scope for the fix.

## Still open (not this bug)
- **Drain/auth `401`.** Org-scoped push still fails → queues never drain → orphaned-daemon risk (see the
  orphaned-mount RCA). Run benches with `SEMFS_NO_SYNC=1` / `--no-sync`, or fix the API-key scope.
- **Product-level fix.** The clean long-term answer is semfs write-through / flush-to-underlying-dir on
  unmount (Rust crate), which would make the staging shim unnecessary. Filed as a separate task.

## Cleanup performed this session
Killed two stale debug daemons holding undrained queues on the workdir
(`semfs-debug-fixed-1779726852` PID 37361, `semfs-debug-houqin-1779726429` PID 31114) and removed the
kernel FUSE entry so the next run starts clean. (See session log / commands.)
