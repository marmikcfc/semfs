# E2B Experiment Runbook — Workspace-Bench PM matrix (semfs)

How to run the WB-PM (chanpin) benchmark matrix on **E2B real-FUSE sandboxes**, end to end.
Written 2026-06-13 from the actual runs this campaign. Pair with the data ledger:
`E2B_EXPERIMENT_LEDGER.md` (same dir) and the RCAs referenced at the bottom.

---

## 0. HARD RULES (do not violate without explicit user approval)
1. **E2B only.** Every semfs benchmark runs on a real FUSE mount in an E2B microVM. Never Modal.
   (User corrected this twice — see memory `all-benchmark-tests-on-e2b`.)
2. **Pull artifacts per cell, IMMEDIATELY.** E2B sandboxes are **ephemeral** — killed or 1h-timed-out
   sandboxes are gone, filesystem and all. We **permanently lost** the two `verifyP` nokg cells
   (912K / 224K) because that run saved only parsed numbers and never tar'd the files before the
   sandbox died. Tar `model_output` + `sandbox_dir/raw` + write `result.json` **after every cell**.
3. **Never bake credentials into a template/image.** Codex `~/.codex/auth.json`, the Claude OAuth
   token (`claude_auth_config.json`), and the OpenRouter key are injected at **runtime** only.
4. **Don't change experiment parameters without approval** (arms, cases, mount mode, seed).
5. **No blocking / no forced-grep arenas** (user principle). Run semfs arms with `SEARCH_ONLY=off`
   so the crawl fallback is present — see §5 and memory `confidence-adaptive-delivery-direction`.

---

## 1. Platform constraints (measured)
- **RAM: hard 8192 MiB cap** per E2B template ("contact support to raise"). User chose to stay at 8 GB.
- **conc = 1 per sandbox.** Each `semfs grep` spikes the daemon RSS from ~1.75 GB baseline to
  **~6.85 GB** (cross-encoder rerank activation, O(batch×seq²)). Two concurrent greps → OOM. For
  concurrency, run **multiple sandboxes**, each serial. (Tech-debt fix: `tickets/semfs-grep-rerank-memory-concurrency/`.)
- **`SEARCH_ONLY=off` fits 8 GB at conc=1** — measured peak **6558 MB**, min-free 111 MB, daemon
  survived. So the no-blocking principle is viable on 8 GB; `=on` is NOT required.
- **Sandbox timeout: 1 h max**, refreshable per op via `sbx.set_timeout(3600)`. Long runs must keep
  touching the sandbox or it dies mid-run.
- **Ephemeral filesystem** — see Hard Rule #2.

---

## 2. The baked template `semfs-baked`
Built by `/tmp/e2b_matrix/e2b_build_baked.py` (~5 min). Template id last build: `9i7dr7igoa89val8t3ua`.
Bakes (so sandboxes boot ready, no per-sandbox 900 MB upload):
- `semfs` release binary (`/usr/local/bin/semfs`), node20 + `@openai/codex`, claude-agent-sdk
  (`/opt/wb/evaluation/node_modules`).
- gemma-q4 embedder `/opt/gemma_q4`; chanpin seed `/opt/chanpin-gemma-q4.db` (723 MB).
- WB harnesses `/opt/wb`, cases `/opt/cases`, shims `/opt/semfs-shims`, driver `/opt/cell_driver.py`.
- **Deliverable deps** `openpyxl xlsxwriter pandas python-docx` (`pip3 install --break-system-packages`).
  REQUIRED: without them the agent (esp. Claude in a fresh `.cchome`) burns ~15 turns hunting/installing
  openpyxl — Ubuntu 24.04 PEP-668 blocks naive `pip install` — which re-pays context and inflates
  tokens with cost unrelated to semfs (we saw 825K of a 912K Claude run go to this). Use `apt-get
  install python3-openpyxl python3-pandas` as the reliable fallback (sidesteps PEP-668).

Rebuild whenever you patch `ClaudeCode.js` or `cell_driver_v4_src.py` (the build copies the repo
versions), or change baked deps.

---

## 3. Boot-prep per sandbox (from the baked template)
```
sbx = Sandbox.create(template="semfs-baked", timeout=3600)
# symlinks to baked assets:
ln -sfn /opt/wb ~/wb ; ln -sfn /opt/cases ~/cases ; ln -sfn /opt/gemma_q4 ~/gemma_q4
cp /opt/chanpin-gemma-q4.db ~/.semfs/chanpin.db          # seed → runtime location
# creds (runtime only):  ~/.codex/auth.json, ~/.codex/config.toml
# upload PATCHED files if not rebuilt into the template:
#   /opt/wb/evaluation/baselines/ClaudeCode.js  and  ~/cell_driver.py
echo user_allow_other | sudo tee -a /etc/fuse.conf
# mount (semfs arms only):
export SEMFS_EMBED_MODEL=gemma-q4 SEMFS_EMBED_ONNX_DIR=~/gemma_q4 SUPERMEMORY_API_KEY=dummy-local \
       SEMFS_NO_PUSH=1 SEMFS_NO_SYNC=1 SEMFS_SEARCH_ONLY=off
semfs mount chanpin --path ~/ws/mnt --backend fuse --key dummy-local --no-sync --no-push
```
Boot-prep to first grep ≈ 1–2 min. Daemon baseline RSS ≈ 1.75 GB.

