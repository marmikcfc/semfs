# Current State — semfs / Workspace-Bench instance

_Last updated: 2026-06-16. Living snapshot. Companion to `rcas/`, Linear (team `SemFS`), and the Notion SemFS page._

## ⮕ Latest (2026-06-16) — grep cross-turn dedup v1 shipped; seed decontaminated; full WB-Lite rubrics pulled

- **Cross-turn dedup (SEM-19, v1) IMPLEMENTED + tested** (414 tests green). Daemon-side sliding-window
  that strips re-sent file content across turns. Env-gated `SEMFS_DEDUP_WINDOW` (default **0 = off** →
  byte-identical to before). Built into the Modal x86_64 binary and validated live on E2B+OpenRouter
  (dedup fires only with W>0, W=0 control clean).
- **Seed decontaminated:** removed the `/model_output/` leak subtree from `chanpin-gemma-q4.db`
  (case-289 gold deliverable + a `tmp/` dir + error sidecars) across fs+chunks+ffts; integrity ok;
  legit `product_data/` copy preserved; backup at `chanpin-gemma-q4.db.preclean-bak`.
- **All 100 WB-Lite rubrics downloaded** from HF on Modal → copied + normalized locally. Backend
  Developer (11) rubrics now in hand for eventual testing; full PM-11 (incl. previously-missing
  171/289/386/388) now stageable.
- **kaifa (backend-dev) seed = index-only** — complete for retrieval, but NO filesystem layer (not
  mountable). Needs an FS-populating rebuild before backend-dev cases can run the mount flow.
- **3-arm PM matrix RAN** (10 cases × {plain, dedup, dedup+turn-brake} × n=3, OpenRouter): raw numbers
  plain 12.6% / dedup 3.8% / dedup+TB 3.0% — **BUT the result is CONFOUNDED** (dead mounts on the fd2
  batch + a query rewriter that corrupts searches + a weak model). **Not a clean semfs-vs-plain verdict.**
  See "PM matrix result" — and note the affordance diagnosis was RETRACTED (it was a dead-mount artifact).

---

## Platform (current)
- **Test env: E2B real-FUSE, x86_64**, template `semfs-baked` (binary + chanpin seed + gemma embedder
  + WB harness baked). **ALL benchmark tests run on E2B, never Modal** (hard rule).
- **Build env: Modal x86_64-linux** (`benchmarks/modal/build_semfs.py`) — local Mac can't cross-compile
  fastembed/ONNX. Volume `semfs-bench-data` holds seeds/corpus/bin/wb.
- Branch: `feat/backend-agnostic-store`. Binary: `semfs 0.0.5`.
- Orchestrator: `benchmarks/e2b/run_matrix.py`; per-cell `cell_driver.py` (uploaded per-run, no rebuild
  needed for Python edits). Knobs via `--knobs <json>` → merged into daemon `mount_env` **and** cell env.

## Binary (this session) — Modal x86_64, with dedup
- Rebuilt on Modal → `benchmarks/e2b/assets/semfs-fixed` (37 MB, ELF x86-64). Boot-pushed over the baked
  binary by `run_matrix.boot_prep` (`/usr/local/bin/semfs`). Verified `SEMFS_DEDUP_WINDOW` + the
  `already in your context … not resending` pointer string are compiled in.
- Contains the prior timeout fixes (`SEMFS_SEARCH_TIMEOUT_SECS`=120, `SEARCH_DEADLINE`=90, client wait
  140; cloud-fallback panic guard) **plus** the new dedup (`SessionCache`, `SearchHit.seen_at_turn`).

## Dedup v1 — what shipped (SEM-19, `tickets/grep-stateless-context-dedup/`)
- `crates/semfs-core/src/daemon/session_cache.rs` — `SessionCache` sliding window (6 unit tests).
- `daemon/ipc.rs::dedup_seen` — partition after `index.search`: strip `memory`/`chunk` + set
  `seen_at_turn` for files already returned within the window (3 tests). DIFF, never REPLAY.
- `SearchHit.seen_at_turn: Option<u64>` (serde default); `daemon_runtime.rs` reads `SEMFS_DEDUP_WINDOW`
  (0 → `None` → disabled); `cmd/grep.rs` renders a pointer line for seen hits.
- **v1 assumption: one mount = one agent** (single daemon-global window, no keying). v2 (deferred) =
  key by `(agent_pid, starttime)` via `SO_PEERCRED` for multi-agent/sequential-reuse safety.

