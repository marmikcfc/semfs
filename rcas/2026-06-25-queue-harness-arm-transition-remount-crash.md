# RCA — Queue harness lost 125 ppr_on cells to in-place arm-transition re-mount crash

- **Date:** 2026-06-25
- **Component:** benchmarks/e2b queue harness (run_matrix.py `worker_batch` + `ensure_mount_for_arm`)
- **Severity:** High — 125/492 cells (mostly `ppr_on`) silently dropped to `infra_fail`; experiment inconclusive for 1 of 4 personas (yunying).
- **Status:** Root-caused; fix shipped (per-persona-per-arm batches + 3× mount-retry); backfill of ~129 cells launched.

## Impact
PPR A/B (WB-Lite, 4 personas, codex on self-hosted GLM-5.1-NVFP4, n=3). After the queue-harness
resume completed:

| persona | ppr_off | ppr_on | note |
|---|---|---|---|
| chanpin | 32/30 ✅ | 30/30 ✅ | small seed |
| kaifa | 34/33 ✅ | 33/33 ✅ | small seed |
| houqin | 91/90 ✅ | **52/90** ⚠ −38 | 1.24 GB seed |
| yunying | 87/93 ⚠ −6 | **8/93** ⚠ −85 | big seed, ran entirely on queue resume |

Total **125 cells `infra_fail`** (no result row). Every `ppr_off` arm completed; every `ppr_on`
arm was hit. Failures scale with seed size.

## Mechanism (data + code path)
The queue harness (`worker_batch`) boots a sandbox ONCE and dequeues an **arm-ordered** queue
(all `ppr_off`, then all `ppr_on`). When a worker crosses the boundary it does an **in-place
re-mount**: `run_cell(..., remount=(arm != cur_arm))` → `ensure_mount_for_arm()` =
`unmount_semfs` → `reset_runtime_seed` (re-copy ~1.24 GB seed) → `do_mount` (new daemon loads
seed; `WB_SEARCH_ONLY=off` → full tree in memory).

On the big houqin/yunying seeds that re-mount crashes: `daemon exited before becoming ready
(exit status 1)`. Strongly-supported cause: the transition transiently needs ~2× RAM (the old
`ppr_off` daemon's seed not fully released as the new `ppr_on` daemon loads the big seed) on a
RAM-limited E2B sandbox → new daemon OOM/exit-1. (Exact exit reason unconfirmed — the crashed
sandboxes were killed and infra_fail cells never pulled a daemon log; but the correlation is
definitive.)

**Cascade:** on the crash, `run_cell` returns `infra_fail_mount` and `worker_batch` sets
`cur_arm=None`. The next `ppr_on` cell therefore re-mounts AGAIN (`ppr_on != None`) → crashes
again. One crashy sandbox loses its **entire** ppr_on workload — hence ~90% loss on yunying.

## Why ppr_off never failed
`ppr_off` is the first arm → mounted at **fresh boot** (one daemon, fits RAM, no transition).
Per-arm fresh-boot mounts succeeded everywhere; only the **in-place transition** crashed.

## Why it slipped through
1. **Symptom mislabeled.** The `daemon exited before becoming ready` monitor events fired hours
   earlier and were assessed as "benign self-healing." That is true for *same-arm* cells (the
   per-cell SEM-35 health gate re-mounts on the next cell) but FALSE on the *arm transition*,
   where a crash → immediate `infra_fail` with no retry. The two cases were not distinguished.
2. **Small seeds ran first and masked it.** chanpin → kaifa → houqin → yunying. The harness
   "proved" itself on small seeds before the big ones exposed the flaw.
3. **The headline feature was the bug.** "Re-mount in place instead of re-booting per arm" is
   what made the harness fast — and is exactly what could not survive a big-seed transition.
4. **No retry on this run's binary.** The 3× mount-retry was added after the run had started, so
   a single re-mount failure was terminal for the cell.

## Fix
- **Per-persona-PER-ARM batches** (`run_ppr_ab_queue.sh`, `WB_ARMS` loop): each batch boots fresh
  and mounts ONE arm (all 3 reps in the queue), so there is **no in-place arm transition**.
  Still collapses reps (8 boots vs the old 24), but mounts exactly like the ppr_off case that
  never failed. Resume-safe (done cells skip).
- **3× mount-retry** in `run_cell` (`ensure_mount_for_arm` loop) as backup for the residual
  fresh-boot mount flakiness on big seeds (yunying ppr_off lost 6 even fresh).

## Prevention / follow-ups
- Treat `daemon exited before becoming ready` on an **arm transition** as a hard failure signal,
  not noise (distinct from same-arm health-gate recovery).
- Consider `SEMFS_SEARCH_ONLY=on` for >1 GB seeds if RAM-during-mount stays marginal.
- Pull the daemon log for `infra_fail` cells (currently lost) so the exact exit reason is
  capturable next time.