**Background-process gotcha:** launch helper scripts with the absolute interpreter
`/Users/marmikpandya/.pyenv/versions/3.10.13/bin/python3` — a bare `nohup python3` misses the pyenv
shim and fails `import e2b`.

---

## 4. The three arms (per cell, via `cell_driver_v4_src.py`)
| arm | mount | env | workspace the agent reads |
|---|---|---|---|
| `plain` | none (no daemon) | `WB_READ_PATHS=~/ws/plain` | raw tree `~/ws/plain` (crawl with find/grep/cat) |
| `nokg` | `=off` | semfs hint + grep shim | `~/ws/mnt` (semfs semantic search) |
| `nokgAK` | `=off` | `nokg` + `SEMFS_ADAPTIVE_K=on` | `~/ws/mnt` |

- Write-outside: cwd = `~/run/<label>`, deliverables go to `~/run/<label>/model_output` (NOT into the
  mount — writing into the FUSE mount leaks into the index and the daemon serves it in-memory until remount).
- `nokgAK` is the arm under test. **Acceptance target (user): `nokgAK` tokens < 50% of `plain`** for the
  same case+agent, at comparable accuracy.

---

## 5. Claude ↔ codex instruction PARITY (critical — was broken, now fixed)
By default the WB Claude harness only delivers the semfs affordance when **cwd is under the mount**;
our write-outside design put cwd outside → Claude got no project `CLAUDE.md`, no grep shim, and the
permission guard **denied** it access to the mount. Codex was unaffected (it auto-loads
`~/.codex/AGENTS.md` regardless of cwd). Result: unfair comparison + Claude flail.

**Fix shipped (option B, codex untouched)** — `baselines/ClaudeCode.js` + `cell_driver`:
- `SEMFS_MOUNT_PATH` (env) sets the mount independent of cwd → re-enables project `CLAUDE.md`
  (written to **cwd**, not the mount) + `settingSources:['project']` + the rg/grep shim.
- `WB_READ_PATHS` (colon-sep) widens READ/SEARCH access to extra roots (the plain tree, the mount)
  without triggering the semfs kit. Writes stay confined to cwd.
- canUseTool split: read tools (`Read/Glob/Grep/LS`) may target cwd OR allowed roots; write tools
  (`Write/Edit/...`) stay cwd-only (no mount leak).
- Shim runtime deps the driver must set: `SEMFS_BIN`, `SEMFS_REAL_RG` (prefer the `*linux*` ripgrep
  variant), `SEMFS_REAL_HOME=/home/user` (where the daemon socket lives), `SEMFS_SHIM_DIR`.
- **codex.py ignores all of these env vars** → codex behaviour unchanged.

**Verified:** Claude wrote project `CLAUDE.md`, shim enabled, 0 permission denials, used `semfs grep`
(1 clean hit), real deliverable; codex unchanged. RCA:
`rcas/2026-06-13-claude-semfs-hint-parity-broken-write-outside.md` (RESOLVED). Background on why a
hint alone is weak for Claude: memory `semfs-claude-affordance` (the durable fix is an MCP tool).

---

## 6. Auth (native-first, OpenRouter fallback)
- **Claude:** native = `USE_CLAUDE_LONG_RUNNING_TOKEN=1` + `CLAUDE_CODE_OAUTH_TOKEN=<token>` +
  `CLAUDE_OAUTH_MODEL=claude-sonnet-4-6` (harness strips `ANTHROPIC_*` so OAuth wins). Fallback =
  `ANTHROPIC_BASE_URL=https://openrouter.ai/api` (NOT `/api/v1`), `ANTHROPIC_AUTH_TOKEN=<OR key>`,
  blank `ANTHROPIC_API_KEY`, model `anthropic/claude-sonnet-4.6`.
- **Codex:** native = `CODEX_USE_CHATGPT=1` (logged-in ChatGPT OAuth, bare model id e.g. `gpt-5.5`).
  Fallback = `baseUrl=https://openrouter.ai/api/v1`, key = OR key, model `openai/gpt-5.4`.
- **Burst limit:** concurrent Claude cells trip a *burst* rate-limit (distinct from the session cap).
  Pace Claude at low concurrency (≤2–3 sandboxes). A single cell on native is fine.

---

## 7. Plain-arm workspace tree
- Use the **true raw corpus** `chanpin_standard` (1452 files), NOT a copy of the mount.
- **Do NOT `cp -a` from the FUSE mount** — reading ~1452 files (+ `.extracted.md` siblings) through
  FUSE is slow and **timed out at ~400 s mid-copy** (incomplete tree, missing case targets).
