# Format trap — binary-document extraction delivery

_Created: 2026-06-09. Status: implemented (two delivery mechanisms, both knob-gated);
benchmark comparison in progress. Companion: `CURRENT_STATE.md`, `EXPERIMENTS.md`,
`rcas/2026-06-08-kg-materialization-race-empty-kg-codex-fabrication.md`._

## Problem — the "format trap"

semfs serves binary documents (xlsx/xls/pdf/docx/pptx/ppt/doc) as their **raw bytes**
over FUSE. When an agent (codex/claude) hits one, it can't read the bytes, so it shells
out to `openpyxl`/`pandas`/`libreoffice`/`zipfile` to parse the file itself. That parsing:

- burns tokens (tool output + the agent re-reading its own parse across turns), and
- is pure waste, because semfs **already extracted the text at index time** — it lives in
  the `chunks` table (it's what semantic search runs over).

Measured on case 289 (chanpin-gemma-q4 seed): the format trap was the dominant token sink
— **493K tokens**, with 6 `format_trap` parser invocations in the trace. It is the
`<100K`-tokens lever (graph-fs fixes engagement/turn-count, not this).

## Insight

The extracted text is **already stored** in `chunks` (ordered by `ord` per `filepath`).
No re-extraction is needed — we only have to *deliver* it to the agent on a cheap path
instead of letting the agent re-derive it from raw bytes. Two ways to deliver:

### Mechanism 1 — grep-inline (shipped default)

`semfs grep` stitches all chunks for a binary-extension hit and prints the **full
extracted text inline**, marked `# ^ COMPLETE FILE — use it directly, do not open it`.
The agent never opens the binary.

- Code: `crates/semfs/src/cmd/grep.rs` (`is_binary_ext`, `grep_db`, the inline block);
  `Db::get_extracted_text` in `crates/semfs-core/src/cache/db.rs`.
- Knob: **`SEMFS_GREP_INLINE`** (default `on`; `off|0|false` disables).
- Trade-off: delivers the **whole file every grep** — cheap for small files, but the
  agent can't choose to read just a few lines.
- Storage: **zero duplication** (text already in `chunks`).

### Mechanism 2 — `.extracted.md` siblings (knob, off by default)

On flush, materialize a read-only `<file>.extracted.md` sibling holding the extracted
text, so the agent can `cat`/`head`/`sed` **a few lines on demand** rather than receiving
the whole file inline.

- Code: `Db::upsert_extracted_sibling` (`crates/semfs-core/src/cache/db.rs`), called from
  `SqliteFile::flush` (`crates/semfs-core/src/cache/file.rs`) when the knob is on;
  `.extracted.md` registered in `DERIVED_SIBLING_SUFFIXES`
  (`crates/semfs-core/src/cache/fs.rs`) so unlink/rename reap it.
- Knob: **`SEMFS_EXTRACT_SIBLING`** (default `off`; `on|1|true` enables).
- Trade-off: lets the agent read partial content (fewer tokens *if* it reads selectively),
  but **duplicates** the extracted text into `fs_data` (~11 MB on chanpin).
- Pair with `SEMFS_GREP_INLINE=off` so grep returns the chunk pointer and the agent reads
  the sibling, rather than getting both.

For an already-built seed (no flushes happen on a read-only benchmark mount), siblings are
retrofit via the backfill script (`/tmp/backfill_siblings_fixed.py` on the box — writes the
same row shape: `derived=1`, mode `S_IFREG|0444`, chunks by `chunk_size`). **Run destructive
backfills on a COPY of the db only.**

## Knob matrix

| `SEMFS_GREP_INLINE` | `SEMFS_EXTRACT_SIBLING` | Delivery |
|---|---|---|
| `on` (default) | `off` (default) | grep inlines whole file; no sibling. **Shipped.** |
| `off` | `on` | grep returns pointer; agent `cat`s `.extracted.md` (partial reads). |
| `on` | `on` | both available (redundant; for debugging). |
| `off` | `off` | raw bytes — the unmitigated format trap (baseline only). |

## Results (case 289, Seed-2.0-Lite judge, `agent_eval.py`)

| Config | tokens | format_trap | rubrics |
|---|---:|---:|---:|
| pre-fix (raw bytes) | 493K | 6 | 6/15 |
| grep-inline (default) | 135K | 0 | 4/15 |
| graphfs ON + siblings (inline off) | _pending_ | | |
| graphfs OFF + siblings (inline off) | _pending_ | | |

Token win is large (−73% from grep-inline alone, trap → 0). Accuracy is gated by a separate
issue (codex distrusting the 403 source and fabricating — the honesty rubrics), not by the
delivery mechanism. The sibling-vs-inline comparison tests whether on-demand partial reads
cut tokens further.

## Open / next

- Land the benchmark numbers for the two sibling configs above.
- Decide the default: if siblings don't beat inline on tokens, keep grep-inline (zero
  storage); otherwise consider siblings for large-file corpora.
- The honesty/403 problem is tracked separately (see `case289-retrieval-investigation`).
