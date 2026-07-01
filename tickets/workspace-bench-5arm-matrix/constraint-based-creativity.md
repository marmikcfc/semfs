# Constraint-based ideation record — making semfs win both axes

> Deliverable of the `constraint-based-creativity` skill (2026-06-11). Problem framing from
> [`TOKEN_ECONOMY.md`](TOKEN_ECONOMY.md); winners promoted into
> [`EXPERIMENTS_NEXT.md`](EXPERIMENTS_NEXT.md). Failed ideas kept deliberately — several
> encode lessons (integrity-banner, SEARCH_ONLY) we must not relearn.

## Problem statement

Local semfs matches plain on accuracy (E1) but loses tokens to a behavior cascade
(hint → KG read → os.walk) and run-to-run call-count variance. Best run: 76.8K vs plain
79K (n=1). Goal: a *consistent* (n≥3) both-axes win — or a principled decision to change
arenas. Ideation was stuck on "tune the knobs" (RESULT_LIMIT/DOC_CAP) which E-runs proved
insufficient.

## Active constraints (chosen to break the "tune the knobs" fixation)

- **C1 — The 1KB budget:** at most ~1KB of search output may enter context per call.
  *(counter to: 43KB inline blobs / 300K uncapped greps)*
- **C2 — Zero extra turns:** semfs may not cost the agent a single turn more than plain.
  *(counter to: "search is one more thing to try"; forces search to REPLACE probes)*
- **C3 — No instructions:** the mount must help with NO injected hint at all.
  *(subtraction game; counter to: fixing everything with more hint text)*
- **C4 — Inversion sprint:** design the configuration that MAXIMIZES tokens, then negate it.

## Ideas generated (24; ✗ = rejected, kept for the lesson)

**Under C1 (1KB budget):**
1. Path-first render: ranked paths, 1-line snippet each.
2. Two-tier render: top-1 hit gets ~800B excerpt; hits 2–5 paths only.
3. Per-hit confidence score so the agent knows when to STOP searching.
4. Fold duplicate chunks → one line per *file*, not per chunk.
5. Result-ID + stored full result; follow-up `semfs slice <id> --range/--grep` (Firetiger artifact pattern).
6. Truncation notice BEFORE content + auto-suggested narrower query (Firetiger).
7. Echo the rewritten query (cross-lingual transparency → trust → fewer re-greps).
8. ✗ gzip/base64 the payload — tokenizes worse, model can't read it (already rejected in caveman ticket).
9. ✗ Paths-only with zero snippets — PwC measured codex collapsing 93.1%→55.2% on file-based delivery; too risky as default.

**Under C2 (zero extra turns):**
10. Search+acquisition fused: the grep response carries the top hit's *relevant section*
    (capped) so find+read = 1 turn.
11. Meta-confidence line in the response: "hit #1 contains the answer rows; no further
    search needed."
12. ≤1KB workspace map (top dirs + file-type census + where-things-live) injected at
    mount — discovery starts pre-paid (Aider repo-map pattern).
13. Per-directory 3-line manifest files (lazy tree bootstrap, hermesagent pattern).
14. Hint: "semfs grep REPLACES find/ls/os.walk" with SEARCH_ONLY=off as safety net.
15. ✗ SEARCH_ONLY=on to force it — H2: catastrophic flail, removes the escape loop.

**Under C3 (no instructions):**
16. `.extracted.md` siblings — the proven silent affordance (format trap −80%); extend the
    same move: siblings ARE delivery, no hint needed.
17. Per-dir README.md manifests the agent reads naturally.
18. Zero-hint control arm — measures the mount's intrinsic value (also de-confounds every
    hint A/B).
19. ✗ Sort/rename answer-bearing dirs to surface them — adjacent to the integrity-banner
    cheat; benchmark-gaming, reverted once already.
20. ✗ Dynamic /top-hits/ virtual dir — undiscoverable without a hint, and browsable
    surfaces invite crawling (gfs_on lesson: 471K mean tokens).

**Under C4 (inversion — "maximize tokens" → negate):**
21. Max-token design: command big reads at turn 1; re-read after every search; advertise
    crawlable surfaces; return low-confidence noise → forces re-query. **Negations:**
    defer any big read to the LAST turn; hint "never re-read, never re-grep the same
    terms"; remove crawlable surfaces (KG/by-topic); top-1 precision over recall.
22. Position rule as a product principle: "early-small, late-big" — search results tiny at
    the start, the single full read just before writing.

**Meta (cost constraint: no new code this week):**
23. Re-score existing 25+ runs with cache-adjusted pricing (pure analysis, zero runs).
24. Measure the codex clip layer with controlled dummy outputs (no semfs change at all).

## Top 3 (and why the constraint created them)

1. **The scout stack with two-tier render + stop-signal** (ideas 1,2,3,6,10,11,14):
   C1 forced the question "which ~1KB?" → answer: *the top hit's answer-bearing section,
   plus paths, plus a stop signal*. C2 forced fusing find+read into the first response.
   Without C1/C2 we'd have kept tuning DOC_CAP. → **E8/E9**.
2. **Measure the clip before optimizing the payload** (idea 24): C4's inversion exposed
   that we don't know who controls the bytes today (95_cloud anomaly). → **E6**, runs first.
3. **Workspace map ≤1KB + zero-hint control** (ideas 12,18): C3's subtraction revealed we
   never measured the mount *without* its instructions; the map is the cheapest way to
   delete discovery turns for both plain-like and semfs behavior. → **E13 + control arm in E7**.

## Next steps

Promoted to [`EXPERIMENTS_NEXT.md`](EXPERIMENTS_NEXT.md) as E6–E14 with predictions and
kill conditions.