## Seeds
| seed | where | index | FS layer | usable for |
|---|---|---|---|---|
| `chanpin-gemma-q4.db` (PM) | E2B template `/opt` + local assets | 7153 chunks / 2800 inodes; ~98.2% | **full** | mount-based PM benchmark |
| `kaifa-gemma-q4.db` (backend-dev) | Modal `seeds/` (229 MB) | 2415 files / 6386 chunks / 21,115 ent / 323,101 rel; 0 unindexed | **ABSENT** (fs_inode=1, dentry=0, fs_data=0) | retrieval only — NOT mountable |

⚠️ **The PM seed is BAKED into the E2B template** (`/opt/chanpin-gemma-q4.db`), copied to `~/.semfs/chanpin.db`
at boot — so the local decontaminated seed is **not used** unless we (A) re-bake the template, or (B)
boot-push the cleaned 690 MB seed (reuse the chunked-upload path). Leak was 289-only (irrelevant to the
other 10 cases).

## WB-Lite rubrics + case universe
- **Rubrics (all 100, normalized):** `benchmarks/e2b/assets/wb_lite_all/lite_all/task_lite_clean_en/<id>/metadata.json`.
  HF stores list fields as JSON strings → normalized to lists, `id` set = `absolute_id`.
- **Judge source (live):** `benchmarks/e2b/assets/wb_lite/task_lite_clean_en/` — currently only the
  original **5** (15/44/45/53/55); copied to `/tmp/wb_lite/` at judge time. Stage more from `wb_lite_all/`.
- **Personas (lite):** Backend Developer 11 (ids 3,7,91,92,94,226,242,266,286,300,311) · Product Manager 11
  · Researcher 17 · Logistics Manager 30 · Operations Manager 31.
- **PM-11 = {15,44,45,53,55,95,171,175,289,386,388}.** 289 was the seed-leak (now cleaned); 10 were
  "valid", 289 now potentially includable pending the `product_data/` corpus question.

## Token cost model (corrected)
- **OpenRouter (`openai/gpt-5.4`): `cache_read=0`** → full context re-billed every turn (billed ≈ total).
- **Native ChatGPT subscription (`gpt-5.5`): ~80% cached** → billed ≪ total.
- Earlier testing total: **145 cells, ~71.3 M raw / ~20.4 M billed** tokens (codex 33 M raw / claude 38 M).
- **Turn count is the first-order lever** (`total ≈ Σ context over turns`; corr(calls,total)=0.82).
  **Dedup is second-order** (trims re-sent payload, not turn count). Turn-brake (p2b prompt) cuts turns
  but is non-deterministic; dedup is the deterministic backstop.

## PM matrix result (2026-06-16) — CONFOUNDED (dead mounts + query-rewrite corruption); NOT a clean verdict
- Raw: 10 PM cases × 3 arms × n=3, OpenRouter (289 excluded). plain 12.6% / 339K · dedup(W5) 3.8% / 296K · dedup+TB 3.0% / 113K. **Do not read this as "plain beats semfs"** — it's confounded (below).
- **VERIFIED ROOT CAUSE (2026-06-16, supersedes ALL earlier diagnoses — retrieval/affordance/over-exploration/synthesis are RETRACTED):** the cell I deep-dived (`53_nokg_rfd2`) had a **DEAD MOUNT** — `semfs list` → "no active mounts", `semfs grep` → "No container tag found". No FUSE filesystem to `ls`/`find`/`readdir`; the agent reverse-engineered the raw `.semfs/chanpin.db` because it was the only on-disk copy. So the "affordance lures the agent into the DB / over-exploration" story was an **infra artifact**, not semfs behavior.
- **Prevalence:** dead mounts hit the **fd2 batch only** (5 cells: 44/53/55/95/386); **54/59 nokg cells had working mounts.** Localized infra failure — but it poisoned the exact cell analyzed.
- **Real semfs issues (from working-mount cells, e.g. 53_fd3/ft1, 171_ft3):** (1) **the query rewriter CORRUPTS searches** — `semfs grep "PO_4"` (a purchase-order id) was rewritten to "phosphate (PO4) ion / phosphate fertilizer" (case 171); (2) **grep CLI arg errors** — `semfs grep "q" /ws/mnt/Desktop` → `error: unexpected argument`. Working-mount cells are LEAN (7–8 cmds, like plain), so over-exploration was an fd2/dead-mount artifact too.
- **The matrix is confounded 3 ways:** (a) dead mounts (infra, fd2 batch), (b) query-rewrite corruption (retrieval), (c) a weak model (`gpt-5.4`; absolute scores low for plain too, 12.6% vs historical 46%).
- **VERIFIED ROOT CAUSE (2026-06-16, deepest — RCA `rcas/2026-06-16-semfs-agent-doesnt-transcribe-grep-content.md`):** on a healthy mount, `semfs grep` returned the source records **VERBATIM** into context (c0b: all 4 DES records + dates + thresholds, ~50 KB), yet the deliverable c0b wrote contains **0× `DES-0006`, 0× `2024-12-18`, 0× the thresholds** — a generic template. **Retrieval is fine; the agent doesn't transcribe content it has.** Cause = delivery-form mismatch (plain `cat`s 4 clean files → transcribes 8/11; semfs hands a ranked repeated blob → summarizes 0/11) + a FUSE enumeration defect (`find -type f` → 0 on the live mount) blocking the clean-`cat` fallback. Mount-health gate now shipped in run_matrix; mount-gate confirmed clean on the controlled re-run; rewrite-off did NOT help (refuted).
- **FIX TESTED (2026-06-16, small n, mount-gated — RCA "Verification RESULTS"):** two fixes tried on
  53/171 vs plain. Verified scores (judge `passed`): plain 53={1,5}/11 171={11,12}/18 ·
  **fix_v2** (block-render code `print_block` + dedup + rw0, NO prompt) 53=0/11 171={0,11}/18 ·
  **fix_v1** (fix_v2 + transcription prompt) 53={0,0,0,8,11}/11 171={0,0,0,0,11}/18.
  → **Block-render code alone does NOT close the gap** (fires, but insufficient). **Transcription
  prompt is bimodal** — occasional big win (11/11, 8/11, *confirms* the root cause) but collapses to 0
  on most reps; **not shippable**. n is tiny → variance-dominated, **NOT a clean verdict**.
