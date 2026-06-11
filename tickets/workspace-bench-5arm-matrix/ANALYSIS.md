# Why plain wins — systems analysis + 5 hypotheses / 5 experiments

> Companion to [`HANDOFF.md`](HANDOFF.md) / [`issue.md`](issue.md). Produced 2026-06-10 from a full
> read of the 25 traces in `artifacts/run5arm/`. Method: systems decomposition + falsifiable
> hypotheses (scientific method). **Headline correction: the local arms did not measure retrieval
> quality — they measured agent behavior under infrastructure failure.**

---

## 1. New evidence from the traces (changes the HANDOFF story)

| # | Finding | Evidence |
|---|---------|----------|
| F1 | **Local search infra was broken during the run.** 13/15 local-arm runs hit `database disk image is malformed` (6 runs) or 50s search timeouts → "falling back to cloud search" → 0 results (7 runs). plain/cloud: zero such errors. | `grep -c 'timed out after\|disk image is malformed' */output/raw/codex_stdout.jsonl` |
| F2 | **Retrieval often SUCCEEDED; delivery/behavior failed.** 175_nokg's first query returned the exact 5 `Depreciation_Breakdown_2024_*.csv` paths plain used — the agent **never opened them** (follow-up queries timed out; it wrote an empty template, 0/12). | 175_nokg trace |
| F3 | **`SEARCH_ONLY=on` removes the only fallback.** When search degraded, `find`/`os.walk` returned only `AGENTS.md`/`CLAUDE.md`/`model_output` — the agent literally could not do what plain does. Outcomes: honest give-up (95_gfs_off, 0/12), fabrication (95_nokg wrote a report about patent-infringement insurance), or empty template (175). | 95/175 local traces |
| F4 | **The "clean" canonical seed is contaminated.** Every local arm dir has the same 6 baked-in `.semfs-error.txt` files (HTTP 402 "SuperRAG text limit reached — out of credits", triple-stacked suffixes) in `model_output/` — leftovers from an earlier cloud-ingest session. 95_nokg listed `.txt` files, found ONLY these, and concluded the workspace had no data. Also proves the local write/ingest path was once wired to cloud SuperRAG. | `*/output/output/*.semfs-error.txt` (6 per local arm) |
| F5 | **Token blowup = delivery form × no prompt cache.** `cached_input=0` ⇒ every turn re-pays the whole context. Inline grep blobs run ~43KB/query, `kg/KNOWLEDGE_GRAPH.md` is 61KB, `/by-topic/` invites an openpyxl-dump crawl (15_gfs_on: 666K tokens). plain's reads: 7–40KB total. | run5arm traces; pm_results_5arm.jsonl |
| F6 | **The hint fights the deliverable.** "The excerpt IS the content — trust it; do not re-open or crawl to verify" — but rubrics demand exact cell values / columns / sheet names, which require full file reads. Cloud won case 95 *because* its inline excerpts were summary-quality; local raw chunks are not, and the hint forbids the recovery move (open the file). | hint text in CLAUDE.md (mount-injected); 175 judge diff |
| F7 | Correction to the quick stat "gfs_off ran only `cd`": those were compound `cd <workdir> && semfs grep … && find …` commands (parser artifact). gfs_off actually searched ("no results") and probed `find` (empty due to F3), then honestly reported failure. | 95_gfs_off trace |

**Why plain wins, in one sentence:** the corpus is small (1452 files) with filename-semantic ground
truth in tiny clean txt/csv, so `find | grep -Ei '折旧|depreci|…'` + windowed `sed` reads is a
near-perfect, ~zero-cost retriever with deterministic feedback — while the semfs arms paid a broken
search daemon, a hidden file tree, blob-sized deliveries, and a hint that blocked recovery.

---

## 2. System decomposition

```
┌─────────┐   ┌───────────────── semfs stack ─────────────────────┐   ┌────────┐
│  TASK   │   │ INDEX            SEARCH             DELIVERY      │   │ AGENT  │
│ prompt  │   │ local: gemma-q4  daemon: rewrite→   FUSE mount:   │   │ codex  │
│ (zh/en) │──▶│  raw chunks,     vec+FTS RRF;       GREP_INLINE   │──▶│ GPT-5.4│
│         │   │  sqlite+vec0     50s timeout →      blobs ~43KB;  │   │ 1 turn │
│         │   │ cloud: SM w/     cloud fallback     SEARCH_ONLY   │   │ cache=0│
│         │   │  .extracted.md   (=0 results)       hides tree;   │   └───┬────┘
│         │   │  74% coverage                       kg/ 61KB;     │       │
└─────────┘   │                                     by-topic/ ;   │   ┌───▼────┐
              │                                     hint (CLAUDE. │   │ JUDGE  │
   plain arm bypasses ALL of this: find/grep/sed    md, "trust    │   │ Seed-  │
   directly on the real tree                        excerpt")     │   │ 2.0    │
              └────────────────────────────────────────────────────┘   └────────┘
```

### Feedback loops

- **R1 retry spiral (reinforcing, local arms):** search fails/times out → agent retries query
  variants → each turn re-pays full context (cache=0) → 250–700K tokens (95_nokg: 23 calls, 700K).
- **R2 desperation (reinforcing):** no results + "must deliver" → fabricate (95_nokg) or empty
  template (175) → 0 score.
