# RCA: full q4 seed OOMs (~15.6 GB) — three stacked contributors, NOT just KG

**Date:** 2026-06-08 · **Host:** ubuntu@13.201.35.159 (15 GiB box)
**Severity:** high — full local seeds with the live daemon die ~1/6 in; the daemon is
OOM-killed and the seed script's "stable count" heuristic mis-reports it as DONE.

> **CORRECTION (read first).** An earlier version of this RCA concluded "live KG extraction
> during the bulk warm is the cause." **That was wrong — a confounded A/B.** The KG-off and
> KG-on runs stopped at *different* file counts (150 vs 200), so they never overlapped in the
> zone where the climb appears (~file 163+). When the corrected seed (KG **off**) ran past that
> zone it climbed **identically**, and the KG worker had written **zero** entities. KG is **not**
> the dominant cause. The real picture is below.

## Symptom
gemma-q4 full seed (1451-file chanpin_standard) stalled at **249/1451** then `SEED_DONE`. `dmesg`
shows the daemon OOM-killed at **anon-rss ≈ 15.6 GB** (six times historically, all ~15.6 GB).

## Investigation (corrected — controlled runs, RSS vs indexed-file-count every 5 s)
| Run | Config | Result |
|-----|--------|--------|
| 1 | `--no-push --no-sync`, KG off | flat 2.46 GB **but stopped at idx 159** (before the climb zone) |
| 2 | `--no-push --no-sync`, KG on | climbed 2.46→4.1 GB by idx 187 — **but graph worker never ran (0 entities, import still going)** |
| safe | `--no-push --no-sync`, KG off, **full corpus** | **same climb** at idx ~163→183 → **plateau ~4.08 GB**, indexing crawls, survives |

The safe run is the decisive control: KG-off climbs the same way Run 2 did, so **the climb is not
KG**. Run 1 simply never reached idx 163.

## Root cause (corrected): three additive contributors stacked past 15 GiB
1. **Embed arena floor (~4 GB) — the dominant, irreducible piece.** The ort CPU arena "grows to the
   largest tensor it ever sees and never shrinks" (`embed/local.rs`). The corpus has large CJK
   `.extracted.md` transcriptions (575 / 509 / 366 KB). Embedding those grows the arena to ~4 GB,
   where it **plateaus** (it is a floor, NOT a leak — it levels off). q4 is also very slow on them
   (~3.5 files/min), so indexing crawls.
2. **Live L7 KG extraction** (when `SEMFS_KG=on`): 8 concurrent `spawn_blocking` LLM extractions,
   each holding a file's content + reqwest/JSON buffers, plus per-extraction `IMMEDIATE` db-lock
   contention with the embed writer. Additive overhead on top of (1).
3. **Push 402-churn** (push ran the whole time): `SEMFS_NO_PUSH`/`SEMFS_NO_SYNC` are **silent
   env-var no-ops** — only the `--no-push`/`--no-sync` CLI flags are honored. The seed exported the
   env vars and never passed the flags, so push hammered an out-of-credits Supermemory key (HTTP
   402), writing an error-sibling per file. Additive overhead.

(1)+(2)+(3) on a 15 GiB box → 15.6 GB → OOM at ~249 files. Removing (2)+(3) drops the peak to the
~4 GB floor of (1) — proven: the safe seed survives there.

Two things kept it silent: the env-var no-ops (push couldn't be turned off as intended), and the
seed poller treating "chunk count stopped growing" as success (an OOM reads as a clean finish).

## Fix
**Operational (proven, applied):** seed with `--no-push --no-sync` **flags** + `SEMFS_KG=off`
(`benchmarks/workspace_bench/seed_q4_safe.sh`), then build the KG **offline** via
`examples/build_kg.rs` over the settled db. Lands at ~4 GB, under the ceiling.

**Open / candidate code fixes (none are "the" fix — they reduce stacked overhead):**
1. **Embed arena (the floor):** investigate why the q4 BYO path's arena reaches ~4 GB and why it is
   so slow on large CJK chunks — does `InitOptionsUserDefined::with_max_length` actually clamp the
   sequence length on the BYO path the way the registry path does? If not, a single huge CJK chunk
   blows the tensor. This is the dominant lever AND the impractical-slowness problem.
2. **Defer live L7 during a bulk import** (sequencing) — removes contributor (2) for incremental
   mounts too.
3. **`SEMFS_NO_PUSH`/`NO_SYNC` env→flag bridge** in `mount.rs` — removes contributor (3)'s footgun.
4. **Seed poller must distinguish OOM/`DAEMON_GONE` from completion** (now samples RSS + pid).

## Status
Safe seed (KG off + flags) running, plateaued ~4.08 GB, surviving; user opted to let it finish,
then build KG offline + verify end-to-end. Identity genuine `byo:gemma-q4-onnx:768`.
**Caveat:** q4 is ~3.5 files/min on this corpus → a full seed may take hours; q4 is also not the
token lever (Gemma≈e5) — falling back to a faster embedder for the seed is on the table.

## Refs
`embed/local.rs` (arena, `EMBED_MAX_LENGTH`, BYO `with_max_length`),
`backend/sqlite_vec.rs` (`index`, `index_graph`), `cache/graph_queue.rs`,
`cmd/mount.rs`/`daemon_runtime.rs` (no env→flag bridge),
prior: `rcas/2026-05-30-...unbounded-rehydration.md`, `rcas/2026-06-08-partial-seed-indexing.md`.
