# E2B EXPERIMENT LEDGER — semfs WB-Lite PM matrix

```
╔══════════════════════════════════════════════════════════════════════════╗
║                                                                            ║
║   ███  EVERY TEST RUNS ON E2B — REAL FUSE MOUNT. NO MODAL.  ███            ║
║                                                                            ║
║   Modal = gVisor = NO FUSE = mountless only = NOT how a user runs semfs.   ║
║   E2B  = Firecracker microVM = real `semfs mount` = realistic usage.       ║
║                                                                            ║
║   Credentials are injected at RUNTIME into the ephemeral sandbox           ║
║   (export CLAUDE_CODE_OAUTH_TOKEN / upload ~/.codex/auth.json) —           ║
║   never baked into an image, never a Modal secret.                         ║
║                                                                            ║
╚══════════════════════════════════════════════════════════════════════════╝
```

Owner: Marmik · Created 2026-06-13 · Status: ACTIVE
This is the single source of truth for what we run, on which platform, and the result.
**Rule: if an experiment is not on E2B, it does not count. Update this file after every run.**

---

## 0. Why E2B (the platform rule, in plain words)

`semfs grep` only does its real job — semantic search — **when the agent runs inside a
mounted semfs filesystem**. A normal `grep` inside a `semfs mount` becomes a semantic
grep. That mount needs **FUSE**.

| Platform | Sandbox tech | FUSE? | What semfs can do | Verdict |
|----------|-------------|-------|-------------------|---------|
| **E2B**  | Firecracker microVM | ✅ yes | real `semfs mount`, agent sees a live semantic FS | **USE THIS** |
| Modal    | gVisor | ❌ no  | mountless only (`semfs grep --tag` against a `.db`) | do **not** use for tests |

The `/goal` itself says *"Claude's token can be used `export CLAUDE_CODE_OAUTH_TOKEN=<token>`"* —
that is a **runtime env export into a sandbox**, i.e. E2B. Reading that as Modal secrets was
the mistake that this ledger exists to prevent from recurring.

---

## 1. The matrix we are running (the `/goal`)

> "Run Kg off, kg off with adaptive k, and plain. All the test cases in workspace bench lite
> for pm's workspace. Start with codex subscription, if it fails move to openrouter. Same test
> on claude code with sonnet 4.6 (token in claude_auth_config.json), same fallback. Start with
> claude first then codex."

```
  agents : claude (sonnet-4.6)  →  then codex (ChatGPT subscription)
  arms   : plain  /  nokg  /  nokg+adaptive-K
  cases  : 15,44,45,53,55,95,171,175,386,388        ← 10 of 11 PM cases (289 EXCLUDED, see below)
  auth   : native subscription FIRST → OpenRouter fallback (per agent)
  reps   : 1 (first full pass)
  PLATFORM: E2B real mount — every cell.
```

