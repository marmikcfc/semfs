# Experiment: plain arm on WB-Lite (4 personas, n=3)

**Linear:** [SEM-39](https://linear.app/semfs/issue/SEM-39) · folder `tickets/wblite-plain-4persona-n3/`
**Created:** 2026-06-22

## Goal
Measure the **plain** (no-semfs baseline) arm across the WB-Lite persona suite,
**excluding research(er) and xafs** — i.e. the 4 "small" personas with baked E2B
templates. n=3 repetitions. Establishes the plain baseline (accuracy + token usage)
the semfs arms are compared against.

## Scope
Plain arm only. Agent runs in the raw persona workspace (corpus baked into the
template's `/opt/corpus.tgz`, extracted in-sandbox). **No GPU for the plain arm
itself** (no semfs mount, no GLM/gemma seed) — UNLESS we run the agent on the
self-hosted GLM-5.1-NVFP4 for token-comparability (decision below).

| Persona | Role | Template (corpus source) | Cases | n |
|---|---|---|---|---|
| chanpin | Product Manager | semfs-mount-chanpin | 10 | 3 |
| kaifa | Backend Developer | semfs-mount-kaifa | 11 | 3 |
| houqin | Logistics Manager | semfs-mount-houqin | 30 | 3 |
| yunying | Operations Manager | semfs-mount-yunying | 31 | 3 |

**Total: 82 cases × n=3 = 246 cells per agent.** (chanpin 289 dropped — see audit.)

Case sets:
- chanpin: 15,44,45,53,55,95,171,175,386,388  (289 EXCLUDED — leak)
- kaifa: 3,7,91,92,94,226,242,266,286,300,311
- houqin: 23,35,37,47,54,72,79,83,85,87,100,102,116,207,251,255,258,267,274,276,314,328,329,337,354,357,358,372,373,374
- yunying: 33,38,107,108,137,139,143,146,154,158,159,160,161,191,192,224,244,269,277,278,284,287,288,291,306,334,340,346,359,380,381

## Harness
`benchmarks/e2b/run_matrix.py --arms plain --cases <csv> --agents <agent> --rep <N> --parallel <P>`
- Per non-chanpin persona: `WB_E2B_TEMPLATE=semfs-mount-{persona}` + `WB_LITE_DIR=<unified metadata dir>`.
- Metadata: `benchmarks/e2b/assets/wb_lite_all/lite_all/task_lite_clean_en/` (all personas).
- Output: `tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs/` (labels `pm_{agent}_{case}_plain_r{rep}`).
- ALL runs on E2B real-FUSE (HARD RULE) — never Modal.

## Config (confirmed 2026-06-22)
- **Agent:** codex only (matches recent PM/kaifa runs; primary).
- **Model:** GLM-5.1-NVFP4 self-hosted (token-comparable with the semfs arms).
  Path: codex → litellm proxy (`glm51-nvfp4-litellm`) → vLLM (`glm51-nvfp4-vllm`, 4×B200).
  GPU FENCED to the run (deploy → warm → run → auto-stop via `trap … EXIT`).
- **Parallelism:** `--parallel 8` (under E2B ~20-sandbox cap; cuts wall-clock → GPU bill).
- **Run script:** `tickets/wblite-plain-4persona-n3/run_plain.sh`.
- Est. GPU window: ~30–43 min cold start + ~4–8 hr for 249 cells ≈ ~$100–200.

## Prerequisite — E2B templates rebuilt in NEW account (2026-06-22)
The E2B API key was rotated to a **new account**, so all `semfs-*` templates were gone
(E2B templates are account-scoped). The root base `semfs-baked` recipe was never committed
(built 2026-06-13 by ephemeral `/tmp/e2b_matrix/e2b_build_baked.py`), so it was **reconstructed
from scratch** per `E2B_RUNBOOK.md §2` from still-addressable inputs → new script
`benchmarks/modal/bake_semfs_baked.py` (ubuntu:24.04 + fuse3/python3/node20 + `@openai/codex@0.133.0`
+ WB harness w/ `npm install` + gemma-q4 embedder + semfs binary + shims + `/opt/cases`; NO baked creds).
Then the 4 persona templates were re-baked on top via `bake_e2b_persona.py`.

- Modal secret `e2b` + local `.env E2B_API_KEY` → new account (verified masked readback).
- Built: `semfs-baked` (id ff140u4btlhn9yl4l0bd) → `semfs-mount-{chanpin,kaifa,houqin,yunying}`.
- Plain smoke `benchmarks/e2b/smoke_plain_template.py --persona all` = **ALL_PASS**: each template
  extracts its OWN persona's corpus (chanpin 1452 / kaifa 2724 / houqin 3838 / yunying 2852 files;
  top-level `{persona}_standard/` confirmed — closes the silent chanpin-fallback trap), codex-cli 0.133.0 runs.
- NOTE: runtime is **amd64/x86_64** (sandbox apt = `:amd64`). The build-time `arm64-linux/rg` was
  just the claude-agent-sdk's vendored ripgrep selection, NOT the runtime arch. So x86_64
  `semfs-fixed` execs fine → mount/cloud arms are arch-OK (earlier "arm64" caveat was wrong).
- NOTE: kaifa plain corpus is the UNPRUNED 1.9 GB tree (extracts fine, 2724 files); fairness-vs-seed
  nuance only, not a blocker.
- FIX (2026-06-23): office writer libs (python-docx/pptx/openpyxl) were NOT in the rebuilt
  `semfs-baked` (original v3 lineage had them) → `.pptx` cases produced no deliverable. Now
  installed at boot via `run_matrix.boot_prep` (apt python3-pip + pip `--break-system-packages`,
  ~16s/sandbox, idempotent, `WB_BOOT_WRITER_LIBS=0` to disable). Verified: pptx write works on the
  live template. See rcas/2026-06-23-semfs-baked-empty-cases-and-litellm-proxy-degradation.md.

## Pre-flight contamination + completeness audit (2026-06-22, `/tmp/audit_personas.py`)
Volume-side scan of all 4 personas' corpus + seed.
- **chanpin corpus: 1 real leak → case 289.** `best_selling_product_core_data_list.txt` (the
  finished top-10 answer) is baked in the corpus AND the rubric wants a "403 Forbidden / source
  inaccessible" report → doubly broken. **Dropped from the run** (chanpin now 10 cases).
- chanpin case 53 flagged but NOT a leak: `interaction_document_{6,8,10,13}.txt` are the task's
  SOURCE docs (real deliverable = "Integrated Data Interaction Report", absent). Kept.
- **kaifa / houqin / yunying corpora: clean** — 0 answer leaks, 0 solution dirs, 0 surface artifacts.
- **Seeds (all 4): healthy** — `dup_groups=0`, KG present (graph_entity 6.2k–10.4k), surface-clean,
  no answer leaks (except chanpin's 289/53 files, moot: plain doesn't use the seed; 289 excluded).
  distinct_files: chanpin 656 / kaifa 2415 / houqin 2313 / yunying 1622.
- Resolves the `_r{1,2,3}` namespace collision: `run_plain.sh` now passes `--force` for **chanpin only**
  (the only persona with stale ChatGPT/OpenRouter priors); others run fresh.

## Status
- [x] Config confirmed (codex × GLM-5.1-NVFP4, n=3, 4 personas)
- [x] E2B templates rebuilt in new account + plain smoke ALL_PASS (2026-06-22)
- [x] Pre-flight audit done: 289 dropped (leak), chanpin `--force`, others clean
- [ ] Run launched (n=3) — chanpin canary first (`WB_PERSONAS=chanpin`), then kaifa/houqin/yunying
- [ ] Judged (accuracy + tokens)
- [ ] Results written here + Drive export
