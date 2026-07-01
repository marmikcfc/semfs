# RCA: semfs-codex graded "empty path list" — orphaned-mount teardown race

**Date:** 2026-05-27
**Host:** ubuntu@13.201.35.159 (EC2 i-0c491c7cc23de8555)
**Run:** `semfs-codex` smoke, PM case 289 (chanpin), reader flow `SEMFS_NO_PUSH=1`, seeded container `workspace-bench-chanpin`.
**Symptom:** `status=failed`, check `returned_paths_exist` → "Agent returned empty path list", despite the agent producing output. Previously **passed** under the old `smfs` binary (`layer2-fix-v2`, 2026-05-25).

## It is NOT the rebrand
The user asked "what changed with the rebrand that it stopped working." The `smfs`→`semfs`
**rename did not touch unmount/teardown logic.** The agent and semantic retrieval worked fine:
the agent wrote `model_output/best_selling_product_core_data_list.txt` and the semfs daemon made
**140 supermemory calls** (rehydrate/pull). What changed is the **run mode**, which exposed a
pre-existing race.

## Evidence chain
1. Agent succeeded — `raw/last_message.txt` = `['model_output/best_selling_product_core_data_list.txt']`.
2. `_stage_outputs_from_mount` captured it — `…/289/semfs_staged/model_output/best_selling_product_core_data_list.txt` exists.
3. Post-run the workdir was a **dead/orphaned FUSE mount** — `Transport endpoint is not connected`
   (ENOTCONN), `mountpoint=yes`, daemon already exited.
4. `fusermount3 -u <workdir>` cleared it cleanly when run **manually** (minutes later) — so the
   orphan is clearable; it just wasn't cleared during the run.
5. Grader read the dead-mount workdir → `os.path.isfile` fails → `returnedPaths=[]` → "empty path list".

## Root cause
`semfs unmount` tears the daemon down **asynchronously**; for a moment the workdir is an
in-transition / orphaned FUSE entry. The adapter's `_force_clear_mount` fired **one**
`fusermount3 -u` immediately after and **raced the teardown** — it didn't stick.
`_restore_outputs_to_workdir` then copied the staged output into the still-dead mount (ENOTCONN)
and silently skipped (its `except OSError: continue`), so the grader saw nothing.

**Why it passed before:** the old passing run used `--no-sync` (push **on** → ~30 s drain at
unmount → daemon lingered → teardown settled before `force_clear`) on an **unseeded** container
(fast). The new run uses **`--no-push`** (drain **skipped** → daemon exits immediately) on a
**seeded** container (~1,300 rehydrated docs → heavier teardown). That combination widened the
race window that the single-shot `fusermount3` couldn't reliably win.

## Fix
`_force_clear_mount` now **retries `fusermount3 -u` (then `fusermount -u`, `umount`) in a loop**
(up to ~20×1 s) until the path is a real directory, instead of one shot — waiting out the async
daemon teardown. Applied to both `benchmarks/workspace_bench/semfscodex.py` and
`semfsclaudecode.py`.

## Verification
Re-ran semfs-codex on case 289: **`status=passed`**, `returned_paths_exist` passed (count=2),
140 supermemory calls, workdir clean (force_clear retry cleared the orphan → restore succeeded).

## Still open (product follow-up, not this bug)
The orphaned-mount-on-unmount is a **Rust daemon** issue (clean kernel FUSE teardown on unmount
would make the adapter's `force_clear` shim unnecessary). The adapter now works *around* it
robustly; the real fix belongs in `semfs unmount`.
