# RCA: summary seed drops `.extracted.md` → agent can't read xlsx (case 44 = 0/16)

**Date:** 2026-06-11
**Symptom:** In the E3 summary-vs-raw A/B, case 44 (consolidate 120 dev tasks from 3
xlsx → HTML dashboard) scored **0/16 on the summary arm** (16K tokens, 3 tool-calls,
status=failed) vs **4/16 on the raw arm**. The agent's own final message: *"源文件
development-task-list.xlsx … 在当前可访问工作目录中未找到"* ("source files not found in
the accessible working directory").

## Execution path / data flow
The agent (codex) cannot parse a raw binary `.xlsx`; it reads the **`.extracted.md`**
sibling (the table rendered as markdown). Retrieval finds the file via search chunks;
the agent then opens `<file>.xlsx.extracted.md` to read the actual rows.

- `chanpin-clean.db` (raw arm): **201** `*.xlsx.extracted.md` siblings present.
- `chanpin-sum.db` (summary arm): only **35** present — **166 missing**, including all 3
  of case 44's dev-task files. The `.xlsx` binaries and summary search-chunks exist; the
  readable `.extracted.md` does not.

## Root cause (two code facts)
1. **`.extracted.md` is materialized only when `SEMFS_EXTRACT_SIBLING=on`.**
   `crates/semfs-core/src/cache/file.rs:30` — `extract_sibling_enabled()` defaults to
   **false**. The shipped delivery path inlines extracted text in `semfs grep` instead.
2. **`.extracted.md` is a "derived sibling" reaped when its source is unlinked.**
   `crates/semfs-core/src/cache/fs.rs:22-24` — it's in the derived-sibling list so
   "unlink/rename of the source file also reaps the derived sibling."

The summary-seed build (`/tmp/build_sumseed.sh` on the box, + the resume passes
`resume_sumseed.sh`/`resume_sumseed2.sh`) did:
- **Step 2:** mount `--no-import`, `find /tmp/summnt -name '*.xlsx' -delete`. Each FUSE
  unlink **reaped that xlsx's `.extracted.md`** (fact 2). → all readable tables gone.
- **Step 4:** re-mount with import ON to re-embed xlsx as gpt-4.1-mini summaries — but
  the env set OPENROUTER_API_KEY / embedder / KG flags and **omitted
  `SEMFS_EXTRACT_SIBLING=on`** (fact 1). → summaries written, **no `.extracted.md`
  regenerated**.

`chanpin-clean` has the siblings because its earlier (2026-06-09 format-trap) backfill
ran with the flag on; the summary build silently dropped it.

## Why it invalidated the A/B
The summary arm fails *every* xlsx-dependent case on **unreadable tables**, not on
summary quality. The 44-sum "16K tokens / 0-of-16" is the agent giving up early, not a
token win. No valid summary-vs-raw signal was produced.

## Fix
Rebuild the summary seed so re-import generates **both** the summary chunks **and** the
`.extracted.md` siblings: set **`SEMFS_EXTRACT_SIBLING=on`** in the re-import env. Either
(a) re-run the xlsx re-import once more with the flag on (regenerates `.extracted.md`
for the re-imported files), or (b) backfill the 166 missing `.extracted.md` from
`chanpin-clean` (which already has all 201). Then the summary seed = summary embeddings
(FIND) + `.extracted.md` (ANSWER), and the A/B becomes valid.

## Deeper root cause (found while applying the fix): the table is discarded at build time
Setting `SEMFS_EXTRACT_SIBLING=on` and re-importing the 3 case-44 files worked
*mechanically* — `md=3 ch=3 mdch=0, quickcheck=ok`: each file got a `.extracted.md`, the
summary chunk survived, and the sibling was NOT re-indexed (no raw-chunk pollution). BUT
the `.extracted.md` content is the **summary** (687 B), not the 120-row table.

