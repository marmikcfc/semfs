# Token & Accuracy Attribution — 9-Arm WB Matrix (GLM-5.1-NVFP4, no prefix cache)

**Scope:** 140 in-scope NVFP4 cells across 9 logical arms × 5 cases (15, 44, 53, 95, 175).
Of these, **34 are infra failures** (`*2` second-rep, 1500 s wall, 0 calls, 0 tokens — uniformly the `ra*2` reps) and are excluded from token/accuracy means; **106 ran**, **89 produced clean token+turn data** (`turns>0, prompt>0`). Data pipeline: `_token_attrib/aggregate.py` → `cells.json`; analysis `_token_attrib/analyze.py`.

**Cost lens (verified):** the vLLM endpoint has `cache_read == 0` in every cell, so every turn re-prefills the entire prompt. Across all valid cells, **completion_tokens = 1.6 % of prompt_tokens** (median per-cell ratio 0.027). Prompt re-prefill IS the cost; completion is noise. Therefore total prompt cost ≈ Σ_turns(context size at that turn), and an output produced at turn *s* is re-paid `(T−s)` more times.

---

## (a) Per-arm token & accuracy table (clean valid cells; medians resist the outliers)

| Arm | nV | med prompt_tok | med compl | med turns | med #grep | med #reads | mean acc | best acc |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 plain | 10 | 279,280 | 9,755 | 24 | 0 | 7 | 0.125 | 0.91 |
| 2 compress | 9 | 306,463 | 9,145 | 19 | 3 | 8 | 0.116 | 0.45 |
| 3 comp+dedup | 9 | 354,505 | 9,395 | 20 | 3 | 7 | 0.178 | 0.64 |
| 4 best | 8 | 314,112 | 9,384 | 33 | 12 | 4 | 0.222 | 0.82 |
| 5 hkg-edges | 9 | 273,146 | 8,679 | 18 | 6 | 5 | 0.196 | 0.91 |
| 6 hkg-rerank | 8 | 269,569 | 6,992 | 17 | 5 | 4 | 0.034 | 0.08 |
| 7 hkg-l7 | 9 | 316,113 | 5,700 | 18 | 8 | 4 | 0.114 | 0.45 |
| 8 hkg-retrieval | 13 | **750,305** | 9,543 | 23 | 7 | 4 | 0.089 | 0.45 |
| 9 hkg-retrieval-l7 | 14 | **906,626** | 9,569 | 27 | 10 | 6 | 0.101 | 0.91 |

