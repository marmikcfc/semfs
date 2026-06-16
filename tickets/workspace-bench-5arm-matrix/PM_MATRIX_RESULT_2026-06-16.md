# PM matrix result — 2026-06-16 (OpenRouter `gpt-5.4`)

10 PM cases × 3 arms × n=3 = 90 cells (289 excluded; arm 2/3 share the `nokg` arm with distinct rep
labels fd*/ft*). All on the same new binary (dedup compiled in). `n/a` = no scorable deliverable (≈0%).

## Verdict (accuracy-gated, lexicographic)

| Arm | accuracy | tokens | turns (median) | cost / accuracy-pt |
|---|--:|--:|--:|--:|
| **plain** | **12.6%** | 339K | ~22 | **26.9K** |
| dedup (W5) | 3.8% | 296K | ~37 | 77.9K |
| dedup + turn-brake | 3.0% | 113K | ~19 | 37.7K |

**Plain wins.** semfs arms are cheaper (turn-brake −67% tokens) but carry a **3–4× accuracy regression**
(cheap-wrong-answer). Plain also best on cost/accuracy-pt.

## CORRECTED diagnosis — NOT retrieval accuracy (trace-verified, case 53)

semfs grep **DID surface the answer files** (ranking log: `interaction_document_6/8/10/13` all returned),
but the nokg agent **flailed (31 cmds: 15 grep + 20 find) and produced an EMPTY deliverable**, while plain
found the 4 files by name (`find`, 8 cmds) and wrote all 4. → bottleneck is **agent convergence on the mount
+ task-fit** (case 53 = "reproduce these named files" = a find-by-name job, not semantic-grep), NOT retrieval.
The **turns** column confirms it: dedup/nokg flails to 75–114 turns; turn-brake cuts turns but collapses
accuracy (cut exploration before converging). Dedup (token lever) and ranking (retrieval lever) both miss this.
Caveat: trace classified for case 53 only (n=1 trace) — classify other losses before generalizing.

Absolute accuracy is low (plain 12.6% vs historical 46%) — OpenRouter `gpt-5.4` + this 10-case mix; the
**relative** plain≫semfs is the result, not the absolute level.

## Full per-cell data (accuracy / tokens / turns)

### plain
| case | fp1 | fp2 | fp3 |
|---|---|---|---|
| 15 | 6% / 388K / 30 | 44% / 361K / 32 | 6% / 531K / 34 |
| 44 | 6% / 138K / 16 | 6% / 97K / 16 | 6% / 113K / 20 |
| 45 | 16% / 292K / 24 | 11% / 616K / 44 | 0% / 459K / 32 |
| 53 | 0% / 56K / 8 | 64% / 83K / 12 | 73% / 222K / 14 |
| 55 | 0% / 139K / 10 | 0% / 98K / 10 | 0% / 135K / 14 |
| 95 | 0% / 167K / 24 | 0% / 190K / 20 | 0% / 98K / 18 |
| 171 | 17% / 127K / 16 | 6% / 198K / 22 | 72% / 246K / 28 |
| 175 | 0% / 112K / 14 | 8% / 219K / 22 | 33% / 264K / 20 |
| 386 | 0% / 666K / 42 | 0% / 513K / 30 | 4% / 560K / 38 |
| 388 | 0% / 1361K / 50 | 0% / 459K / 26 | 0% / 1263K / 52 |

### dedup (W5)
| case | fd1 | fd2 | fd3 |
|---|---|---|---|
| 15 | 19% / 648K / 75 | 6% / 1408K / 92 | 44% / 331K / 24 |
| 44 | n/a / 18K / 6 | n/a / 31K / 14 | n/a / 32K / 16 |
| 45 | 0% / 181K / 40 | 5% / 456K / 46 | 11% / 158K / 14 |
| 53 | n/a / 125K / 31 | 9% / 417K / 40 | 0% / 95K / 19 |
| 55 | 0% / 104K / 28 | 0% / 57K / 26 | n/a / 60K / 14 |
| 95 | n/a / 124K / 34 | 0% / 364K / 76 | n/a / 21K / 8 |
| 171 | 0% / 90K / 22 | (no result) | 0% / 176K / 40 |
| 175 | 0% / 323K / 43 | 0% / 239K / 37 | 0% / 322K / 39 |
| 386 | 0% / 222K / 29 | 0% / 242K / 42 | 4% / 580K / 56 |
| 388 | 0% / 1214K / 114 | 0% / 529K / 64 | 0% / 177K / 30 |

### dedup + turn-brake (W5 + p2b)
| case | ft1 | ft2 | ft3 |
|---|---|---|---|
| 15 | 6% / 296K / 20 | 6% / 115K / 18 | 6% / 213K / 16 |
| 44 | n/a / 42K / 16 | n/a / 23K / 14 | n/a / 31K / 16 |
| 45 | 37% / 62K / 20 | 16% / 159K / 19 | 5% / 185K / 14 |
| 53 | 0% / 58K / 15 | 0% / 81K / 21 | 0% / 190K / 24 |
| 55 | 0% / 42K / 12 | 0% / 64K / 16 | 0% / 106K / 5 |
| 95 | 0% / 42K / 17 | 0% / 116K / 25 | n/a / 70K / 19 |
| 171 | 0% / 96K / 22 | 0% / 77K / 24 | 6% / 98K / 26 |
| 175 | n/a / 30K / 9 | 0% / 136K / 31 | 0% / 81K / 25 |
| 386 | 0% / 99K / 16 | 0% / 123K / 19 | 0% / 238K / 41 |
| 388 | 0% / 190K / 28 | 0% / 162K / 31 | 0% / 170K / 29 |
