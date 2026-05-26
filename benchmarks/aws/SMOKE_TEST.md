# Workspace-Bench Smoke Test

This host is provisioned for four smoke-test variants:

1. `codex`
2. `semfs-codex`
3. `claudecode`
4. `semfs-claudecode`

## Host

- EC2 public IP: `REDACTED_BENCH_HOST`
- Benchmark root: `/srv/semfs-benchmark`
- Workspace-Bench root: `/srv/semfs-benchmark/Workspace-Bench`
- Runner: `/srv/semfs-benchmark/semantic-filesystem/benchmarks/aws/run_workspace_bench.sh`
- Secrets file: `/srv/semfs-benchmark/benchmark.env`

## Required secrets

Populate `/srv/semfs-benchmark/benchmark.env` with:

```bash
OPENROUTER_API_KEY=...
SUPERMEMORY_API_KEY=...
HF_TOKEN=...
GPT54_BASE_URL=https://openrouter.ai/api/v1
GPT54_API_KEY=${OPENROUTER_API_KEY}
SONNET46_BASE_URL=https://openrouter.ai/api/v1
SONNET46_API_KEY=${OPENROUTER_API_KEY}
SONNET46_ANTHROPIC_BASE_URL=https://openrouter.ai/api
SONNET46_ANTHROPIC_MODEL=anthropic/claude-sonnet-4.6
SEMFS_CONTAINER_PREFIX=workspace-bench
SEMFS_MOUNT_TIMEOUT_SEC=120
SEMFS_UNMOUNT_TIMEOUT_SEC=60
```

`HF_TOKEN` is optional if the dataset is already cached.

## SSH

```bash
ssh -i ~/.ssh/semfs-benchmark ubuntu@REDACTED_BENCH_HOST
```

## Run one smoke test

```bash
DATASET=smoke /srv/semfs-benchmark/semantic-filesystem/benchmarks/aws/run_workspace_bench.sh codex
```

Swap `codex` for any other variant:

- `semfs-codex`
- `claudecode`
- `semfs-claudecode`

## Metrics returned

Each run prints a compact JSON summary with:

- `accuracySummary`
- `latencySummary`
- `tokenSummary`
- `modelSummary`
- per-case entries under `cases`

For SEMFS runs, mount overhead is stored in the per-case trace under:

- `trace.semfs.mountDurationMs`
- `trace.semfs.unmountDurationMs`

## Workspace telemetry

Each run now also writes workspace telemetry under:

```bash
/srv/semfs-benchmark/Workspace-Bench/evaluation/output/_telemetry
```

Per run, it captures:

- `snapshot_before_prepare.json`
- `snapshot_after_prepare.json`
- `snapshot_after_run.json`
- `diff_prepare.json`
- `diff_run.json`

This shows:

- which workspaces changed
- counts of created, deleted, and modified files
- sample changed paths
- prep-stage changes versus agent-run changes

Each run also writes a readable narrative:

- `run_narrative.json`
- `run_narrative.md`

Those combine:

- agent status and checks
- token counts
- last assistant message
- returned output paths
- SEMFS timing, when applicable
- workspace diff summaries for prep and run phases

## Raw outputs

Workspace-Bench writes raw artifacts under:

```bash
/srv/semfs-benchmark/Workspace-Bench/evaluation/output
```

The main benchmark report file is:

```bash
agent_runner_report.json
```
