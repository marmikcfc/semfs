# Current State — semfs / Workspace-Bench instance

_Last updated: 2026-06-08 (evening). Companion to `EXPERIMENTS.md`, `rcas/`._

## ⮕ Latest (2026-06-08 evening) — q4 full-coverage seed + KG + graph-fs E2E
- **Active seed:** `chanpin-gemma-q4` (BYO-ONNX q4, `byo:gemma-q4-onnx:768`), **696/704 contentful
  files (98.9%)**, no OOM. KG: 9,298 entities / 637 communities / 672 god-nodes.
- **Three extractor fallbacks shipped** (uncommitted, deployed to box): `pdftotext` (CJK PDFs),
  `soffice` (legacy OLE), `ocr_pdf_paged` (page-split vision OCR). PDF chain: pdf-extract →
  pdftotext → ocr_pdf_paged → ocr_pdf. 49 extract tests pass. `extract/{pdf,ocr,mod}.rs`.
- **KG-materialization race FIXED** (`daemon_runtime.rs`): `refresh_knowledge_graph()` moved before
  `mount_fs()` so "ready" ⇒ "KG ready" (was: codex read empty KG → fabricated 0/15).
- **Case-289 graph-fs E2E (valid run):** 0/15 → **6/15**; codex now reads KG (1,727 B), uses
  `/by-topic` (21×) + grep, sees 403. BUT **493K tokens** — hit the **format trap** (parsed xlsx
  with openpyxl/pandas/libreoffice ×6). Graph-fs fixes engagement, NOT tokens. Format trap is the
  <100K lever. See EXPERIMENTS.md §8 + `rcas/2026-06-08-kg-materialization-race-…`,
  `…-extraction-coverage-…`.
- **Uncommitted local changes:** `extract/{pdf,ocr,mod}.rs`, `Cargo.toml`, `daemon_runtime.rs`.
  **Recurring footgun:** `source`-not-`export` of `OPENROUTER_API_KEY` (broke push/build_kg/OCR).

---

## Box (benchmark instance)
- Host: `ubuntu@13.201.35.159` · key `~/.ssh/semfs-benchmark` · `m7i.xlarge` (4 vCPU/16 GB, no GPU).
- Repo (rsync, **not git**): `/srv/semfs-benchmark/semantic-filesystem`.
- Binary: `/home/ubuntu/.local/bin/semfs` (v0.0.5). Mounts open `~/.semfs/<tag>.db`.
- Seed env: `/home/ubuntu/.semfs_seed_env` (OPENROUTER_API_KEY). Do **not** print keys.
- Corpus source (clean, 1,368 files, 0 junk): `/srv/semfs-benchmark/extract-test/chanpin_seed`.

## Deployed binary (this session) — md5 prefix `3fbf919…` (banner-reverted)
Built from local branch `feat/backend-agnostic-store`. Contains:
- **Graph-as-filesystem** (`/by-topic` overlay): persisted Louvain projection
  (`graph_community`/`graph_god_node`), bounded read model, synthetic inodes
  (`ino≥1<<48`), readdir/lookup/getattr branching under `SEMFS_GRAPH_FS=on`,
  symlink cross-edges; **kind-tiered god-node labels** (Concept/Org≫dates/values).
- **H1 trust marker**: local snippet grep leaves `memory` None → renders via chunk
  presenter (`# ^ COMPLETE FILE …do not open it` + line ranges; cloud parity),
  keeping the `[semfs: SOURCE INACCESSIBLE]` 403 surfacing.
- **H1b**: `model_output/` excluded from search results (`is_agent_output_path`).
- **FS-contract**: `agent_hint.rs` describes the `/by-topic` overlay + call behavior.
- ⚠️ **REVERTED**: the "integrity banner" atop `KNOWLEDGE_GRAPH.md` (listing inaccessible
  sources w/ "REPORT and STOP") was implemented then **removed as CHEATING** (spoon-fed
  the case-289 answer). Do not reintroduce. `build_digest` is back to the honest digest.
- 314 lib tests green.

