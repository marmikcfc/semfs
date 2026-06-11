# semfs benchmark environment on Modal

A parallel, reproducible complement to the EC2 box (`EC2_RUNBOOK_CURRENT.md`).
The box keeps two jobs Modal can't do; Modal fixes two problems the box can't.

## The one architectural decision: MOUNTLESS semfs

Modal containers run in gVisor — **no FUSE**, so `semfs mount` cannot run here.
But the E-campaign established that under `SEARCH_ONLY=off` the mount's
*agent-visible* surface is exactly three things, all replicable without a mount:

| mount provides | mountless equivalent |
|---|---|
| the real file tree (SO=off shows it anyway) | corpus copied to local disk |
| `AGENTS.md` at the root | extracted from the seed's `fs_data` (byte-identical hint) |
| `semfs grep` answering from the index | `semfs grep --tag` → daemonless direct-open of the seed db |

Accepted differences: no FUSE latency; agent writes land on plain disk (fine —
benchmarks never read them back through the index). **FUSE-fidelity runs (mount
behavior itself, daemon paths, pglite) stay on the EC2 box.**

## What Modal buys us

- **Parallel reps**: `run_batch --reps 10` = ten runs in one wave. The box
  serializes (one mount at a time); n≥3 discipline stops being expensive.
- **Reproducibility**: the semfs binary is compiled into the image from a pinned
  git ref (`SEMFS_GIT_REF` in `semfs_modal.py`) — no more "which binary is on the
  box" archaeology. The git sha is stamped into every result.
- **Disposable infra**: no 95%-full disk, no stale daemons, no seed contamination —
  every run starts from the volume's pristine copy.

## Setup (once)

```bash
pip install --upgrade modal && modal token new          # if not authed
modal secret create openrouter OPENROUTER_API_KEY=sk-or-...
# for the one-time data pull only (your call — this places the box key in Modal):
modal secret create semfs-box-ssh SSH_KEY="$(cat ~/.ssh/semfs-benchmark)"
modal volume create semfs-bench-data

modal run benchmarks/modal/semfs_modal.py::verify_image   # builds image (~10 min first time)
modal run benchmarks/modal/semfs_modal.py::pull_from_box  # seeds volume (~1.5GB, ~5-10 min)
modal run benchmarks/modal/semfs_modal.py::smoke_grep     # ← the feasibility gate
```

`smoke_grep` is the go/no-go: it verifies the **daemonless `semfs grep --tag`**
path serves the index without a mount, across all three render modes. If it
fails on tag resolution, the fallback is registering a marker or passing the db
path explicitly — see `resolve_index` in `crates/semfs/src/cmd/grep.rs`.

## Running

```bash
# one case, parallel reps
modal run benchmarks/modal/semfs_modal.py::run_batch --case 289 --reps 4 --render-mode paths

# single run with knob overrides
modal run benchmarks/modal/semfs_modal.py::run_case --case 289 --label t1 \
  --extra-env "SEMFS_GREP_COMPRESS=on,SEMFS_GREP_COMPRESS_MIN=3000"
```

## Volume layout (`semfs-bench-data` → `/data`)

```
/data/seeds/chanpin-clean.db, chanpin-leanhint3.db   # leanhint3 = shipped v4.1 hint
/data/models/gemma_q4/                                # BYO-ONNX embedder
/data/corpus/chanpin_standard/                        # the 1,452-file persona (pristine)
/data/wb/evaluation/                                  # Workspace-Bench harness incl. judge
/data/codex/config.toml                               # the box's codex provider config
```

## Known gaps / next steps

- **Judging** is not yet wired in `run_case` (tokens/calls/deliverables are
  collected; rubric scoring needs `agent_as_a_judge.py` deps from `/data/wb` —
  add once smoke passes). Interim: pull deliverables and re-judge on the box.
- `--tag` daemonless resolution is the one untested assumption (gate: `smoke_grep`).
- Cases other than 289/95 need their `--memory-paths` analog (case-scoped corpus
  subsets) if we want import-scoped parity with the box driver.
- Token metering parity: codex env on Modal uses the same OpenRouter provider
  config as the box (`/data/codex/config.toml`), so usage numbers are comparable.
```
