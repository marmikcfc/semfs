# Ticket: Solve OOM issue (semfs local pre-warm)

- **Status:** RESOLVED — OOM #1 fixed & verified; **OOM #2 fixed & verified (2026-06-02).**

## RESOLUTION (2026-06-02)

**OOM #2 root cause (corrected):** the embedding models are **dynamically quantized**, so
fastembed runs ALL of a file's chunks in ONE ONNX pass (`batch_size=None` → `texts.len()`; it
*rejects* `Some(n<len)`). A 273-chunk transcription → a single 273-sequence batch through the 768-d
code model → ~7 GB arena (ONNX retains the high-water mark). Invariant to a naive `Some(16)` (which
would error, not sub-batch) — which is why the first attempt failed identically.

**Fix** (`crates/semfs-core/src/embed/local.rs`):
1. **Sub-batch in our code** — `embed()` splits chunks into windows of `EMBED_BATCH_SIZE=16` and calls
   `model.embed(window, None)` per window (the only form dynamic-quant accepts), bounding every ONNX
   pass to 16 sequences regardless of a file's chunk count.
2. **Cap sequence length** — `with_max_length(EMBED_MAX_LENGTH=1024)` bounds the *quadratic* attention
   term (the text model is already 512-capped by its own max; this caps the code lane). 2048 plateaued
   the warm at ~9 GB; 1024 → ~5.5 GB.

**Verified on EC2** (full chanpin warm, `-P8`): sailed **past the chunk-963 / 273-chunk danger zone**,
**peak RSS 5.5 GB** (was 14 GB OOM), no OOM. Tests: 223 (semfs-core) + 43 (semfs) pass, clippy clean.
Quality note: only the code lane truncates at 1024 (≈ rare >1024-token code/CJK chunks); recall
unaffected for the common case.

---

### (original report below)
- **Status (at filing):** PARTIALLY RESOLVED — OOM #1 fixed & verified; **OOM #2 OPEN (blocker).**
- **Created:** 2026-06-01
- **Branch:** `feat/backend-agnostic-store`
- **Component:** `semfs` daemon — mount / cache / local indexer
- **Related RCA:** `rcas/2026-06-01-semfs-prewarm-oom-import-collection.md`
- **Goal it blocks:** building a *complete* local index (pre-warm to 100%) for the Workspace-Bench `chanpin` (PM) container, which is the prerequisite for trustworthy local-vs-cloud token comparisons and for workbench-lite. Also blocks "seed entire PM dataset + run smoke" at full coverage.

---

## TL;DR

Pre-warming the full `chanpin` container with the local (fastembed) backend OOM-kills the daemon. Investigation found **two distinct OOMs**, plus one throughput blocker discovered en route:

1. **OOM #1 — import-collection (FIXED).** Mounting onto a non-empty directory buffered every file's bytes into one in-memory `Vec` (~15.7 GB for the corpus). Fixed by streaming the import one file at a time.
2. **OOM #2 — indexing memory ratchet (OPEN).** With the fast indexing path running, daemon RSS ratchets up step-wise to **~14 GB** and OOMs the 16 GB box after only **~1,280 / ~14,400 chunks (~9%)**. Signature points to ONNX-runtime CPU arena retention (and may be amplified by parallel reads). **Not yet fixed.**
3. **Throughput blocker — L7 entity extraction (WORKED AROUND).** `index()` makes a synchronous LLM call per file when `OPENROUTER_API_KEY` is set → ~1 file/s, CPU 84% idle. Not an OOM, but it masked OOM #2 by making indexing too slow to reach the ceiling.

---

## Environment

