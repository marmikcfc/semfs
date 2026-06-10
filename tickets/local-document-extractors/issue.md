# Feature: local document text extractors in semfs (L1 parse) — index Office/PDF/legacy/images without Supermemory

- **Type:** Feature (backend-agnosticism / local-authoritative seeding)
- **Status:** IMPLEMENTED 2026-06-03 (pure-Rust extractors + flush hook + accounting; Codex-reviewed; large-doc hang fixed). **FAST-FOLLOWS IMPLEMENTED 2026-06-08** (CLI-tool fallbacks: pdftotext / soffice / page-split OCR — closes the named CJK-PDF + legacy-`.ppt` gaps; coverage 663→696/704 on the q4 seed). See "Update — 2026-06-08" below. Pending: commit (`feat/backend-agnostic-store`, currently deployed-to-box-only) + merge.
- **Created:** 2026-06-03
- **Component:** `semfs-core` — new `extract` module + hook in `cache/file.rs::flush()`; `semfs status` (`unindexed_files`)
- **Branch context:** `feat/backend-agnostic-store`

---

## Problem

semfs's **local** index pipeline has **no document parser**. On flush, `cache/file.rs:292`
does `String::from_utf8(content)` and indexes only when it succeeds; **binary files
(docx/xlsx/pdf/pptx) fail the UTF-8 check and are SILENTLY DROPPED** — no log, no trace,
no accounting. So a local seed of a real workspace misses most of its content, and you
can't even tell what's missing.

This is why the whole seed today is routed through **Supermemory**: Supermemory owns the
document-understanding pipeline (Office/PDF parsing, OCR, image/PDF→markdown transcription).
The local side is only an "embed text you already have" pipeline. Measured on the PM
(`chanpin`) workspace by actual byte content (not extension):

| Class | files | indexed today? |
|---|---:|---|
| Raw workspace | 2,128 | — |
| **Empty 0-byte placeholders** | 747 | n/a (no content) |
| **Non-empty UTF-8 text/code** | 952 | ✅ (js 289, ts 276, txt 154, md 70, json 65, py 16, yml 14…) |
| **Non-empty BINARY** (real extraction targets) | **429** | ❌ silently dropped |
| — xlsx | 198 | ❌ |
| — pdf | 135 | ❌ |
| — docx | 52 | ❌ |
| — pptx | 22 | ❌ |
| — jpg (image/OCR) | 10 | ❌ |
| — xls | 8 | ❌ |
| — ppt (legacy) | 1 | ❌ |
| — noext/pyc (junk) | 3 | ❌ |
| Audio / video | 0 | — |

> The naive "Office files = 1,165" figure (by extension) was an artifact of **empty stubs**:
> 404 of the 456 `.docx` are 0-byte placeholders. The real extraction workload is **429
> files**, dominated by **xlsx (198) + pdf (135)**.

Net of meaningful content (952 text + 429 binary = **1,381 files**): only the **952 text
files (~69%)** are locally searchable today; the **429 answer-bearing binary documents are
invisible to `semfs search`** — and silently dropped with no trace.

## Desired end state

Local import reads raw bytes → **extracts text in-process** → feeds the existing L1–L7
pipeline. A local seed of `chanpin` covers ~all 2,128 files with **zero Supermemory calls
for parsing**. **No file is ever silently dropped** — it's extracted-and-indexed or
explicitly accounted for.

Stays **pure-Rust / single static binary** for documents (no native libs, no LibreOffice
shell-out). The one exception is **image OCR**, which uses a small cloud vision model
(OpenRouter `gpt-4.1-mini`) gated on `OPENROUTER_API_KEY` exactly like L7 — *not Supermemory*.

## Decisions (settled 2026-06-03)

**Crate strategy — mature best-of-breed; legacy `.ppt` descoped (see below):**

