# RCA: local pre-warm OOMs the daemon (~15 GB) — unbounded concurrent rehydration

**Date:** 2026-05-30
**Host:** ubuntu@13.201.35.159 (EC2 i-0c491c7cc23de8555, m7i.xlarge, 16 GB)
**Scope:** Full local pre-warm of the PM/chanpin container (516 MB, 2,141 files) — building the local fastembed index to 100% so benchmark runs reuse a complete index.

## Symptom
Trying to drive the index to 100% by mounting and reading every file (`find /mnt -type f -exec cat`), the semfs daemon's RSS balloons to **~15.6 GB** and the kernel OOM-kills it → mount orphaned (`Transport endpoint is not connected`) → pre-warm stalls at ~50% (583 / 1,172 indexable text files).
```
Out of memory: Killed process … task=semfs … anon-rss:15,634,076 kB
```

## Key eliminations (what it is NOT)
- **Not L7 entity-graph extraction.** Re-ran with `OPENROUTER_API_KEY` unset (L7 + L4 disabled, confirmed no "entity-graph extraction enabled" line). It **OOM'd identically** — free RAM 14.3 GB → 1.5 GB in ~3 minutes.
- **Not embedding.** During the L7-off OOM run, `embedded` stayed flat at 583 (the read hadn't reached unindexed files yet) while RAM ballooned ~13 GB. So the memory is consumed by **reading/rehydrating**, not by `embed()`.
- **Not the 749 "empty" files.** They are genuinely `size=0` (verified via `fs_inode.size`), not unhydrated — so the true index target is 1,172 text files, not 2,141.

## Root cause
`crates/semfs-core/src/cache/hydration.rs`: rehydration spawns **one task per file into a `JoinSet` with NO concurrency limit** (no semaphore). The code comments acknowledge "otherwise the JoinSet grows unbounded" but only does a non-blocking reap (`timeout(0) join_next`), which does not bound in-flight tasks. When a tree walk (`find`/`readdir`) or an agent touches many files quickly, the daemon spawns a **flood of concurrent R2 rehydrations, each holding the raw file bytes in memory** → unbounded growth → ~15 GB on this corpus → OOM.

This also explains earlier production symptoms: the ENOTCONN orphaned-mount crashes during heavy agent runs were likely the same OOM.

## Fix (recommended)
**Bound rehydration concurrency with a semaphore** (e.g. `tokio::sync::Semaphore`, permit ≈ 8–16) around the per-file rehydration spawn in `hydration.rs`. Caps peak memory regardless of how many files are touched at once. Small, well-scoped change; also protects *agents* that read many files (not just the pre-warm).
- Secondary: stream/evict rehydrated bytes rather than holding them; cap the read cache.

## Interim workarounds (no rebuild)
- **Read sequentially in tiny batches with pauses** so in-flight rehydration drains between batches (keeps peak memory low). Slower, fragile.
- **Bigger box** (e.g. 64 GB) for the one-time warm — survives the ~15 GB peak.

## Status
Pre-warm persisted at 583/1,172 (50%); daemon cleaned, RAM reclaimed. Completing it on the 16 GB box needs either the semaphore fix or batched-sequential reads. Related: the local-indexing starvation RCA (indexing is flush-triggered, not eager) and the HOME-pollution RCA.