- **EC2:** `i-0c491c7cc23de8555` (`semfs-benchmark-host`), `m7i.xlarge` (4 vCPU / 16 GB, **no swap**, no GPU), `ap-south-1`, IP `13.201.35.159`.
- **Binary:** `semfs 0.0.5`, release build (OOM #2 also reproduced on a `--features pglite` build). Deployed to `~/.local/bin/semfs`.
- **Container:** `workspace-bench-chanpin` — **903 docs pulled** (`fs_remote`); ~1,181 nonempty text/code files + ~1,178 raw binaries; full index ≈ **14,400 chunks** (2× the 7,217 at the persisted 50%).
- **Models (local default):** text = `Snowflake/snowflake-arctic-embed-s` (384d), code = `jinaai/jina-embeddings-v2-base-code` (768d). Cached at `/home/ubuntu/.fastembed_cache`.

---

## OOM #1 — Import-collection (FIXED)

### Symptom
Daemon OOM-killed at ~15.6 GB, deterministically, when pre-warming the full container. Earlier notes mis-attributed this to `initial_sync`/the embedder.

### Root cause
`crates/semfs/src/cmd/daemon_runtime.rs`:
- `:146` `let created_dir = !cfg.mount_path.exists();`
- `:151` `pre_existing_files = if cfg.import_existing && !created_dir { collect_files_recursive(...) }`
- `:641` (original) `collect_files_recursive() -> Vec<(String, Vec<u8>)>` did `std::fs::read(&path)` on **every file** and pushed full bytes into one `Vec`, held across the whole `initial_sync`.
- `import_existing` defaults to **true** (`crates/semfs/src/cmd/mount.rs:135  let import_existing = !args.no_import;`).

So mounting onto a **non-empty** dir (`!created_dir`) slurped the entire corpus into RAM before the FS even mounted: 983 docs × ~16 MB ≈ **15.7 GB** (exact match). Because this happens at mount setup *before any embedding*, it explained the prior "embedded count flat while RAM balloons" and the "scoped never OOM / full always OOM" correlation (incidental: benchmark runs mount *fresh empty* workdirs → import skipped; the pre-warm re-mounted over a *populated* dir → slurp).

### Fix applied
`collect_files_recursive` → **`collect_file_paths_recursive`** returning `Vec<(String, PathBuf)>` (paths only). The import loop now reads each file's bytes **lazily, one at a time** (`std::fs::read(real_path)` inside the loop), so peak RAM = one file. Safe because import runs *before* `mount_fs`, so the underlying files are still readable by real path.

### Verification (end-to-end)
Repro: mount over a non-empty dir of 150 × 20 MB ≈ 3 GB, `SEMFS_EMBED_BACKEND=hash` (no embedding):

| | Peak daemon RSS at `collected 150 file(s) for import` | Import result |
|---|---|---|
| Before fix | **3,022,456 KB (~3 GB)** | — |
| After fix  | **155 MB** (flat) | `import: 150 imported, 0 already existed, 0 failed` |

`cargo test -p semfs` → 43/43 pass. Real 983-doc pull still reconciles. **Not yet committed.**

---

## OOM #2 — Indexing memory ratchet (OPEN — primary blocker)

### Symptom
With OOM #1 fixed and L7 disabled (so indexing runs fast), a full read-all warm drives daemon RSS up **step-wise** and OOMs the 16 GB box after ~1,280 chunks (~9% of the full index).

### Evidence (sampled every ~5 s, local backend, L7 off, `xargs -P8` reads)
```
sample   chunks   daemon RSS     MemAvailable
s1          1      0.98 GB        14.5 GB
s4        263      2.11 GB        13.4 GB
s5        312      2.77 GB        12.7 GB
s11       371      3.63 GB        11.8 GB
s17       534      4.54 GB        10.9 GB
s19       618      6.90 GB         8.5 GB   <- held flat at 6.9 GB for ~9 samples...
s27       943      6.96 GB         8.5 GB
s28       963     13.98 GB         1.5 GB   <- +7 GB on a SINGLE operation → near-OOM → killed
```
- Peak RSS recorded: **13,983,096 KB ≈ 14 GB**; `MemAvailable` crashed to 150 MB.
- CPU peaked at **~150%** (never 300%) → embed is Mutex-serialized + memory-bound, **not** CPU-saturated.
- RSS **ratchets to high-water-marks and holds** (does not release between files), with discrete jumps — classic **ONNX-runtime CPU arena retention** on progressively larger embed operations. The +7 GB jump near chunk 960 likely corresponds to the **768-dim jina code embedder**'s first large batch over the ~290 `.js/.ts` files.

### What it is NOT
- **Not corpus size** — OOMs at <10% coverage.
- **Not one giant file** — largest *indexed* file is only **273 chunks**. The 15–20 MB files are raw binaries (xlsx/pdf), correctly skipped by the read filter (only UTF-8 text is indexed; their text lives in `.md` transcription siblings).
- **Not the import path** (OOM #1, already fixed) and **not hydration** (R2 fetches finished during `initial_sync`, before the read pass).

### Relevant code
- `crates/semfs-core/src/cache/file.rs:284` `flush()` → `indexer.index(ino, filepath, &text)` — indexing fires on FUSE `close()`/flush (incl. read-only `cat`).
- `crates/semfs-core/src/backend/sqlite_vec.rs:404` `index()` → `embedder.embed(&chunks)` embeds **all of a file's chunks in one call** (fastembed sub-batches internally at ~256, so the per-call forward pass is bounded — but the ort arena it allocates is retained).
- `crates/semfs-core/src/embed/local.rs:26` embedder is a single `Mutex<TextEmbedding>` → all indexing serializes through one ONNX session/arena.

### Open hypotheses (need confirmation)
1. **ort CPU arena retention** — ONNX runtime's default arena grows to the largest tensor shape seen and never shrinks. Fix: configure a bounded/non-retaining arena allocator, or `disable_cpu_mem_arena`, or recycle the session.
2. **Code-lane (768d) memory** — the jina code embedder's arena dominates; the +7 GB step aligns with first large code batches.
3. **Harness amplification (my fault):** the warm used `xargs -P8` → up to 8 files chunked/pre-embedded concurrently, each holding allocations before the Mutex. A **sequential (`-P1`)** warm may have a materially lower peak and could fit 16 GB. **This is the cheapest next test.**

### Status: OPEN. Not yet fixed.

---

## Throughput blocker — L7 entity extraction (worked around, not a fix)

- `crates/semfs/src/cmd/resolve.rs:153` `build_llm()` attaches an LLM **iff `OPENROUTER_API_KEY` is set** (no separate flag).
- `index()` then calls `extract_entities(llm, content)` **per file** → a synchronous LLM API round-trip on the write path.
- Effect: warm runs at **~1 chunk/s with CPU 84% idle** (network-bound). Disabling it (unset `OPENROUTER_API_KEY`; embedding is local, rerank was `none`) gave a **~10× speedup** and a CPU-bound profile — which is what *un*masked OOM #2.
- **Decision needed:** does the benchmark/smoke index need the entity graph (`edges`)? If yes, L7 must run but is ~100× slower and needs batching/concurrency. If no, keep it off for warms.

---

## How to reproduce

**OOM #1 (now fixed — to regression-test):**
```bash
# build a non-empty mount target, then mount over it with import on (default)
mkdir pop; for i in $(seq 1 150); do head -c 20000000 /dev/zero > pop/doc_$i.md; done
SEMFS_EMBED_BACKEND=hash semfs mount workspace-bench-chanpin --path ./pop --foreground --no-push --key $KEY
# watch RSS at "collected 150 file(s) for import": before fix ~3 GB, after fix ~150 MB
```

**OOM #2 (open):**
```bash
# fresh cache -> cold pull, mount onto EMPTY dir, then read every text file (close->flush->index)
unset OPENROUTER_API_KEY                 # disable L7 so indexing is fast enough to hit the ceiling
SEMFS_RERANK_BACKEND=none semfs mount workspace-bench-chanpin --path ./mnt --foreground --no-push --key $KEY
find ./mnt -type f -not -iregex '.*\.\(pdf\|xlsx\|docx\|jpg\|png\|...\)' -print0 | xargs -0 -P8 -I{} cat {} >/dev/null
# sample daemon RSS vs MemAvailable -> ratchets to ~14 GB, OOMs
```
Scripts on laptop `/tmp/` and EC2 `/tmp/`: `oom_import.sh` (OOM #1 repro), `warm.sh` (OOM #2 warm + sampler), `seedcheck.py` (coverage).

---

## How to verify index coverage ("how much is seeded")
`python3 seedcheck.py ~/.cache/semfs/<org>/workspace-bench-chanpin.db` →
- `fs_remote` = cloud-seed (docs pulled).
- `COUNT(DISTINCT filepath) FROM chunks` = files embedded; `COUNT(*) FROM chunks` = vectors.
- Current real index: **610 / 1,181 nonempty text files = 51.7%**, 7,217 chunks, 806 MB on disk.
- Note: `vchunks`/`vchunks_code` are `vec0` virtual tables (need the extension); `chunks` is the plain 1:1 proxy. pglite stores chunks in its own data dir, **not** the SQLite cache — `seedcheck.py` reports 0 there.

---

## Index state: current vs. expected after the fix

**Measured current state** of the persisted SQLite index
(`~/.cache/semfs/CjeeM2Seni3y9xfovYsfv4/workspace-bench-chanpin.db`, built incrementally by the *old* binary across benchmark runs + partial pre-warms — never completed because of the OOM):

| Metric | **Current (now)** | **Expected after fix (full warm)** |
|---|---|---|
| Coverage (indexed / nonempty-text files) | **610 / 1,181 = 51.7%** | **~1,181 / 1,181 ≈ 100%** (minus any empty / non-UTF-8 skipped) |
| Embedded chunks (= vectors) | **7,217** | **~12,000–14,000** |
| └ text lane (`vchunks`, 384d) | 329 files / 2,076 chunks | ~870 files / ~5,500 chunks |
| └ code lane (`vchunks_code`, 768d) | 281 files / 5,141 chunks | ~310 files / ~5,700 chunks (already ~91% done) |
| Fulltext rows (`ffts`) | 7,217 (== chunks ✓) | == chunks (1:1 invariant holds) |
| Vector rows (`vchunks` + `vchunks_code`) | == chunks (1:1, needs `vec0` to read) | == chunks |
| Entity-graph edges (`edges`, L7) | **110** (stale, from a partial L7 run) | **0 if L7 off** for the warm / scales with files **if L7 on** — see decision below |
| Cloud docs pulled (`fs_remote`) | 903 | 903 (unchanged — content is already fully pulled) |
| File content (`fs_data`) | complete (all docs hydrated) | complete (unchanged) |
| DB size on disk | **806 MB** | **~0.9–1.0 GB** (content already present; only vectors + fts grow) |
| Daemon peak RSS during warm | **OOMs at ~14 GB** before completion | **bounded, well under 16 GB** — the success criterion |
| `semfs grep` recall | coin-flip for files outside the indexed 610 | reliable across the whole corpus |

Notes:
- The DB is already 806 MB because **`fs_data` (raw + transcription content) is fully pulled**; finishing the index only adds the remaining vectors + fts rows, so the on-disk growth is modest (not 2×).
- The **code lane is nearly complete already** (281/~310 files); most of the *remaining* work — and the OOM #2 exposure — is in the **text lane** (~540 more files).
- `edges` (L7 graph) will **not** advance under the current "L7-off" warm plan. If the benchmark needs the graph, that's a separate, slower indexing mode to settle (see L7 section).
- Numbers in the "expected" column are projections from the current per-lane chunk densities (text ≈ 6.3 chunks/file, code ≈ 18 chunks/file); treat as ±10%.

## Proposed next steps (priority order)
1. **Cheapest test first:** re-run the warm with **sequential reads (`-P1`)** + RSS instrumentation. If it stays bounded → OOM #2 was harness-amplified and 16 GB suffices.
2. If it still ratchets: **bound the ort arena** (disable/limit CPU mem arena, or recycle the embedder session), and/or **sub-batch** the code-lane embeds. Re-test on 16 GB.
3. **Decide on L7** for warms (graph needed or not); if needed, batch/parallelize entity extraction off the hot write path.
4. **Commit OOM #1 fix** (`daemon_runtime.rs`) + this ticket + RCA.
5. Only if a code fix is deferred: resize to ~32 GB for the one-time warm (it peaked at 14 GB) — brute force, not a real fix, and wasteful for pglite (WASM32-capped at 4 GB anyway).

## Artifacts
- Fix diff: `crates/semfs/src/cmd/daemon_runtime.rs` (uncommitted).
- RCA: `rcas/2026-06-01-semfs-prewarm-oom-import-collection.md`.
- EC2 sample logs: `/srv/semfs-benchmark/{oom-exp,warm}/sample_*.log`.