| Format | Non-empty files (chanpin) | Extractor | Notes |
|---|---:|---|---|
| docx, pptx | 52 + 22 | own `zip` + `quick-xml` | unzip → join `<w:t>` / `<a:t>` runs; one shared module |
| xlsx, xls | 198 + 8 | `calamine` | mature Rust standard; the bulk of the workload |
| pdf | 135 | `pdf-extract` | pure-Rust; scanned/image PDFs remain a gap |
| ppt (legacy OLE2) | 1 | **DESCOPED → unindexed** | `litchi 0.0.1` evaluated: compiles pure-Rust, doesn't panic, but returns `"*"` (no real text) for the corpus `.ppt`. Indexing junk pollutes search, so it's NOT worth an alpha dep for 1 file. Routed to the unindexed bucket; proper OLE2-PPT decoder is a fast-follow. |
| jpg (images) | 10 | OpenRouter `gpt-4.1-mini` vision | **key-gated**; absent ⇒ unindexed bucket |
| noext/pyc / parse-fail / image-without-key | 3 + … | **surfaced** | WARN + counter, never silent |

- **Pure-Rust posture:** documents need no native deps. `litchi` core (`ole`/`ooxml`) has
  none; we do **not** enable its `iWA` (protoc) or `fonts` (fontconfig) features.
- **OCR is the deliberate exception** (network call for images only; air-gapped runs skip it).
- Rejected: `ppt-rs` (it's `.pptx`-only, a generation lib — can't open legacy `.ppt`);
  LibreOffice headless (heavy runtime dep for 1 file — ticket as fast-follow if `.ppt`
  ever appears at scale in other personas).

**Route by magic bytes, not extension (added 2026-06-03, from real-corpus profiling):**
`file -b` over the 201 non-empty `.xlsx` in `chanpin_raw` shows the extension lies for
~26% of them: 148 are real Excel, but **20 are PDFs**, 18 are legacy OLE2 `.xls`, **3 are
HTML** (one a saved `403 Forbidden` page), 2 are bare zips, and **1 is a `.docx`** — all
wearing an `.xlsx` name. `.docx` similarly hides 2 OLE2 legacy `.doc`. Routing purely on
the suffix would hand those PDFs/HTML to `calamine` → silent drop, re-introducing the exact
bug this ticket kills. So `extract_text` sniffs leading bytes (`%PDF`, `FFD8FF`, `PK\x03\x04`
+ inner `word/`·`xl/`·`ppt/` marker, `D0CF11E0` + UTF-16LE stream name, `<html`) and routes
on what the file *actually is*. Implemented + tested against real fixtures in
`semfs-core::extract::sniff` (`tests/fixtures/chanpin/`, see its `MANIFEST.md`).

## Design summary (full spec: `docs/superpowers/specs/2026-06-03-local-document-extractors-design.md`)

- New `semfs-core::extract` module: `async fn extract_text(filepath, bytes) -> Option<String>`
  routes by **content sniffing (magic bytes)**, not by extension, to per-format extractors
  behind a `DocExtractor` trait (swappable in one place). See the routing decision below.
- **Single hook** in `cache/file.rs::flush()`: UTF-8 path unchanged for text/code; on
  UTF-8 failure, route bytes to `extract_text`; index the returned text, or record it as
  unindexed. **Raw inode bytes are never modified** — the agent still opens the real file;
  only the *embedded text* is extraction-derived. No transcription siblings.
- CPU parsers run in `spawn_blocking`; OCR is network-async. Failures never panic / never
  abort import.

## Implementation status (2026-06-03)

Landed in `crates/semfs-core/src/extract/` + `cache/file.rs` + `cache/db.rs` +
`cache/fs.rs` + `daemon/{protocol,ipc}.rs` + `semfs/src/cmd/status.rs`.

- **Extractors:** docx/pptx (`zip` + `quick-xml`), xlsx/xls (`calamine`), pdf
  (`pdf-extract`). All pure-Rust; the 4 new deps add **no new native/`-sys`**
  crates. `extract_text` sniffs magic bytes, dispatches, runs CPU parsers in
  `spawn_blocking`, OCR (`gpt-4.1-mini`, key-gated) on the same path.
