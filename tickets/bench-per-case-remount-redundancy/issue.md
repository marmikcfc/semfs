# Tech debt: per-case re-mount re-reconciles the whole container (redundant)

- **Type:** Tech debt / performance + architecture
- **Status:** Cheap fix APPLIED (delta-on-warm-cache); proper fix OPEN
- **Created:** 2026-06-02
- **Component:** `semfs` mount startup (`sync::initial_pull`) + the Workspace-Bench semfs adapter
- **Severity:** was a hard blocker for benchmarking on a 16 GB box (stuck mount, near-OOM); now mitigated.

---

## Scenario

Workspace-Bench runs each case in its **own isolated workdir** (`filesys/<persona>_workdir_<Agent>_<Model>/`) so the agent's task data (`./data`) and outputs (`./output_cc`) never collide across cases or contaminate the seed. The semfs adapter mounts the **container** (e.g. `chanpin`, the Product-Manager knowledge base) into each case's workdir.

The container content — and its semantic **index** — is **identical across all cases of the same persona**. It is a per-container asset that should be built **once and reused** (already noted in `EC2_TESTING_PROGRESS.md` §4.3: *"embed once per container, reuse"*).

## The issue

Every `semfs mount` runs, **unconditionally**, on startup (`crates/semfs/src/cmd/daemon_runtime.rs:371` → `SyncEngine::initial_pull_with_progress`):

```
initial_pull  =  deletion_scan(walk ALL doc IDs)  +  full_pull(reconcile ALL docs)
```

`full_pull` reconciles **every** doc (`crates/semfs-core/src/sync/mod.rs` → `pull::full_pull`), never a delta, on **every** mount — regardless of whether the cache is already fully hydrated. And `--no-sync` does **not** skip it (that flag only gates the *periodic* delta/deletion loop at `daemon_runtime.rs:458`, not the initial pull).

So for **N cases** of the same persona:
- **N × full reconcile** of all ~983 docs (redundant network + CPU), and
- **N × re-embed** of the whole container if each case uses a fresh cache.

### How it bit us (2026-06-02)
Re-running the smoke benchmark (`semfs-codex`, case 289) against the **warm shared `cache_sqlite`** (493 MB DB, complete 9,521-chunk index) on the 16 GB EC2 box: the per-case mount entered `initial_pull` and **stuck at 900/983 docs for ~21 min**, daemon pinned at **355% CPU / 4.4 GB RSS**, box thrashed to **374 MB free**. The codex agent never started. (The 355% CPU during *reconcile* suggests it was re-embedding — see open question #3.)

## Cheap fix (APPLIED)

`sync::initial_pull` / `initial_pull_with_progress` now branch on cache warmth:
- **Warm cache** (a prior pull recorded the `last_seen_updated_at` watermark → `pull::cache_is_warm`): do a cheap **`delta_pull`** — it pages only until the watermark, so on an already-hydrated container it reconciles ~0 docs in milliseconds. No re-reconcile, no re-embed, no thrash.
- **Cold cache** (no watermark): unchanged — full hydrating `full_pull`.

Deletions are still caught by the `deletion_scan` (kept) + the periodic deletion loop. Correctness is preserved: a warm cache is already consistent; delta catches any new/updated docs since the watermark.

Result: a per-case mount of a warm container goes from ~minutes (or stuck) to **~seconds**, reusing the one index. Cold first-mount behavior is unchanged.

## Remaining tech debt (OPEN)

1. **`deletion_scan` still runs in full on warm mounts** (~10 ID-only API pages/case). Kept for deletion correctness, but it's redundant for a static benchmark seed. Could be gated/skipped on `--no-sync` or trusted-cache mounts.

2. **Proper architecture — mount once, many views.** The per-case re-mount exists only because each case needs an isolated *writable* workdir. The *read* side (container + index) should be shared. Target design: a **single long-lived daemon per container** (mount once), with each case getting a thin **writable overlay** on the shared read-only container, all sharing the index via the **IPC search path that already exists** (the single-IPC-daemon work). This eliminates per-case mounting entirely. Bigger change to the Workspace-Bench harness (union/overlay FS + daemon lifecycle).

3. **ROOT-CAUSED (2026-06-02): the ~300% CPU thrash is the AUTO-IMPORT, not the pull.** The delta-on-warm fix above did NOT resolve the stuck benchmark mount — even on the delta path the mount burned ~300% CPU for minutes and `chunks` grew 9,521 → 12,869 (re-embedding ~3,300 chunks). Mechanism: the harness's `prepare_workdirs` copies a **~516 MB persona workspace** into each case's workdir, then the adapter runs `semfs mount <tag> --path <workdir>` over that **non-empty** dir with **no `--no-import`**, so `import_existing` (default true) imports + RE-INDEXES the entire workdir copy on every mount (same path as OOM #1 — the streaming fix stopped the *OOM* but not the per-case re-embed). So the real per-case cost is: copy 516 MB (disk, `prepare`) + import-and-re-embed 516 MB (CPU, mount). The `initial_pull` delta gate is orthogonal and does NOT help this.

   **Caveat on the obvious fix:** simply passing `--no-import` would *hide* the workdir's task files (`./data`) from the mounted view — the import is also how those become visible/searchable. So the real fix must either (a) NOT copy the 516 MB persona workspace into the workdir per case (prepare optimization — only stage `./data`), (b) scope import-indexing to the task files (not the whole workspace), or (c) the mount-once/overlay design in #2. The delta-on-warm `initial_pull` fix is kept (it removes a genuine, separate redundancy), but it is NOT the fix for this thrash.

4. **Compounding: the box is disk-saturated.** `filesys/` has grown to ~108 GB of accumulated per-case workdir copies (70% disk, ~52% iowait), so even the 516 MB `prepare` copy is slow. Cleaning stale run workdirs is needed before any semfs-local benchmark is practical on this box.

## Verification
- Unit: `cargo test -p semfs-core` 223 pass; cold-cache tests exercise the `full_pull` path (unchanged), so the gate is covered both ways.
- E2E: pending — re-run `codex`/`claude × {sqlite, pglite}` reusing the warm indexes; per-case mount should now be ~seconds and not thrash.

## Links
- Fix: `crates/semfs-core/src/sync/{pull.rs,mod.rs}` (`cache_is_warm` gate).
- Related: `tickets/solve-oom-issue/`, `tickets/parallelize-l7/`, `EC2_TESTING_PROGRESS.md` §4.3.