- Build a clean tarball, excluding semfs artifacts AND macOS sidecars:
  `COPYFILE_DISABLE=1 tar czf … --exclude='*.extracted.md' --exclude='*.semfs-error.txt'
  --exclude=AGENTS.md --exclude=CLAUDE.md --exclude=.semfs --exclude=kg .`
  (We forgot `COPYFILE_DISABLE=1` once → ~1368 `._` AppleDouble files extracted as crawl noise;
  impact was negligible that time but strip them.)
- Upload chunked (~25 MB parts, reassemble in sandbox) to survive local network blips. Plain needs
  NO daemon/seed/mount.

---

## 8. Token metric & how to compare the two agents
- Convention: **`cached_input=0`** — count cached re-reads at FULL price (`use total_tokens`). In our
  runs **80–90% of every bill is cache_read** = re-paying the accumulating context each turn;
  generation is only ~6–7 K tokens. The lever is *context carried across turns*, not work done.
- The two harnesses report usage differently; normalize both to "all tokens the model processed,
  cache counted at full price":
  - Claude (`claudecode.py`): `total = prompt(uncached) + cache_read + cache_write + completion`.
  - Codex (`codex.py`): `total = prompt(includes cached) + completion`; `cache_read` is a subset of prompt.
- Note codex tool events are logged **begin+end** → its "calls" count is ~2× the unique commands.

---

## 9. Accuracy (NOT YET MEASURED — separate step)
Cell `status:ok` only means a deliverable was returned. Real accuracy = the WB **15-rubric
LLM-as-judge** (`evaluation/src/agent_eval.py`) over each case's `metadata.json` rubrics, producing
`rubrics_judge--<model>.json`. **No cell this campaign has been judged.** Judge gotchas (from prior
runs): base-URL must be set; large traces (>~320 K) break the judge; filename-lottery on ~34/100 WB
cases (memory `wb-judge-filename-lottery`) → match on content + n≥2, not the exact filename.

---

## 10. Standard per-cell loop
```
for each (agent, case, arm):
    label = f"pm_{agent}_{case}_{arm}_r{rep}"
    sbx.set_timeout(3600)                      # refresh kill-timer
    run cell_driver.py --label --agent --case --arm   (inject CLAUDE_CODE_OAUTH_TOKEN + OPENROUTER_API_KEY)
    tar ~/run/<label>/model_output + /tmp/sbx_<label>_* → pull bytes → extract locally   # IMMEDIATELY
    write result.json ; append results.jsonl ; refresh ledger
```
Resumable: skip labels already `ok` in `results.jsonl`. Mirror artifacts into the repo
(`tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs/`) so they survive `/tmp` clears.

---

## 11. Gotchas checklist
- [ ] Background scripts: use the absolute pyenv python (else `import e2b` fails).
- [ ] `Sandbox.list()` returns a paginator (not directly iterable); use `next_items()`.
- [ ] Ubuntu 24.04 PEP-668 blocks `pip install` → use `apt-get install python3-openpyxl python3-pandas`
      or `pip3 --break-system-packages`. Bake them.
- [ ] macOS tar adds `._` sidecars → `COPYFILE_DISABLE=1`.
- [ ] FUSE `cp -a` of the whole tree is too slow — upload the raw corpus instead.
- [ ] Case **289 excluded**: its `model_output` leak is baked into the seed and FUSE-rm + remount
      does NOT purge it (LEAK_PRESENT) → run the 10-case set, not 11.
- [ ] Pull artifacts before the sandbox dies (Hard Rule #2).

---

## 12. Current data state (case 15 only; everything else unrun)
See `E2B_EXPERIMENT_LEDGER.md` for the live table. As of 2026-06-13: only **case 15** has any cells;
the only clean, comparable pair is `plain` (claude 174 K vs codex 254 K). All `nokg`/`nokgAK` numbers
are confounded (=on flail, openpyxl-hunt, or cwd=mount) and **no cell is accuracy-scored**. A clean
6-cell matrix requires: claude `nokg`(clean), claude `nokgAK`, codex `nokg`(clean), codex `nokgAK`,
all under one regime (`=off` + parity fix + deps baked), then the judge for accuracy.

## Refs
- Ledger: `tickets/workspace-bench-5arm-matrix/E2B_EXPERIMENT_LEDGER.md`
- Artifacts (durable copy): `tickets/workspace-bench-5arm-matrix/artifacts/e2b_runs/`
- Scripts: `/tmp/e2b_matrix/` (`e2b_build_baked.py`, `cell_driver_v4_src.py`, `run_matrix.py`, `run_plain_15.py`)
- RCAs: `2026-06-13-claude-semfs-hint-parity-broken-write-outside.md`
- Tech debt: `tickets/semfs-grep-rerank-memory-concurrency/issue.md`
- Memory: `all-benchmark-tests-on-e2b`, `e2b-mount-platform`, `semfs-claude-affordance`,
  `confidence-adaptive-delivery-direction`, `wb-judge-filename-lottery`
