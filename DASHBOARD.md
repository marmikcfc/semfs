> <!-- STALE-BANNER --> ⚠️ **STALE (2026-06-25)** — point-in-time matrix-dashboard snapshot; superseded by the live dashboard + [CURRENT_STATE.md](CURRENT_STATE.md).

# 7-Arm Matrix Dashboard — 2026-06-19 19:14:35

**98/135 cells done (72%)**  `██████████████░░░░░░`   ·   live sandboxes: 2
**mean tokens/cell: 587,928**   ·   total tokens so far: 31,160,218

| arm | done | ok | timeout | mean tokens |
|---|---|---|---|---|
| 1 · plain | 10/15 | 5 | 5 | 573,706 |
| 2 · compress-only | 10/15 | 4 | 6 | 268,421 |
| 3 · compress+dedup | 10/15 | 4 | 6 | 277,802 |
| 4 · cdp (L7 off) | 10/15 | 3 | 7 | 378,097 |
| 5 · cdp (L7 on) | 10/15 | 4 | 6 | 401,289 |
| 6 · hiddenKG-rerank (L7 off) | 10/15 | 3 | 7 | 296,636 |
| 7 · hiddenKG-rerank (L7 on) | 9/15 | 5 | 4 | 464,246 |
| 8 · hiddenKG-retrieval (L7 off) | 15/15 | 13 | 2 | 714,324 |
| 9 · hiddenKG-retrieval (L7 on) | 14/15 | 12 | 2 | 905,832 |

_Tokens are FRESH (NVFP4 endpoint, no caching — comparable across arms). Model: GLM-5.1-NVFP4. Auto-refreshes every 5 min._
