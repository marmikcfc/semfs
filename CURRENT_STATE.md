# Current State — semfs / Workspace-Bench instance

_Last updated: 2026-06-18. Living snapshot. Companion to `rcas/`, Linear (team `SemFS`), and the Notion SemFS page._

## ⮕ Latest (2026-06-18 evening) — chanpin-4arm.db seed verified; hidden-KG architecture decided; implementation ticket reviewed

### Seed: `chanpin-4arm.db` — fully verified, on Modal volume

Built from `chanpin-leanhint3.db` with Leiden communities rebuilt (`materialize_kg`) and all surface artifacts SQL-cleaned. Verified table counts:

| Table | Rows | Notes |
|---|---|---|
| `chunks` | 7,134 | text content |
| `edges` | 13,322 | file → entity paths (`to_path = /memories/<slug>.md`) |
| `graph_entity` | 9,300 | entity names (CJK preserved); Concept 3698 · Org 1683 · Artifact 1355 · Person 730 · Project 713 · Task 457 · Event 408 · Decision 256 |
| `graph_relation` | 5,139 | entity → entity typed relations |
| `graph_community` | 636 | 32 Leiden communities, one row per file |
| `graph_god_node` | 128 | 4 ranked god nodes per community (rank 0 = most central) |

Surface clean: no `/AGENTS.md`, `/CLAUDE.md`, `/kg/` visible on mount. One seed covers all 4 experiment arms via env flags:

- `best` → `SEMFS_COMENTION=off`
- `hiddenkg_edges` → `SEMFS_COMENTION=on` (current proxy)
- `hiddenkg_leiden` → `SEMFS_COMENTION=leiden` (not yet implemented)
- `hiddenkg_routing` → `SEMFS_KG_ROUTING=on` (not yet implemented)

### "in the corpus language" hint removed — 4 locations

The phrase instructed agents to search in Chinese (corpus-specific overfitting). Removed from:

1. `crates/semfs-core/src/agent_hint.rs` — home-level hint (`render_block`)
2. `crates/semfs-core/src/agent_hint.rs` — workspace-root hint (`render_workspace_root`)
3. `benchmarks/e2b/cell_driver.py` — `SEMFS_HINT` constant (semfs arms)
4. `benchmarks/e2b/cell_driver.py` — cloud arm hint

### Hidden KG architecture decided

Reviewed `tickets/workspace-bench-5arm-matrix/HIDDEN_KG_IMPLEMENTATION_TICKET.md`. Architecture is a **bounded soft prior** (0.0–0.15 score boost), not a hard filter:

```
kg_prior(file) = entity_overlap_score
               + community_match_score
               + neighbor_file_score
               - giant_community_penalty
```

Applied after all retrieval lanes (vec KNN + code KNN + BM25 + path lane) complete, before cross-encoder rerank. Controlled by `SEMFS_HIDDEN_KG=on`. The `chanpin-4arm.db` seed has all four data sources needed (`edges`, `graph_entity`, `graph_community`, `graph_god_node`).

Current proxy arm (`SEMFS_COMENTION=on`) is the existing L7 co-mention boost — a post-rerank nudge, not the full hidden KG prior. The real implementation requires `hidden_kg.rs`.

### New Modal utilities added

- `benchmarks/modal/semfs_modal.py::build_4arm_seed` — builds the shared 4-arm seed (copy → Leiden rebuild → surface clean → verify)
- `benchmarks/modal/semfs_modal.py::inspect_seed_tables` + `::inspect_seed` — queries table row counts on any seed in the Modal volume (used to verify `graph_entity`)

## ⮕ Latest (2026-06-18) — hidden-KG E2B template rebuilt on Modal; seeds now baked; preflight blocked by surface contamination in `best`

- **Experiment target clarified:** the desired 3-arm PM experiment is now
  `plain` vs `best_exp0002` vs `best_exp0002 + hidden internal KG only (proxy)`.
  The proxy arm is current `SEMFS_COMENTION=on` with surfaced KG off; the true
  hidden-routing KG design still does **not** exist in product.
- **E2B harness cleaned for that experiment** in `benchmarks/e2b/run_matrix.py` and
  `benchmarks/e2b/cell_driver.py`:
  - new arms: `best`, `hiddenkg`
  - **arm-specific remount per cell**
  - `SEMFS_SEARCH_ONLY=off`
  - arm-specific seed contract:
    - `best` → `/opt/chanpin-leanhint3.db`
    - `hiddenkg` / `nokg` / `nokgAK` → `/opt/chanpin-clean.db`
    - `kg` → `/opt/chanpin-gemma-q4.db`
  - explicit `--preflight`
  - **hard fail** if seeds are missing or surfaced KG artifacts remain in a surface-off arm
- **Template contract updated** in `benchmarks/e2b/bake_template_v2.py`:
  `semfs-baked-v2` should now bake:
  - `/opt/corpus.tgz`
  - `/opt/chanpin-gemma-q4.db`
  - `/opt/chanpin-clean.db`
  - `/opt/chanpin-leanhint3.db`
- **Local-disk issue addressed:** the first attempt staged ~2.5 GB under
  `/private/tmp/e2b_ctx` (3 seeds + corpus tarball). That directory has been
  **removed**. The rebuild path was moved to **Modal → E2B** instead:
  `benchmarks/modal/semfs_modal.py::build_e2b_template_v2_modal`.
  It reads the seeds and corpus from the Modal volume `semfs-bench-data`,
  builds `corpus.tgz` inside Modal, and calls `e2b.Template.build(...)` there.
  Modal secret `e2b` was created with `E2B_API_KEY`.
