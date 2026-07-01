# Get all 6 Gemma-KG seeds E2B-ready

Linear: SemFS team (key `SEM`), project SemFS. Folder paired with the Linear issue of the same name.

## Context
All 6 seeds are built on the **Modal volume** (`semfs-bench-data:/data/seeds/`) as of 2026-06-20:
gemma-q4 embeddings + uniform **Gemma-4-31B-IT-NVFP4** KG (entities → embedding-kNN graph → Leiden).

| seed | files | entities | relations | communities | seed size (approx) |
|---|---|---|---|---|---|
| chanpin | 700 | 6,131 | 7,925 | 166 | ~small |
| kaifa | 2,415 | 17,283 | 328,783 | 1,054 | ~small |
| houqin | 2,313 | 10,377 | 14,027 | 584 | ~150 MB |
| yunying | 1,622 | 9,510 | 9,419 | 262 | ~small |
| research | 9,082 | 48,439 | 132,927 | 1,363 | large (144K chunks) |
| xafs | 19,170 | 57,585 | 134,616 | 78 | **large (615K chunks, ~GBs)** |

Full record: `memory/xafs-wb-embedding-setup.md`.

## Problem
**HARD RULE: semfs benchmarks run on E2B (real FUSE mount), NEVER Modal** (`memory/all-benchmark-tests-on-e2b.md`).
The seeds currently exist **only on the Modal volume** → not benchmark-ready. Building ≠ E2B-ready.

## Definition of done
1. **Export** the 6 seeds from the Modal volume → **Google Drive** `semfs/experiments/` (large binaries don't go in the repo; per CLAUDE.md §0). xafs/research are the big ones.
2. **Stage the corpora** the agent runs in (xafs cases, WB persona workdirs, research) for the E2B workspace.
3. **Get seeds+corpora into E2B** — bake into the `semfs-mount` template or pull onto the E2B box.
4. **Verify each seed in the E2B real-FUSE mount**: `semfs grep` returns ranked hits + the `/kg/` overlay reads (entities/communities). One smoke per seed.
5. **Link the Drive artifacts from this Linear issue** (Drive is the store, Linear holds the pointer).

## Known caveats to carry over
- **xafs community granularity is coarse**: 78 communities over 19K files (~244 files/comm; target <60). Entities/relations are solid; a finer Leiden pass on xafs may be needed if the `/kg/` overlay is exercised. See `kg-quality-leiden-knn-result`.
- **Don't rebuild KG on preemptible Modal SQLite writers** — see `rcas/2026-06-20-sqlite-corruption-incremental-commit-modal-preemption.md` (houqin was corrupted this way; re-embedded clean). If re-running, use non-preemptible workers or local-disk+copy-at-end.
- A `_houqin_corrupt_bak.db` backup + merged shard partials (`_shard_*of12.db`) + toy seeds (`_toy_a/b.db`) remain on the volume — clean up after export.

## Seeding Supermemory (cloud backend) — one seed, BOTH backends (2026-06-25)

semfs is backend-agnostic (`SEMFS_STORAGE_BACKEND`): `sqlite` = the local seed, `cloud` = the
**Supermemory** backend (`CloudIndex` adapting the Supermemory API). The **same seed serves both** —
the seed's `push_queue` + semfs's push worker is the bridge.

> **Gotcha (found 2026-06-25):** the xafs seed was built **search-only** — `fs_data=0` (no FUSE tree,
> not mountable) and `push_queue=0` (nothing to push). Same `fs_data` trap as houqin/yunying. A seed
> must be **completed** (`materialize_fs`) before it can mount OR push.

**Recipe (Modal-side; tools added 2026-06-25):**
```bash
# 0. complete the seed if fs_data=0 — materialize_fs ONLY (skips the KG; it already exists, and
#    re-running Leiden on xafs's 57K-entity KG times out). New "fs" phase:
SEMFS_SEED_ONLY=1 modal run benchmarks/modal/semfs_modal.py::index_corpus \
  --corpus-name xafs --out-name xafs-gemma-q4.db --phase fs
SEMFS_SEED_ONLY=1 modal run benchmarks/modal/semfs_modal.py::inspect_seed_tables --seed xafs-gemma-q4.db
#   → expect fs_data > 0  (also reports push_queue + the fs_* tree now)

# 1. SMOKE — push ONE case (dp_001) to Supermemory, then verify it's searchable:
SEMFS_SEED_ONLY=1 modal run benchmarks/modal/semfs_modal.py::seed_supermemory \
  --seed xafs-gemma-q4.db --container xafs --prefix /dp_001

# 2. FULL — push all 19,170 files:
SEMFS_SEED_ONLY=1 modal run benchmarks/modal/semfs_modal.py::seed_supermemory \
  --seed xafs-gemma-q4.db --container xafs
```

**How it works:**
- `push_seed <seed.db> <container> --backfill` — MOUNTLESS push (`CacheFs::with_api` + `run_push_worker`,
  no FUSE → runs on Modal, unlike `semfs sync` which needs a live daemon). `--backfill` enqueues every
  real file into `push_queue` (`Db::enqueue_all_real_files_for_push`: recursive-CTE over `fs_dentry`,
  `derived=0` regular files; `SEMFS_PUSH_PREFIX` scopes a subtree for the smoke). The push worker drains
  the queue → POSTs to Supermemory `/v3/documents`.
- Needs the **`supermemory` Modal secret** (`modal secret create supermemory SUPERMEMORY_API_KEY=…`).
- After the push, search via `SEMFS_STORAGE_BACKEND=cloud` (Supermemory) or `sqlite` (local seed) —
  same corpus, both backends.

Tools: `crates/semfs-core/examples/push_seed.rs`, `Db::enqueue_all_real_files_for_push` (`cache/db.rs`),
`seed_supermemory` / `_push_seed_remote` + `index_corpus --phase fs` (`benchmarks/modal/semfs_modal.py`).

## Pointers
- memory/xafs-wb-embedding-setup.md (embed+KG record)
- memory/all-benchmark-tests-on-e2b.md (E2B hard rule + ledger)
- memory/e2b-mount-platform.md (`semfs-mount` template)
- rcas/2026-06-19-vllm-enforce-eager-throughput-collapse.md, rcas/2026-06-20-sqlite-corruption-incremental-commit-modal-preemption.md
- Modal apps: gemma4-31b-nvfp4-vllm (STOPPED), semfs-bench (build/merge/KG/materialize fns in benchmarks/modal/semfs_modal.py)
