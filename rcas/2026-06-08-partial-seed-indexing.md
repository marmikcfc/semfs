# RCA: seeds index only ~half the corpus (imported-but-unindexed)

> **⚠️ SUPERSEDED / MIS-DIAGNOSED (corrected 2026-06-15).** This RCA's headline ("blind to
> ~55% of corpus", "warm interrupted, gap permanent") was **wrong**. The "750 missing files"
> are overwhelmingly the **747 EMPTY WB placeholder files** (`size==0`) — they have no content
> to index, so having no `chunks` is correct, not a gap. Measuring **non-empty** original files
> (excluding semfs's own `.extracted.md`/`.semfs-error.txt` sidecars), the chanpin-gemma-q4
> seed is **98.2% complete (616/627 reachable)** — see
> `tickets/seed-completeness-gate/SEED_COMPLETENESS.md`. The real content loss is **~8 files**
> (legacy `.ppt`/`.xls`, a few PDFs/PPTX) plus 1 image + 2 build artifacts. The fix that
> matters is the **completeness GATE** (`semfs seed-verify`, shipped 2026-06-15) so this class
> of mis-measurement can't recur — NOT a re-warm. Keep reading for the original (flawed) analysis.

**Date:** 2026-06-08 · **Severity:** ~~high~~ → **low** (corrected: 98.2% real-content coverage)

## Symptom
`chanpin-gemma` (and `chanpin-e5-nosum`) embed far fewer files than the corpus:
- corpus (`/srv/semfs-benchmark/extract-test/chanpin_seed`): **1,368 files**
- gemma embedded (have chunks+vectors): **647** (47%)
- e5 embedded: **725** (53%)
- failed-extraction (`fs_unindexed`, mostly JPEGs): 56
- **missing entirely (no chunks, not in fs_unindexed): 750** — 89% of docx, 69% pptx, 46% pdf, 47% xlsx.

## Evidence / data flow
- All corpus files share mtime 2026-06-03; **0/750 missing are newer than the Jun-7 seed build** ⇒ NOT corpus growth.
- Missing files are **mixed within the same directories** as embedded ones ⇒ not a dir-level subset.
- **747/750 missing files have `fs_inode`/`fs_dentry` (IMPORTED) but no `chunks` (NOT INDEXED)**; only 3 never discovered; none in `fs_unindexed`.
- ⇒ The break is between **import** (mount → fs_inode, completed for all) and **index** (warm: extract→chunk→embed, stopped at ~647).

## Root cause
The background indexing/**warm was interrupted before completion** — the seed was
unmounted / timed out / stopped at ~half. Same pattern across BOTH seeds (different
embedders, e5 fast vs gemma fp32 slow) ⇒ it's the seed-build process being stopped,
not embedder speed. Exact trigger (warm time-budget vs mount `--startup-timeout` vs
manual stop) NOT yet confirmed — needs the seed-build log or the warm-loop code.

## Impact
- Retrieval (grep) is blind to ~55% of the corpus — incl. most docx/pptx documents.
- The KG (entities/relations/communities) is built only over the embedded subset.
- "gemma on par with e5" inherits this incompleteness; BOTH seeds are half-corpus.
- Any benchmark run is on a half-indexed corpus — answers in unindexed docx are unreachable.

## Fix options
1. Re-mount the seed and let the warm run to COMPLETION (indexes the 747 imported-but-
   unindexed files), then rebuild the KG over the complete set.
2. Full re-seed that runs to the end (verify count == corpus count before use).
3. Add a seed-completeness gate: assert indexed_files >= corpus_files - known_binary_fails
   before declaring a seed ready.

## Open
- Confirm the trigger (why the warm stopped at ~650-725) via seed log / warm-loop code.
- Whether the warm resumes on re-mount (incremental) or needs a forced re-index.

## TRIGGER CONFIRMED (2026-06-08)
1. No count/time budget in the warm/index path — indexing is bounded only by daemon lifetime.
2. `mount.rs:257` returns "ready" when the daemon answers Ping, NOT when the warm completes;
   `startup_timeout` is a no-progress detector only. The warm runs on after "mounted".
3. gemma seed daemon log (`~/.cache/semfs/logs/chanpin-gemma.log`) ends with a CLEAN unmount
   mid-pipeline: `--no-push: discarding 681 unpushed local write(s) at unmount` (~= the 747
   imported-but-unindexed). Import (fs_inode) is fast; local embed is slow + memory-heavy;
   the daemon was unmounted before the async warm drained.
4. Compounding: `rcas/2026-06-01-semfs-prewarm-oom-import-collection.md` — full local pre-warm
   OOM-kills the daemon at ~15.6 GB on the 16 GB box; even left running, a full warm risks
   OOM-dying mid-way. `--no-push` then discards the pending pipeline, making the gap permanent.

**Root trigger:** incomplete warm — daemon unmounted / OOM-died before the slow, memory-
constrained local embed finished; no budget caps it, and "ready" != "indexed".

## Prevent recurrence
- Seed build must WAIT for indexing completion before unmount: poll `semfs status`
  (indexed vs fs_unindexed vs total-imported) until `indexed + failed == imported`, with a
  generous ceiling; only then unmount. Add a completeness gate that REFUSES to bless a seed
  whose indexed_count < corpus_count - known_binary_fails.
- Bound embed memory during warm (batch caps already exist: EMBED_BATCH_SIZE=16,
  EMBED_MAX_LENGTH=1024) and ensure the streaming-import fix is in the deployed binary.
