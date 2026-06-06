# semfs × Workspace-Bench — Runbook / Handoff

**Last updated:** 2026-05-27. Audience: a coding agent continuing this benchmarking work.

## 1. What this benchmarks

We run coding agents (**Codex/GPT-5.4**, **Claude Code/Sonnet-4.6**) on **Workspace-Bench**,
**with and without `semfs`** (the semantic filesystem), to compare:
- **token usage** (does semantic retrieval cut context?),
- **accuracy** (does the grade hold?),
- and to **confirm supermemory is called only in the semfs variants**.

Four variants per case: `codex`, `semfs-codex`, `claudecode`, `semfs-claudecode`.

## 2. The benchmark dataset (HuggingFace `Workspace-Bench`)

Three artifacts:
| HF resource | What | Size |
|---|---|---|
| `Workspace-Bench-Lite` | 100-task subset, **bundles each task's input files only** (`data_manifest`) | 262 MB |
| `Workspace-Bench-Workspaces` | the **5 file-system environments** (the realistic, distractor-laden workspaces) | 18.7 GB |
| `Workspace-Bench` (full) | 388 tasks | — |

**Personas → workspaces** (on host as `filesys/<persona>_raw`):
| Persona | dir | size | files | Lite tasks |
|---|---|---:|---:|---:|
| Product Manager | `chanpin` | 516 MB | 2,128 | 11 |
| Backend Developer | `kaifa` | 2.5 GB | 2,745 | 11 |
| Logistics Manager | `houqin` | 1.0 GB | 3,838 | 30 |
| Operations Manager | `yunying` | 808 MB | 2,854 | 31 |
| Researcher | `research` | **18 GB** | 11,726 | 17 |

- **The 18 GB is almost entirely Researcher.** The other 4 personas (83 tasks) total ~5 GB. You do **not** need all 18 GB for a meaningful Lite run — pick a persona/subset.
- The **Lite 262 MB bundle is NOT a substitute for the workspaces** — it has only task-relevant files, so it trivializes "Workspace Exploration" (73/100 tasks). Faithful runs need the workspace environments.
- **Grading:** the harness's default check is `returned_paths_exist` (weak — just confirms the agent returned a path to an *existing* output file; an agent that writes any file to `model_output/` "passes"). Real quality grading is the **Agent-as-Judge + rubrics** (`agent_as_a_judge.py`), which is **not** run by the smoke flow. If you care about quality, wire that in.

## 3. The machine

- **EC2 `i-0c491c7cc23de8555`** (`semfs-benchmark-host`), **`m7i.xlarge`** (4 vCPU / 16 GB), `ap-south-1`, Ubuntu 24.04. Public IP `13.201.35.159`, key `~/.ssh/semfs-benchmark`.
- SSH is gated by security group `sg-0aa6ffbdc8cbc852f` (currently `0.0.0.0/0` on :22 per request — tighten or move to SSM later; rotating office/home IPs were the reason).
- **`semfs` is only on the login-shell PATH** → always `ssh … 'bash -lc "…"'` or use full path `~/.local/bin/semfs`.

## 4. Host layout (`/srv/semfs-benchmark/`)

