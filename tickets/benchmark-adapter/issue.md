# Light BenchmarkAdapter — one reproducible harness, extensible to other benchmarks

_Created: 2026-06-09. Status: design (no code yet). Companions: `benchmarks/workspace_bench/judge_pipeline.md`,
`benchmarks/workspace_bench/EC2_RUNBOOK_CURRENT.md`, `tickets/format-trap-extraction-delivery/issue.md`._

## Hypothesis we are trying to test (the reason this exists)

**semfs reduces token usage while maintaining or improving accuracy** — because serving *less but
correct* context (semantic grep + extracted text + a guided `/by-topic` overlay) should both cut
tokens and raise accuracy vs. a blind `os.walk`/`grep`/open over a large real directory tree.

We want to test this **identically across multiple benchmarks**, eventually:
1. Workspace-Bench (have it)
2. supermemory/xAFS (best fit — agent-file-system dataset)
3. terminal-bench (Docker/tmux, test-scored)
4. TheAgentCompany (multi-service Docker, checkpoint-scored)

This ticket is the **light, Workspace-Bench-only, extensible** first step.

---

## Problems it solves (all observed, this session and prior)

1. **Runs are not reproducible.** The real per-case drivers — `run289.sh`, `kgrun.sh`,
   `parse289.py`, `cmd_seq.py`, `showjudge.py`, `judge_seed.yaml` — live in **ephemeral `/tmp` on
   the EC2 box**. A reboot loses them. `EC2_RUNBOOK_CURRENT.md §7` already flags "SHOULD be
   committed." There is no version-controlled definition of "how we ran X."

2. **Config is tribal knowledge.** A whole session went into re-discovering which env vars matter:
   `SEMFS_EMBED_ONNX_DIR`, `SEMFS_GRAPH_FS`, `SEMFS_KG`, `SEMFS_GREP_INLINE`, `SEMFS_EXTRACT_SIBLING`,
   the embed model, the `tag → ~/.semfs/<tag>.db` mapping. They are scattered across `run289.sh`,
   shell exports, and compile-time consts. No single declarative "run config."

3. **The judge has two runners with opposite base-URL needs.** `agent_eval.py` (OpenAI-style
   `{baseUrl}/chat/completions`, needs `…/api/v1`) vs `agent_as_a_judge.py` → `ClaudeCode.js`
   (Anthropic SDK, appends `/v1/messages`, needs `…/api`). This cost real time (the double-`/v1`
   404, the `${JUDGE_MODEL}` unexpanded-literal debris). The working recipe should be encoded once.

4. **Confounds are uncontrolled and manual.**
   - **Corpus identity (DOWNGRADED 2026-06-09 — was overstated):** an earlier claim said the semfs
     seed (1368) ≠ WB workspace (1452). Measured: the **seed DB actually holds 1454 real
     (non-derived) files**, matching `filesys/chanpin_standard` (1452) within 2 — the 1368 was the
     `chanpin_seed` *extract-source dir*, not the seed's contents. So baseline and semfs run over
     **essentially the same corpus**; this is NOT a real confound. Residual: a filename-level diff to
     prove the sets are identical (not just equal-sized), and 8 contentful files unindexed (~99%
     coverage; worth a glance). Coverage itself is proper.
   - **Layout deviation:** the semfs arm runs with `cwd = mount root`, so the agent reads from
     semfs paths and writes to `model_output/`, instead of the native `./data` (input) / `./output_cc`
     (output) layout. This *guarantees* losing rubrics `[5][6]` and makes our absolute scores
     non-comparable to the paper.
   - **Destructive seed edits** (sibling backfill) are done by hand on copies — easy to forget the
     "copies only" rule.

5. **No first-class A/B.** Baseline vs semfs is run by hand with different scripts/labels. There is
   no single command that runs both arms over the same cases and emits a uniform tokens+score table.

6. **Single-benchmark lock-in.** Everything is hardwired to Workspace-Bench. Adding xAFS /
   terminal-bench means copy-paste-mutating `run289.sh`. No seam.