`crates/semfs-core/src/extract/summary.rs::build_content` (lines 162-186) maps each sheet
to `summarize(...).unwrap_or_else(|| s.text.clone())` — i.e. **summary OR raw cells, never
both**. Its doc is explicit: *"When a summary exists we index ONLY the summary… with
summaries on, the raw table is no longer in the index… Preserving table-return (a separate
non-embedded store) is a follow-up; this build is for measuring retrieval/rerank of the
summary-only representation."* The module header's *"raw table is still returned verbatim"*
/ *"weave it ahead of the raw cells"* describes the INTENDED end state, never implemented.

So `SEMFS_EXTRACT_SIBLING=on` materialises whatever `build_content` stored — the summary —
not the table. The flag is necessary but **not sufficient**: the table was thrown away at
summary-build time. "Summary FINDS, table ANSWERS" is half-built — FINDS works, ANSWERS
does not exist. Summaries can only help cases the summary itself answers; consolidation /
extraction / computation cases (44, 289, most of WB) need the raw rows and cannot be
served by the current summary seed.

## The real fix (two options)
- **Weave** (matches the header): `build_content` returns `summary + "\n" + raw_cells`
  (1-line change). Simple; embedding then includes number-noise again, diluting the
  retrieval gain summaries exist for.
- **Dual-store** (the NOTE's plan): plumb a separate `embed_text` (summary) vs
  `content_text` (raw table) through the indexer so search runs on the summary while
  read/return hands back the table. Clean; larger change.

## Resolution — dual-store implemented (2026-06-11)
Chose **dual-store** (embed summary, return raw table). Change (2 hunks):
- `crates/semfs-core/src/extract/mod.rs`: new `pub fn raw_table_for_sibling(bytes)` —
  re-extracts the spreadsheet's flattened cells (None for non-spreadsheets).
- `crates/semfs-core/src/cache/file.rs`: at the `.extracted.md` materialize site, use
  `raw_table_for_sibling(&bytes).unwrap_or_else(|| text)` for the sibling while
  `indexer.index(.., &text)` still embeds the summary. So FIND uses the summary vector;
  ANSWER reads the raw table. No-op for the raw/no-key path (there `text` already IS raw).

Verified on case 44's 3 dev-task files after rebuild + re-import with `SEMFS_EXTRACT_SIBLING=on`:
- summary chunk = the gpt-4.1-mini prose (embedded); `.extracted.md` = the **raw table**
  (6831/8862/4450 B, `Task ID  Feature  Priority  Assignee …`), not the 687 B summary;
  `mdch=0` (sibling not re-indexed → no raw-chunk pollution); `quick_check=ok`.

Result (n=1): `44-sum dual-store` went 0/16 (broken) → **2/16, passed, 125K tok** — the
agent now reads the tables and builds a real dashboard. `44-raw` = 4/16, 130K. The 2-pt
gap is two tech-stack rubrics; both pass {generated, 120 tasks} and both fail the demanding
spec rubrics (Chart.js 4.4.0, 5 named charts, exact counts) — n=1 dashboard-construction
noise on a low-ceiling case, NOT a retrieval regression (both read identical tables).

**Why summaries can't help case 44 structurally:** the task *names* its 3 source files, so
the agent never needs semantic retrieval to find them — summary-augmented retrieval has no
lever to pull. Summaries help only when the agent must SEARCH for the right data file among
many. None of the 5 WB cases cleanly fit (44 names files; 289 = 403 source data; 95 = txt;
15 needs a missing file + low ceiling; 175 = csv). The dual-store fix is still necessary
for any future summary test to be valid.

## Lessons
- "Summary FINDS, table ANSWERS" requires the **table** to survive the rebuild — but here
  it never even reached the seed: `build_content` stored summary-only by design until the
  dual-store fix routed the raw table to the `.extracted.md` sibling.
- A single missing env var (`SEMFS_EXTRACT_SIBLING=on`) AND a code-level table-discard
  combined to produce a seed that passes `quick_check`, has correct chunk counts and real
  summaries, yet is unusable by the agent. Verify **read access to the actual answer
  content** (open the file, see the rows), not just index/chunk health, before trusting a
  seed.
