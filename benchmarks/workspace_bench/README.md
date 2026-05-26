# Workspace-Bench Lite Setup

This directory sets up the official [Workspace-Bench](https://github.com/OpenDataBox/Workspace-Bench) harness to benchmark:

- plain `Codex`
- `Codex + semfs`
- plain `ClaudeCode`
- `ClaudeCode + semfs`

## Which path to benchmark

Use the mounted filesystem for the public Workspace-Bench result.

Reason:
- Workspace-Bench already benchmarks real CLI agents like `codex` and `claudecode`.
- Your thesis is about POSIX-style agents navigating a workspace better with a semantic filesystem.
- The mounted path preserves the real agent surface: local files, local `grep`, local working directory.

Use the TypeScript virtual bash path as a secondary backend bakeoff, not as the primary public benchmark.

Reason:
- `sqlite-vec` and `pgvector` exist today in the TypeScript agent path, not in the mounted Rust path.
- A TS benchmark is useful to compare `supermemory` vs `sqlite-vec` vs `pgvector` while keeping the retrieval interface constant.
- It is not a clean apples-to-apples replacement for Codex CLI because the agent surface changes from native POSIX to tool calls.

## Railway worker scope

The `railway-worker/` image is for unattended plain-agent smoke runs on Workspace-Bench.

It can run:
- plain `Codex`
- plain `ClaudeCode`

It cannot run the mounted SEMFS variants on Railway because Railway containers do not expose FUSE privileges (`/dev/fuse` plus `SYS_ADMIN`). For mounted SEMFS comparisons, use a privileged Linux VM or Docker host.

For OpenRouter-backed runs, set `OPENROUTER_API_KEY` on the Railway service. The worker maps it to the generated Workspace-Bench provider config for:
- `openai/gpt-5.4`
- `anthropic/claude-sonnet-4.6`

The current Railway CLI creates the volume at the plan/default size. If the Workspace-Bench workspace archive exceeds the attached volume size, resize the volume from Railway's dashboard volume settings before running Lite or Full.

## What this setup does

`setup_workspace_bench_semfs.py`:
- installs an `SEMFSCodex` or `SEMFSClaudeCode` agent adapter into a local Workspace-Bench checkout
- generates the plain official run config
- generates the mounted-semfs run config

`semfscodex.py`:
- mounts `semfs` on the task workdir
- delegates the task to the official `codex.py` harness
- unmounts afterward
- records SEMFS mount and unmount timings separately in the task trace

`semfsclaudecode.py`:
- mounts `semfs` on the task workdir
- delegates the task to the official `claudecode.py` harness
- unmounts afterward
- records SEMFS mount and unmount timings separately in the task trace

## Prerequisites

- local clone of `https://github.com/OpenDataBox/Workspace-Bench`
- `codex` CLI on `PATH` for Codex runs
- Workspace-Bench Claude Code baseline dependencies for Claude Code runs
- `semfs` binary on `PATH`, or `SEMFS_BIN=/abs/path/to/semfs`
- `SUPERMEMORY_API_KEY` configured via `semfs login` or environment
- Python 3 with `PyYAML`

## Install the adapter

```bash
python3 benchmarks/workspace_bench/setup_workspace_bench_semfs.py \
  --workspace-bench-root /private/tmp/Workspace-Bench \
  --harness codex \
  --model kimi-k2.5 \
  --dataset lite
```

For Claude Code:

```bash
python3 benchmarks/workspace_bench/setup_workspace_bench_semfs.py \
  --workspace-bench-root /private/tmp/Workspace-Bench \
  --harness claudecode \
  --model kimi-k2.5 \
  --dataset lite \
  --provider-type anthropic
```

That prints both the plain config path and the mounted-semfs config path.

## Download the official data

From the Workspace-Bench checkout:

```bash
cd /private/tmp/Workspace-Bench/evaluation
python3 scripts/download_hf_assets.py --lite --workspaces
python3 scripts/prepare_workdirs_for_run.py --run-config .generated/run_configs/runs/semfscodex-kimi-k2.5-lite.yaml
```

## Run the benchmark

Recommended environment:

```bash
export CODEX_API_KEY=...
export KIMIK25_API_KEY=...
export KIMIK25_BASE_URL=...
export SEMFS_CONTAINER_PREFIX=workspace-bench-lite
export SEMFS_MOUNT_TIMEOUT_SEC=120
export CODEX_SANDBOX_MODE=danger-full-access
```

Then run:

```bash
cd /private/tmp/Workspace-Bench/evaluation
python3 -u src/agent_runner.py --run-config .generated/run_configs/runs/semfscodex-kimi-k2.5-lite.yaml
```

For plain Claude Code:

```bash
python3 -u src/agent_runner.py --run-config .generated/run_configs/runs/claudecode-kimi-k2.5-lite.yaml
```

For `ClaudeCode + semfs`:

```bash
python3 scripts/prepare_workdirs_for_run.py --run-config .generated/run_configs/runs/semfsclaudecode-kimi-k2.5-lite.yaml
python3 -u src/agent_runner.py --run-config .generated/run_configs/runs/semfsclaudecode-kimi-k2.5-lite.yaml
```

## Metric locations

Official Codex metrics:
- `evaluation/output/.../agent_runner_report.json`
- includes per-case `durationMs`
- includes token totals parsed from Codex JSONL

Official Claude Code metrics:
- `evaluation/output/.../agent_runner_report.json`
- includes per-case `durationMs`
- includes token totals parsed from the Claude Code result stream

SEMFS-specific timings:
- each case trace gets `trace.semfs`
- includes `mountDurationMs`, `unmountDurationMs`, `containerTag`, and command outputs

## Important caveat

This adapter mounts per task. That is fine for smoke testing and initial Lite runs, but it includes mount/import overhead that you may want to separate from agent execution in your analysis.

For the final benchmark write-up, use two views:
- `agent_duration_ms`: the delegated Codex task duration
- `semfs_overhead_ms`: mount + unmount duration

If you want, the next step is to move from per-task mounting to per-workspace mounting inside Workspace-Bench's group runner so the overhead is amortized across tasks that share a workspace.
