# RCA — SEM-39 chanpin canary: empty /opt/cases + litellm proxy degradation

**Date:** 2026-06-23
**Component:** E2B template reconstruction (new account) + codex→GLM-5.1-NVFP4 path
**Status:** RESOLVED — canary3 ran 30/30 clean (chanpin, 10 cases × n=3)

## Context
E2B API key rotated to a new account → all `semfs-*` templates gone (account-scoped).
Rebuilt `semfs-baked` (root base) from scratch + the 4 persona templates. Two distinct
bugs surfaced while running the chanpin plain canary on self-hosted GLM-5.1-NVFP4.

## Bug 1 — empty `/opt/cases` → every cell exits 1 (canary 1, all 30 cells)
**Symptom:** every cell `CommandExitException(exit_code=1, stdout='', stderr='')`. Empty
stdout = crash before any print.
**Root cause:** `cell_driver.py:134` reads `/opt/cases/<id>.task` (the task instruction)
before any output. `run_matrix.py:5` documents cases are *baked* into the template (it does
NOT push them at boot). The original `semfs-baked` baked all cases into `/opt/cases`
(RUNBOOK §2: "cases /opt/cases"); my reconstruction created the dir **empty** (`mkdir -p`),
so the open() → `FileNotFoundError` → exit 1 on every cell.
**Why missed:** RUNBOOK §2 line was read as "a dir" not "a dir of .task files"; and the
smoke validated boot contract (corpus/codex/harness) but never executed `cell_driver`.
**Fix:** `bake_semfs_baked.py` bakes all 100 case `.task` files (from volume
`wb/lite_all/task_lite_clean_en/<id>/metadata.json["task"]`) into `/opt/cases`; build-time
sanity now asserts `15.task`. `smoke_plain_template.py` now checks `/opt/cases/*.task` →
catches this with **no GPU**. Confirmed: canary2 rep1 = 10/10 after the fix.
**Gotcha:** `--force` re-runs a cell but does NOT pre-delete `result.json`; a crash mid-cell
leaves the prior run's file, so a ghost `status=ok` (here: June-14/15 `native(chatgpt)` cells)
masquerades as success. Always check `auth_used`/mtime, not just status.

## Bug 2 — litellm proxy degrades across reps → cells stall (canary 2, rep 2)
**Symptom:** rep1 = 10/10 clean; rep2 hung after ~1 cell — 6/8 cells stuck "running" but
not completing for 20+ min.
**Key evidence:** vLLM engine logs showed `Running: 0 reqs` / idle during the hang — the
requests never reached the GPU. So the stall was **upstream of vLLM**, at the shared
litellm proxy (the one component that persists across reps; per-cell codex adapters are
fresh each cell). rep1 (fresh proxy) clean; rep2 (proxy hammered by rep1's parallel-8 load)
stalled.
**Confirmation:** small watched repro of the 6 hung cases with a **freshly-restarted proxy**
(parallel 2) → 6/6 clean, incl. a 1.16M-token cell.
**Fix:** `run_plain.sh` restarts the litellm proxy (`modal app stop` + `deploy`) **before
each rep** (~15s CPU redeploy). Validated: canary3 = 30/30 at PAR=8, incl. rep2 (10/10) and
rep3 (10/10) — the watchdog stayed silent.

## Tooling added (for future hang forensics)
- `/tmp/capture_sandbox.py` — `Sandbox.connect()` to live sandboxes; dumps codex/cell_driver
  process state, `/tmp/*.err`, codex_stdout/chat_adapter_log tails, and open network conns
  (is codex blocked on the proxy?). Run the instant a stall fires, before sandboxes are killed.
- Stall watchdog (Monitor loop): alerts ≤7 min into a hang (gap since last cell start/completion
  while cells in-flight) with the live sandbox IDs. v1 bug: measured gap from arm-time (swept in
  the warm window) → false alarm; v2 measures gap from last *progress*.

## Bug 3 — office writer libs missing → .pptx cases produce no deliverable (canary3 sanity)
**Symptom:** canary3 ran 30/30 but chanpin cases 386 (×3) + 388 (×1) had empty `deliverables`;
`model_output` dirs were empty (not a filename-lottery — no file at all).
**Root cause:** the rebuilt `semfs-baked` omitted python-docx/pptx/openpyxl. The agent's
`/usr/bin/python3` has no pip and no network-pip at runtime, so `.pptx` cases can't write their
deliverable (GLM loops on `pip install python-pptx`). The original v3 lineage baked these (→58/60,
CURRENT_STATE). `.xlsx`/`.docx` mostly survive (agent hand-builds the OOXML zip via stdlib).
**Fix:** install at boot in `run_matrix.boot_prep` (apt python3-pip + pip
`--break-system-packages python-docx python-pptx openpyxl`, ~16s/sandbox, idempotent,
`WB_BOOT_WRITER_LIBS=0` to disable). Verified on the live amd64 template: install 16s, wrote a
valid 28 KB .pptx. (User chose boot-install over re-baking to avoid another rebuild cycle.)

## Observations (not bugs)
- **Runtime is amd64/x86_64** (sandbox apt = `:amd64`). The build-time `find` returned
  `arm64-linux/rg` only because that's the claude-agent-sdk's vendored ripgrep selection — NOT the
  runtime arch. So x86_64 `semfs-fixed` execs fine; the mount/cloud arms are arch-OK. (An earlier
  "new account is arm64 → semfs won't exec" reading was WRONG.)
- chanpin plain crawls heavily on GLM (no index): raw per-cell totals 60K–3.03M tokens
  (codex caches ~80–88%, so fresh input is far lower). Expected for the plain baseline.