- **REMAINING DURABLE LEVER:** the **FUSE enumeration fix** (make `find`/`ls`/`readdir` surface files
  in `fs_dentry`) so the agent can use plain's deterministic "locate → cat → transcribe" path instead
  of synthesizing from a ranked blob. Needs a Modal rebuild + a direct mount probe of the FUSE
  `readdir`/`getattr` code path — **not yet done**. (Offered as the next step; pending user go.)
- **CODE STATUS:** `print_block` + dedup are in the working tree
  (`crates/semfs/src/cmd/grep.rs`, `crates/semfs-core/src/daemon/{ipc.rs,mod.rs,session_cache.rs}`)
  and in the ephemeral `assets/semfs-fixed` binary — **NOT git-committed** yet.

## Dedup A/B result (8-cell, OpenRouter, cases 45/53) — INCONCLUSIVE (superseded by the PM matrix above)
- Mechanism validated (pointer fires with W=5, W=0 control clean). But n=2 is variance-dominated
  (calls swung 8→58 same config) and the accuracy spread is the known case-45 coin-flip. **No token/
  accuracy win demonstrable at this n.** Honest next step = a daemon bytes-stripped counter to isolate
  the dedup effect, OR more reps. Not vs-plain yet (would need plain in the same matched run).

## Pending / ON HOLD
- **3-arm matrix on ChatGPT subscription** (user request): arms = (1) dedup-on, (2) dedup-on + turn-brake
  (prompt hint only), (3) plain; **n=3**. HELD until explicit go-ahead.
  - User wants **clean seed everywhere** → must wire the cleaned-seed boot-push first.
  - Native-auth uncertain (earlier native attempts fell back to OpenRouter) → run a 1-cell native smoke
    **only after explicit go** (user instruction).
  - Open decisions: scope (5 staged vs full PM-11 — full needs staging rubrics from `wb_lite_all/` +
    resolving 289's `product_data/` corpus copy); turn-brake p2a (mild) vs p2b (strong).
- **kaifa FS rebuild** for eventual backend-dev mount-based testing.

## Routing (CLAUDE.md §0)
- Tickets → Linear team `SemFS` (SEM-19 = dedup, updated w/ design+plan; SEM-35 = WB matrix).
- RCAs → `rcas/*.md` (canonical) + Notion RCAs DB digest. Design/status docs → Notion SemFS page.
- Large artifacts → Google Drive `semfs/`. Don't commit seeds/corpus/binaries (assets/ gitignored).

## Security / ops (standing)
- Never print/commit secrets (`codex_auth.json`, `claude_auth_config.json`, `openrouter_logs.csv`
  gitignored). Credentials injected at E2B RUNTIME only, never baked into a template.
- Destructive DB edits only on COPIES (seed clean kept `.preclean-bak`). Case 289 excluded historically
  for seed leak — now cleaned in the local seed (not yet re-baked into the template).