```
semantic-filesystem/benchmarks/
   aws/run_workspace_bench.sh        ← entrypoint
   workspace_bench/semfs{codex,claudecode}.py, setup_workspace_bench_semfs.py   ← semfs adapters
Workspace-Bench/evaluation/
   src/agent_runner.py               ← orchestrator + grader
   src/agents/{codex,claudecode,semfscodex,semfsclaudecode}.py   ← agents (semfs* copied in by setup at run time)
   baselines/ClaudeCode.js           ← Claude Agent SDK driver (OAuth handling here)
   scripts/{build_run_config,prepare_workdirs_for_run}.py, src/filesys_utils.py
   filesys/<persona>_{raw,standard,workdir_*}   ← workspaces (~87 GB: raw + standard + per-agent copies)
   .generated/hf_downloads/{lite,full}/         ← task sets
   tasks_lite/<caseId>/                          ← the live lite task dirs
   output/<Agent>--<Model>--Smoke[-SEMFS]/<caseId>/agent.json   ← results
   output/_telemetry/<RUN_STAMP>/run_narrative.json             ← per-run summary
benchmark.env                        ← sourced by the runner (SUPERMEMORY_API_KEY, OPENROUTER, CLAUDE_CODE_OAUTH_TOKEN, SONNET46_*, SEMFS_*)
semfs-latest/  (or smfs-src/)        ← Rust source; build with `cargo build --release -p semfs`
~/.local/bin/semfs                   ← installed binary
~/.cache/semfs/{logs,<org>/<tag>.db} ← semfs cache + per-container daemon logs
~/.config/semfs/credentials.json     ← semfs creds (set via `semfs login --key …`)
```

## 5. How to run

