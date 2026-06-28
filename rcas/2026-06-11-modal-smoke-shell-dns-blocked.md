# RCA: Modal smoke cannot run from current shell — DNS/outbound network blocked

**Date:** 2026-06-11
**Scope:** Modal smoke for `tickets/workspace-bench-5arm-matrix`, using `benchmarks/modal/semfs_modal.py`.

## Symptom

Running:

```bash
modal run benchmarks/modal/semfs_modal.py::e9w2_smoke --no-seed-if-missing
```

fails before the Modal app can execute:

```text
Could not connect to the Modal server.
```

## Evidence

- `modal --version` works locally (`modal client version: 1.5.0`).
- `modal profile current` works locally (`ada-diffusion-llm`), so the CLI exists and has a profile.
- `python3 -m py_compile benchmarks/modal/modal_min_smoke.py benchmarks/modal/semfs_modal.py` passes.
- Local imports of both Modal modules pass.
- Even the minimal smoke, which does not use semfs, EC2, OpenRouter, Codex, or the benchmark image, fails before app code runs:

```bash
modal run benchmarks/modal/modal_min_smoke.py::smoke
```

```text
Could not connect to the Modal server.
```

- Direct DNS resolution from the shell fails:

```text
api.modal.com gaierror [Errno 8] nodename nor servname provided, or not known
modal.com gaierror [Errno 8] nodename nor servname provided, or not known
status.modal.com gaierror [Errno 8] nodename nor servname provided, or not known
```

- `curl https://api.modal.com` fails with:

```text
Could not resolve host: api.modal.com
```

- SSH to the EC2 benchmark box by raw IP also fails:

```text
ssh: connect to host 13.201.35.159 port 22: Operation not permitted
```

## Root Cause

The Codex execution shell cannot make the outbound network/DNS connections required by the Modal CLI. This is an environment restriction outside the repo. It is not a Modal outage and not a syntax/import failure in the Modal runner.

User terminal evidence later showed `curl -Iv https://api.modal.com` resolves DNS, connects TCP, completes TLS, negotiates HTTP/2, and receives an HTTP response. That proves the user's terminal/network can reach Modal. The blocker is specific to the Codex sandbox shell used for these tool calls.

## Fix Applied

`benchmarks/modal/modal_min_smoke.py` now provides the smallest possible Modal check:

- tiny `python:3.11-slim` image
- shared `semfs-bench-data` volume mount
- write/read `/data/_smoke/modal_min_smoke.json`
- no EC2, secrets, Rust build, Codex, OpenRouter, or benchmark data

Command:

```bash
modal run benchmarks/modal/modal_min_smoke.py::smoke
```

`benchmarks/modal/run_modal_smoke.sh` wraps the complete local flow for a network-enabled terminal:

```bash
benchmarks/modal/run_modal_smoke.sh
```

Use `RUN_BENCHMARK=0 benchmarks/modal/run_modal_smoke.sh` to run only the cheap Modal + Volume smoke.

`benchmarks/modal/semfs_modal.py` now has a hardened one-command smoke path:

- local preflight for `api.modal.com` DNS reachability
- remote `verify_image`
- shared-volume readiness check
- optional `pull_from_box` when volume assets are missing
- `smoke_grep`
- case-289 `run_case` with E9w2-shaped knobs

Command:

```bash
modal run benchmarks/modal/semfs_modal.py::e9w2_smoke
```

## Remaining Work

Run the command from the user's terminal, which has already proven outbound connectivity to Modal. This Codex sandbox cannot prove the final smoke because it cannot reach Modal or EC2.