**Headline:** The hypothesis "semfs reduces tokens vs plain" does NOT hold at the median. Plain (279K), compress (306K), best (314K) and the edge/rerank KG arms (269–316K) are all in the same band. **Only the two KG-*retrieval* arms (8, 9) move the needle — and they move it the wrong way: +170 % and +225 % tokens vs plain**, with no accuracy gain (0.09 / 0.10 vs plain's 0.13). Accuracy is low everywhere (best mean = 0.22 for arm 4).

---

## (b) Dominant token sink(s) — quantified

### Sink #1 — TURNS (over-exploration), amplified by re-prefill. This is the master variable.
`corr(turns, prompt_tokens) = +0.72`. Because there is no cache, prompt grows roughly quadratically in turns (each new turn re-prefills all prior outputs). The worst cells are pure turn-spirals:

- `pm_codex_95_nokg_rrb1d`: **170 turns → 3.80 M prompt tokens.** Top re-prefill contributor is one `find … -type d` (21,922 chars) re-paid 116× = 2.54 M weighted chars; the rest is ~27 semfs-grep renders (~13 K chars each) each re-paid ~150×. No single giant output — it's *many medium outputs × a huge turn count*.
- `pm_codex_95_hiddenkg_retrieval_l7_ra893`: **101 turns → 2.60 M tokens**, 34 grep calls.
- `pm_codex_175_hiddenkg_retrieval_ra891`: 40 turns → 1.76 M.

### Sink #2 — one unbounded plain `cat` (the single largest token event in the matrix).
- `pm_codex_95_plain_ra1p1`: at turn 54 the agent ran `cat …/project_release_management/…` → **289,881 chars (~72 K tokens) in ONE output**, then re-prefilled 55× = **15.9 M weighted chars (~6.2 M tokens of re-prefill from one read).** Per-output, **semfs grep is bounded (mean 7.1 K chars, max 17.3 K) while plain `cat` is unbounded (max 289.9 K, 17× larger).** semfs's compression caps the *size* lever; it does nothing for the *turns* lever.

### Sink #3 — KG retrieval inflates per-turn context (the arm-8/9 penalty).
Within-case, controlling for difficulty, the retrieval-L7 arm costs more tokens than `best` at the *same* turn count → the extra is per-turn size, i.e. the injected candidate pool:

| case | best turns / tok | retrieval-L7 turns / tok |
|---|---|---|
| 175 | 34 / 548K | 31 / **1,058K** |
| 95 | 76 / 1,432K | 65 / **2,163K** |

Ranking logs confirm the injection is large and noisy: `candidate_count` **mean 60.5, median 69, max 80** per query, re-injected on every grep (up to 27 injection events in one cell). The matched entities are dominated by spurious hits — e.g. case-175 (fixed-asset depreciation) matched `"Nordea Asset Management"`, `"Point72 Asset Management"`, `"ABCI Asset Management Ltd"`, `"Xcode Asset Catalog"` and injected investor-relations summaries — irrelevant files that enlarge context and bait further exploration.

### Token concentration
Total valid prompt_tokens = **50.9 M**. **Case 95 alone = 45 %** (16 of 89 cells). **Top-10 cells = 42 %.** The cost is not spread across the matrix; it is a handful of runaway cells.

---

## (c) Where tokens & accuracy go, per case (difficulty dominates arm)

| case | n(turns>0) | med turns | med prompt | mean acc | best acc (arm) | total grep calls | repeat-query % | character |
|---|---:|---:|---:|---:|---|---:|---:|---|
| 53 | 20 | 12 | 92K | **0.426** | 0.91 (5 hkg-edges, 89K/8turns) | 33 | 6 % | **solvable** — fast, cheap, accurate |
| 15 | — | 18 | 229K | 0.062 | 0.06 | 75 | 0 % | xlsx-synthesis ceiling: best passes 1/16 rubrics |
| 44 | — | 23 | (mean 302K) | 0.043 | 0.19 | 267 | 2 % | heavy thrash, low acc |
| 175 | — | 22 | 306K | 0.042 | 0.25 | 249 | 2 % | heavy thrash, low acc |
| 95 | 20 | **71** | **963K** | **0.000** | 0.00 | **356** | **11 %** | **black hole** — every arm, every cell = 0 acc |

**Case 95 mechanism (deep-read of `pm_codex_95_hiddenkg_retrieval_l7_ra893`):** the task is a **named-file lookup** — "Based on `description_9.txt` and `description_12.txt` … combine `description_15.txt` with `description_22.txt`." In a semantic index, `semfs grep "description_9"` returns ranked *excerpts*, never a guaranteed pin on the exact file. The agent issued **34 greps / 28 distinct queries**, thrashing through reformulations (`description` → `description_9` → `description_9.txt` → `description_9 description_12 version` → `V1.2 V2.4 V5.15 …`) and never converged. 71-turn median × ~13K-char renders × re-prefill = ~1 M tokens for 0 accuracy. **Semantic grep is an anti-pattern for exact-named-file tasks.**

**Case 53 (the win):** same agent, same arms, converges in 8–12 turns at ~90K tokens. `pm_codex_53_hiddenkg_edges_ra471` = **10/11 rubrics (0.91) at 89K tokens in 8 turns** — the single best result in the matrix. When the corpus matches the query semantics, semfs+KG-edges wins on **both** axes.

---

## (d) Patterns

1. **Spending more tokens does NOT buy accuracy — it signals failure.** Clean correlations over 89 judged cells: `corr(prompt_tokens, acc) = −0.31`, `corr(turns, acc) = −0.31`. Token tertiles: low-tok (52K) → acc 0.22; mid (274K) → 0.07; high (901K) → **0.04** at 46 turns. The agent over-explores *precisely when it is lost*; extra turns are wasted reformulation, not productive search.

2. **The "transcribe-brake" prompt (arm 4 "best") does not brake.** Arm 4 has the **highest** median turns of any semfs arm (33 vs 17–20) and the most greps (12). The stop-and-transcribe instruction is being ignored — same failure mode the memory notes flagged for Claude, now confirmed for GLM-5.1.

3. **KG rerank/edges are token-neutral; KG *retrieval* is a token tax.** Arms 5/6/7 sit in the plain token band (269–316K). Arms 8/9 add +170/+225 % via the 60–80-candidate injection rendered per grep. The injection is also semantically noisy (spurious entity matches), so it adds tokens *and* misdirects exploration without lifting accuracy.

4. **Two distinct token mechanisms, both gated by turns:** (i) *turn-spiral* (nokg/plain on hard cases — many medium grep/find outputs × huge T); (ii) *unbounded single read* (plain `cat` of a 290K-char file). semfs compression fixes (ii)'s per-output size but is powerless against (i), which is the bigger driver (`corr(turns,tok)=0.72`).

5. **Difficulty ≫ arm.** Within a case, arms cluster; across cases they diverge 10× (case 53: 92K/0.43 vs case 95: 963K/0.00). Case 95 (45 % of all tokens, 0 % accuracy) and the xlsx-synthesis ceiling of case 15 (best 1/16 rubrics) are corpus/task properties no retrieval knob fixes.

---

## (e) Concrete levers to cut tokens

1. **Hard turn cap + forced-transcribe at K turns (biggest lever).** Turns drive ~0.72 of token variance and correlate −0.31 with accuracy. Cap at the solvable-case budget (~12 turns; case 53 converges there) and force a deliverable. Kills the 170/110/101-turn spirals that hold 42 % of all tokens. The current prompt-only brake (arm 4) demonstrably fails — needs a *runtime* cap, not a prompt.
2. **De-duplicate / suppress repeated & near-duplicate grep queries.** Case 95 = 11 % exact-repeat queries plus heavy near-duplicate reformulation. Refuse a grep whose query is a near-restatement of one already in context (the prompt already says "do not repeat" — enforce it in the tool).
3. **Shrink & gate KG retrieval injection.** Cap injected candidates from 67→~10 and drop the spurious DirectEntity matches (the "Nordea/Point72/Xcode Asset" noise). This removes the arms-8/9 +170–225 % per-turn tax with no accuracy loss (their acc already ≈ plain). Prefer KG-*edges* rerank (arm 5: token-neutral, produced the 0.91 case-53 win) over KG-*retrieval injection*.
4. **Route named-file-lookup tasks to exact path resolution, not semantic grep.** Detect literal filename tokens (`description_9.txt`) in the task and expose a name→path lookup so the agent pins the file in 1 call instead of 34. This is the case-95 fix.
5. **Bound plain reads.** A single `cat` dumped 290K chars re-prefilled 55× (~6.2M tokens). Any read path (even "plain") should head-cap large files; this is the one place semfs's compression already wins (grep max 17K vs cat max 290K) — extend the cap to direct reads.
6. **Stop spending on dead cases.** Case 95 (0 acc, all arms) and case-15's xlsx synthesis are ceiling-bound; gate them out of the cost denominator or fix the task/modality before re-running, so headline token numbers aren't dominated by unwinnable work (currently 45 % of tokens, 0 % return).

---

### Appendix — verification artifacts
- `_token_attrib/aggregate.py`, `_token_attrib/analyze.py`, `_token_attrib/cells.json` (per-cell record: arm, case, rep, status, prompt/compl/turns, #grep vs #read vs #search, grep render chars, max single output, re-prefill-weighted chars, deliverables, accuracy).
- Re-prefill identity validated: `corr(prompt_tokens, turns)=0.76`, `corr(prompt_tokens, reprefill_weighted_output_chars)=0.74`.
- Infra-fail exclusion: 34 cells = all `ra*2`/`*2` second-reps, `status=timeout`, `wall_s=1500`, `calls=0`, `prompt=0`.
