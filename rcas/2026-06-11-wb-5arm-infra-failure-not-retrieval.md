# RCA — WB 5-arm "semfs loses" was infra failure, not retrieval quality

**Date:** 2026-06-11
**Context:** Re-running the ANALYSIS.md hypotheses (E1–E5) on a cleaned, health-gated
environment to find a local semfs config that beats the plain codex baseline
(plain = 46% acc @ 89K mean tokens). Box: EC2 `13.201.35.159`, codex/GPT-5.4 agent.

## Symptom (original 5-arm matrix, 2026-06-10)
Local semfs arms scored 3–14% accuracy at 2–5× plain's tokens; `nokg` 289 **timed
out** (2024s, 0/15). Headline read as "plain beats all semfs on both axes."

## Root causes found (data flow + execution path traced)

### RC1 — Catastrophic failures were INFRASTRUCTURE, not retrieval (H1 CONFIRMED)
On a clean+healthy seed, `289 nokg` (SEARCH_ONLY=off) went **timeout/0/15/2024s →
114–130s / 5–6 of 15 / passed**. The deficit was infra, not retrieval quality.
Three infra faults, all fixed:
- **Disk-full corruption (F1):** box at 93–95% (12–15G free). `chunks` has
  `last_accessed_at`/`access_count` → **search triggers writes**; a write under
  ENOSPC tears a page → "database disk image is malformed". The *canonical* seed's
  `quick_check` was always `ok` → corruption was runtime, in the working copy.
  Fix: disk guard (abort <6G) + `PRAGMA quick_check` health-gate on the fresh copy.
- **Seed contamination (F4, bigger than the "6 files" estimate):** the supposedly
  clean `chanpin-gemma-q4.db` had **716** `<file>.semfs-error.txt` sidecars (HTTP
  402 "out of credits" from a failed cloud-ingest) scattered across the whole
  corpus + a `model_output/` dir baked in. `find`/`ls` surfaced error-twins for
  nearly every source file → agent concludes data is inaccessible → give-up/fabricate.
  Fix: built `chanpin-clean.db` via **daemon FUSE `rm`** (never raw SQL — would
  desync ffts/vchunks); 716→2 residual KG-artifact twins, search verified healthy.
- **Silent cloud-fallback on local-search timeout:** local search timing out (the
  real trigger) → "falling back to cloud search" → 0 results. `.semfs_seed_env` has
  no `SUPERMEMORY_API_KEY` so the fallback couldn't even reach cloud — pure waste.
  Fix: dummy SM key for local arms; healthy infra means search no longer times out.

### RC2 — SEARCH_ONLY=on flails without a fallback (H2 CONFIRMED)
`289 nokg SEARCH_ONLY=on` (clean seed) flailed >30 min toward timeout, exactly like
the contaminated original, while SEARCH_ONLY=off completed in 114s. `SEARCH_ONLY=on`
hides the tree (cuts os.walk) but removes the only escape path when grep underdelivers.
**SEARCH_ONLY=off is strictly the better default** (the "never lose catastrophically"
guarantee).

### RC3 — Token cost on simple cases = the baked-in KG cascade (un-removable)
`289 nokg so_off` token breakdown: **35.7K reading `kg/KNOWLEDGE_GRAPH.md`** +
**22.4K os.walk** + ~2K useful work. The seed's materialized `AGENTS.md` commands
"read kg/KNOWLEDGE_GRAPH.md FIRST"; the agent obeys, then greps *scoped to kg/*
(returns ~138 chars), loses confidence, and os.walks. A clean simple grep returns
the exact answer file as top hit (verified). **The KG hint is the root token sink.**
- KG cannot be subtracted: `/kg/` files + `AGENTS.md` are daemon-managed **read-only**
  (FUSE rm/overwrite → Permission denied); clearing `graph_*` tables makes the next
  mount do a **full 1471-file re-import + L7 entity extraction** (failing OpenRouter
  400s) that **hangs the mount**. → only a fresh KG-off rebuild removes it.

### RC4 — Embedder pollutes the corpus workdir → mount hang
The adapter runs `semfs mount` with **cwd = $WORKDIR**, so fastembed caches the jina
reranker model blobs to **`$WORKDIR/.fastembed_cache` (898MB)**. An unscoped import
(case with no `--memory-paths`) then tries to **re-index 898MB of model blobs** →
mount hangs for minutes. Cases with `--memory-paths` (e.g. 289) scope past it and
mount fast — explaining why some cases worked and others hung.
Fix: driver strips `$WORKDIR/.fastembed_cache` before each mount.

## Status / next
H1, H2 confirmed. The aggregate token axis can't beat plain (KG read inflates every
case). Remaining win shots: **case 15 tokens** (plain's 184K weakness) and **case 95
accuracy** (clean seed has `.extracted.md` siblings; plain 11/12, cloud 12/12).
Fallback lever if those miss: **fresh KG-off seed** (removes the 35K KG read at source).

## Artifacts
Driver `run_case_e.sh` (disk guard + health gate + dummy SM key + .fastembed_cache
strip + clean seed). Seeds: `chanpin-clean.db` (verified clean). Per-run results in
`/tmp/e_batch*.jsonl` on the box; archived under `matrix_artifacts/rune_b*`.
