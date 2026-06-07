# GOAL — beat pure-codex on case 289, no gaming, < 35K tokens

**Set:** 2026-06-07 (via `/goal`). **Case:** Workspace-Bench 289 (chanpin, codex GPT-5.4).
**Judge:** `bytedance-seed/seed-2.0-lite` (the paper's judge) via OpenRouter (`agent_eval.py`).

## Objective
Beat the **pure-codex** rubric score on case 289 — **without gaming** — by improving the real
levers across all system layers, while keeping **token usage < 35K**.

> No gaming = no hardcoded case strings, no copy-VERBATIM-to-output, no rubric-keyword-tuned
> wording. Only factual metadata + generic agent guidance + genuine retrieval/index improvements.

## Reference numbers (seed-2.0-lite judge unless noted)
| condition | tokens | tool calls | rubrics |
|---|---|---|---|
| pure codex (no semfs) | 108K | 8 | 4–6/15 (bimodal: 4 copy-list, 6 when it discovers the 403) |
| semfs kg_off (current) | ~25K | 2–3 | 4/15 |
| semfs kg_on (current) | ~72K | 7 | 6/15 (over budget) |
| gamey (reverted, not allowed) | ~25K | 2 | 7/15 |
| **realistic ceiling** | — | — | **~10/15** ([5][6] path-convention + [8][9][10] metadata meta-task are structurally unwinnable here) |

## Success criteria
- [ ] rubric score **>** pure codex (target **≥ 7/15**), seed judge
- [x] tokens **< 35K** (kg_off path ≈ 25K)
- [x] no gaming (gamey copy-verbatim reverted, commit 7947f1b)
- [x] no regression: data file still ranks #1 on normal queries (reserve-slot verified)

## The constraint (Theory of Constraints)
Retrieval is solved (`saw403=1` — codex sees the 403). The bottleneck is the **reporting step**:
codex narrates its *tool process* ("grep didn't return data") instead of the *source's status*
("source is 403 / HTML / access-denied"). Rubrics [3][13][14] need the literal facts in the output.

## Levers / hypotheses tracker
| # | lever | layer | status |
|---|---|---|---|
| L1 | integrity lane (surface 403 for codex's real query) | retrieval | ✅ done — `saw403=1` |
| L2 | reserve-slot (errors don't outrank real data) | retrieval | ✅ done — verified |
| H-C | reframe protocol: output IS the result; report source status, not tool process | protocol | 🔄 running (kg_off ~25K) |
| H-A | index-time integrity classification (HTTP-error/corrupt/empty) | extract | ⬜ next if H-C < 7 |
| H-B | graphify "knowledge-gaps" section in KNOWLEDGE_GRAPH.md (keep tiny) | KG artifact | ⬜ next |

## Run log (append per step)
- 2026-06-07: baseline four-condition (claude judge) — plain 5.3, kg_off 4.3, kg_on 4.7, cloud 3.7.
- 2026-06-07: H1 integrity lane → `saw403` 0→1 (kg_on); kg_off 4/15 (RESULT_LIMIT crowding).
- 2026-06-07: reserve-slot → 403 visible without displacing data; gamey 7/15 (reverted).
- 2026-06-07: generic (no gaming) → kg_off 4/15, kg_on 6/15 @ 72K (seed judge).
- 2026-06-07: H-C reframe protocol → kg_off run in flight.
- 2026-06-07: H-C run #1 hit codex stdin bug (1 call, grep=0, saw403=0) → INVALID. Need n>=3.
