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

modal run benchmarks/modal/modal_min_smoke.py::smoke     # cheapest Modal + Volume sanity check
modal run benchmarks/modal/semfs_modal.py::e9w2_smoke     # preflight + seed/check + grep + case-289 smoke
```

`modal_min_smoke.py::smoke` is the first gate: it starts a tiny Python container
and writes/reads `/data/_smoke/modal_min_smoke.json` on the shared Modal Volume.

`e9w2_smoke` is the benchmark path. It verifies the image, checks the shared
volume, seeds from EC2 if needed, runs `smoke_grep`, then runs case 289 with the
E9w2-shaped knobs. `smoke_grep` is the go/no-go: it verifies the **daemonless `semfs grep --tag`**
path serves the index without a mount, across all three render modes. If it
fails on tag resolution, the fallback is registering a marker or passing the db
path explicitly — see `resolve_index` in `crates/semfs/src/cmd/grep.rs`.

## Running

```bash
# one command: local checks + minimal smoke + benchmark smoke
benchmarks/modal/run_modal_smoke.sh

# only the cheap Modal + Volume smoke
RUN_BENCHMARK=0 benchmarks/modal/run_modal_smoke.sh

# cheapest Modal + shared Volume smoke
modal run benchmarks/modal/modal_min_smoke.py::smoke

# one case, parallel reps against the benchmark-materialized corpus
modal run benchmarks/modal/semfs_modal.py::run_batch --case 289 --reps 4 --render-mode paths

# the closest Modal equivalent to the planned EC2 E9w2 smoke
modal run benchmarks/modal/semfs_modal.py::e9w2_smoke

# if the volume is already seeded and you want to avoid the EC2 pull path
modal run benchmarks/modal/semfs_modal.py::e9w2_smoke --no-seed-if-missing

# single run with knob overrides
modal run benchmarks/modal/semfs_modal.py::run_case --case 289 --label t1 \
  --extra-env "SEMFS_GREP_COMPRESS=on,SEMFS_GREP_COMPRESS_MIN=3000"

# same runner, but against the clean extract-source corpus instead of the WB materialization
modal run benchmarks/modal/semfs_modal.py::run_case --case 289 --label raw \
  --corpus-name chanpin_seed
```

## Volume layout (`semfs-bench-data` → `/data`)

```
/data/seeds/chanpin-gemma-q4.db                      # current canonical q4 seed
/data/models/gemma_q4/                                # BYO-ONNX embedder
/data/corpus/chanpin_seed/                            # clean extract source (1,368 files, 0 junk)
/data/corpus/chanpin_standard/                        # WB materialized corpus (~1,452 files, parity target)
/data/wb/evaluation/                                  # Workspace-Bench harness incl. judge
/data/codex/config.toml                               # the box's codex provider config
```

Why both corpora:

- `chanpin_seed` matches the clean source directory documented in `CURRENT_STATE.md`.
- `chanpin_standard` matches the materialized workspace the benchmark harness actually runs over.

Use `chanpin_standard` when you want EC2 benchmark parity. Use `chanpin_seed` when you want the raw clean source.

## E9w2 parity note

The planned EC2 E9w2 run in the handoff uses:

- case `289`
- arm `nokg`
- `SEMFS_SEARCH_ONLY=off`
- `SEMFS_GREP_RENDER_MODE=two-tier`
- `SEMFS_RESULT_LIMIT=5`
- `SEMFS_GREP_RESULT_CAP=6144`
- `SEMFS_GREP_TOTAL_CAP=10240`

`e9w2_smoke` encodes that same shape for the Modal mountless runner. It is not byte-for-byte identical to the EC2 FUSE harness, but it is the closest equivalent in Modal's no-FUSE environment.

## Shared volume across apps

Yes: a named Modal Volume can be mounted by multiple apps/functions. The existing code already uses:

```python
data_volume = modal.Volume.from_name("semfs-bench-data", create_if_missing=True)
```

Any other Modal app in the same workspace/environment can attach that same volume by name. The right pattern here is:

- keep immutable-ish assets in the shared volume: seed DB, ONNX model dir, raw dataset, WB harness
- mount read-only in reader/eval apps where possible
- avoid concurrent writes to the same files
- if you need stronger multi-writer behavior, prefer writing distinct files or put the raw dataset in S3/R2 and mount it with `CloudBucketMount`

## Known gaps / next steps

- **Judging** is not yet wired in `run_case` (tokens/calls/deliverables are
  collected; rubric scoring needs `agent_as_a_judge.py` deps from `/data/wb` —
  add once smoke passes). Interim: pull deliverables and re-judge on the box.
- `--tag` daemonless resolution is the one untested assumption (gate: `smoke_grep`).
- Cases other than 289/95 need their `--memory-paths` analog (case-scoped corpus
  subsets) if we want import-scoped parity with the box driver.
- Token metering parity: codex env on Modal uses the same OpenRouter provider
  config as the box (`/data/codex/config.toml`), so usage numbers are comparable.
