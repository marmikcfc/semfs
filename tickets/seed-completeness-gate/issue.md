# Ticket: seed completeness — never bless / ship a half-indexed seed

**Status:** Part A DONE (2026-06-15) — `semfs seed-verify` shipped + verified E2E. Part B (warm-to-completion) still open. Created 2026-06-15.
**Owner:** Marmik · **Root cause:** `rcas/2026-06-08-partial-seed-indexing.md`

> **MAJOR CORRECTION (2026-06-15):** the premise below ("~47–53% indexed", "the build was
> unmounted mid-warm so the gap is permanent") was a **measurement artifact**. The chanpin
> seed is **98.2% complete** on real content (616/627 non-empty original files reachable).
> The naive `indexed/imported` ratio was padded by 747 empty WB placeholders, 716 stale
> `.semfs-error.txt` stubs (their source docs ARE indexed), and 412 `.extracted.md` sidecars
> (content already chunked under the original inode). Full evidence + reproduction:
> [`SEED_COMPLETENESS.md`](SEED_COMPLETENESS.md). Part A's design was updated to this honest
> accounting; `fs_unindexed` is NOT used (it held 8 of 716 real fails).
**Related:** `rcas/2026-06-01-semfs-prewarm-oom-import-collection.md`,
`benchmarks/workspace_bench/seed_complete.sh` (process-level workaround),
`tickets/gemma-q4-embedder/issue.md`, `tickets/ast-kg-code-lane/` (Modal-GPU warm seed).

## Problem
Every local seed (`chanpin-gemma-q4`, `chanpin-e5-nosum`, …) indexes only ~47–53% of the
corpus. **All files are IMPORTED** (`fs_inode`/`fs_dentry`) but ~750 are **NOT INDEXED**
(no `chunks`/vectors) → `semfs grep` is blind to ~55% of files (incl. 89% of `.docx`).
Per the RCA, the root trigger:
1. **"ready" ≠ "indexed":** `mount.rs:257` returns ready when the daemon answers a Ping;
   the async warm (extract→chunk→embed) keeps running after "mounted".
2. **No budget/gate** makes the build WAIT for the warm; the daemon was unmounted (or
   OOM-died, prewarm-OOM RCA) mid-pipeline, and **`--no-push` discarded the pending
   ~747 embeds** → the gap is permanent and silent.
3. **`semfs status` can't even detect it** — it reports `fs_unindexed` (extraction
   *failures*), NOT the imported-but-unindexed gap. So nothing refuses a bad seed.

Impact: every semfs benchmark arm runs on a half-blind index → accuracy handicapped by
*coverage*, not retrieval quality; the comparison vs `plain` (reads the raw tree) is unfair.

## The fix — two parts

### Part A (v1, DONE 2026-06-15): a completeness GATE — `semfs seed-verify <db>`
`crates/semfs/src/cmd/seed_verify.rs`. Opens a seed DB **read-only** and computes
**content reachability** (the CORRECTED metric — see correction note above):
- **content files** = non-empty regular files that are NOT semfs sidecars
  (`.extracted.md` / `.semfs-error.txt`). Empty WB placeholders + sidecars excluded.
- **reachable** = a content file whose own inode is in `chunks`, OR whose
  `<name>.extracted.md` sibling (same parent dir) is in `chunks`.
- **unreachable** = content − reachable. (We do **not** use `fs_unindexed`.)

Verdict: **COMPLETE** iff `unreachable <= --allow-unindexed` AND `coverage >= --min-coverage`
(defaults 0 / 0.0 → require zero gap). **Exits non-zero on INCOMPLETE.** `--json` for CI.
No daemon, no network. Verified E2E on the real 690 MB chanpin seed: 627 content / 616
reachable (98.2%) / 11 unreachable → INCOMPLETE at allow=0 (exit 1), COMPLETE at
`--allow-unindexed 11` (exit 0). 8 unit/integration tests (pure `assess()` + in-memory SQL).

Done: `seed_complete.sh` / Modal-GPU build / CI can call `semfs seed-verify <db> --allow-unindexed N`
as the gate. The 11-file allowance for chanpin is documented in `SEED_COMPLETENESS.md`.

### Part B (v2): warm-to-completion (close the gap at build time)
- `semfs warm --wait` (or `mount --wait-indexed`): block until `indexed + failed == imported`
  with a generous ceiling, polling the DB — so the build never unmounts mid-warm.
- Make daemon "ready" optionally mean "indexed", not just Ping (`mount.rs`).
- Bound embed memory during warm (`EMBED_BATCH_SIZE=16`, `EMBED_MAX_LENGTH=1024` exist) +
  ensure the streaming-import fix is deployed → a full warm doesn't OOM mid-way.
- Surface imported/indexed counts in `Response::Status` (so `semfs status` shows the gap live).

## Design (Part A)
New subcommand `crates/semfs/src/cmd/seed_verify.rs`:
- pure fn `assess(imported, indexed, failed, allow) -> Verdict{complete, gap, coverage}` (unit-tested)
- `run(db, allow_unindexed, min_coverage, json)`: open `rusqlite` read-only, run the COUNT
  queries, call `assess`, print, return `Err`/exit≠0 on INCOMPLETE.
- Register in the CLI command enum + dispatch.

## Test plan
- Unit: `assess` — complete (gap=0), incomplete (gap>allow), within `--allow-unindexed`, coverage math.
- Integration: build a tiny sqlite with K `fs_inode` regular files + M `chunks.filepath` (M<K) →
  `seed-verify` reports gap=K−M and exits non-zero; with `--allow-unindexed (K−M)` exits zero.

## Refs
- RCA: `rcas/2026-06-08-partial-seed-indexing.md` (import vs index; trigger confirmed)
- `crates/semfs/src/cmd/status.rs` (reports `unindexed_files` only — the gap hook to extend in Part B)
- `benchmarks/workspace_bench/seed_complete.sh` (the process-level wait, calls this gate)
