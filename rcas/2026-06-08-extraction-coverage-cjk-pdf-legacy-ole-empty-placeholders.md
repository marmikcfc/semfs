# RCA: q4 seed "44% coverage" → 98.9% — empty placeholders + three missing extractors

**Date:** 2026-06-08 · **Host:** ubuntu@13.201.35.159
**Severity:** medium (coverage gap caps achievable benchmark recall; misleading headline number).
**Scope:** `chanpin-gemma-q4` seed of Workspace-Bench `chanpin_standard` (1,451 files).

## Symptom
Fresh q4 seed indexed only **663 of 1,451 files (~46%)** — alarming on its face.

## Investigation → the headline was misleading
`find -size 0` on the corpus: **747 of 1,451 files are 0 bytes.** Verified they're 0 bytes in EVERY
variant (`chanpin_standard`, `chanpin_raw`, and the agents' own workdirs) — so it's intrinsic to the
Workspace-Bench dataset, not our copy. The dataset is a realistic file *tree* (plausible corporate
doc names: `recruitment_plan.xlsx`, `org_structure_diagram.pptx`, …) where only the **704 files
with content** are populated; the rest are **empty placeholders / distractors by design.**

So the real number is **663 / 704 contentful (~94%)**, not 46%. (NB: these empties are
*fabrication bait* — see the KG-race RCA: an agent `cat`s a perfectly-named empty stub and invents
data. Full *content* coverage can't prevent that; the bait file is empty on purpose.)

## Root causes of the real gap (the ~41 unindexed contentful files)
1. **`OPENROUTER_API_KEY` sourced-not-exported** (the recurring footgun — bit push-disable, build_kg,
   and OCR this session). `seed_q4_safe.sh` did `source ~/.semfs_seed_env` (plain `KEY=…`, no
   `export`), so the seed daemon had no key → all LLM OCR/vision/xlsx-summary fallbacks returned
   `None`. Fix: `export OPENROUTER_API_KEY` after sourcing (durable fix: make `.semfs_seed_env` use
   `export`). This alone is most of the gap.
2. **No legacy-OLE extractor** — `.doc`/`.ppt`/`.xls` (and mislabeled OLE) had no pure-Rust path
   (`ooxml` only does OOXML zip; `legacy_ppt` is a stub returning `None`).
3. **CJK PDFs failed expensively** — `pdf-extract` (Rust) chokes on CJK CID fonts → fell back to
   whole-PDF `mistral-ocr`, which **timed out (120s budget)** on multi-MB / many-page scans. The
   text was sitting in a text layer the whole time; `pdftotext` (poppler) pulls 28,701 CJK chars
   out of a 57-page PDF in <1s.

## Fix — three CLI fallbacks (the tools WB's own agents shell out to), shipped + deployed
Pattern: when the pure-Rust extractor returns `None`, shell out to the mature CLI built for the job
(content-sniffed, never extension-trusted). All gated, bounded, graceful-when-absent.
- **`pdftotext` (poppler)** — `extract/pdf.rs::pdftotext`, wired as PDF tier 2 (before OCR). Reads
  CJK CID-font text layers, fast + local. RCA-of-record for the "OCR was never needed" insight.
- **`soffice` (LibreOffice)** — `extract/mod.rs::soffice_to_text`, gated to office binaries (OLE2 /
  OOXML zip), format-aware filter (Writer→txt, Calc→csv, Impress→txt), private per-call profile.
- **`ocr_pdf_paged` (poppler + vision)** — `extract/ocr.rs`, the path for image-only scans of ANY
  size: `pdftoppm` rasterizes ≤40 pages → downscaled ~100 KB JPEGs → per-page `gpt-4.1-mini` vision
  OCR (bounded-parallel, 6 workers) → concat. Recovered the **141-page** and 28-page scanned PDFs
  the whole-blob OCR couldn't touch. PDF chain is now: `pdf-extract → pdftotext → ocr_pdf_paged →
  ocr_pdf`. (49 extract tests pass.)

## Result
**663 → 696 / 704 contentful (98.9%).** Remaining 8 are genuinely hard:
- 2 truly unindexable (a `.pyc` bytecode, a product `.jpg` photo) — no text by nature.
- 2 `P0202…` PDFs poppler **cannot parse** (malformed/encrypted — `pdfinfo`/`pdftoppm` return nothing).
- ~4 office/slide-image edge cases (image-only `.pptx`, odd legacy `.xls`).

## Prevention
- **`export` the keys** in `.semfs_seed_env` (kills the recurring footgun across push/build_kg/OCR).
- Verify a deploy by content (`strings | grep`) — done for each fallback.
- "Coverage" must distinguish **imported** (inode exists) from **ingested** (has embeddings); only the
  latter is searchable. Report against *contentful* files, not total (the 747 empties can never index).

## Refs
`crates/semfs-core/src/extract/{pdf.rs,ocr.rs,mod.rs}` (the three fallbacks),
`crates/semfs-core/Cargo.toml` (tempfile → regular dep),
prior: `rcas/2026-06-06-pdf-ocr-fallback-native-engine-rejects-scanned-pdfs.md`.