```bash
# plain (no semfs) — no supermemory calls
DATASET=smoke RUN_STAMP=$(date -u +%H%M%SZ) run_workspace_bench.sh codex
USE_CLAUDE_LONG_RUNNING_TOKEN=1 DATASET=smoke run_workspace_bench.sh claudecode   # claude via OAuth subscription

# semfs (reads a seeded container, read-only)
DATASET=smoke SKIP_PREPARE=1 \
  SEMFS_CONTAINER_TAG=workspace-bench-<persona> SEMFS_NO_PUSH=1 SEMFS_MOUNT_TIMEOUT_SEC=900 \
  run_workspace_bench.sh semfs-codex
```
- `DATASET=smoke` = `task_limit=1` (first dir in `tasks_lite/`, sorts to case `100`). **To target a specific case, swap `tasks_lite/` to contain only that case dir** (back up the full set first), then restore.
- **Auth:** Codex → OpenRouter (`GPT54_*` / `SONNET46_*` keys). Claude → set `USE_CLAUDE_LONG_RUNNING_TOKEN=1` to use the Claude subscription (`CLAUDE_CODE_OAUTH_TOKEN`); otherwise it goes through OpenRouter.
- **semfs reader flags:** `SEMFS_NO_PUSH=1` (read-only: never push the agent's writes → no contamination of the shared seed), `SEMFS_CLEAN=1` (wipe local cache, re-pull — avoid it for repeat runs; it forces a slow full re-hydration), `SEMFS_CONTAINER_TAG` (point at the seeded per-workspace container), `SEMFS_MOUNT_TIMEOUT_SEC=900` (big seeds take minutes to hydrate).
- Runs are long (Claude esp.); launch with `setsid bash -lc "… > /tmp/x.log 2>&1"` so SSH drops don't kill them; poll `semfs list` + `output/.../agent.json`.

## 6. Seeding (the expensive prerequisite for semfs runs)

semfs runs read a **per-workspace supermemory container** that must be **seeded once**:
```bash
# 1. create the container (mounting an EMPTY container 404s):
curl -s -XPOST $SUPERMEMORY_API_URL/v3/documents -H "Authorization: Bearer $SUPERMEMORY_API_KEY" \
     -H 'Content-Type: application/json' -d '{"content":"init","containerTag":"workspace-bench-<persona>"}'
# 2. mount the workspace with sync ON → it imports + pushes + embeds:
semfs mount workspace-bench-<persona> --path filesys/<persona>_raw --no-inject-hint
# 3. wait for `semfs list` QUEUE → ~0, then `semfs unmount`.
```
- **One workspace seed is an overnight job** (embed throughput ~25–40 docs/min; chanpin ≈ 1,377 embeddable docs ≈ ~50 min, kaifa ≈ overnight). A small tail of `400`/`409` docs (unparseable `.xlsx`, dups) never drains — **that's normal, not a failure.**
- Seed lives **server-side** (keyed by container tag), so it survives binary/cache changes — `semfs` pulls it via the API.

## 7. Where it fails (read this before debugging)

| Failure | Symptom | Fix |
|---|---|---|
| **Out of credits** | `POST /v3/documents → 402 "Text tokens limit reached"`; seed stalls | **Free plan can't seed even one workspace (~700-doc cap). Use Pro.** |
| **Empty container** | `semfs mount → "daemon exited before becoming ready"`; daemon log `Error: not found (404)` | push 1 init doc to create the container first (§6) |
| **Orphaned FUSE mount** | post-run workdir = `Transport endpoint is not connected` (ENOTCONN); grade `failed` "empty path list" | adapter `_force_clear_mount` now **retries `fusermount3 -u`** to win the async-teardown race; manual: `fusermount3 -u <workdir>`. Real fix belongs in the Rust daemon's unmount. |
| **Prepare rmtree crash** | `OSError: Directory not empty: 'objects'` in `make_filesys` | read-only `git objects` in workspace node_modules; **patched `make_filesys` to use `_safe_rmtree`** (chmod-retry). If it recurs, `chmod -R u+w <persona>_standard && rm -rf` it. |
| **Mount timeout** | `semfs mount failed`; daemon log full of `rehydrated raw file …r2…` then cut off | `--clean` re-pulls the whole seed (>120 s default). Use `SEMFS_MOUNT_TIMEOUT_SEC=900` **and drop `--clean`** so the cache is reused. |
| **Case ≠ seeded workspace** | semfs run 404s / reads nothing | smoke defaults to case 100 (houqin). Run a case whose persona's workspace is seeded, or seed houqin. |
| **Stale resume** | run "passes" instantly with identical numbers | `agent_runner` **skips a case if `output/<...>/<case>/output/` exists**. `rm -rf` that case dir to force a fresh run. |
| **`smfs` vs `semfs`** | `semfs: command not found` / wrong creds / empty cache | rebrand: binary is `semfs`, cache `~/.cache/semfs`, creds via `semfs login --key …` (separate namespace from old `smfs`). PATH only in login shell. |
| **Token accounting mismatch** | claude shows `prompt≈0` | Codex (OpenRouter) counts full prompt; Claude (OAuth subscription) reports the big prompt as cached. **Don't compare raw totals across the two models.** |

## 8. What we measured (case 289, PM/chanpin, n=1 — illustrative)

| Variant | Total tokens | Prompt | Supermemory **API** calls | Grade |
|---|---:|---:|---:|---|
| plain codex | 143,837 | 140,799 | **0** | passed (2 outputs) |
| semfs-codex | **35,763** | 34,767 | **2** (`search_v4`+`profile_v4`) | passed (2 outputs) |

- **semfs-codex used ~75% fewer tokens for the same pass** — it made **one `search_v4`** call and read targeted files instead of exploring the whole workspace. (n=1; confirm across cases.)
- **Supermemory is called only in semfs variants** (0 vs 2 API calls).
- **R2 ≠ API calls.** The daemon log's many `…r2.cloudflarestorage.com/supermemory-bucket/…` lines are **supermemory's own object storage** (raw file bytes via **presigned URLs** — no Cloudflare keys on our side). They are **storage egress, not billable API calls** — use the **supermemory dashboard** for the real API count, not `grep` on the daemon log.

## 9. Open items / cautions
- **Default grader is weak** (`returned_paths_exist`); use Agent-as-Judge + rubrics for real quality.
- **Orphaned-mount-on-unmount is a Rust daemon bug**; the adapter works around it. Fixing `semfs unmount`'s kernel teardown removes the need for `_force_clear_mount`.
- **`make_filesys` rmtree fix** (read-only handler) should be upstreamed to the vendored Workspace-Bench.
- RCAs in `rcas/` (orphaned-mount, empty-returned-paths, 402, teardown-race) have the detailed histories.
- Per-workspace shared container (`workspace-bench-<persona>`) lets codex+claude read one seed; reader runs are cheap (2 API calls). Don't use per-agent containers (doubles the embed).