- **Accounting:** new `fs_unindexed` table + `mark/clear/count_unindexed`;
  surfaced as `unindexed_files` in `semfs status`. Kept consistent across flush,
  unlink, and rename/overwrite (relabel is atomic in the rename tx).
- **Known gaps (both accounted as unindexed, never dropped):**
  - **Legacy `.ppt` DESCOPED** — `litchi 0.0.1` returns only `"*"` for the corpus
    file; not worth an alpha dep for 1 file. Fast-follow: a real OLE2-PPT decoder.
  - **CJK PDFs** — `pdf-extract` panics on non-Identity-H CMaps; the panic is
    contained (muted thread-local hook) → unindexed. Fast-follow: OCR/CID path.
- **Extension lies** (e.g. an HTML 403 page named `.xlsx`): `sniff` routes by
  content so they never reach the wrong parser. UTF-8-valid ones (HTML/CSV) are
  still indexed as their source text via the unchanged text path (not dropped).
- **Large-doc / large-text hardening** (RCAs `2026-06-03-extract-unbounded-large-doc-hang`
  + `…-uncapped-utf8-text-path-node-modules-hang`): an E2E seed froze on large files —
  uncapped content drove unbounded chunking/embedding (4 ONNX threads pegged 25 min,
  RSS → 4.4 GB). Fixed authoritatively at the **`index()` choke point**:
  `chunk::cap_index_content` (**1 MiB**) caps content before chunking in BOTH backend
  `index()` impls, so every source is bounded — the UTF-8 text path (a minified
  `node_modules` bundle was the deterministic killer), extracted-doc text, and re-index
  alike. Plus a **45 s per-extractor timeout** (`pdf-extract` CPU-loop → unindexed) and
  the extract-path cap as defense-in-depth. (Follow-up: exclude vendored `node_modules`
  in the seed copy — fidelity, not correctness.)
- **Review:** Codex adversarial pass (2 HIGH + 4 MEDIUM) + a verification pass
  (2 MEDIUM) + `codex exec review` (1 P1) — all resolved. `cargo test
  -p semfs-core` green (260), clippy + fmt clean.

## Update — 2026-06-08: fast-follows implemented (CLI-tool fallbacks)

The two "fast-follow" gaps named above (legacy `.ppt`/OLE, and CJK PDFs) are now closed by
shelling out to the **mature CLI tools the Workspace-Bench agents themselves use** — a
**deliberate reversal of the original "pure-Rust, no LibreOffice shell-out" posture**, justified
because the pure-Rust extractors structurally cannot decode CJK CID fonts (`pdf-extract`),
legacy OLE (`.doc`/`.ppt`), or scanned-image PDFs, while these tools can, fast and locally. The
pattern: when the pure-Rust extractor returns `None`, fall back to the CLI tool (still
content-sniffed, never extension-trusted; gated, bounded, graceful when the tool is absent).

**Shipped (49 extract tests pass; deployed to box, uncommitted):**
- **`pdftotext` (poppler)** — `extract/pdf.rs::pdftotext`. Reads CJK CID-font **text layers** that
  `pdf-extract` panics on (verified: 57-page PDF → 28,701 CJK chars, <1 s, no OCR). This is the
  big win: the prior whole-PDF `mistral-ocr` fallback **timed out (120 s)** on multi-MB CJK PDFs
  whose text was sitting in a layer the whole time.
- **`soffice` (LibreOffice headless)** — `extract/mod.rs::soffice_to_text`. Universal fallback for
  legacy OLE `.doc`/`.ppt`/`.xls` + any office binary the Rust path can't read. Gated to OLE2/OOXML
  containers, format-aware filter (Writer→txt, Calc→csv, Impress→txt), **private per-call
  `UserInstallation` profile** (so concurrent warm extractions don't collide on soffice's lock).
