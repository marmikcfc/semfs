> <!-- STALE-BANNER --> ⚠️ **HISTORICAL HANDOFF (2026-06-25)** — point-in-time session handoff; its blockers/next-steps are closed. Current state → [/CURRENT_STATE.md](../../CURRENT_STATE.md).

# Session Handoff — 2026-06-11 night

_Written at session end. Companion: `EC2_RUNBOOK_CURRENT.md`, `EXPERIMENTS_NEXT.md`, `EXPERIMENT_MATRIX.md`._

---

## What we did this session

### 0. Analysis pass
Read the full ticket stack (EXPERIMENT_MATRIX, LEARNINGS, RUN_MANIFEST, EXPERIMENTS_NEXT, TOKEN_ECONOMY, OPINIONS), the EC2 runbook, `docs/ARCHITECTURE.md`, and `BENCH_ARCHITECTURE.md`. Produced a prioritized 6-task stack (reproduced below).

### 1. E9w2 — CODED + TESTED (box binary built; NOT yet installed)

**Problem diagnosed (E9w1):** RRF fuses scores into a compressed band near `1/(60+rank)`. The old relative margin `(s1−s2)/s1` never cleared 15%, so the HIGH confidence path literally never fired across all of wave 1. The COMPLETE FILE marker existed but was never guarded — a truncated top hit could falsely claim HIGH.

**Fix (`crates/semfs/src/cmd/grep.rs`):**
- `confidence_line()` now takes `(top, second, min, top_complete: bool)`
- **Spread-normalized margin**: `margin = (top − second) / (top − min)` — uses the result set's own spread as the denominator, so the gap is meaningful even when all RRF scores are near 0.016
- **COMPLETE-FILE gate**: HIGH is only emitted when `top_complete == true` (the excerpt IS the whole file, not a truncated snippet) — prevents a dishonest HIGH on a clipped excerpt
- Three output branches: HIGH (dominant + complete) / MEDIUM (dominant + truncated → "open the ONE top file") / MIXED (close scores)
- **6 tests added/updated**; `confidence_high_on_rrf_compressed_dominant` is the regression test that would have caught the wave-1 bug
- **All 61 `semfs` binary tests pass** (`cargo test -p semfs`)

**Box build status:** Built successfully. `BUILD_EXIT=0` confirmed in `/tmp/build.log`. Binary at `/srv/semfs-benchmark/semantic-filesystem/target/release/semfs`.

**BLOCKER:** Install step (`install -m755 target/release/semfs ~/.local/bin/semfs`) was blocked by the auto-mode classifier twice. **Box binary is still the old pre-E9w2 version.** Next session: get explicit user "go" before attempting install.

Install command (run when permission confirmed):
```bash
S="ssh -i ~/.ssh/semfs-benchmark -o ConnectTimeout=20 -o ServerAliveInterval=10 ubuntu@13.201.35.159"
$S "install -m755 /srv/semfs-benchmark/semantic-filesystem/target/release/semfs /home/ubuntu/.local/bin/semfs && /home/ubuntu/.local/bin/semfs --version && /home/ubuntu/.local/bin/semfs init"
```

### 2. Judge filename lottery — DIAGNOSED, NOT FIXED

**Question:** Why does the judge expect a specific filename for case-95, but the task never mentions it?

**Answer (code-confirmed):**
- `metadata.json` `output_files` field = `["system_version_full_lifecycle_iteration_report.doc"]`
- Every rubric embeds that name ("check [system_version_full_lifecycle_iteration_report.doc]…")
- The agent-visible `task` field does NOT contain the filename — confirmed across `tasks/`, `tasks_lite/`, `tasks_lite.full/` AND in the upstream `OpenDataBox/Workspace-Bench` repo (HEAD `4bf11e4`)
- `agent_runner._wrap_prompt()` sends only `meta['task']` in the prompt; `metadata.json` is written to the results dir, NOT the agent's `work_dir`; `__expected_output_files__` is used only for post-run output collection (rescuing files from the mount before unmount)
- **This is upstream WB design**: 34 of 100 cases have this pattern (task names no output file; only rubrics do). Cases 95 AND 175 are both in this set.

