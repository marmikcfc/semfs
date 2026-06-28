# Seeds — semfs benchmark seeds (authoritative)

_Last updated: 2026-06-25. The single source of truth for what seeds exist, what's in them, how they're
built, and how to verify/rebuild. Companion to `CURRENT_STATE.md`._

## What a seed is

A seed is a single SQLite DB that an agent mounts (FUSE) and searches. It carries **three independent
layers** — a seed is only fully usable when all three are present:

| layer | tables | built by | used for |
|---|---|---|---|
| **search index** | `chunks`, `vchunks` (vec0), `ffts` (BM25) | `index_corpus` (extract → chunk → embed) | `semfs grep` / retrieval |
| **FUSE tree** | `fs_dentry`, `fs_inode`, `fs_data` | `materialize_fs` | the browsable mount (`ls`/`cat`/`grep` on a real path) |
| **knowledge graph** | `graph_entity`, `graph_relation`, `edges`, `graph_community`, `graph_god_node` | `materialize_kg` (+ Leiden) | hidden-KG prior / co-mention / PPR |

> **The fs_data trap:** a seed built without `materialize_fs` indexes fine but **mounts empty**
> (`fs_dentry/fs_inode/fs_data` are empty → the FUSE tree serves nothing). This bit houqin + yunying
> (fixed 2026-06-25 — see below). Always verify all three layers before baking.

## The seeds (2026-06-25)

Four WB-Lite **persona** seeds, all `<persona>-gemma-q4.db`, all with the three layers, all baked into a
per-persona E2B template. Two more seeds (`research`, `xafs`) carry the same uniform KG for other benches.

| seed | persona / corpus | E2B template | FS layer | status |
|---|---|---|---|---|
| `chanpin-gemma-q4.db` | Product Manager | `semfs-mount-chanpin` | ✅ full | original, well-validated (~98% indexed); 289 seed-leak cleaned (`.preclean-bak`) |
| `kaifa-gemma-q4.db` | Backend Developer | `semfs-mount-kaifa` | ✅ full | fs_* + code lane rebuilt via **SEM-38** fresh-seed rebuild (2026-06-21) — *was* retrieval-only |
| `houqin-gemma-q4.db` | Logistics Manager | `semfs-mount-houqin` | ✅ full | fs_* **re-materialized 2026-06-25** (~255K rows); 1.24 GB → needs `--startup-timeout 240` |
| `yunying-gemma-q4.db` | Operations Manager | `semfs-mount-yunying` | ✅ full | fs_* **re-materialized 2026-06-25** (~204K rows) |
| `research-*.db` | Researcher corpus | runtime-pull | ✅ | uniform Gemma KG; xAFS/research benches |
| `xafs-*.db` | xAFS corpus | runtime-pull (snapshot_download) | ✅ | uniform Gemma KG; 13 xAFS cases |

**KG across all 6 seeds:** uniform **Gemma-4-31B-NVFP4** embeddings + KG, ~**149K entities / ~627K
relations**, embedded on 4×B200 (data-parallel 12-shard fan-out). Leiden communities (full multi-level
Leiden + embedding-kNN edges — the kg-quality fix, commit `0106b2e`, singletons 38%→3%).

## Build + embed pipeline (Modal)

All seed build/embed runs on **Modal** (local Mac can't cross-compile fastembed/ONNX); the binary is built
x86_64-linux (`benchmarks/modal/build_semfs.py`). Seeds + corpus + binary live on Modal volume
`semfs-bench-data`. Pipeline:

```
corpus → index_corpus (extract → chunk → embed)          # search index
       → index_corpus --phase finalize                    # materialize_kg + materialize_fs  ← the FUSE tree + KG
       → embed KG (uniform Gemma-4-31B-NVFP4, 4×B200)      # graph_entity/relation/community
       → WAL-checkpoint                                    # BEFORE baking (else partial DB)
       → bake into E2B template semfs-mount-{persona}      # benchmarks/modal/bake_e2b_persona.py
```

The **fs_data fix (2026-06-25)**: houqin + yunying had been built without `materialize_fs`. Re-ran
`index_corpus --phase finalize` (CPU-only — `materialize_kg` + `materialize_fs`, no re-embed):
**houqin 0→~255K fs rows, yunying 0→~204K**. Both now mount with a full tree.

## Gotchas (hard-won)

- **fs_data** — verify `fs_dentry/fs_inode/fs_data` are non-empty before baking (see the trap above).
- **WAL checkpoint** — checkpoint the seed before baking, or the template gets a partial DB.
- **Big seeds need a longer mount watchdog** — houqin (1.24 GB) → `WB_MOUNT_STARTUP_TIMEOUT=240`
  (the 30 s default is too tight; mount hangs at `configuring_api`).
- **Surface cleanliness** — surface-off arms must not expose `/AGENTS.md`, `/CLAUDE.md`, `/kg/` on mount;
  SQL-clean before baking.
- **SQLite corruption under Modal preemption** — incremental commits + preemption corrupted a seed once;
  re-embed after corruption. RCA `rcas/2026-06-20-sqlite-corruption-incremental-commit-modal-preemption.md`.
- **vLLM `--enforce-eager`** collapsed embedding throughput — removed; the 4× win was data-parallel.
  RCA `rcas/2026-06-19-vllm-enforce-eager-throughput-collapse.md`.
- **Benchmark contamination** — a *results* dir (not the seed) accumulated old rep labels that polluted a
  baseline; restrict graders to clean reps. (See `tickets/wblite-ppr-ab/EXPERIMENT.md`.)

## Verify / rebuild

```bash
# verify a seed's three layers (row counts):
#   chunks/vchunks/ffts (index) · fs_dentry/fs_inode/fs_data (tree) · graph_entity/edges/graph_community (KG)
sqlite3 <seed>.db "SELECT 'fs_data', count(*) FROM fs_data UNION ALL SELECT 'edges', count(*) FROM edges;"
semfs seed-verify <seed>.db        # the shipped gate (extracted-vs-real, sidecar padding, leak check)

# re-materialize a missing FUSE tree / KG (CPU-only, no re-embed):
index_corpus --phase finalize      # materialize_kg + materialize_fs
```

## Pointers

- State: `CURRENT_STATE.md` → Seeds. PPR experiment: `tickets/wblite-ppr-ab/EXPERIMENT.md`.
- SEM-38 fresh-seed rebuild: `tickets/fresh-seeds-gemma-uniform/`. KG quality: `tickets/kg-quality/`.
  Embedder choice (gemma-q4): `tickets/embedder-config-search/`.
- Build/bake tooling: `benchmarks/modal/{build_semfs.py,semfs_modal.py,bake_e2b_persona.py}`,
  `benchmarks/e2b/smoke_persona_template.py`.
- Large seed artifacts → Google Drive `semfs/` (not committed). Don't commit seeds/corpus/binaries.
