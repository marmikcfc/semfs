# RCA: PDF OCR fallback silently drops every scanned PDF (`native` engine 400s)

**Date:** 2026-06-06
**Area:** `crates/semfs-core/src/extract/ocr.rs` (`ocr_pdf`)
**Severity:** seed-coverage (whole class of files unindexed on BOTH local + cloud)

## Problem (what / when / where / extent)
- **What:** Scanned / image-only PDFs (no text layer) are recorded as `fs_unindexed`
  instead of being extracted, even with `OPENROUTER_API_KEY` set.
- **Where:** local extraction path (`extract::extract_text` → PDF arm → `ocr::ocr_pdf`).
- **Extent:** ~19 PDFs unindexed in `chanpin-e5-nosum`, **40** in `chanpin-gemma`
  (gemma seed ran with no key, so even text-layer-less files that OCR *could* have
  recovered fell through). All the `P02…`-named gov-style scanned PDFs in the
  `chanpin` corpus are in this class.

## Why-chain (evidence)
1. **Why unindexed?** `ocr_pdf` returned `None`. — `extract_failed` MISS on every
   `P02*.pdf`; `pdf-extract` text layer empty (scanned), so it fell to OCR.
2. **Why did OCR return `None`?** `.ok()?` on the HTTP call swallowed an error. —
   Direct API probe (`/tmp/ocr_diag.py`) reproduced it.
3. **Why an HTTP error?** Provider returned **HTTP 400 `unsupported_file`**:
   *"The file type you uploaded is not supported. Please try again with a pdf."* —
   verbatim from OpenRouter (Azure/OpenAI upstream) with `pdf.engine="native"`.
4. **Why does `native` 400?** The `native` file-parser feeds the PDF's *parsed text*
   to the model. An image-only PDF parses to no text, and the upstream provider
   rejects it as unsupported rather than OCR'ing it. `native` is not an OCR engine.
5. **Root cause:** wrong file-parser engine for the fallback. The fallback only ever
   runs *after* `pdf-extract` already failed — i.e. exactly the no-text-layer case —
   yet it asked for `native` (text-passthrough), the one engine that can't OCR.

## Proof of fix
Same file, same key, only the engine changed:
- `engine="native"`     → HTTP 400 `unsupported_file`, 0 chars.
- `engine="mistral-ocr"` → HTTP 200, 1647 chars of correct transcribed text.

## Fix
`ocr.rs` `ocr_pdf_with_key`: `pdf.engine` `"native"` → `"mistral-ocr"`. Single line.
(Plus doc-comment correction: it IS a separate OCR service now, by design.)

## Stopping criteria
- Actionable ✓ (one-line engine change) · Controllable ✓ · Fundamental ✓ (recovers
  the whole scanned-PDF class) · Evidenced ✓ (HTTP 200 vs 400 A/B) · System-not-blame ✓.

## Counter-analysis
- *Does `mistral-ocr` regress text-layer PDFs?* They never reach OCR — `pdf-extract`
  handles them first. So the fallback's only inputs are OCR targets; `mistral-ocr`
  is strictly better here.
- *Cost?* ~ $0.0007 / scanned PDF (probe usage). Bounded by the 10 MiB size cap and
  only on the fallback path. Acceptable for seed-time.
