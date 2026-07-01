> <!-- STALE-BANNER --> ⚠️ **HISTORICAL HANDOFF (2026-06-25)** — point-in-time session handoff; its blockers/next-steps are closed. Current state → [/CURRENT_STATE.md](../../CURRENT_STATE.md).

# HANDOFF — WB 5-arm matrix → analysis & optimization (next session start here)

> You're picking up a **completed** Workspace-Bench benchmark of semfs vs a plain codex baseline.
> The data is in; your job is **analysis + optimization**. This doc is self-contained — read it,
> then [`issue.md`](issue.md) for full detail. Date of run: **2026-06-10**.

---

## 1. What happened (60-second version)

We ran a **5-arm × 5-case matrix** (25 runs) on EC2 to test: *does semfs reduce an agent's tokens
while maintaining accuracy?* Agent = codex / GPT-5.4. Judge = Seed-2.0-Lite rubrics.

**Arms (capability ladder):** `plain` (no semfs) · `nokg` (local search+grep-inline) · `gfs_off`
(+KG) · `gfs_on` (+/by-topic/) · `cloud` (Supermemory cloud search).

**Result — hypothesis NOT supported as configured:**

```
arm       accuracy (Σpass/Σtot)   mean tokens   note
plain     33/71 = 46%             89,270        best on BOTH axes
cloud     19/71 = 27%             93,443        only competitive arm; won case 95 only
gfs_off    7/71 = 10%            177,711
gfs_on    10/71 = 14%            471,217        worst tokens (/by-topic/ inflates crawling)
nokg       2/71 =  3%            247,300        + 1 timeout (45 min)
```

**The one lever that matters: index quality (summaries vs raw chunks)** — NOT the KG/by-topic
knobs. `cloud`'s container has `.extracted.md` summaries indexed → it's the only arm in plain's
league, and it *beat* plain on the high-ceiling case 95 (12/12 vs 11/12, 6 tool-calls vs 14). But
cloud scored 0/12 on case 175 → its edge is **coverage-dependent** (cloud container ~74% coverage).
Local raw-chunk semfs (gemma-q4 seed) scored 0 on both synthesis cases (95, 175).

Per-cell table + 8 detailed findings: [`issue.md`](issue.md) §4–5.

---

## 2. Where everything is

