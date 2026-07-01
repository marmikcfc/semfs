# Tech debt: local-seed coverage gaps — (1) node_modules not auto-skipped on import, (2) ~28% of PDFs unextractable (need OCR)

- **Type:** Tech debt (robustness + extraction coverage)
- **Status:** OPEN
- **Created:** 2026-06-03
- **Component:** `semfs` import walk (`cmd/daemon_runtime.rs` `collect_file_paths_recursive`); `semfs-core::extract::{pdf,ocr}`
- **Branch context:** `feat/backend-agnostic-store`
- **Found by:** E2E local-extraction seed of the PM (`chanpin`) workspace on EC2 — the run that
  verified the document extractors (`tickets/local-document-extractors/`). These are the two
  honest caveats on an otherwise-successful full seed.

---

## Context (the successful run these caveats came from)

A full local seed of `chanpin` (no Supermemory parsing) completed: **562 files indexed**
(371 binary docs — xlsx 192, pdf 102, docx 50, pptx 20, xls 7 — + 191 text/code), 5,676
chunks, **59 explicitly accounted as unindexed**, 747 empty stubs skipped, RSS bounded
~5.5 GB, daemon idle at finish. `semfs grep` returned hits inside real `.xlsx`/`.pdf`/`.docx`.
Two caveats remain.

---

## Caveat 1 — `node_modules` (vendored deps) are not skipped on import; required manual stripping

### Problem
The import walk (`collect_file_paths_recursive`) indexes **every** file, including vendored
`node_modules` trees. On `chanpin` that's **760 of 2,128 files** — mostly large minified JS/TS
**`dist/` bundles** (e.g. `node_modules/docx/dist/index.umd.cjs`, 858 KB / 23k lines). These:
- are **distractor noise**, not workspace content (they pollute search results), and
- are **slow/heavy to embed** (one 858 KB bundle → ~700–2,000 chunks). They were the
  deterministic cause of the seed **hang + box-wedge** documented in
  `rcas/2026-06-03-extract-uncapped-utf8-text-path-node-modules-hang.md`.

The E2E only completed after **manually `rm -rf`-ing `node_modules`** from the seed copy.
That manual step is not part of the product; any real workspace with `node_modules` would
hit the same wall.

### Fix
Skip vendored/dependency dirs in `collect_file_paths_recursive` (the same place the existing
`is_macos_noise_path` filter lives). Minimal: skip recursing into any directory named
`node_modules`. Consider a small denylist (`node_modules`, `.git`, `vendor`, `dist` of a
vendored package, `target`, `__pycache__`) — but keep it conservative; `node_modules` alone
removes the observed killers.

### Notes / decisions to settle
- **Index vs mount visibility:** skipping in the walk removes these files from *both* the
  semantic index *and* the materialized VFS view. For a search index that is correct (you
  don't want to retrieve minified library internals). If a use case needs the files *visible*
  (navigable) but *not indexed*, that's a larger change (index-skip without mount-skip) — out
  of scope unless required.
- The `cap_index_content` (1 MiB) backstop is **not** a substitute: the killer bundles are
  <1 MiB, so the cap never bites on them (see the RCA). The skip is the real fix; the cap
  stays as defense-in-depth for genuinely large legit files.

---

## Caveat 2 — ~28% of PDFs are unextractable by the pure-Rust path (need OCR)

### Problem
Of 142 non-empty PDFs in `chanpin`, **102 extracted** and **40 went to the `fs_unindexed`
bucket** (`format=Pdf`). `pdf-extract` (pure-Rust) returns little/no text for **scanned or
image-only or complex-layout PDFs**, and the 45 s extractor timeout routes pathological ones
to unindexed rather than hanging (working as designed — no silent drop). But those 40 PDFs'
content is **not searchable**. This is the same capability gap as the **10 `.jpg` images**
(`format=Jpeg`, unindexed because OCR was disabled in that run) and the **3 legacy `.ppt`**
(`format=Ppt`, descoped) — all need vision/OCR or a heavier engine, which pure-Rust can't do.

### Fix options
- **OCR fallback for image-only PDFs + images:** when `pdf-extract` yields empty/near-empty
  text (or for `.jpg`), render/pass the page image to the **OpenRouter `gpt-4.1-mini` vision**
  path already built in `extract::ocr` (key-gated like L7). Closes both the 40 PDFs and the
  10 images. Reintroduces a per-image network call (not Supermemory) — acceptable, gated.
- **Better PDF engine** for complex-but-text layouts: evaluate alternatives if `pdf-extract`
  quality is the issue rather than scanned-ness (keep pure-Rust posture if possible).
- **At minimum:** surface the per-format unindexed counts (already in `fs_unindexed` /
  `semfs status`) so coverage gaps are measurable, not silent.

### Acceptance (when addressed)
- Image-only PDFs + `.jpg` images are OCR-indexed when `OPENROUTER_API_KEY` is set (and remain
  cleanly accounted as unindexed when it is not).
- A re-seed of `chanpin` with OCR on indexes the 40 PDFs + 10 images (≈ full coverage of
  meaningful content); legacy `.ppt` (×3) remains the only documented gap.

---

## Why it matters
- **Caveat 1** is a correctness/robustness blocker: without it, seeding any workspace with
  `node_modules` hangs (verified to wedge the box). The manual workaround is not shippable.
- **Caveat 2** is a coverage-quality gap: scanned PDFs and images are exactly the kind of
  content benchmark tasks may depend on; they're currently invisible to search.

## Related
- `tickets/local-document-extractors/` (the extractor feature these caveats qualify).
- `rcas/2026-06-03-extract-uncapped-utf8-text-path-node-modules-hang.md` (caveat 1 root cause).
- `rcas/2026-06-03-extract-unbounded-large-doc-hang.md` (the cap/timeout this builds on).
- `tickets/decouple-backends-from-supermemory/` (local-authoritative seeding this enables).