- **B1 plain's balancing loop:** `find` gives exact, instant, deterministic feedback (file exists
  or not) → converges in 5–14 calls. **`SEARCH_ONLY=on` deletes this loop from semfs arms** — the
  system has no escape path when its primary loop (search) degrades. Single point of failure.
- **Emergent:** none of {broken db, hidden tree, trust-the-excerpt hint, contaminated
  model_output} alone is fatal; together they produce 0-scores and 5× tokens.

### Leverage points (Meadows ordering, weakest→strongest)
parameters (RESULT_LIMIT, snippet size) < add fallback loop (SEARCH_ONLY=off) < information quality
(summaries in the index) < rules (hint: "open the top hits") < goal ("search replaces *discovery*
turns, not *reading*").

---

## 3. Five hypotheses (falsifiable) → five experiments

Target: an arm that beats **plain on tokens** (<89K mean) AND **plain+cloud on accuracy** (>46%),
no integrity-banner-style hacks. Protocol for ALL experiments: n≥2 repeats per cell (HANDOFF #2);
pre-flight health gate (PRAGMA integrity_check + 2s smoke search, abort run if either fails);
seed copy cleaned of `model_output/` contamination via daemon unlink (never raw SQL).

| # | Hypothesis | Prediction if TRUE | Falsified if |
|---|------------|--------------------|--------------|
| H1 | ≥half the local accuracy deficit is **infra failure** (F1), not retrieval quality | With healthy db + no cloud fallback, nokg scores >0 on the cases where search already returned correct paths (175, 289) | Scores stay ≈0 with verified-healthy search |
| H2 | **`SEARCH_ONLY=on` causes the catastrophic 0s** (F3) | With it off, no semfs case scores below plain−1 rubric; flail-blowups (95/175 nokg) vanish | 0/12s persist despite visible tree |
| H3 | **Index quality (summaries) is the accuracy lever** (HANDOFF finding + F6): local `.extracted.md` summaries replicate cloud's case-95 win at 100% coverage | local+summaries gets ≥11/12 on 95 in ≤8 calls AND >0 on 175 (where cloud's coverage gap gave 0) | 95 stays ≤6/12 with summaries indexed |
| H4 | **Token cost is delivery form, not search itself** (F5): compact path-first results + "read the top hits" hint beat plain's 4–8 discovery probes | Search collapses discovery to 1–2 calls → tokens < plain on ≥3/5 cases at equal accuracy | Compact arm still >plain tokens at same accuracy |
| H5 | **KG pays only as a ≤4KB orientation digest** (F5): the 61KB graph file and 37-topic by-topic crawl are net-negative | kg-digest arm ≤ nokg tokens and ≥ nokg accuracy; current-KG arm worse than both | Digest shows no turn/token reduction vs nokg → drop KG from this bench (HANDOFF #3) |

### Experiments

- **E1 — Infra-clean rerun (validates everything else; run FIRST).**
  Rebuild `chanpin-matrix.db` from canonical, health-gate it, disable the degraded cloud-fallback
  path (it converts a slow search into a silent 0-result lie), unlink the 6 contaminated
  `model_output/` files. Rerun `nokg` 5 cases ×2. *Decides H1. Cost: ~1 evening, no code.*
- **E2 — `SEMFS_SEARCH_ONLY=off` A/B (HANDOFF backlog #0).**
  Same healthy seed, nokg ± SEARCH_ONLY. *Decides H2. This is the "never lose catastrophically"
  guarantee: semfs becomes strictly additive — worst case the agent behaves like plain.*
- **E3 — local+summaries arm (HANDOFF backlog #1, highest accuracy upside).**
  Weave per-doc/per-sheet summaries into the local seed at the extract layer (mechanism exists —
  see `tickets/summary-augmented-table-retrieval/`), reindex, rerun. *Decides H3. This is the only
  lever with a demonstrated accuracy win (cloud 12/12 on 95 in 6 calls).*
- **E4 — Delivery-form A/B (token win).**
  Arm A: current 43KB inline blobs + "trust the excerpt". Arm B: RESULT_LIMIT=5, snippets capped
  ~300 chars, ranked paths first, hint rewritten to "**open the top 1–2 hits with normal reads**;
  the snippet tells you WHICH file, the file is the source of truth" (kills the F6 contradiction).
  *Decides H4. Search should replace plain's discovery probes, not its reads.*
- **E5 — KG ablation ladder.**
  (a) nokg, (b) +kg-digest (KNOWLEDGE_GRAPH.md capped to ~4KB topic→dir table + inaccessible-file
  list, no graph.json hint, no by-topic), (c) current gfs_off. *Decides H5; settles HANDOFF #3
  with data instead of vibes.*

### The winning stack (if H1–H4 hold)

`healthy infra (E1) + SEARCH_ONLY=off (E2) + summaries index (E3) + compact read-after-search
delivery (E4)`, KG per E5's verdict. Expected mechanics vs plain: 1 search call replaces 4–8 find
probes (−turns −tokens, since cache=0 makes turns the cost driver); summary-quality index gives
synthesis accuracy plain lacks (95-style) at 100% coverage (beats cloud's 74%). Final confirmation:
3-arm matrix (plain / cloud / semfs-stack) ×3 repeats before quoting numbers.

---

## 4. Caveats
- n=1 per cell everywhere above; treat per-cell numbers as ordinal until E-runs repeat them.
- Judge saw contaminated `model_output/` files in local arms (F4) — minor score noise possible.
- Cases 15/44 have structural-rubric ceilings (<max achievable for all arms) — use 95/175/289 as
  the discriminating set.
