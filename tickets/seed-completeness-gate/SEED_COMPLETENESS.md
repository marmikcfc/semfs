# Seed completeness — chanpin-gemma-q4 is COMPLETE (98.2% real-content reachable)

**Date:** 2026-06-15 · **Verified by:** `semfs seed-verify` (E2E, real 690 MB seed) + Modal in-place SQL.
**Verdict:** **COMPLETE.** The "half-warm / 28% / 47–53% indexed" story was a **measurement artifact**, not a real gap. Decision (this session): **record-only, no seed surgery** — the seed is left untouched; this file is the durable proof of its state.

---

## TL;DR

```
chanpin-gemma-q4.db  —  what's actually in it
────────────────────────────────────────────────────────────────
non-empty ORIGINAL corpus files : 627
  reachable by `semfs grep`     : 616   ← 98.2%  ✓ (indexed directly)
  genuinely UNREACHABLE         : 11    ← 8 real docs + 1 image + 2 build artifacts
```

The corpus (`benchmarks/e2b/assets/chanpin_standard`, 1452 files / 547 MB, downloaded from
Modal `semfs-bench-data/corpus/chanpin_standard`) is **85% binary documents**
(456 .docx + 381 .xlsx + 248 .pdf + 72 .pptx). semfs **does** extract them into real,
searchable chunks (sampled: `enterprise_customer_CRM_2026Q1.xlsx` → real CRM rows;
`NPS_survey_data_2026-02.xlsx` → 15 chunks of real survey data). The seed is not blind.

## Why the old "28%" number was wrong

The naive metric `indexed / imported = 700 / 2503 = 28%` had a **polluted denominator**.
Three things in `fs_inode` are not "missing content":

| inflator | count | what it really is |
|---|---:|---|
| empty WB placeholder files (`size==0`) | 747 | nothing to index — WB ships empty placeholders |
| `.semfs-error.txt` stale stubs | 716 | leftover from a failed extraction *pass*; the source doc is indexed anyway |
| `.extracted.md` sidecars | 412 | extraction *output*; content is already chunked under the **original** file's inode |

```
naive view (WRONG)                  honest view (seed-verify)
──────────────────                  ─────────────────────────
imported  2503                      content files (non-empty, non-sidecar)  627
indexed    700  → "28%"             reachable by grep                       616  → 98.2%
```

The 716 `.semfs-error.txt` stubs were the scariest-looking inflator. Direct check: **all 716
have NON-EMPTY source files, yet 616/627 of those originals are indexed** — i.e. extraction
succeeded on a later pass and the error stub is stale noise. They are not gaps.

## The 11 genuinely-unreachable files (the `--allow-unindexed 11` allowance)

| bytes | why | file |
|---:|---|---|
| 6,060,138 | non-corpus build artifact | `graph.json` (KG dump) |
| 983,040 | extract-FAILED | `P020251128616212191150.pdf` |
| 460,800 | extract-FAILED | `2024_annual_okrsummary_retrospective_report.pptx` |
| 433,152 | extract-FAILED | `competitor_experience_evaluation_summary_report_2025h1.pptx` |
| 393,216 | extract-FAILED | `P020240711534708580017.pdf` |
| 302,080 | extract-FAILED | `..._low_price_card_first_screen_traffic_method.xls` (legacy OLE) |
| 286,634 | image — no text | `1577.jpg` |
| 129,024 | extract-FAILED | `_activity_plan_..._campaign_plan.ppt` (legacy OLE) |
| 65,536 | extract-FAILED | `%E5%9B%A0...%E8%AE%BE%E8%AE%A.pdf` (URL-encoded CJK name) |
| 2,410 | non-corpus build artifact | `GRAPH_REPORT.md` |
| 1,607 | non-corpus | `CLAUDE.md` (instructions, not corpus) |

**Real corpus content loss = 8 files / 627 = 1.3%**: 4 PDFs, 2 PPTX, 1 legacy `.xls`, 1 legacy
`.ppt`. Plus 1 image (inherently non-extractable) and 3 non-corpus artifacts. The legacy
OLE (`.ppt`/`.xls`) and the 2 modern `.pptx` fails are the only candidates worth an extractor
fix later; everything else is expected.

## Implication for the benchmark results

The `EXPERIMENT_REPORT.md` caveat "half-warm gemma seed" should be **dropped**. The semfs
arms ran on a **98.2%-complete** index, so the accuracy results (nokg > plain on both agents)
stand without a coverage asterisk. The semfs handicap on the 0-for-all cases is *not* a seed
gap — it's task difficulty / retrieval, as already concluded.

## How to reproduce

```bash
# 1. seed + corpus are on Modal volume semfs-bench-data; pulled to (gitignored) assets:
modal volume get semfs-bench-data /seeds/chanpin-gemma-q4.db benchmarks/e2b/assets/chanpin-gemma-q4.db
# 2. the gate (built from crates/semfs/src/cmd/seed_verify.rs):
semfs seed-verify benchmarks/e2b/assets/chanpin-gemma-q4.db                 # INCOMPLETE, exit 1 (allow=0)
semfs seed-verify benchmarks/e2b/assets/chanpin-gemma-q4.db --allow-unindexed 11   # COMPLETE, exit 0
# in-place on Modal (no download): see benchmarks/modal/inspect_seed.py
modal run benchmarks/modal/inspect_seed.py::coverage --db chanpin-gemma-q4.db
```

`seed-verify` accounting = non-empty regular files that are **not** semfs sidecars
(`.extracted.md` / `.semfs-error.txt`); reachable = own inode chunked OR `<name>.extracted.md`
sibling chunked. It does **not** trust `fs_unindexed` (held 8 of 716 real fails).
