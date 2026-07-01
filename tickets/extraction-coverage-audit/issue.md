# Extraction coverage audit — which files weren't embedded, and why

- **Type:** Investigation → fix
- **Status:** OPEN
- **Created:** 2026-06-05
- **Component:** `semfs-core/src/extract/*` + the import/index path

## Symptom
During the from-scratch Gemma seed of `chanpin_raw` (2,128 files), the daemon log streamed many:
```
WARN binary file not extractable; recording as unindexed  filepath="…/foo.pdf"  fmt=Pdf
WARN binary file not extractable; recording as unindexed  filepath="…/bar.xlsx" fmt=Unknown
WARN extracted text exceeded cap; indexing the head only (partial)  capped_bytes=1048575
```
The Gemma seed ended with **5,670 chunks** vs the e5 seed's **5,777** — i.e. coverage differs, and an
unknown number of files are **silently unsearchable**. We need to know exactly which files, by format,
and why — because a file that isn't embedded can never be retrieved (no embedder fixes that).

## Observed failure buckets (from logs)
1. **`fmt=Pdf` not extractable** — many; likely **scanned PDFs with no text layer** (need OCR) or
   encrypted/edge PDFs. (semfs has an OCR path — is it firing? gated? failing?)
2. **`fmt=Unknown`** for some `.xlsx`/`.docx`/`.xls` — extension says Office, content sniffer says
   Unknown. Two known sub-cases: (a) **403-HTML masquerading as `.xlsx`** (the `top10_product_status_table.xlsx`
   class — error pages stored as files), (b) **legacy/edge Office** the parser rejects.
3. **`.ppt`/`.pptx` not extractable** (`fmt=Ppt`) — legacy PowerPoint coverage gap.
4. **`.xls` (legacy)** — old BIFF format coverage.
5. **Index cap** — `extracted text exceeded cap; indexing the head only` (1 MB cap) — large docs
   partially indexed (tail unsearchable) e.g. the 2 MB answer dashboard.

## Audit plan (read-only; do NOT touch seeds)
1. **`fs_unindexed` table** — the schema records unindexed files. Dump it from each seed
   (`chanpin-gemma`, `chanpin-e5-nosum`) read-only: `SELECT filepath, reason/fmt FROM fs_unindexed`.
   Group by extension + reason → the definitive "what's missing" list + counts.
2. **Cross-check 403-HTML** — count files whose stored content starts with `<html>…403 Forbidden`
   (ingestion corruption vs genuine extraction failure — different fixes).
3. **OCR path** — confirm whether the OCR extractor runs for text-less PDFs (logs say "not
   extractable" — is OCR disabled/gated/erroring?). Check `extract/ocr.rs` + `extract/pdf.rs`.
4. **Coverage delta e5 vs Gemma** — why 5,777 vs 5,670? Same raw files, same extractor → the 107-chunk
   diff suggests non-determinism or a code drift between when each was seeded; reconcile.
5. **Index cap** — list files hitting the 1 MB cap; decide raise vs smarter-truncate (the answer
   dashboard is a victim — see `tickets/embedder-config-search`).

## Why it matters
Retrieval quality work (embedder/ranking) is moot for any file that never got embedded. If the answer
file or its siblings are in `fs_unindexed`, no amount of Gemma/Qwen3/RRF tuning helps. This audit
quantifies the ceiling.

## Deliverable
A table: `extension × reason × count` for the chanpin corpus, the list of high-value files missing, and
a prioritized fix list (OCR enablement, legacy Office, 403-re-hydration, index-cap policy).

## Related
- `crates/semfs-core/src/extract/{pdf,ocr,ooxml,spreadsheet,legacy_ppt}.rs`
- `tickets/embedder-config-search/` · `tickets/explore-agent-search-behavior/` (403-HTML finding)
- `rcas/2026-06-03-extract-unbounded-large-doc-hang.md`