7. **Result capture is ad-hoc.** Tokens parsed by hand (history of the cache-field undercount bug);
   judge JSON copied to `/tmp/judge_<stamp>.json`; behavior signals (`by-topic` nav, sibling cats,
   `semfs grep` count, 403 mentions) grepped manually each time. No structured comparison artifact.

---

## Design (light, Python, mirrors the 5 stages we already have)

The current flow is already five stages — `prepare → mount → run → score → measure`. The adapter
just makes those stages **benchmark-agnostic** and **declarative**, and **wraps (never reimplements)
each benchmark's native runner/grader**.

### 1. `BenchmarkAdapter` interface (≈5 methods)

```python
class BenchmarkAdapter(Protocol):
    name: str
    def list_cases(self) -> list[CaseId]: ...
    # Reproduce the benchmark's NATIVE layout. arm ∈ {"baseline","semfs"}.
    def prepare(self, case: CaseId, arm: Arm) -> RunContext: ...   # workdir, input_dir, output_dir, corpus_dir
    def run(self, case: CaseId, arm: Arm, ctx: RunContext) -> Trace: ...   # invoke agent; returns outputs+raw
    def score(self, case: CaseId, ctx: RunContext) -> Metrics: ...  # call NATIVE grader
    def tokens(self, trace: Trace) -> TokenBreakdown: ...           # sum all 4 usage fields
```

### 2. Declarative `RunConfig` (one version-controlled place; replaces the /tmp env soup)

```yaml
benchmark: workspace-bench
cases: [289]                 # or "smoke" / "lite"
arm: semfs                   # baseline | semfs
agent: codex                 # codex | claudecode
semfs:                       # only read when arm == semfs
  seed_tag: chanpin-gemma-q4-sib
  embed_model: gemma-q4
  embed_onnx_dir: ~/gemma_q4
  graph_fs: off              # the knobs, named once
  kg: on
  grep_inline: off
  extract_sibling: on
judge:
  model: bytedance-seed/seed-2.0-lite
  base_url: https://openrouter.ai/api/v1   # correct for agent_eval.py; encoded so nobody re-derives it
```

### 3. Thin runner — first-class A/B

`python -m benchmarks.harness.run --config runs/wb_289.yaml --arms baseline,semfs`
loops `cases × arms`, calls the adapter, writes a uniform `comparison.json`:

```
case 289 | baseline: tokens=…, score=…/15  | semfs(graphfs=off,sibling): tokens=…, score=…/15
         | behavior: by-topic=…, sibling_cats=…, semfs_grep=…, saw_403=…
```

### 4. `WorkspaceBenchAdapter` (the only implementation for now)

- `prepare()` reproduces the **native** WB run layout — the agent runs at the **root of the full
  persona filesystem** (e.g. `Desktop/ Sales/ desktop/fashion_ecommerce/product_data/…`), inputs at
  their realistic deep paths. **NO `./data` / `./output_cc` at runtime** — those exist only in the HF
  dataset *packaging* (`hf_downloads/.../289/data/`), unpacked into real paths by the harness.
- `run()` shells the EXISTING upstream `agent_runner.py` / `agents/codex.py` (baseline) and the
  semfs mount logic from `semfscodex.py` (semfs arm) — no reimplementation.
- `score()` shells the EXISTING `agent_eval.py` + the encoded `judge_seed.yaml`.
- `tokens()` reuses the validated 4-field usage sum.

---

## Key changes baked in (each fixes a Problem above)

