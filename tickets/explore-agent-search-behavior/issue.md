# Exploration: why does semfs+sqlite take 24 tool calls when Supermemory takes 1? — dissecting agent search behavior on case 289

- **Type:** Exploration / investigation (open-ended — no fix committed yet)
- **Status:** OPEN — framing + 2 of 3 trace artifacts collected; analysis + 3rd trace TBD.
- **Created:** 2026-06-05
- **Component:** agent runtime behavior over `semfs grep` (not a single code file) — codex GPT-5.4 on
  Workspace-Bench case 289 (`"best-selling product"`).
- **Branch context:** `feat/backend-agnostic-store`

## The thing we don't understand yet

The codex E2E on case 289 gave us three very different agents, same task, same model:

| Run | Tokens | Tool calls | Notes |
|---|---:|---:|---|
| plain codex (no semfs) | 143,837 | ? | explores the filesystem itself (ls/grep/read) |
| **semfs+sqlite, RRF fix (config #8)** | **145,696** | **24** | prompt 143,045 + completion 2,651 |
| semfs+Supermemory | 35,763 | **1** | one search, whole-doc return, answer file ranked #1 |

The RRF fix got sqlite from 28 → 24 searches and from *worse-than-plain* to *≈ plain* — **but it's still
24 tool calls vs Supermemory's 1**, and ~4× the tokens. We have a *ranking* explanation (dashboard at
cross-encoder #6, not #1, so codex can't grab it in one shot) — but we've **never actually read the
trace** to see what the agent *does* across those 24 calls. This ticket is about looking.

Note the token shape: **98% of config #8's tokens are prompt (143,045 / 145,696)**, almost none
completion. So the cost is **input** — the agent is *reading* a lot into context, not generating. That
hints the lever may be "what gets fed into context per step," not just "number of searches." Worth
confirming.

## Open questions (exploratory — answer from the traces)

1. **Breakdown of the 24 tool events:** how many are `semfs grep` (searches) vs file reads vs writes vs
   shell/other? Is "24 calls" mostly searching, or searching once then reading many files?
2. **What did codex search for?** Extract every query string. Did it **reformulate / repeat** searches
   (a sign the first results didn't surface the answer), or search once then iterate on reads?
3. **Did it find the dashboard, and when?** Trace the step where the answer file
   (`6-product-sales-analysis-dashboard.xlsx`) first appears in results vs when it's finally read.
   The dashboard at rank #6 — did codex page past it, re-query, or read several wrong files first?
4. **Why 1 search for Supermemory?** Confirm the mechanism: whole-document return + answer file at #1 →
   the agent gets everything in one shot. Is it the rank, the whole-doc payload, or both?
5. **What does plain codex do with no search?** 143,837 tokens, *no* semfs — how does it explore
   (recursive ls? read-everything? its own grep)? Why is it *cheaper* than broken sqlite but ≈ fixed
   sqlite?
6. **Where do the prompt tokens go?** Are large file reads (whole `.xlsx` extractions, the 251 KB docs)
   dominating context? Would smaller/snippet returns cut tokens without hurting the answer?

## Method
Parse each `agent.json` trace, categorize every tool event, extract search queries + read targets, and
build a **side-by-side step timeline** of the three agents (search → result → read → write). Look for the
behavioral divergence that explains 24 vs 1, and whether it's *ranking*, *payload size*, or *agent
strategy*.

## Artifacts (logs)
Collected locally under `tickets/explore-agent-search-behavior/artifacts/`:
- ✅ `plain-codex-289.agent.json` — plain codex, 143,837 tok (696 KB trace).
- ✅ `semfs-sqlite-rrffix-289.agent.json` — config #8, 145,696 tok, 24 tool events (254 KB trace).
- ⏳ `semfs-supermemory-289.agent.json` — **NOT yet collected.** The old Supermemory run shared the
  `SEMFSCodex/289` output path and was **overwritten** by our config-#8 run, so the 35,763/1-search
  trace is gone. **Needs a fresh run** against the cloud `chanpin` Supermemory container (billable;
  requires the cloud container to still be seeded + a valid key), then `scp` the `agent.json` here.

## Why it matters
Tells us *where* the remaining gap to Supermemory actually lives at the **agent** level — is it purely
the reranker rank (#6→#1 would collapse the 24 calls), or also payload size / agent strategy? That
decides whether the next investment is the reranker (per `tickets/rrf-chunk-mass-and-lane-fusion` #2/#3)
or the return-shape (snippet vs whole-doc), or both. Right now we're inferring from ranking numbers;
this is about *reading the actual run*.

## Related
- `tickets/rrf-chunk-mass-and-lane-fusion/` — the retrieval fix; this explores its agent-level effect.
- `rcas/2026-06-04-rrf-chunk-mass-bias-code-lane-pollution.md` — the ranking arc + holistic table.
