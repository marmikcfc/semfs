# RCA: semfs pre-warm OOM (~15.6 GB) — root cause is auto-import file collection, not pull or embed

**Date:** 2026-06-01
**Status:** ROOT CAUSE CONFIRMED + **FIX APPLIED & VERIFIED** (2026-06-01). Streaming-import fix landed in
`crates/semfs/src/cmd/daemon_runtime.rs` (`collect_files_recursive` → `collect_file_paths_recursive`,
returns `Vec<(String, PathBuf)>`; content read lazily one file at a time in the import loop). E2E re-run of
the non-empty-dir repro (150×20 MB ≈ 3 GB, hash backend): peak daemon RSS **155 MB** (was ~3 GB), and
`import: 150 imported, 0 already existed, 0 failed` — correctness preserved. Not yet committed.
**Supersedes:** `2026-05-30-semfs-local-prewarm-oom-unbounded-rehydration.md` (wrong root cause) and the
"H-pull/import vs H-indexer" hypotheses in `benchmarks/workspace_bench/EC2_TESTING_PROGRESS.md` §6.

## Symptom
Pre-warming the full chanpin container with the local (fastembed) backend OOM-kills the daemon at
~15.6 GB on the 16 GB EC2 box (`m7i.xlarge`), deterministically. Blocks building a complete local index.

## Method (EC2 `13.201.35.159`, branch `feat/backend-agnostic-store`, binary 0.0.5)
The "decisive isolation experiment" from EC2_TESTING_PROGRESS §6 plus two follow-ups. All mounts were
COLD (fresh `XDG_CACHE_HOME` → forces a full pull). RSS sampled every 3 s vs `MemAvailable` and the
`startup/<tag>.json` phase. Container = `workspace-bench-chanpin`, 983 docs.

| # | Run | Backend | Mount target | Peak daemon RSS | OOM? |
|---|-----|---------|--------------|----------------:|------|
| 1 | cold full pull | **hash** (no embedder) | empty dir | **~118 MB** | no — reached `ready`, all 983 docs |
| 2 | cold full pull | **local** (ONNX) | empty dir | **~966 MB** | no — `ready`; delta vs hash = fixed model load |
| 3 | read-all (cat every file) on #2's live mount | local | — | **~2.0 GB** (slow climb) | no |
| 4 | cold mount, **hash**, over a NON-EMPTY dir (150×20 MB = ~3 GB) | hash | populated dir | **~3.02 GB** at `collected 150 file(s) for import` | (stopped) |

**Run 4 is the proof:** with ZERO embedding (hash backend), RSS jumped to ≈ the exact byte-total of the
files already present in the mount target, at the moment the daemon logged `collected N file(s) for import`.
RSS tracks pre-existing-corpus bytes **1:1**.

## Root cause
`crates/semfs/src/cmd/daemon_runtime.rs`:

```rust
// line ~146
let created_dir = !cfg.mount_path.exists();
...
// line ~151
let pre_existing_files = if cfg.import_existing && !created_dir {
    collect_files_recursive(&cfg.mount_path, &cfg.mount_path)   // <-- slurps EVERY file's bytes
} else { Vec::new() };
```
```rust
// line ~641
fn collect_files_recursive(...) -> Vec<(String, Vec<u8>)> {
    ...
    match std::fs::read(&path) {                 // full file into RAM
        Ok(data) => out.push((vfs_path, data)),  // held in one Vec for the whole tree
    ...
}
```

`import_existing` defaults to **true** (`mount.rs:135  let import_existing = !args.no_import;`). When semfs
is mounted onto a directory that **already contains files** (`!created_dir`), `collect_files_recursive`
eagerly reads **every file's full content** into a single in-memory `Vec<(String, Vec<u8>)>`
(`pre_existing_files`) BEFORE the FS is even mounted (line 152, vs `mount_fs` at line ~470). For the chanpin
corpus (~983 docs, ~16 MB raw each incl. rehydrated/transcription content → ~15.7 GB) this is an exact match
for the observed 15.6 GB OOM.

This happens at mount setup, **before any embedding** — which is why the prior investigation saw
"embedded count stays flat while RAM balloons" and mis-attributed the OOM to `initial_sync`/the embedder.

## Why the earlier hypotheses were wrong
- **NOT pull/import-pull** (`full_pull` / `reconcile_one`): Run 1 shows the full cold pull of all 983 docs
  peaks at 118 MB. Each page's `resp` is dropped per-iteration; `reconcile_one` streams content to SQLite.
- **NOT the embedder/index path:** Run 2 (local cold) peaks at 966 MB (just the ONNX model load); Run 3
  (read-all → flush → ONNX index of real content) climbs only to ~2 GB. The indexer does not balloon.
- **The "scoped (`--memory-paths`) never OOM / full always OOM" correlation was incidental.** `--memory-paths`
  is push-side scoping and does not gate import. The real variable was **empty vs non-empty mount target**:
  benchmark runs mount fresh empty workdirs (`created_dir=true` → import skipped); the pre-warm re-mounted
  over a populated directory (`created_dir=false` → whole corpus slurped).

## Fix directions (not yet applied)
1. **Stream the import instead of buffering it.** Don't build `Vec<(String, Vec<u8>)>` for the whole tree —
   walk + `import_file_with_ownership` one file at a time (read, import, drop), so peak RAM is one file, not
   the whole corpus. Smallest, most correct fix.
2. **Bound/iterator-ize `collect_files_recursive`** (return an iterator or process via a bounded channel).
3. **Pre-warm should pass `--no-import`** (or mount onto an empty dir) — the seed already lives in
   Supermemory, so importing the materialized copy is redundant for a read-only pre-warm.
4. Consider not enabling `import_existing` by default for read-only (`--no-push`) mounts.

## Artifacts
EC2 `/srv/semfs-benchmark/oom-exp/sample_{hash,local,readall,import}.log`. Repro scripts (laptop `/tmp/`):
`oom_exp.sh` (cold-mount + sampler), `oom_readall.sh`, `oom_import.sh` (the decisive non-empty-dir test).
