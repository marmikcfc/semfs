# LEARNINGS — the token-economy campaign, compiled (2026-06-11)

> The distilled digest of everything this campaign established: E1–E5 (clean-infra reruns),
> the first-principles decomposition, E6 (clip calibration), E7/E8-289 (scout + W′ runs),
> and what shipped. Detail lives in the linked docs; this is the part worth remembering.
> Visual versions: [`hypotheses.html`](hypotheses.html) (5-level case file),
> [`mechanics.html`](mechanics.html) (pseudocode mechanics).

## TL;DR

On grep-friendly ground (1,452 well-named files), local semfs went from **losing 2–5× on
tokens with broken infra** to **−52% mean tokens at accuracy parity** (case 289,
distribution-vs-distribution, all scores re-judged clean). The wins came from physics
(turn count × context re-pay), measurement (codex's hidden output clip), and behavior
design (a hint that commands nothing and a provenance check) — and they now ship as
product defaults (`eae0980`), so a fresh GitHub install gets the winning configuration.

---

## 1. The physics (why anything worked at all)

With `cached_input=0`, every turn re-pays the entire accumulated context:

```
TotalTokens ≈ T·S + Σₛ (T−s)·oₛ + G
```

- **Law I — turn count multiplies everything.** Same config: 2 calls = 21K, 12 calls = 169K.
- **Law II — early big reads are the most expensive reads.** A tool output at turn s is
  re-paid (T−s) times. The old hint commanded a 35.7K read at turn 1 — the worst square
  on the board.
- **Law III — even the winner is wasteful.** ~60–70% of plain's bill is re-paying its own
  turn-1/2 `find` dumps. The floor (search → read → write) is ~17–22K; we hit it twice
  (21,473 / 21,743).

## 2. The hidden layer (E6 — the discovery of the campaign)

codex truncates every tool output **before the model sees it** — found because a winning
run emitted 830K chars but billed only 138K tokens. Measured on codex 0.133.0 with
marker-file probes (the model itself reports which numbered lines survived):

| payload | outcome |
|---|---|
| ≤ ~10 KB | passes whole |
| ~15 KB | boundary truncation notice |
| 49 KB | **gutted to ~1.2K tokens** (head+tail, middle silently gone) |

Consequences: (a) overflow is *catastrophic, not graceful* — "slightly over" loses ~85%;
(b) the grep cap's real value is **choosing which bytes survive**, not saving bytes;
(c) put answer content first; (d) gutted results breed re-query turns — the agent
distrusts shredded output and searches again. A 12KB-ish capped render arrives intact
and the agent can act on it.

## 3. The turn problem — solved half, unsolved half

**Solved (deterministic, shipped, 0 occurrences in 6 runs):**
1. *Commanded waste* — the hint no longer orders any read (the KG-first command cost ~58K/run).
2. *Flail spirals* — `SEARCH_ONLY=off` keeps the file-tree fallback; the 45-min-timeout
   class of failure is extinct.
3. *Blob distrust* — capped, honestly-marked renders (COMPLETE FILE / TRUNCATED) arrive
   intact, so the agent doesn't re-search out of confusion.
4. *Dead-end hunts* — the provenance check makes the agent look at task-named sources
   once, see the 403, report it, and stop hunting for data that doesn't exist.

**Unsolved (the model's own policy):** call count is bimodal — 2 calls when codex trusts
the first result, 9–12 when its verify instinct fires. Same config, same seed, same hint.
We shifted the distribution (plain 7–15 calls → scout 2–12) but cannot pin it. Next lever:
**per-query confidence in the render itself** ("rank-1, high agreement — no further search
needed") — closed-loop instead of a static promise (E9).

## 4. The night's numbers (case 289, all re-judged)

| arm | tokens (each run) | calls | score |
|---|---|---|---|
| plain | 322K(⊘) · 118K · 71.5K — mean 171K | 15/9/7 | ⊘ / 5 / 7 → clean mean 6.0 |
| scout (lean hint) | 21.5K · 169K · 107K | 2/12/9 | 4/15 ×3 |
| **scout + W′** | **94K** · 21.7K · 80.5K — class mean 82K | 9/2/9 | **6/15** / 4 / 5 |

- Token axis: **−52% on means**; our *worst* run beat plain's mean; floor band ×2.
- Accuracy: W′ recovered exactly the two 403-cluster rubrics (rubric-diff verified) →
  plain's clean mean. The gap was never "retrieval quality" — semfs's clean extraction
  *hides corpus corruption*, so the efficient agent never saw the 403s plain trips over.
- Pre-registered honesty: this is the 289 **cell**; E8's ≥3-of-5-cases condition is open.

## 5. Component learnings (each one a design rule)

- **The hint is a probabilistic lever, not control.** Compliance ~2/3 on the provenance
  check; w1 obeyed the economy, w2 ignored it. Prompts *suggest* turns away; the tool must
  make skipping them safe. Corollary: the old hint proves prompts can also *create* waste —
  the biggest hint improvement of the campaign was **deletion**.
- **Never remove the agent's fallback to make your feature look necessary.**
  `SEARCH_ONLY=on` (hide the tree, force search) produced timeouts, give-ups, and one
  confidently fabricated report. `=off` = worst case the agent behaves like plain.
- **Honesty in the delivery surface is load-bearing.** The old render labeled truncated
  content "COMPLETE FILE"; the agent calibrates trust on these labels. Lies → re-verification
  turns. The fix (TRUNCATED + "open the file for the rest") converts paranoia into one
  targeted read.
- **Efficiency can hide ground truth.** Clean extraction concealed the 403 stubs. Provenance
  checking is generic agent hygiene, not benchmark gaming (the product-injected
  "integrity banner" remains the rejected cheat; instructing the *agent* to look is the
  defensible form).
- **The judge is infrastructure too.** It parse-failed, then got 429-rate-limited, and
  p1 (a 322K-trace run) is *reproducibly unjudgeable* — which silently drops plain's worst
  runs and flatters the baseline. Re-judge offline before reading any score; cap judge inputs.
- **Plain's bar is soft.** The canonical "79K @ 6/15" was n=1. Fresh n=3: 71.5–322K,
  ⊘/5/7. Every comparison made against an n=1 bar was mis-calibrated, in *our* disfavor.
- **n=1 lies, in both directions.** The first sub-plain run (76.8K) was real but
  unrepresentative; so was plain's 79K. Distributions or nothing.

## 6. What shipped (and what a fresh install now gets)

Commit `eae0980` (+ docs `cf71a68`, `f228af7`), branch `feat/backend-agnostic-store`:

1. **grep.rs** — `SEMFS_GREP_RESULT_CAP` (default 6KB/hit, inside the measured clip
   window) at all 3 render sites, honest TRUNCATED markers. The documented cap knobs were
   inert on the CLI path before this.
2. **agent_hint.rs** — the validated hint as the DEFAULT render (both home block and
   workspace root): ONE grep → top hit for exact values; COMPLETE/TRUNCATED semantics
   explained; KG referenced but never commanded ("do not read kg/ first"); PROVENANCE
   CHECK paragraph. Fresh imports render it automatically — the box's seed surgery was
   only ever needed because rebuilding an existing seed is ~3h.
3. **cache/file.rs + extract/mod.rs** — dual-store: embed the summary (FIND), materialize
   the raw table in `.extracted.md` (ANSWER).
4. **daemon_runtime.rs** — L7 entity extraction gated on `SEMFS_KG`, not key presence.

Tests: semfs-core 326, semfs 51, green (one known env-race flake, pre-existing).

## 7. Opinions scoreboard — credences updated on tonight's evidence

| # | opinion | before | after | what moved it |
|---|---|---|---|---|
| O1 | delivery form = #1 token lever | 85% | **85%** | cap validated, but clip discovery shows part of the win is behavioral → split with O2 |
| O2 | the 2KB hint outweighs the pipeline | 80% | **75%** | hint works but compliance ~2/3; it's a distribution-shifter, not a switch |
| O3 | scout beats plain both-axes ≥3/5 cases | 60% | **70%** | the 289 cell won decisively; 4 cases still unrun |
| O4 | summaries = accuracy-only, needs new cases | 75% | **75%** | untouched tonight |
| O7 | KG surfaces net-negative for codex-class | 85% | **85%** | 0 KG reads in all wins; digest arm still untested |
| O8 | WB is the wrong arena | 70% | **60%** | semfs just won both axes *on hostile ground* — the arena complaint weakened |

## 8. Next queue (in leverage order)

1. **E9 — stop-signal / two-tier render** (+ global render budget: per-hit cap can sum
   past the clip, 5×6KB > 15KB). Attacks the only unsolved turn source.
2. **Hint-position fix** — provenance paragraph above the search instructions (compliance).
3. **E8 full matrix** — cases 15/44/95/175 × n≥3 against the pre-registered ≥3/5 condition.
4. **E11 — discovery-stressed + cross-lingual cases** — the arena where retrieval can win
   accuracy outright; the only valid summary test (dual-store is ready).
5. **Judge hardening** — input caps so 300K-trace runs stop being unjudgeable.

## Doc index

[`RUN_MANIFEST.md`](RUN_MANIFEST.md) (provenance — read first) · [`ANALYSIS.md`](ANALYSIS.md)
(H1–H5 design) · [`RESULTS.md`](RESULTS.md) (E1–E5 + tonight's addendum) ·
[`TOKEN_ECONOMY.md`](TOKEN_ECONOMY.md) (cost equation + five-whys) ·
[`RESEARCH_NOTES_EXTERNAL.md`](RESEARCH_NOTES_EXTERNAL.md) (field survey) ·
[`OPINIONS.md`](OPINIONS.md) (full credence audits) · [`EXPERIMENTS_NEXT.md`](EXPERIMENTS_NEXT.md)
(E6–E15 specs) · [`constraint-based-creativity.md`](constraint-based-creativity.md)
(ideation record) · RCAs: `rcas/2026-06-11-*.md`
