# RCA 2026-06-27 — ppr_map run lost 41/52 cells to per-sandbox map-gen exit-2

**Component:** benchmarks/e2b (run_matrix / cell_driver, ppr_map arm) · **Severity:** high (silent ~79% arm loss) · **Status:** fixed (`c8dfe22`)

## Symptom
Full houqin n=2 run reported `ALL WORKERS DONE / exit=0`, but only **60/104 new cells** produced results. ppr_on 49/52 ✓; **ppr_map 11/52** — 41 missing. The `DONE` masked it (no completeness check existed).

## Misdiagnosis (recorded for honesty)
First glance at the error tail showed `StreamReset` → I blamed GLM instability and proposed PAR=4. **Wrong.** Categorizing all 44 errors:
- **39 × CommandExitException(exit_code=2, stdout='', stderr='')** — all ppr_map
- 3 × RemoteProtocolError (StreamReset) — minor GLM hiccup
- 2 × JSONDecodeError — minor

Only 3 StreamResets. GLM was never the bottleneck (later confirmed: KV-cache max 37% at PAR=12).

## Root cause
`ppr_map`'s only extra step vs `ppr_on` is generating the workspace map **in-sandbox, once per sandbox** (`test -f workspace_map.txt || python3 semfs_map.py <seed> --out ... 2>/tmp/map.err`, run_matrix.py). That gen exited 2 → `run_cell` raised before the agent ran → no result. Because the map is cached per-sandbox, a sandbox whose first gen failed lost **every** ppr_map cell on it (~5-6 of 8 sandboxes failed → 39 cells). Intermittent (~21% success), so not a hard bug — a resource/race in the per-sandbox gen (heavy: 2313 files / ~627K-relation KG). Exact byte-cause unrecoverable: stderr was redirected to `/tmp/map.err` inside now-dead sandboxes.

## Fix (`c8dfe22`)
- **Pre-ship a known-good map.** The map depends only on the *seed* (identical for every cell of a persona), so generating it 8× independently was needless risk. `boot_prep` now ships `WB_PRESHIP_MAP` → `/home/user/workspace_map.txt` once per sandbox → `test -f` passes → gen skipped entirely. Byte-identical map across cells (cleaner) + zero gen failures. Verified: 12/12 sandboxes pre-shipped, **exit-2 = 0** on the re-run (60/60 complete).
- **Capture, don't lose, the fallback error:** gen `2>/tmp/map.err` → `2>&1` so any future gen failure surfaces in the CELL ERROR log.
- **Completeness check** in the orchestrator: prints missing `(case,arm,rep)` after the run so `DONE` can't hide gaps again.

## Lessons
1. **Categorize ALL errors before diagnosing** — the loud one (StreamReset) wasn't the common one (exit-2). The first-8-lines glance cost a wrong PAR recommendation.
2. **Don't redirect diagnostic stderr to ephemeral storage** — `/tmp/map.err` in a killed sandbox is unrecoverable; merge to the captured stream.
3. **"DONE/exit=0" ≠ complete** — always assert expected cell count.
4. **Per-unit work that depends only on a shared input should be done once + shipped**, not regenerated per worker.
