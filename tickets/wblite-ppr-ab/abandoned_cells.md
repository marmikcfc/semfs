# Chanpin abandoned cells — PPR A/B run (2026-06-24)

During the PAR 8→16 concurrency bump, chanpin was **skipped on resume** (it was ~95% done and
its 2 slowest cases were blocking kaifa). Result: chanpin landed at **54/60 cells (90%)**.
The other 3 personas (kaifa/houqin/yunying) run complete at PAR=16.

NOTE: the "2" earlier were the rep1 **timeouts** (44, 95 ppr_off) — those ARE recorded
(`status=timeout`), NOT abandoned. The **6 abandoned** below have *no result row at all*.

| # | case | arm | rep | deliverable type | why it has no result |
|---|------|-----|-----|------------------|----------------------|
| 1 | 44  | ppr_off | r2 | `DevTask_Dashboard.html` | died mid-cell earlier — sandbox death/error before `run_cell` wrote its result row (no clean timeout) |
| 2 | 388 | ppr_off | r2 | `.pptx` | died mid-cell earlier — same (writer-lib/.pptx case, runs long) |
| 3 | 95  | ppr_off | r3 | `.doc` | **in-flight at teardown** — killed (kill -9 at 18:20) before result write |
| 4 | 95  | ppr_on  | r3 | `.doc` | in-flight at teardown |
| 5 | 386 | ppr_on  | r3 | `.pptx` | in-flight at teardown |
| 6 | 388 | ppr_on  | r3 | `.pptx` | in-flight at teardown |

## Pattern
All 6 cluster on the **hardest cases — 44, 95, 386, 388**: the two 33-min timeout cases
(44 dashboard, 95 `.doc` over-exploration) + the `.pptx` writer-lib cases (386, 388). These
run the longest, so they're the most exposed to (a) being mid-flight when the run was killed
for the PAR bump, or (b) a sandbox death without a clean timeout-result.

## Root causes
- **4 of 6 (all rep3):** caught in-flight by the `kill -9` teardown during the PAR 8→16 bump
  (ppr_on rep3 was actively running). `run_cell` appends to results.jsonl only at the END, so
  a killed cell leaves no row.
- **2 of 6 (rep2):** sandbox death / mid-cell error earlier. A *clean* timeout writes
  `status=timeout`; a sandbox that *dies* exits `run_cell` before the append → no row.

## Impact / backfill
A/B is still solid (ppr_off 27, ppr_on 27 judged chanpin cells). These 6 are a cheap backfill
(~6 cells, a few min at PAR=16) — run AFTER houqin/yunying complete, e.g.:
`WB_PERSONAS=chanpin PAR=16 bash tickets/wblite-ppr-ab/run_ppr_ab.sh` (done cells skip; only
these 6 re-run).
