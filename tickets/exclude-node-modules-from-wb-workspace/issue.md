# Tech debt: exclude `node_modules` from the Workspace-Bench workspace (prepare copy + search corpus)

- **Type:** Tech debt / benchmark hygiene + perf — vendored JS deps bloat prepare AND pollute retrieval
- **Status:** **IMPLEMENTED 2026-06-05** (both workstreams; unit test + clippy + full `semfs` suite
  green). Prepare-time wall-clock improvement to be confirmed on the next bench run. See
  "Implementation".
- **Created:** 2026-06-05
- **Component:** WB harness `make_filesys` / `prepare_workdirs_for_run.py` (workspace copy);
  semfs seed/import (search corpus). Dataset-side: the WB `chanpin` workspace ships `node_modules`.
- **Branch context:** `feat/backend-agnostic-store`

## Problem

The WB `chanpin` workspace contains vendored **`node_modules`** trees — the installed dependency graph
of the `docx` npm package (used by the dataset authors to generate the sample `.docx` files). E.g.
`…/interview_assessment_records/node_modules` = **380 tiny files / 9.4 MB** (`docx`, `jszip`, `pako`,
`sax`, `readable-stream`, `@types/node`, …), with sibling `package.json` + `package-lock.json`. There
are multiple such trees across the corpus.

This debris hurts in **two** places:

1. **Prepare is ~6 min, file-count-bound.** `make_filesys` does `rmtree` + `shutil.copytree` of the
   2,128-file / 516 MB workdir **every run**. 516 MB copies in seconds; the cost is the **thousands of
   tiny `node_modules` files** (per-file Python `copy2` syscalls, no parallelism, slow chmod-retry).
2. **Retrieval/RRF pollution.** Those same files (`package-lock.json`, `@types/*`, `gen_*.js`) are
   indexed and matched "product" via the code lane, flooding the RRF candidate pool and burying answer
   files on content queries — a primary cause analyzed in
   `rcas/2026-06-04-rrf-chunk-mass-bias-code-lane-pollution.md`. (We already skip `node_modules` at
   *seed* time in one place, but they're still copied every prepare and still appear in some indexes.)

## Is it safe to remove? — Yes (with one guard)

`node_modules` is the **dataset author's** build tooling (used once to *generate* the `.docx` files at
dataset-creation), **not an agent runtime dependency**. Agents (codex GPT-5.4, Claude Code) write
deliverables with **their own** capabilities (direct file writes; `python-docx`/`openpyxl`), never the
workspace's vendored JS — confirmed by case 289, where codex produced a `.txt` with no JS involvement.
`node_modules` is regenerable (`npm install`); you never commit it.

**Guard:** if any WB task explicitly instructs the agent to *run the provided JS generator*, it would
need the deps. Not audited across all tasks → keep `package.json`/`package-lock.json` and the produced
`.docx` outputs; drop only the installed `node_modules/` dirs (so `npm install` could restore if ever
needed). The deliverable docs stay; only the deps go.

## Proposal

1. **Search corpus / index (definitely safe, do first):** ensure semfs seed/import **skips
   `node_modules`** (and `package-lock.json`) everywhere — extend the existing seed skip so no index
   contains them. Directly removes the RRF pollution.
2. **Prepare workspace copy:** exclude `node_modules/` from `make_filesys`'s copy (e.g. `copytree(...,
   ignore=shutil.ignore_patterns("node_modules"))`, or switch to `rsync --exclude node_modules` / `cp
   --reflink`). Cuts the per-run prepare from ~6 min toward seconds. Keep `package.json` so the rare
   "run the generator" task can `npm install`.

## Acceptance
- Seeded indexes contain **no** `node_modules`/`package-lock.json` entries; the RRF top for content
  queries is free of JS-dep files.
- `prepare` time drops substantially (no longer copying thousands of tiny dep files).
- Agent task results are **unchanged** (deliverable `.docx` and `package.json` retained; agents never
  used the vendored deps).

## Notes
- Partly an **upstream dataset** issue (WB ships `node_modules` in `chanpin`); the prepare-side ignore
  and the corpus-side skip both live in our harness/seed code, so we can fix it locally without waiting
  on the dataset.
- Faster-copy is a complementary win (native `cp -a`/`rsync`/reflink vs `shutil.copytree`) — see the
  separate "prepare speed" note; this ticket's `node_modules` exclusion is the highest-leverage part.

## Implementation

Two surgical edits, one per workstream.

1. **Corpus / index skip** — `crates/semfs/src/cmd/daemon_runtime.rs`, `collect_file_paths_recursive`.
   The `node_modules` *directory* was already skipped (added by the seed-hang RCA). The only residual
   was `package-lock.json`, a *sibling* of `node_modules` (outside the dir skip) whose long dependency
   listing pollutes the code lane. Added a file-level skip for exactly `package-lock.json`; **kept
   `package.json`** per the guard (a generator task could `npm install`). `is_macos_noise_path` was
   deliberately *not* reused — it's strictly macOS-noise and also gates a different (cache-insert) call
   site.
   - Unit test `collect_skips_node_modules_and_lockfile_keeps_package_json`: builds a tempdir with
     `report.docx`, `package.json`, `package-lock.json`, `node_modules/docx/index.js`; asserts the doc
     and `package.json` are collected, and neither `package-lock.json` nor anything under
     `node_modules` is. Passes; clippy clean; full `semfs` suite 46/46.

2. **Prepare workspace copy** — `benchmarks/vendor/Workspace-Bench/evaluation/src/filesys_utils.py`,
   `make_filesys`. Added `ignore=shutil.ignore_patterns("node_modules")` to the `copytree(raw →
   standard)`. `ignore_patterns` runs per-directory, so every nested `node_modules` tree is pruned, not
   just the top one. `standard` no longer carries the deps, and `filesys_rollback` (which syncs
   `work_dir` to match `standard`) propagates the exclusion to the per-case workdir for free.
   `package.json`/`package-lock.json` are retained on disk (only the installed `node_modules/` dirs go).

**Not done (out of scope, by design):**
- The complementary **faster-copy** swap (`cp -a`/`rsync`/reflink vs `shutil.copytree`) — tracked
  separately; this ticket's `node_modules` exclusion is the highest-leverage part.
- `gen_*.js` generator scripts are **kept** (the guard wants the generator runnable; only the lockfile
  was named in Acceptance).
- Prepare wall-clock delta is asserted by construction (thousands of tiny files no longer copied) but
  the measured number lands on the next bench run.

## Related
- `rcas/2026-06-04-rrf-chunk-mass-bias-code-lane-pollution.md` — `node_modules`/JS files polluting RRF.
- `tickets/rrf-chunk-mass-and-lane-fusion/` — the RRF fix that this complements (cleaner corpus + cleaner
  fusion).