**Case 289 EXCLUDED — seed-leak finding (2026-06-13):** the canonical `chanpin-gemma-q4.db`
seed has a prior run's deliverable baked into its filesystem: `/model_output/best_selling_
product_core_data_list.txt` (Jun 9) = case 289's **answer**, `cat`-able from the mount. Verified
isolated to 289 (no other case's `output_files` appear in `model_output`). Clean removal needs the
file gone from the FS, not just its embedding:
- `rm` via mount (semfs `unlink`, which cascades to drop the embedding — confirmed
  `cache/fs.rs:1849`→`sqlite_vec.rs:drop_file_vectors`) **hangs**: the daemon is busy with a
  ~17-min deferred re-extraction this incompletely-warmed seed runs on every mount.
- Offline `DELETE FROM chunks` works but the **file still exists in the FS** (agent can `cat` it);
  editing the fs tables risks corrupting the index the other 10 cases share.
- Rebuild without `model_output` = cleanest but **vetoed**.
So 289 is excluded from this pass; revisit via a rebuild if its data point is needed.

**Arm definitions (verified from the harness, do NOT redefine):**
- `plain` — raw chanpin file tree, **no semfs**. Agent uses ordinary `find`/`grep`/`cat`.
- `nokg` — chanpin seed **mounted via FUSE**; agent uses `semfs grep`. Seed = `chanpin-gemma-q4.db`
  (the KG tables exist in the seed but the hint does not steer the agent to them — "kg off"
  is a *render/hint* setting, not a different index).
- `nokg+adaptive-K` — same mount, same seed, **only** `SEMFS_ADAPTIVE_K=on` added. grep decides
  how many results to render from the score curve (1 dominant … up to 10 flat).

**Embedder (FIXED — do not change without approval):** `gemma-q4` (BYO-ONNX, 768d),
`SEMFS_EMBED_MODEL=gemma-q4`, `SEMFS_EMBED_ONNX_DIR=<gemma dir>`.

**`SEMFS_SEARCH_ONLY=on` — PLATFORM-FORCED (2026-06-13):** E2B hard-caps sandbox RAM at
**8192 MiB**. With `SEARCH_ONLY=off` (Modal campaign default) the mount daemon runs an
on-mount **deferred re-extraction** of the whole half-warmed chanpin seed and balloons to
**7.2GB RSS → OOM-killed** (dmesg confirmed). `SEARCH_ONLY=on` skips re-extraction → daemon
stable at **1.75GB**, real mount works (`ls`/`cat`/`semfs grep` all functional). It's applied
identically to both semfs arms (plain untouched) so it does not bias the comparison; `off`
literally cannot run on E2B. Caveat: this measures semfs on the seed's **already-indexed
portion** (it's ~half warm) — a seed-quality limitation, not a `SEARCH_ONLY` effect; a fully
warm seed would need a separate offline re-index. (Modal never hit this: mountless
`semfs grep --tag` is a transient per-call process — no persistent daemon, no re-extraction.)
Earlier valid codex-on-E2B run (E16) avoided OOM only because it mounted the tiny 403-file
e11 seed, not this 2,811-file one.

**Agent integration (E2B real mount):** agents read/search via the mount but **write
deliverables OUTSIDE it** (`/home/user/run/<label>/model_output`) — proven necessary: a fresh
write is grep-surfaceable instantly and `rm` clears the DB row but the daemon keeps serving it
from its in-memory index (only a remount clears it), so in-mount writes are an unscrubsable
in-run leak. Write-outside = zero mount writes = zero contamination. The semfs hint (the exact
`AGENTS.md` content) is injected into the task prompt so Claude gets the full guidance without
needing cwd=mount (the SDK ignores `~/.claude/CLAUDE.md`).

**Cases note:** WB-Lite is a curated subset of WB-Full; the 11 PM case files are
byte-identical to their full-set counterparts (verified on case 289). Persona
"Product Manager" = 产品人员 = the `chanpin` workspace.

---

## 2. Experiment history — platform of record (the honest accounting)

| Exp | What | Platform | Mount? | Headline result | Counts? |
|-----|------|----------|--------|-----------------|---------|
| WB 5-arm matrix | plain vs semfs arms, 5 cases | EC2 / Modal | mountless | plain 46%@89K beat all semfs | ⚠️ wrong platform |
| E8 | 30-cell nokg/plain matrix | **Modal** | mountless | pre-registered FAILED (2/5 wins) | ⚠️ wrong platform |
| E11 | discovery corpus, 12 cells | **Modal** | mountless | 1/2 wins, corpus too small | ⚠️ wrong platform |
| E16 (RUN1) | adaptive-K A/B, gemma+native auth | **E2B** | ✅ real | INVALID — stale template binary (adaptive was a no-op) | ❌ void |
| **E16 (RUN2)** | adaptive-K A/B, gemma+native ChatGPT auth | **E2B** | ✅ real | adaptive safe 5/5, `--all` adopted; native auth $0, 10/10 no-401 | ✅ **valid (codex, E2B)** |
| **PM-MATRIX** | 10 PM cases × 3 arms × n=3, codex/OpenRouter | **E2B** | ✅ real | **CONFOUNDED** — plain 12.6% / dedup(W5) 3.8% / dedup+TB 3.0%; dead mounts (fd2 batch) + query-rewrite corruption + weak model. NOT a clean plain-vs-semfs verdict | ⚠️ confounded |
| Dedup A/B (SEM-19) | 8-cell, cases 45/53, OpenRouter | **E2B** | ✅ real | mechanism validated (pointer fires W5, clean W0); n=2 variance-dominated, **no token/accuracy win demonstrable** | ⚠️ inconclusive |
| Transcription fix | cases 53/171, fix_v1/v2/v3, codex/OpenRouter | **E2B** | ✅ real | **root-caused** (agent doesn't transcribe available grep content). plain 53={1,5}/11 171={11,12}/18 · block-render code (fix_v2) fires but 53=0/11 (insufficient) · transcription prompt (fix_v1) bimodal 53={0,0,0,8,11}/11 (**not shippable**). FUSE-enumeration lever outstanding | ✅ root-caused, fix not yet reliable |

**Detailed outcomes (consolidated 2026-06-16):** PM matrix → `PM_MATRIX_RESULT_2026-06-16.md`;
transcription RCA + verified per-cell scores → `rcas/2026-06-16-semfs-agent-doesnt-transcribe-grep-content.md`;
dedup A/B + overall snapshot → `CURRENT_STATE.md`. (Section 3 run-tracker is auto-generated from
`results.jsonl` by `update_ledger.py` and is stale — re-run the script to refresh; the honest
accounting above is the hand-maintained source of truth.)

**Answer to "previous codex test was on E2B, right?":** Yes — E16 RUN2 (the valid one) ran
codex on **E2B with a real mount + native ChatGPT auth**. The Modal E8/E11 codex runs are the
ones we are explicitly leaving behind.

---

## 3. E2B RUN TRACKER

Auto-generated from `/tmp/e2b_matrix/results.jsonl` by `update_ledger.py` after every cell.
Design: agent **reads/searches via the FUSE mount, writes deliverables OUTSIDE the mount**
(plain dir) — avoids the push-on-write 402 retry loop and keeps the seed pristine per cell.

<!-- RUNTRACKER:START -->
Coverage — completed-ok / total runs per cell. (⬚ not started)

| case | claude·plain | claude·nokg | claude·nokgAK | codex·plain | codex·nokg | codex·nokgAK |
|------|:---:|:---:|:---:|:---:|:---:|:---:|
| 15 | 1/1✅ | 1/1✅ | 1/1✅ | ⬚ | ⬚ | ⬚ |
| 44 | ⬚ | ⬚ | ⬚ | ⬚ | ⬚ | ⬚ |
| 45 | ⬚ | ⬚ | ⬚ | ⬚ | ⬚ | ⬚ |
| 53 | ⬚ | ⬚ | ⬚ | ⬚ | ⬚ | ⬚ |
| 55 | ⬚ | ⬚ | ⬚ | ⬚ | ⬚ | ⬚ |
| 95 | ⬚ | ⬚ | ⬚ | ⬚ | ⬚ | ⬚ |
| 171 | ⬚ | ⬚ | ⬚ | ⬚ | ⬚ | ⬚ |
| 175 | ⬚ | ⬚ | ⬚ | ⬚ | ⬚ | ⬚ |
| 386 | ⬚ | ⬚ | ⬚ | ⬚ | ⬚ | ⬚ |
| 388 | ⬚ | ⬚ | ⬚ | ⬚ | ⬚ | ⬚ |

### Per-run results

| label | agent | case | arm | status | tokens | calls | semfs_grep | judge | wall_s | deliverables |
|-------|-------|------|-----|--------|--------|-------|------------|-------|--------|--------------|
| pm_claude_15_nokg_r1 | claude | 15 | nokg | ok | 1216443 | 61 | ✓ | — | 544 | financial-table-key-expense-analysis-concise-version.xlsx |
| pm_claude_15_nokgAK_r1 | claude | 15 | nokgAK | ok | 2066948 | 74 | ✓ | — | 538 | financial-table-key-expense-analysis-concise-version.xlsx |
| pm_claude_15_plain_r1 | claude | 15 | plain | ok | 210280 | 9 | · | — | 130 | financial-table-key-expense-analysis-concise-version.xlsx |

_3 runs recorded · 3 ok · planned 60 (10 cases × 3 arms × 2 agents × 1 rep; case 289 excluded — seed leak)_
<!-- RUNTRACKER:END -->

---

## 4. E2B asset inventory & runtime wiring

| Asset | Local path | Goes to sandbox as | Used by |
|-------|-----------|--------------------|---------|
| semfs binary (adaptive-K) | template `semfs-mount` `/usr/local/bin/semfs` | baked in template | all semfs arms |
| gemma-q4 ONNX model (196MB) | `/tmp/e2b_assets/gemma_q4/` | `/home/user/gemma_q4` | all semfs arms |
| chanpin seed (KG-on, gemma-q4) | `/tmp/e2b_assets/seeds/chanpin-gemma-q4.db` | mounted via FUSE | nokg, nokgAK |
| chanpin raw file tree | _pull from Modal vol `corpus/chanpin_standard`_ | `/home/user/ws/plain` | plain arm |
| 11 PM cases (task + rubrics + data) | _download from HF WB-Lite_ | per-case prompt + judge input | all |
| codex ChatGPT auth | `~/.codex/auth.json` | upload to `~/.codex/auth.json` (runtime) | codex |
| Claude OAuth token | `claude_auth_config.json` → `token` | `export CLAUDE_CODE_OAUTH_TOKEN=…` (runtime) | claude |

**Credential rule:** injected at runtime into the ephemeral sandbox only. Never an image layer.

**Auth fallback per agent:**
- codex: `CODEX_USE_CHATGPT=1` + uploaded `auth.json` → on failure, OpenRouter (`OPENROUTER_API_KEY`).
- claude: `USE_CLAUDE_LONG_RUNNING_TOKEN=1` + `CLAUDE_CODE_OAUTH_TOKEN` (model `claude-sonnet-4-6`)
  → on rate-limit/failure, OpenRouter. **Note:** the Claude SDK speaks Anthropic's API and
  OpenRouter is OpenAI-shaped, so the OAuth path is the reliable one; OpenRouter fallback for
  Claude is best-effort.

---

## 5. Judge plan

Each deliverable is scored against the case's `rubrics` (from its metadata.json) by an LLM
judge. The judge is **scoring only — it does not change the realism of the agent run**, so it
may run wherever an LLM key is available (locally / via OpenRouter), but the **agent execution
that produces the deliverable is always on E2B**.

---

## 6. Open items
- [ ] Pull `corpus/chanpin_standard` (plain-arm raw tree) to local for upload.
- [ ] Download all 11 PM cases (task + rubrics + data files) from HF WB-Lite.
- [ ] Confirm `semfs-mount` template binary has adaptive-K (`semfs grep --help` shows `--all`).
- [ ] Add Claude agent to the E2B driver (claude-agent-sdk + ClaudeCode.js + OAuth export).
- [ ] Decide reps (start with 1 to pace the Claude subscription).
- [ ] Secure an OpenRouter key for the judge (and as agent fallback).
- [ ] CLEANUP: delete the Modal placeholder secrets `claude` / `codex-auth` (created in error).