| what | path |
|---|---|
| **Full ticket** (methodology, results, findings, caveats) | [`issue.md`](issue.md) |
| **Per-run artifacts** (all 25: codex traces, judge json, deliverables, diffs, timing) | [`artifacts/run5arm/<case>_<arm>/`](artifacts/) |
| **Raw results** (one JSON line/run) | [`artifacts/pm_results_5arm.jsonl`](artifacts/pm_results_5arm.jsonl) |
| **Drivers** (single-run + matrix) | [`artifacts/run_case_fixed.sh`](artifacts/run_case_fixed.sh), [`artifacts/run_pm_matrix_5arm.sh`](artifacts/run_pm_matrix_5arm.sh) (also live on box at `/tmp/`) |
| **Harness architecture** (arm ladder, data flow, contamination model, diagrams) | [`../../benchmarks/workspace_bench/BENCH_ARCHITECTURE.md`](../../benchmarks/workspace_bench/BENCH_ARCHITECTURE.md) |
| **Every env knob** (core + harness, with defaults) | [`../../benchmarks/workspace_bench/KNOBS.md`](../../benchmarks/workspace_bench/KNOBS.md) |
| **How to run / box access / gotchas** | [`../../benchmarks/workspace_bench/EC2_RUNBOOK_CURRENT.md`](../../benchmarks/workspace_bench/EC2_RUNBOOK_CURRENT.md) |
| **Core product arch** (incl §6 delivery layer: grep-inline, siblings, /kg/, hint) | [`../../docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) |
| **Cloud container state** (workspace-bench-chanpin verified searchable) | [`../../benchmarks/workspace_bench/cloud_env_state.md`](../../benchmarks/workspace_bench/cloud_env_state.md) |
| **Seed coverage** per container | [`../../benchmarks/workspace_bench/seed-coverage.md`](../../benchmarks/workspace_bench/seed-coverage.md) |

Full uncurated artifacts (snapshots + trace history, ~700M+) remain on the box at
`/srv/semfs-benchmark/matrix_artifacts/run5arm/` — pull if you need workspace snapshots.

---

## 3. The box

```bash
S="ssh -i ~/.ssh/semfs-benchmark -o ConnectTimeout=20 -o ServerAliveInterval=8 ubuntu@13.201.35.159"
# NOTE: SSH was flaky during the run — use ServerAliveInterval and keep monitor commands short.
```
- EC2 `m7i.xlarge` (4 vCPU / 16 GB), `ap-south-1`. **Disk is 93% full (15G free)** — watch it.
- `semfs` is on the **login shell** PATH only → use `$S 'bash -lc "..."'` or `/home/ubuntu/.local/bin/semfs`.
- **Seeds** (`~/.semfs/*.db`): `chanpin-gemma-q4.db` (clean local, **read-only canonical** — never write it),
  `workspace-bench-chanpin.db` (cloud local cache, 599M). `chanpin-matrix.db` = disposable working copy.
- Secrets in `/home/ubuntu/.semfs_seed_env`. **Never print keys.** The exposed Supermemory key
  (`sm_...`, visible in `ps`) and earlier OpenRouter keys **still need rotation by the user.**
- **Don't reboot. Keep seeds intact. Unmount via `semfs unmount <tag> [--force]`, never pattern-kill.**

---

## 4. How to run an experiment (the mechanism)

Single cell: `MATRIX_RESULTS=/tmp/x.jsonl MATRIX_ART=/srv/semfs-benchmark/matrix_artifacts/x \
  /tmp/run_case_fixed.sh <case> <arm> <stamp> <skipprep>`
- `<arm>` ∈ plain|nokg|gfs_off|gfs_on|cloud · `<skipprep>`=1 for semfs arms (skips the ~6-min
  copytree; plain uses 0). Driver: fresh-copies the canonical seed per run (contamination-proof),
  mounts, runs codex, judges, archives all telemetry to `$MATRIX_ART/<case>_<arm>/`.
- Knobs the semfs arms set: `GREP_INLINE=on RETURN_MODE=snippet RESULT_LIMIT=8 SEARCH_ONLY=on
  REWRITE=1` + per-arm `KG`/`GRAPH_FS` (see the driver). To add a new arm, add a `case` branch.

Full matrix: `/tmp/run_pm_matrix_5arm.sh` (serial, ~2.5–3h; first result ~8 min).

---

## 5. Optimization backlog (prioritized — this is your TODO)

0. **`SEARCH_ONLY=off` arm** — case 95/175 showed `SEARCH_ONLY=on` hides the file tree, so when
   local retrieval is weak the agent has no fallback (flails → 0/12). Re-run local arms with
   `SEMFS_SEARCH_ONLY=off`. Cheap, high-information. **Likely flips local semfs from net-negative
   to competitive on file-manipulation tasks.**
1. **"local + summaries" arm (highest-value)** — cloud beat plain *because* of indexed
   `.extracted.md` summaries; local serves raw chunks. Add per-doc/per-sheet summaries to the
   **local** gemma-q4 seed (see memory `summary-augmented-table-retrieval`; mechanism exists at the
   extract layer) and re-run. Hypothesis: replicates cloud's win **locally + reliably**.
2. **2–3 repeats per cell** — n=1 variance is large (same-arm reruns swung 171K↔666K). Required
   before quoting any per-cell number; the current aggregate is only ordinally trustworthy.
3. **Drop or rethink `gfs_on`** — `/by-topic/` consistently inflated tokens (471K mean) with no
   accuracy gain. Either remove it or change how it's surfaced.
4. **Fix `nokg` arm purity** — `/kg/` is baked into the canonical seed as files, so `SEMFS_KG=off`
   doesn't actually remove it (nokg ≈ gfs_off). Build a KG-free seed or `rm` `/kg/` from the
   matrix-tag copy (via the daemon's unlink, **never raw SQL** — see contamination note below).
5. **2-way parallelism** (~halves wall time) — deferred; needs a lightweight symlinked second
   `WB_ROOT` + per-worker tag + serialized cloud arm. Worth it only if iterating many matrices.
   Disk: ~90G reclaimable from unused `research_*`/`kaifa_*` personas (ask before deleting).

---

## 6. Gotchas / landmines (learned the hard way this run)

- **NEVER raw-`DELETE FROM chunks`** to clean contamination — it desyncs the FTS5 (`ffts`) + vec0
  (`vchunks`) companion indexes (orphaned rows → search returns 0). Use fresh-copy-per-run (current
  approach) or `rm` via the daemon. This is why the driver copies the canonical seed each run.
- **`pgrep -af <pattern>` self-matches** your own ssh command when the pattern is in the command
  line — gives false "still running". Use exact PIDs or `ps -eo` filters.
- **Nested heredocs over ssh** (`bash -lc "cat <<EOF ... EOF"`) expand vars at the wrong layer →
  silent corruption. Write scripts **locally and pipe** (`cat local | ssh 'cat > remote'`).
- **n=1 variance**; **structural rubrics unwinnable** for the chanpin persona (ceiling < max for
  all arms); **cloud ≠ matched A/B** (different container/embeddings/74% coverage).
- **codex token cache**: `cached_input=0` on every run (single-turn, no prefix reuse) → every turn
  re-pays full context → tokens are latency- and turn-count-bound (the real lever is turn count).

---

## 7. Open analysis questions

- Why did cloud win case 95 (12/12) but lose 175 (0/12)? → likely 95's needed files are summarized
  in the cloud index, 175's fall in the 26% coverage gap. Verify against `seed-coverage.md`.
- Is the local seed's failure *retrieval* (wrong files surfaced) or *delivery* (right files, bad
  form)? The `95/nokg` trace (`artifacts/run5arm/95_nokg/output/raw/codex_stdout.jsonl`) shows the
  agent semantic-grepping for filenames — read it to confirm the mechanism.
- Does `SEARCH_ONLY=off` alone rescue local, or is summaries also required? (experiments #0 vs #1)

---

## 8. Other important files & tickets to check

The matrix says *what* happened; these explain *why local loses* and *how to fix it* — read the
top group before starting optimization.

**Why local retrieval underperforms cloud (the core problem):**
- `rcas/2026-06-04-semfs-codex-clean-seed-timeout-poor-local-search-recall.md` — **poor local
  recall** is a known prior failure; directly explains the 0/12 cases.
- `tickets/local-ranking-precision-vs-supermemory/` — local-vs-cloud ranking gap.
- `tickets/rrf-chunk-mass-and-lane-fusion/` + `rcas/2026-06-04-rrf-chunk-mass-bias-code-lane-pollution.md` — fusion/ranking bugs that hurt local precision.
- `tickets/embedder-config-search/`, `tickets/embedder-upgrade-gemma-qwen3/`, `tickets/gemma-q4-embedder/` — the local embedder + upgrade path (a better embedder may close the gap).

**The two optimization experiments (backlog #0/#1):**
- `tickets/summary-augmented-table-retrieval/` — **the local-+-summaries plan** (mechanism exists at extract layer). This is experiment #1.
- `tickets/ls-kg-semantic-readdir/` — `SEARCH_ONLY`/readdir behavior; relevant to experiment #0 (`SEARCH_ONLY=off`).
- `tickets/format-trap-extraction-delivery/` — grep-inline vs `.extracted.md` siblings delivery.
- `tickets/local-document-extractors/`, `tickets/extraction-coverage-audit/` — extraction quality (feeds summaries + coverage).

**Seed quality (the local seed has known gaps):**
- `rcas/2026-06-08-partial-seed-indexing.md`, `rcas/2026-06-08-stale-build-gemma-q4-seeded-as-e5.md`, `rcas/2026-06-08-extraction-coverage-cjk-pdf-legacy-ole-empty-placeholders.md` — why the local seed may be incomplete/mislabeled.
- `tickets/local-seed-coverage-gaps/`, `benchmarks/workspace_bench/seed-coverage.md` — coverage per container + missing-file lists.
- `rcas/2026-05-30`/`2026-06-01` prewarm-OOM — gotchas if you **re-seed** (the box is 16 GB).

**Agent behavior / token mechanics:**
- `rcas/2026-06-05-agent-search-token-blowup-turn-multiplication.md` — why turn-count drives tokens.
- `tickets/explore-agent-search-behavior/`, `tickets/case289-retrieval-investigation/` — prior deep-dives (token lever = codex exploration).
- `rcas/2026-06-06-cross-lingual-recall-miss-case289.md` — the corpus is Chinese; `SEMFS_REWRITE` matters.

**Harness / infra:**
- `tickets/benchmark-adapter/` — plan for a multi-benchmark harness (WB → xAFS/terminal-bench/TheAgentCompany).
- `tickets/bench-per-case-remount-redundancy/` — remount cost (relevant to SKIP_PREPARE + parallelism).
- `benchmarks/workspace_bench/{SESSION_STATE_AND_LEARNINGS,judge_pipeline,cloud_env_state}.md` — accumulated learnings, judge setup, cloud state.

**Memory (auto-loaded each session; the distilled findings):**
`~/.claude/projects/-Users-marmikpandya-semantic-filesystem/memory/` — read `MEMORY.md` index, then
especially `wb-5arm-matrix-result`, `summary-augmented-table-retrieval`,
`case289-token-lever-is-codex-exploration`, `q4-graphfs-e2e-format-trap`, `semfs-claude-affordance`,
`semfs-seed-quality-findings`.

## 9. State to be aware of

- **All this session's work is UNCOMMITTED** on branch `feat/backend-agnostic-store` (docs, this
  ticket, scripts, + pre-existing core changes). Decide whether to commit before/after analysis.
- Box is clean (no mounts, orchestrator exited); `chanpin-matrix.db` working copy remains (harmless).
- Memory written: `wb-5arm-matrix-result` (in `~/.claude/.../memory/`) — the headline finding.