## Seeds inventory (`~/.semfs/*.db`)
| tag | embedder | files indexed | KG | clean? | note |
|---|---|---:|---|---|---|
| `chanpin-e5-nosum` | e5-small | 725 / 1368 (53%) | full (9156 ent / 4783 rel / 602 comm) | ❌ 3 model_output (incl. **fabricated list**) + 5 .semfs-error + 37 .venv | the one all graph-fs/H1 runs used |
| `chanpin-gemma` | gemma fp32 | 652 / 1368 | sparse (763 ent, 0 rel, 0 comm) | ❌ model_output cruft (honest report, NOT fabricated) | original gemma seed (Jun 7) |
| `chanpin-gemma-clean` | gemma fp32 | 647 / 1368 (47%) | **full** (8652 ent / 4741 rel / 602 comm / 665 god) | ✅ clean ALL lanes (text/BM25/dense/fs) | cleaned + KG-rebuilt copy; STILL partial-corpus |
| `chanpin-gemma-full` | gemma fp32 | **(re-seeding in progress)** | pending | clean | **complete** reseed via `/tmp/seed_complete.sh` (waits for index) |

## Key measured results (case 289, clean prompt, Seed-2.0-Lite judge)
| run | embedder | tokens | calls | rubrics | note |
|---|---|---:|---:|---:|---|
| kg4 (no graph-fs) | e5 | 203.7K | 11 | 7/15 | grep→8 crawl→fabricate |
| gfs1 / gfs2 / gfs3 (graph-fs) | e5 | 87K / 490K / 686K | 5/20/13 | 10 / — / — | **HIGH VARIANCE**; gfs1 honest 10/15 (ceiling) |
| gfsh1 / gfsh3 (+H1) | e5 | 207K / 173K | 9/6 | 5 / 6 | format-trap KILLED (0); tail 686K→207K, still >100K |

**Honest conclusion:** <100K is *achievable* (gfs1=87K honest) but **NOT reliable**. The lever is
**turn count** (tokens ≈ turns × ~20K uncached overhead; Exa-cited literature confirms the
no-caching quadratic law). graph-fs kills os.walk-blowup; H1 kills the format-trap; the residual
is codex **distrusting the 403 and hunting**. Embedder is NOT the lever (Gemma ≈ e5).

## Findings + RCAs this session
- **Seed contamination** (e5): the fabricated `model_output/best_selling…` list (months-as-product
  titles) is indexed and ranks #1 on the answer query → baits codex into copying it (the dishonest
  207K run). Cleaning is higher-leverage than switching embedders.
- **Partial-seed indexing RCA** (`rcas/2026-06-08-partial-seed-indexing.md`): BOTH seeds index only
  ~half the corpus (e5 53%, gemma 47%). Root cause: **incomplete warm** — `mount` returns "ready"
  when the daemon answers Ping, NOT when indexing finishes; the seed build unmounted (or OOM-died,
  per `rcas/2026-06-01-…prewarm-oom…`) before the slow local embed drained. The warm is **not
  resumable** (`import_file_with_ownership` returns early on AlreadyExists) → a full re-seed is
  required. **FIX**: `/tmp/seed_complete.sh` polls chunks-count to stability before unmount.
- **Composio/Exa research**: over-search literature → **H4** (evidence-stabilization "no new
  results" signal) as the one legit env-side tool-call lever; model-side/hard-cap methods are
  unavailable or = harness-gaming. See `tickets/ls-kg-semantic-readdir/TOKEN_REDUCTION_HYPOTHESES.md`.

## In progress
- **gemma-full reseed** (`/tmp/seed_complete.sh` → `chanpin-gemma-full`): complete-corpus gemma fp32
  seed that WAITS for index completion. Measuring live rate to estimate ETA. Next: rebuild KG over
  the full set + materialize projection + verify coverage ≈ 100% extractable, then it becomes the
  canonical clean+complete gemma seed.

## Open work / next steps
- Finish gemma-full reseed → KG rebuild → **clean baseline** (tokens/calls/accuracy) on a
  complete, uncontaminated corpus — the honest starting point we never had.
- Then the real lever: **tool-call/turn-count reduction** (H4, pending user's "is it fair" ruling).
- Durable fixes: git-track `seed_complete.sh`; add a `--wait-for-index` mount flag + a seed
  completeness gate (`indexed >= corpus − known_binary_fails`); fix xlsx→Pdf mis-detect (fs_unindexed).

## Security / ops (standing)
- Never print API keys (`${VAR:+SET}` only). Earlier-exposed OpenRouter/Supermemory keys still
  need rotation by the user.
- Do NOT reboot the EC2 instance without explicit OK. Keep all seeds intact.
- Mount cleanup: `semfs unmount <tag>` (never pattern-kill). Destructive DB edits only on COPIES.
