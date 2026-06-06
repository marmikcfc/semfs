# RCA: large UTF-8 text files (node_modules JS bundles) hang the local seed — the size cap is on the wrong code path

- **Date:** 2026-06-03
- **Found by:** E2E local-extraction smoke test on chanpin (EC2), *re-run* after the binary-doc
  cap+timeout fix landed. See sibling RCA `2026-06-03-extract-unbounded-large-doc-hang.md`.
- **Severity:** blocking — a full local seed of chanpin cannot complete; box CPU-wedges
  (all vCPUs pegged → SSH unreachable).
- **Status:** FIXED 2026-06-03 — cap moved to the `index()` choke point (bounds
  every source incl. the UTF-8 text path). Pending EC2 E2E re-run.

## Summary

The earlier fix added a 1 MiB size cap + 45 s timeout to `extract::extract_text` (the
binary-document path). The E2E re-run **stalled at the identical deterministic point**
(452 files / 3,110 chunks / 23 binary docs, `capped_or_timed=0`) and wedged the box — proving
the fix did not touch the real killer. The real killer is a **large UTF-8 *text* file**, which
takes a **different, uncapped code path**.

## Symptom

- Seed progressed, then froze at exactly **452 files / 3,110 chunks / 23 binary docs** —
  byte-identical to the pre-fix run (deterministic).
- `capped_or_timed=0`: neither the size cap nor the extractor timeout ever fired.
- 4 threads pegged ~100% (ONNX embed lane), no chunk commit, RSS ~4 GB, no new log.
- All 4 vCPUs saturated → new SSH sessions could not be scheduled (`ConnectTimeout` 255s).
  Recovered with a persistent targeted `pkill` (no reboot); box returned to 15 GB free.

## Investigation

Replicated the import's walk order (`collect_file_paths_recursive` = pre-order DFS in OS
`read_dir` order) and diffed it against the set of filepaths already in `chunks`. The first
expected-to-index file **missing** from the DB — i.e. the file the import was stuck on:

```
/chanpin/team_operations/team_structure_and_hiring/interview_assessment_records/
    node_modules/docx/dist/index.umd.cjs     858 KB, 23,076 lines, UTF-8
```

The corpus contains **12 such >200 KB JS/code bundles** under `node_modules/` (docx, etc.).

## Root cause

`cache/file.rs::flush()` splits content by UTF-8 validity:

```rust
let outcome = match String::from_utf8(content) {
    Ok(text) => ExtractOutcome::Text(text),                 // <-- UTF-8 path: FULL content, NO cap
    Err(e)   => extract::extract_text(filepath, &e.into_bytes()).await,  // <-- cap+timeout live here
};
... indexer.index(self.ino, filepath, &text).await ...
```

The 1 MiB cap (`cap_text`) and 45 s timeout are inside `extract_text`, which is only invoked
on the **`Err` (binary)** branch. A large **UTF-8 text** file takes the **`Ok` branch**,
bypasses extraction entirely, and hands its **full** content (858 KB) to `index()`. `index()`
chunks it (code-embedder lane) into ~1–2k chunks and embeds them with **no bound on text size
or chunk count** → CPU/RAM grind that stalls the import on that single file and saturates the
box. Because the killer never enters the extraction path, `capped_or_timed` stays 0 and the
wall is unchanged from the pre-fix run.

This is a **distinct failure mode** from the first RCA (which was about *binary* docs
extracting to huge text). Same downstream symptom (`index()` embed grind), different entry
path. The binary-doc cap is correct but incomplete: the bound was placed per-extractor, not
on the content actually handed to `index()`.

## Why the prior fix missed it

The first RCA reasoned the hang came from oversized *extracted* text and placed the cap in
`extract_text`. Correct for binary docs, but the bound belongs on **all** content reaching
`index()`. Large plain-text/code files (which never extract) slipped through.

## Proposed fix

1. **Cap content on every path** — apply the size cap *immediately before* `index()` (to both
   the `Ok(text)` and extracted-text branches), so no file — text or binary — can hand an
   unbounded blob to the embedder. Keep the extraction-path cap (harmless / defense-in-depth).
2. **Cap per-file chunk count in `index()`** — a source-independent backstop.
3. **Skip vendored `node_modules` during import** — the 12 killers are all minified library
   `dist/` bundles: distractor noise, not PM-workspace content. Excluding them improves both
   robustness and benchmark fidelity.

Recommended: **#1 + #3**.

## Implemented (2026-06-03)

**#1 done — cap at the `index()` choke point, not per-extractor.** The earlier
1 MiB cap lived in `extract_text` (binary path only). Moved the authoritative
bound to `backend::chunk::cap_index_content` (1 MiB, char-boundary slice) and
applied it at the top of **both** `SqliteVecStore::index` and
`PgVectorStore::index`, before `recursive_chunks`. Now EVERY caller is bounded —
the UTF-8 `Ok(text)` branch (the node_modules killer), the extracted-text branch,
and any future re-index path — regardless of source. WARN logs when it bites. The
`extract_text` cap (`MAX_EXTRACT_BYTES`) + 45 s timeout are kept as defense-in-depth
(they also bound the transient extracted string).

Tests: `index_caps_oversized_content_before_chunking` (real `SqliteVecStore`:
indexing a ~3.7 MiB code-like blob stores ≤ 2 MiB of chunk text — was 4.3 MiB
uncapped, the proven RED) + `cap_index_content_*` unit tests. Full suite green (263).

**#3 NOT done (deferred to seed tooling).** The seed imports by *writing files into
the mount*; there is no core import walk to filter, so excluding vendored
`node_modules` belongs in the corpus-copy step (e.g. `rsync --exclude node_modules`).
With #1 in place the hang is gone regardless; #3 remains a benchmark-fidelity
nicety (drop distractor bundles), not a correctness requirement.

## Verification plan

Re-run the same E2E; expect the seed to complete (~1,371 files / ~415 binary docs), large
text/code files capped or `node_modules` skipped, bounded RSS, no CPU wedge, and `semfs grep`
returning hits inside `.xlsx`/`.pdf`.

## Related

- `rcas/2026-06-03-extract-unbounded-large-doc-hang.md` (the binary-doc cap+timeout; same
  downstream `index()` symptom, different entry path).
- `tickets/local-document-extractors/issue.md`
- `tickets/solve-oom-issue/` (`EMBED_BATCH_SIZE` / `EMBED_MAX_LENGTH` bound per-batch + per-chunk,
  but not per-file content size or chunk count — the gap this RCA closes).