- **What is verified right now:**
  - Modal volume **does contain**:
    - `seeds/chanpin-gemma-q4.db`
    - `seeds/chanpin-clean.db`
    - `seeds/chanpin-leanhint3.db`
  - Modal-side E2B build succeeded from [benchmarks/modal/semfs_modal.py::build_e2b_template_v2_modal](/Users/marmikpandya/semantic-filesystem/benchmarks/modal/semfs_modal.py:165):
    `semfs-baked-v2` now includes `/opt/corpus.tgz`, `/opt/chanpin-gemma-q4.db`,
    `/opt/chanpin-clean.db`, and `/opt/chanpin-leanhint3.db`
  - fresh E2B preflight now gets past seed inventory and reaches mount-time checks
  - the current failure is:
    `surface contamination persists for arm=best; rebuild or replace the seed`
  - therefore the remaining blocker is **seed surface cleanliness for the best arm**, not template plumbing or local disk
- **Open blocker / next step:** inspect what `best` is still surfacing on mount
  (`kg/`, root hint files, or another derived artifact), then decide whether to:
  1. rebuild `chanpin-leanhint3.db` as a surface-clean seed, or
  2. relax the contamination check if the surfaced artifact is intentionally harmless
     and does not affect the agent-visible experiment.

## ⮕ Next experiments to run

Once the surface contamination issue is resolved, run these in order:

1. `python3 benchmarks/e2b/run_matrix.py --preflight --arms best,hiddenkg --knobs benchmarks/e2b/knobs/best_exp0002.json`
2. Cheap validation:
   - cases `53,171`
   - arms `plain,best,hiddenkg`
   - `n=1`
3. Real experiment:
   - same arms
   - increase reps only after the preflight and validation are clean

## ⮕ Latest (2026-06-17) — kg-quality SHIPPED: full Leiden + embedding-kNN → singletons 38%→3%

- **kg-quality fix shipped** (commit `0106b2e`, TDD: 13 community tests + 351 core + 74 semfs green).
  Two changes to the KG projection: (1) `Graph::add_knn_edges` densifies the file graph with cosine-kNN
  edges (each file → 6 nearest embedding neighbours, reusing the `vchunks` vec0 index — ~free); (2) a
  full multi-level **Leiden** detector (`local_move→refine→aggregate→recurse`, self-loop-carrying)
  replaces the single-level Louvain+`leiden_refine` hybrid. Wired into `graph_file.rs::build_file_graph`.
- **Structural measurement** (re-materialized chanpin KG on a `/tmp` copy — deterministic, offline,
  no LLM/FUSE): **singletons 66 (38.2%) → 1 (3.1%)** ✅ beat the <10% target; communities 173→32;
  god-nodes 669→128. ~35% of files that had a *zero* "related-files" pointer now sit in a real cluster.
- **Honest caveat:** overshot into a **135-file bucket** (21% of corpus; target was <60). Validated
  *coherent* not junk-drawer (all `compliance_and_risk_control/*`), power-law spread (top-3 = 43%), no
  single-blob pathology. `RESOLUTION=1.0` is the lever; whether 135 is too coarse *as a pointer* is an
  E2E question — not sweeping the proxy in a vacuum.
- **Next (the "relevant metrics" goal, NOT yet launched):** Modal x86_64 seed rebuild with this code →
  E2B FUSE A/B `SEMFS_KG=on` vs nokg/plain (53/171 + discovery case). Ticket: `tickets/kg-quality/`.

## ⮕ Latest (2026-06-17) — evo /optimize on glm-5.1: PROMPT lever beats plain on both axes; converged

- **`/evo:optimize` (z-ai/glm-5.1, WB-Lite 53+171, E2B real-FUSE) CONVERGED (stall=5).** Objective
  (beat plain on BOTH higher accuracy AND lower tokens) **ACHIEVED**: winner **exp_0002 = 44.4% acc /
  173K tok** vs **plain 27.2% / 242K** (+63% rel. acc, −28% tokens). Simpler robust **exp_0007
  (prompt-only) = 34.9% / 143K** also wins. evo workspace `.evo/run_0000` (full log in `.evo/project.md`).
- **Load-bearing lever = the transcription/stop PROMPT** (WB_TURNBRAKE): stops the agent's re-search
  loop (no-prompt ablation exploded to 0.24/878K) + forces verbatim transcription (≈2× acc). Confirms
  the 2026-06-16 transcribe RCA; refutes its "prompt is bimodal/unshippable" worry (at n=3 it's decisive).
- **Empirical redirect on Task #10:** the win is agent BEHAVIOR (prompt), not delivery form → the RCA's
  Rust cleaner-delivery / FUSE-enum levers target a NON-bottleneck here → deprioritized (low-EV).
- **Robustness:** `SEMFS_GREP_COMPRESS=on` → per-grep OpenRouter calls → timeout risk on grep-heavy
  cases → **prefer prompt-only config**. Held-out 95/386/175 INCONCLUSIVE (beyond glm-5.1 for both arms,
  plain 0% @ 1–2.3M tok). Real next lever = a hard search/turn cap (case-95 over-explored 134 calls).
- New harness: `benchmarks/e2b/{evo_bench.py,evo_token_gate.py}` + knobs `{best_exp0002,prompt_only}.json`
  + `glm_plain_baseline.sh`/`glm_heldout_validation.sh`. Fixed: ENOSPC corpus-tarball leak; worktree
  bloat 907→72MB (untracked artifacts); run_judge `.env`-in-worktree crash. Committed on branch.

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