| Change | Fixes | Note |
|---|---|---|
| **Corpus identity check** — confirm seed file set == WB workspace via a filename-level diff (counts already match: 1454 ≈ 1452) | P4 (downgraded) | NOT a reseed; just verify identity + check the 8 unindexed contentful files |
| ~~Layout mirroring~~ **REJECTED 2026-06-09** — was: mount semfs at `./data`, write `./output_cc`. | — | **Premise was wrong.** Verified the BASELINE (plain codex) ALSO fails `[5][6]` — the run workdir has no `./data`/`./output_cc`; files live at `desktop/fashion_ecommerce/product_data/…` and output at `model_output/`. So `[5][6]` are boilerplate referencing the HF *packaging* layout, unsatisfiable for everyone (baseline + semfs). Mirroring fixes nothing and makes the topology LESS realistic. **Drop it.** The current "agent at the root of the full FS" topology IS the real use case AND matches how the paper runs. |
| **Judge encoded once** — the working `agent_eval.py` + `judge_seed.yaml` (baseUrl `…/api/v1`) captured in `score()`; the `agent_as_a_judge.py`/ClaudeCode path documented as the other runner | P3 | no more base-URL archaeology |
| **Git-track the drivers** — move `run289.sh`/`judge_seed.yaml`/parsers from `/tmp` into `benchmarks/harness/` | P1 | survives reboot; reproducible |
| **Declarative RunConfig** | P2 | knobs named in one file |
| **Uniform `comparison.json`** + behavior signals | P5, P7 | one A/B artifact per run |
| **Score reporting split** — report raw 15 AND an "agent-addressable" subset that excludes the boilerplate `[5][6]` (packaging-path) AND metadata meta-task `[8][9][10]` — all 5 fail for baseline too | clarity | real signal = 4 easy + 6 honesty rubrics; ceiling 10/15 for everyone incl. paper |

---

## Non-goals (keep it light)

- **No** plugin registry / dynamic discovery — one hardcoded `WorkspaceBenchAdapter`.
- **No** Docker/FUSE execution abstraction yet (that lands with terminal-bench / TheAgentCompany).
- **No** xAFS / terminal-bench / TheAgentCompany adapters — only the *interface* is shaped so they
  can slot in later (none are present in the repo or on the box as of this ticket).
- **No** new scorer — always wrap the benchmark's native grader.

## How the others slot in later (sketch, not built)

- **xAFS**: `prepare()` lays out the dataset's file corpus; `run()` host-runs the agent; `score()`
  wraps xAFS's accuracy metric. Likely the cleanest fit — host-runnable, file-grounded.
- **terminal-bench / TheAgentCompany**: add a `DockerRunContext` (FUSE device / host-mount +
  bind-mount); `score()` wraps their test/checkpoint scorer. semfs applies only to the
  document/file-grounded subset of tasks.

---

## Build sequence (with verify criteria)

1. `benchmarks/harness/{adapter.py, config.py, run.py}` — interface + RunConfig + runner skeleton.
   → verify: `run.py --config wb_289.yaml --arms baseline` reproduces today's baseline number.
2. `WorkspaceBenchAdapter` wrapping existing upstream runner + judge.
   → verify: `--arms semfs` reproduces a known semfs number (e.g. Run B 6/15 ~100K) within variance.
3. Corpus identity check (no reseed): filename-diff seed vs `chanpin_standard`; eyeball 8 unindexed.
   → verify: seed file set == WB workspace set (counts already 1454 ≈ 1452).
4. Move `/tmp` drivers into `benchmarks/harness/` and git-track.
   → verify: a clean checkout + box reboot can still run a case end-to-end.

(Layout mirroring step removed — see REJECTED note above. Both arms run at the full-FS root; no
`./data`/`./output_cc` staging needed, and it wouldn't change any score.)

---

## Open questions (need a decision before coding)

1. **Layout mirroring** — RESOLVED (REJECTED): baseline also fails `[5][6]` (no `./data`/`./output_cc`
   at runtime; both arms run at the full-FS root). Mirroring fixes nothing and reduces realism. The
   current topology is already both realistic AND paper-comparable. No action.
2. **Corpus source of truth** — RESOLVED/downgraded: seed DB already holds 1454 files ≈ WB
   workspace 1452, so no reseed needed. Just run a filename-level diff to confirm set identity and
   eyeball the 8 unindexed contentful files. *No action required unless the diff surprises us.*
3. **Language/placement** — `benchmarks/harness/` Python package (matches existing Python harness).
   Confirm this is where it should live.