- **`ocr_pdf_paged` (poppler + vision)** — `extract/ocr.rs`. For image-only scans of ANY size:
  `pdftoppm` rasterizes ≤40 pages → downscaled ~100 KB JPEGs → per-page `gpt-4.1-mini` vision OCR
  (bounded-parallel, 6 workers) → concat. **Recovered the 141-page + 28-page scanned PDFs** the
  whole-blob OCR couldn't touch (the unit of work is now "one page," not "whole document").

**New PDF routing chain** (`extract/mod.rs`): `pdf-extract → pdftotext → ocr_pdf_paged → ocr_pdf`
(cheap+local first, expensive+networked last). Legacy OLE: `<pure-Rust> → soffice`.

**New runtime dependencies (host, not the binary):** `poppler-utils` (`pdftotext`/`pdftoppm`) and
`libreoffice-writer/calc/impress`. The binary degrades gracefully (returns `None` → unindexed) when
they're absent, so the build is still a single static binary — but **full coverage now requires
these on the seed/serving host.** Acceptance criterion "pure-Rust single binary (no native libs)"
is **amended**: extractor *fallbacks* are external CLI tools (opt-in by presence), not linked libs.

**Coverage result (q4 seed, `chanpin-gemma-q4`):** 663 → **696 / 704 contentful files (98.9%)**.
Remaining 8 are genuinely hard: 2 truly-empty (a `.pyc`, a `.jpg` photo), 2 `P0202…` PDFs poppler
**cannot parse** (malformed/encrypted), ~4 image-only `.pptx` / odd legacy `.xls` edge cases.

**Root cause of the low *initial* coverage** (separate from the extractor gaps): the seed daemon ran
**without `OPENROUTER_API_KEY`** — `source`'d from `.semfs_seed_env` (plain `KEY=…`) but never
`export`ed, so every LLM OCR/vision/xlsx-summary fallback returned `None`. The recurring footgun;
durable fix = `export` in `.semfs_seed_env`. RCAs:
`rcas/2026-06-08-extraction-coverage-cjk-pdf-legacy-ole-empty-placeholders.md`,
`rcas/2026-06-08-kg-materialization-race-empty-kg-codex-fabrication.md`; EXPERIMENTS.md §8.

## Acceptance criteria

- Local seed of `chanpin` with **no Supermemory document pull** indexes the binary documents:
  non-empty indexed files jump from **~952 → ~1,371** (no OCR key) / **~1,381** (with key) —
  i.e. ~100% of the 1,381 meaningful-content files — verified by the embedded-vs-on-disk
  coverage diff + `semfs grep` returning hits inside `.docx/.xlsx/.pdf`. (The 747 empty
  0-byte placeholders are correctly never indexed.)
- A binary file that can't be extracted is **logged + counted in `semfs status`
  (`unindexed_files`)** — never silently dropped.
- Parse failure on any single file does not crash or abort the import. **No single
  file can hang/OOM the import either** — extracted text is capped (1 MiB) and each
  extractor is time-bounded (45 s) so a giant or pathological document is
  partially-indexed or routed to `unindexed`, never a stall (RCA
  `2026-06-03-extract-unbounded-large-doc-hang`).
- Build remains pure-Rust single binary (no native libs); `cargo test -p semfs-core` green.
- Supermemory behavior unchanged when that backend is selected.

## Why it matters

- **Benchmark validity:** the semfs vs plain comparison is only fair if the local seed
  actually contains the documents the tasks need. Today it doesn't.
- **Local-authoritative seeding:** the parser is the missing half of
  `decouple-backends-from-supermemory` — with it, sqlite/pglite/pg backends can be seeded
  from a local corpus with no cloud round-trip for parsing.
- **No silent data loss:** binary files become visible/accounted-for instead of vanishing.

## Related

- `tickets/decouple-backends-from-supermemory/` — this is the local-parse half of it.
- `tickets/bench-per-case-remount-redundancy/` — the seed-once/reuse half.
- `tickets/parallelize-l7/`, `tickets/solve-oom-issue/` — the L1–L7 pipeline these feed.
