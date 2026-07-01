# RCA: local document extractor hangs the seed on large files (unbounded text → embedding spin)

- **Date:** 2026-06-03
- **Found by:** E2E local-extraction smoke test on chanpin (EC2), `tickets/local-document-extractors/`
- **Severity:** blocking — a full local seed of chanpin cannot complete; box thrashes.
- **Status:** FIXED 2026-06-03 (cap + timeout landed in `extract::extract_text`); pending EC2 E2E re-run.

## Symptom

Local seed (fresh container, import over a copy of `chanpin_raw`, L7+OCR off) progressed
normally then **froze at 452 / ~1,371 files, 3,110 chunks, 23 / ~415 binary docs**. The
stability watcher mis-reported "drained" because the chunk count stopped growing.

It was **not** idle: `ps -T` showed **4 threads pegged at 91–99% CPU in `R` (running,
userspace) state for 25+ minutes**, no file open in the seed dir, **no chunk committed**,
no new log line for 27 min. RSS climbed past 4.4 GB and the box began swap-thrashing
(SSH dropping with 255). Killing the daemon freed all memory (→ 15 GB free).

## Root cause

`extract_text` returns the **full** extracted text of a document with **no size bound**,
and `LocalIndexer::index()` then chunks + embeds all of it in **one transaction** (chunks
are not committed until the whole file is embedded). The chanpin corpus contains very large
binary docs the seed had not yet reached:

```
44 MB  uxassessment_report_2025q1comprehensive_edition.pdf
26 MB  data_reportingapidocument_v1_1.pdf
23 MB  techflow_2025h2release_schedule.xlsx
14 MB  b2bcustomer_requirements_research_report.pdf   (… 395 binary docs pending)
```

A 23 MB spreadsheet / 44 MB PDF extracts to a very large text blob → an enormous number of
chunks → embedding them runs for tens of minutes (4 ONNX intra-op threads pegged), holding
all chunk vectors in memory (RSS growth), with no incremental commit. The seed cannot
progress past the first such file.

The OOM #2 fix (`EMBED_BATCH_SIZE=16`, `EMBED_MAX_LENGTH=1024`) bounds *batch* and
*per-chunk token length*, but **not the number of chunks per file** nor total extracted
text — so one document produces unbounded chunk count → unbounded embedding time + memory.

The 4-thread (not 1-thread) signature points to ONNX embedding rather than a single-threaded
`pdf-extract` loop; but `pdf-extract` is *also* known to CPU-loop on some malformed/large
PDFs, so both failure modes are plausible and the fix must cover both.

## What works (not in scope of the bug)

- Extraction itself is correct on normal docs: 23 binary docs (xlsx/pdf/docx/pptx) indexed
  with real chunks before the wall.
- The "never silently dropped" accounting works: 10 jpgs correctly recorded in `fs_unindexed`
  with `fmt=Jpeg` (OCR off).
- Fresh-container isolation worked: "initial sync: 1 docs reconciled" — zero cloud confound.

## Proposed fix (extract module hardening)

1. **Cap extracted text per file.** In `extract_text`, truncate the returned text to a
   bound (e.g. `MAX_EXTRACT_BYTES ≈ 512 KB–1 MB`) with a `WARN` and mark the file as
   *partially indexed*. A search index does not need every cell of a 23 MB spreadsheet;
   the head is enough for retrieval. Bounds chunk count, embed time, and memory per file.
2. **Timeout around extraction.** Wrap each `spawn_blocking` extractor in a timeout (e.g.
   30–60 s); on timeout, route to the unindexed bucket (`extract_failed: timeout`). Defends
   against `pdf-extract` infinite loops that a size cap can't (the cap applies only after
   extraction returns).
3. **(Optional) Cap per-file chunk count** at the `index()` layer as a second backstop, and
   consider committing chunks in batches so progress is incremental.

## Implemented (2026-06-03), `crates/semfs-core/src/extract/mod.rs`

1. **Size cap** — `MAX_EXTRACT_BYTES = 1 MiB`. `extract_text` truncates every
   extractor's output on a UTF-8 char boundary (`cap_text`) + WARN, bounding chunk
   count → embedding time → RSS per file. (Primary fix for the observed embedding
   spin: the symptom is downstream in `index()`, which the cap bounds by bounding
   the text it's handed.)
2. **Extractor timeout** — `EXTRACT_TIMEOUT = 45 s`. `blocking()` wraps each
   `spawn_blocking` extractor in `tokio::time::timeout`; on elapse → `None`
   (unindexed) so a `pdf-extract` CPU-loop can't stall the import. Caveat: the
   timed-out `spawn_blocking` thread can't be cancelled and runs to completion
   detached — accepted as the lesser evil (documented in code).
3. Per-file chunk-count cap / incremental commit (#3) NOT done — the size cap
   makes it unnecessary for now; revisit if a 1 MiB head still embeds too slowly.

Tests (`extract::tests`): `extract_text_caps_oversized_output` (synthesized 2 MiB
docx → ≤ cap), `cap_text_truncates_on_char_boundary_no_panic`,
`blocking_times_out_a_slow_extractor`. Full suite green (260).

## Verification plan after fix

Re-run the same E2E; expect the seed to complete (~1,371 files indexed, ~415 binary docs),
the large PDFs/xlsx either partially-indexed (capped) or in the unindexed bucket (timeout),
bounded RSS, and `semfs grep` returning hits inside `.xlsx`/`.pdf`.

## Related

- `tickets/local-document-extractors/issue.md`
- `tickets/solve-oom-issue/` (the batch/length caps this extends)

---

## CORRECTION (2026-06-03, after E2E re-run with the cap+timeout fix)

**The fix did NOT resolve the hang.** The re-run stalled at the *identical* deterministic
point (452 files / 3,110 chunks / 23 binary docs) with `capped_or_timed=0`, then CPU-wedged
the box (all 4 vCPUs pegged → SSH unreachable; recovered via a persistent targeted `pkill`,
not a reboot).

**Actual root cause (different from the original hypothesis):** the deterministic killer is
**not a binary document** — it is a **large UTF-8 *text* file**:
`/chanpin/.../interview_assessment_records/node_modules/docx/dist/index.umd.cjs`
(858 KB, 23,076 lines). The corpus has **12 such >200 KB JS/code bundles** in `node_modules`.

In `cache/file.rs::flush()` the content split is:
- `Ok(text)`  → `index(full content)`  — **the UTF-8 text path, which has NO cap**
- `Err(_)`    → `extract_text` → `cap_text`  — the other thread's 1 MiB cap lives ONLY here

A large UTF-8 file takes the `Ok` branch and hands its **full** content to `index()`, which
chunks it into ~1–2k chunks (code-embedder lane) and embeds them with no bound → CPU/RAM
grind that effectively stalls the import and wedges the box. The extractor cap+timeout are
in a code path the killer never enters — hence `capped_or_timed=0` and the unchanged wall.

**Proper fix (IMPLEMENTED 2026-06-03):** bound the content handed to `index()` on **all**
paths, not just extraction. Done via `backend::chunk::cap_index_content` (1 MiB) applied at
the top of both `index()` impls (sqlite + pgvector), before chunking — so the `Ok(text)`
UTF-8 branch is bounded too. The extraction-path cap is kept as defense-in-depth. See
`rcas/2026-06-03-extract-uncapped-utf8-text-path-node-modules-hang.md` for the full fix.
Optional follow-up: skip vendored `node_modules` in the seed copy (benchmark fidelity).
