# Tech debt: semfs seed storage is ~7× the corpus — optimize (esp. disk-backed mode)

_Filed 2026-06-29. Folder `tickets/semfs-seed-storage-optimization/`. Linear: SEM-48._
_Surfaced while baking the xafs E2B template (SEM-47): the materialized seed is **5.9 GB** for an
**844 MB** corpus, which makes the E2B image build slow/risky._

## Problem

A semfs seed stores the corpus in **4 parallel representations**, three of which duplicate content
that already exists on disk. Measured on `xafs-gemma-q4.db` (19,170 files, 844 MB raw corpus) via
SQLite `dbstat`:

| object | size | role | derivable from disk? |
|---|---|---|---|
| `vchunks_vector_chunks00` | **1,896 MB** | 768-d **float32** embeddings (semantic search) | no — but compressible |
| `ffts_content` | **1,169 MB** | fts5 BM25 **keeps its own text copy** (keyword search) | yes (→ external-content) |
| `chunks` | **1,310 MB** | chunk text `semfs grep` returns | yes (→ offsets into files) |
| `fs_data` | **1,001 MB** | materialized POSIX tree (mountable) | yes (→ passthrough) |
| `ffts_data` | 347 MB | inverted-index postings | no |
| KG + btree idx + overhead | ~211 MB | entities/relations/communities/edges | no |
| **total** | **5.94 GB** | | |

Measured text content: `chunks` text = 963 MB, `fs_data` = 878 MB; FTS holds ~1.1 GB more → the same
corpus content is physically stored **~3×** as text, plus once as vectors. Only the vectors (1.9 GB),
FTS postings (347 MB), and KG (211 MB) are genuinely *new* information.

## Why it's this way (not a bug)

The seed is built to be **self-contained / portable** — bakeable into an E2B image with zero host
dependency. That portability is exactly what costs the duplication. On a host where the corpus is
already on disk (e.g. a local laptop), most of the 3.5 GB of copied content is redundant.

## Optimization tiers

| tier | change | est. size | self-contained? | code touch |
|---|---|---|---|---|
| **0 — today** | build search-only (skip `fs` phase → no `fs_data`) | 5.9 → **~4.9 GB** | ✅ | none (`SEMFS_SEARCH_ONLY` exists, `crates/semfs-core/src/cache/fs.rs:1078`) |
| **1 — small change** | + int8 vector quantization + external-content FTS | ~4.9 → **~2.3 GB** | ✅ (E2B-bakeable) | `vec0(... int8[768])` at `examples/gemma_seed.rs:60`; fts5 `content='chunks'` (insert at `examples/merge_seeds.rs:120`) |
| **2 — disk-backed** | + chunks as `(path, offset, len)` pointers; read text from real files; passthrough mount | ~2.3 → **~1.0 GB** | ❌ needs files alongside | new chunk-store + passthrough VFS mode |

Optional deeper: binary-quantize vectors + rerank (1.9 GB → ~60 MB at some recall cost); Matryoshka
dim-truncation (768 → 256 = 3×); zstd on the text columns (markdown compresses ~3–4×).

## The tradeoff

```
SELF-CONTAINED (current, 5.9 GB)        DISK-BACKED (~1 GB + the files)
  ships anywhere, no originals            lean; indexes huge corpora cheaply
  survives file moves/edits               breaks if files move/change
  required for E2B (no host files)        ideal for a local laptop / dev box
```

This is the "copy then index" (current) vs "index in place" (ctags/ripgrep/Spotlight) choice. Pick by
deployment, not globally — a seed-build flag should select it.

## Proposed work

1. **Tier 1 (self-contained, near-term):** int8 vectors + external-content FTS → a ~2.3 GB seed that
   still bakes. Unblocks/derisks large-corpus E2B bakes (SEM-47 xafs is the trigger). Validate recall
   parity on a few cases before adopting.
2. **Tier 2 (disk-backed mode):** a build flag that stores chunk offsets + passthrough VFS over the
   real corpus dir, for local/laptop use. Biggest win where files are already on disk.
3. Quantization is orthogonal and applies to both — land it first (smallest change, largest single win).

## Acceptance

- A documented seed-build flag (e.g. `--storage {portable|lean|disk-backed}`).
- Lean/disk-backed seed passes the same retrieval smoke as the portable seed (no material recall loss
  on a sample of cases).
- xafs seed rebuilt lean ≤ ~2.5 GB and bakes into E2B without size/timeout failures.

## Refs
- Measured: `dbstat` breakdown (this ticket), `inspect_seed_tables` (5.94 GB, 615,950 chunks).
- Trigger: SEM-47 (xafs PPR A/B) E2B bake at 5.9 GB.
- Code: `crates/semfs-core/src/cache/fs.rs` (search_only), `examples/gemma_seed.rs` (vchunks float[768]), `examples/merge_seeds.rs` (ffts insert), `crates/semfs-core/src/cache/db.rs` (chunks/ffts index tables).