**Why this matters:** Seed-2.0-Lite judges by rubric text matching the literal filename. If the agent names its deliverable differently (e.g. `report.docx`), the judge fails ALL rubrics. This is why case-95 scored 0/12 and 12/12 on identical runs (filename lottery, not a data bug).

**Fix decision: Option B** (judge-side content tolerance). Do NOT edit task ground truth (option A diverges from upstream and only patches 1 of 34 cases).

**Option B approach (not yet coded):** In `agent_as_a_judge.py`, in `_prepare_judge_view` (or equivalent), when the agent's primary deliverable name doesn't match any `output_files` entry, symlink or copy it to the expected name before passing to the judge. This is arm-neutral (applies to plain AND semfs arms equally), upstream-consistent (doesn't alter task data), and fixes all 34 lottery cases at once.

**Note:** 429-retry and per-file input caps already exist in upstream `agent_eval.py` / `agent_as_a_judge.py` — do NOT rebuild those parts.

**Also needed (security):** `judge_seed.yaml` currently hardcodes an OpenRouter API key. Must be rotated + changed to read from env (`${OPENROUTER_API_KEY}`). Never print keys.

---

## Box state (as of this session end)

| Item | State |
|---|---|
| Deployed binary | **OLD** — `711d028…`, pre-dates E9w2 + de-tune commits |
| Built binary (not installed) | `/srv/semfs-benchmark/semantic-filesystem/target/release/semfs`, BUILD_EXIT=0 |
| Active seed | `chanpin-e5-nosum` mount tag |
| Canonical seed | `chanpin-clean.db` (CANON) |
| Current hint seed | `chanpin-leanhint3.db` = v4.1 (facts+costs, de-tuned, shipped default) |
| DEPRECATED seeds | `chanpin-leanhint2.db` (v3 + provenance check — DO NOT USE) |
| Disk | ~9G free — check `df -h /` before big batches |
| Drivers | All in `/tmp` (ephemeral); will die on reboot |
| `judge_seed.yaml` | Hardcoded API key — **security leak, rotate before next run** |

---

## Ordered task stack (pick up where we left off)

### Task #1 — ✅ DONE: E9w2 code change
`grep.rs` spread-normalized margin + COMPLETE-FILE gate. Tests green. Binary built on box.

### Task #2 — NEXT: Install + run E9w2 ×3 on case 289

Precondition: explicit user "go" (classifier blocked twice last session).

```bash
S="ssh -i ~/.ssh/semfs-benchmark -o ConnectTimeout=20 -o ServerAliveInterval=10 ubuntu@13.201.35.159"

# Step 1: install
$S "install -m755 /srv/semfs-benchmark/semantic-filesystem/target/release/semfs /home/ubuntu/.local/bin/semfs && /home/ubuntu/.local/bin/semfs --version && /home/ubuntu/.local/bin/semfs init"

# Step 2: run 3× (backgrounded, survives SSH drop)
for N in 1 2 3; do
  $S "setsid bash -lc 'MATRIX_RESULTS=/tmp/e9w2.jsonl MATRIX_ART=/srv/semfs-benchmark/matrix_artifacts/e9w2 CANON=/home/ubuntu/.semfs/chanpin-leanhint3.db SEMFS_SEARCH_ONLY=off SEMFS_GREP_RESULT_CAP=6144 SEMFS_RESULT_LIMIT=5 SEMFS_GREP_RENDER_MODE=two-tier SEMFS_GREP_TOTAL_CAP=10240 RUNLABEL=e9w2_$N /tmp/run_case_e.sh 289 nokg e9w2_$N 1' >/dev/null 2>&1 < /dev/null &"
done

# Step 3: poll results
$S 'tail -f /tmp/e9w2.jsonl'
```

**Kill condition:** If HIGH fires in the trace (grep `CONFIDENCE: HIGH` in `codex_stdout.jsonl`) but call count stays bimodal (same wide spread as E9w1) → computed-confidence is behaviorally dead; stop optimizing this signal, pivot to E11.

**Success criterion:** HIGH fires on ≥1/3 runs AND mean tool_calls ≤ E9w1 mean (or accuracy improves).

### Task #3 — Judge fix (Option B)

Draft the diff to `agent_as_a_judge.py`:
- In `_prepare_judge_view` (or wherever deliverables are staged for the judge), when the agent's output file name doesn't match the `output_files` expected name, create an alias (symlink or copy) to the expected name in the judging workspace.
- Do NOT break arm-neutrality (applies to ALL arms, not just semfs).
- Do NOT add 429-retry or input caps (already present).
- After patching, re-judge the existing e95v4 runs from `/srv/semfs-benchmark/matrix_artifacts/e95v4/` to verify the lottery score variance collapses.
- Also fix `judge_seed.yaml`: replace hardcoded key with `apiKey: "${OPENROUTER_API_KEY}"` and read from `/home/ubuntu/.semfs_seed_env`.

### Task #4 — Git-track canonical box drivers

Pull these from box `/tmp` into `benchmarks/workspace_bench/drivers/` before a reboot kills them:
- `run_case_e.sh` (hardened driver — heart of the suite)
- `run_matrix.sh` (batch loop)
- `kgrun.sh`, `run289.sh` (single-case wrappers)
- `parse289.py`, `cmd_seq.py`, `showjudge.py` (telemetry + forensics)
- `rejudge.sh`, `recount.py`, `inspect_run.py` (post-hoc judge pass)
- `build_leanhint3.py` (seed hint surgery)
- `e8seq.sh`, `e9w1.sh`, `e9d.sh`, `e95v4.sh` (batch experiment drivers)
- `judge_seed.yaml` (de-keyed — env var only)

Command to pull:
```bash
S="ssh -i ~/.ssh/semfs-benchmark -o ConnectTimeout=20 ubuntu@13.201.35.159"
mkdir -p benchmarks/workspace_bench/drivers
for f in run_case_e.sh run_matrix.sh kgrun.sh run289.sh parse289.py cmd_seq.py showjudge.py rejudge.sh recount.py inspect_run.py build_leanhint3.py e8seq.sh e9w1.sh e9d.sh e95v4.sh judge_seed.yaml; do
  scp -i ~/.ssh/semfs-benchmark ubuntu@13.201.35.159:/tmp/$f benchmarks/workspace_bench/drivers/ 2>/dev/null || echo "MISSING: $f"
done
```

### Task #5 — EXPERIMENTS_NEXT.md reconcile

After E9w2 results are in, update the E8 spec entry in `EXPERIMENTS_NEXT.md`:
- E8 now runs against v4.1 hint (de-tuned, KG off, no gating on E7c)
- Pre-registered kill condition still stands: <2/5 cases with mean tokens < plain at accuracy ≥ plain−1 → stop WB-chanpin token optimization, execute O8 via E11

### Task #6 — E11 case authoring (offline, no box)

Author 2–3 new Workspace-Bench-style cases that are:
1. **Discovery-stressed**: the agent must search to find the relevant file (not trivially at the root)
2. **Cross-lingual**: source files in one language, task in another (probes the x-lingual rewrite fix)
3. **No filename lottery**: task MUST state the expected output filename

This is pure content work (no box needed). Pairs with E11 harness integration once cases exist.

---

## Key decisions made this session

| Decision | Rationale |
|---|---|
| Option B for judge fix (not option A) | A edits upstream WB ground truth; fixes only 1/34 lottery cases. B is arm-neutral and systemic. |
| `forming-opinions` used | Confirmed bias check: option A was motivated by "editing one file = simple"; that was the rationalization. The systemic scan (34 cases) reversed it. |
| Hint v4.1 stays (no re-introduce PROVENANCE CHECK) | v3 backfired on case-95: agent declared findable files missing. The honesty rubrics measure the agent, not the prompt. |
| COMPLETE-FILE gate on HIGH | Without it, a truncated top hit would claim HIGH (lying to the agent about completeness). Guard is necessary for honest confidence. |
| n≥2 before quoting cell results | E9w1 showed bimodal call-count distribution (5 vs 32 calls same config). Single-run numbers are coin flips. |

---

## Standing security reminders

- **Never print API keys.** Use `${VAR:+SET}` to confirm a variable is set.
- `judge_seed.yaml` has a hardcoded OpenRouter key → **rotate it and change to env var** before the next judging run.
- Do NOT reboot the EC2 instance without explicit OK.
- Mount cleanup: `semfs unmount <tag>` only. Never `pkill -f semfs` (self-matches the ssh command string and kills your own shell). Use `pkill -f daemon-inner` or kill by PID.

---

## Quick reference: SSH + box paths

```bash
S="ssh -i ~/.ssh/semfs-benchmark -o ConnectTimeout=20 -o ServerAliveInterval=10 ubuntu@13.201.35.159"
# semfs binary:   /home/ubuntu/.local/bin/semfs
# repo (rsync):   /srv/semfs-benchmark/semantic-filesystem  (NOT a git repo — sync from GitHub feat/backend-agnostic-store before building)
# seeds:          ~/.semfs/chanpin-clean.db (CANON), chanpin-leanhint3.db (v4.1 hint)
# matrix art:     /srv/semfs-benchmark/matrix_artifacts/
# WB eval:        /srv/semfs-benchmark/Workspace-Bench/evaluation/
# drivers:        /tmp/ (ephemeral — git-track per task #4 above)
# secrets:        /home/ubuntu/.semfs_seed_env
```

---

## Modal follow-up state

### What is true right now

- Modal is reachable from the local shell and the active profile is `ada-diffusion-llm`.
- The shared Modal volume `semfs-bench-data` is populated and reusable by multiple apps/functions.
- Present on the volume: `seeds/chanpin-gemma-q4.db`, `models/gemma_q4/`, `corpus/chanpin_seed/`, `corpus/chanpin_standard/`, `wb/evaluation/`, and `codex/config.toml`.
- `benchmarks/modal/semfs_modal.py` now copies the local worktree into the image, builds `semfs` in the image, and materializes a local `.semfs` marker in the modal workdir so `semfs grep --tag` resolves the local seed DB instead of falling back to cloud.
- The first Modal smoke failure was a false fallback: `semfs grep` returned `Error: auth failed (401)` because the workdir had no `.semfs` marker, so it tried the Supermemory cloud backend with the dummy API key.
- After fixing the marker write, `smoke_grep` succeeded at least for the `inline` mode with `rc=0` and hits, which confirms the local seed path is working.
- `_load_case_meta()` has now been tightened to exact-id matching, and the latest `run_case(case="289", render_mode="two-tier")` printed the correct metadata path:
  `/data/wb/evaluation/.generated/hf_downloads/full/task_clean_en/289/metadata.json`
- The smoke stages now pass on Modal:
  - `[inline] rc=0 bytes=12789 hits=True`
  - `[two-tier] rc=0 bytes=2580 hits=True`
  - `[paths] rc=0 bytes=2031 hits=True`
- The remaining failure is inside the codex invocation itself. The returned JSON now includes:
  ```json
  {
    "label": "e9w2-modal-smoke",
    "case": "289",
    "render_mode": "two-tier",
    "wall_s": 19,
    "rc": 1,
    "calls": 0,
    "tokens": 0,
    "usage": {},
    "deliverables": [],
    "semfs_sha": "d5a6eda+loca",
    "runner_err_head": "Reading additional input from stdin...\n2026-06-11T18:47:03.534749Z ERROR codex_api::endpoint::responses_websocket: failed to connect to websocket: HTTP error: 401 Unauthorized, url: wss://api.openai.com/v1/responses\n..."
  }
  ```
- `task_len=197` and `openrouter_present=True` were both printed inside the Modal container, so the task payload and secret injection are present. The blocker is now codex provider/auth configuration inside Modal, not Modal reachability, seeding, or metadata resolution.

### Next steps for the next session

1. Inspect `benchmarks/modal/semfs_modal.py` and the mounted `codex/config.toml` to verify which provider the Modal-side codex CLI is actually using.
2. Check whether the Modal secret injection is supplying a valid OpenRouter key and whether the config is still pointing at `wss://api.openai.com/v1/responses`.
3. Fix the config/provider mismatch if present, then rerun `modal run benchmarks/modal/semfs_modal.py::e9w2_smoke`.
4. Keep the existing seed and volume state; do not reseed unless the volume was lost.
5. If the run still fails, capture the next `runner_err_head` and add a new RCA immediately.
