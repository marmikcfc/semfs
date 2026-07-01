# E2B codex matrix — token/turn analysis (2026-06-14)

**Status: JUDGED (codex, Seed-2.0-Lite via OpenRouter, 2026-06-14).** Accuracy = WB rubric
pass-rate (`agent_eval.py`, HF WB-Lite rubrics). n=1/cell. `15/nokg` unjudged (smoke cell,
no deliverable pulled). The token-only ranking below is kept for the record, but the
**GATED VERDICT supersedes it** — and reverses it.

## GATED VERDICT (accuracy-first, then tokens) — supersedes the token-only ranking

| arm | mean accuracy | mean tokens | verdict |
|---|---:|---:|---|
| **nokg** | **18%** | 484 K | **best accuracy** (2.5× plain), costs more tokens |
| plain | 7% | 331 K | cheapest, **tied-worst accuracy** |
| nokgAK | 7% | 446 K | adaptive-K **trades accuracy for tokens — badly** |

**Best overall = `nokg`** (lexicographic: accuracy first). The token-only view said "plain
cheapest, semfs hasn't earned its keep, nokgAK leanest" — **all three wrong once accuracy
is in.** nokg's accuracy lead is driven by the discriminating cases where semantic
retrieval surfaces data plain misses:

| case | plain (acc/tok) | nokg | nokgAK | note |
|---|---|---|---|---|
| 45  | 1/19 · 363K | **4/19 · 233K** | 2/19 · 289K | **nokg wins BOTH axes** |
| 171 | 2/18 · 164K | **10/18 · 149K** | 4/18 · 170K | **nokg wins BOTH axes** (5× acc, fewer tok) |
| 175 | 3/12 · 145K | **9/12 · 275K** | 0/12 · 166K | nokg 3× acc at 1.9× tok; nokgAK **collapses to 0** |
| 55  | **4/20 · 141K** | 2/20 · 374K | 3/20 · 329K | plain wins (task needs no retrieval) |

**Key findings:**
1. **The −75% token "win" on case 53 was a mirage** — `nokgAK 53` scored **1/11**: a cheap
   *wrong* answer (wrote a schema summary, not the populated dataset). Token-only would
   have shipped it.
2. **adaptive-K (nokgAK) is an accuracy trap**: it trims tokens vs nokg but tanks accuracy
   on report/multi-file cases (175: nokg 9/12 → nokgAK **0/12**; 171: 10 → 4). It returns
   too few results and starves the agent of the data it needs. (Relevant to the
   confidence-adaptive-delivery direction — adaptive-K as currently tuned hurts.)
3. **Where retrieval matters (171, 175, 45), plain-semfs `nokg` clearly beats plain** on
   accuracy — and on 45+171 it's *also* cheaper. That is the real semfs win.

**Caveats:** absolute accuracy is LOW (7–18%) — cases 44/95/386/388 score 0 for ALL arms
(non-discriminating: structural ceilings and/or deliverable-format/filename mismatch
suppressing scores — worth investigating; real accuracy may be higher). n=1 per cell;
`15/nokg` unjudged. The RELATIVE ranking (nokg > plain ≈ nokgAK) rests on the 4
discriminating cases above.

---

## CROSS-AGENT UPDATE (2026-06-15) — claude added, cloud arm broken

Full matrix judged. The codex verdict **replicates on claude** — robust, not a fluke.

| agent | arm | accuracy | mean tokens |
|---|---|---:|---:|
| codex | plain | 7% | 331 K |
| codex | **nokg** | **18%** | 484 K |
| codex | nokgAK | 6% | 455 K |
| claude | plain | 4% | 747 K |
| claude | **nokg** | **11%** | 1,219 K |
| claude | nokgAK | 7% | 2,012 K |

- **`nokg` is the best arm on BOTH agents** (~2.5–2.7× plain's accuracy), at a token premium. Real semfs win.
- **adaptive-K (`nokgAK`) is an accuracy trap on BOTH agents** (below nokg); on claude it also explodes tokens (2.0M).
- **codex > claude** — cheaper and more accurate at every arm.

### Cloud arm — INFRA-VOID (credit exhaustion, NOT a retrieval/accuracy result)
Root cause (diagnosed 2026-06-15, live cloud-grep probe): **credit exhaustion on two services.**
- **codex cloud "0 accuracy" is INFRA-BLOCKED, not real.** The cloud SEARCH is out of credits:
  `semfs grep --tag workspace-bench-chanpin` → `rejected (402): {"error":"Search query limit
  reached","details":"You've run out of credits. Top up to continue."}` — confirmed even on
  the EC2 known-good case-289 query. Every cloud grep 402'd → the agent floundered (path-syntax,
  `--help`, query pivots) → off-topic deliverable. **The cloud-arm code is fine; Supermemory
  search credits are exhausted.** These 10 cells must be RE-RUN, not interpreted.
- **claude cloud no-deliverable = OpenRouter out of credits.** `claude_45_cloud` err:
  `API Error: 402 … requires more credits … can only afford 31889`. 0 calls.
- Drained by the overnight run: claude-local (30 cells) + Seed-2.0 judge (68 cells) → OpenRouter;
  codex-cloud greps + rewrites → Supermemory search credits.

**Fix (NOT code): top up Supermemory + OpenRouter credits, then re-run the 20 cloud cells +
judge.** Optional hardening: make the driver detect a cloud-grep 402 and mark the cell
`infra-blocked` instead of recording it as 0-accuracy (don't conflate infra with retrieval).

### Open
- Absolute accuracy LOW (judge strict / cases hard / 44·95·386·388 = 0 for all local arms → format-suppression suspected; investigate, may lift numbers).
- n=1/cell — raise discriminating cases (45,171,175) to n≥2 before locking.
- Cloud arm broken — diagnose cloud grep + claude-cloud-no-deliverable, then re-run.

---

### (Archived) token-only ranking — DO NOT use as a verdict

Run: E2B real FUSE mount, codex on ChatGPT-subscription, **29/30 cells** (missing
`pm_codex_386_nokgAK`). n=1 per cell.

## Per-arm aggregate

| arm | mean tok | median tok | mean calls | n |
|---|---:|---:|---:|---:|
| plain  | 331 K | 333 K | 39.6 | 10 |
| nokgAK | 446 K | **308 K** | **38.0** | 9 |
| nokg   | 469 K | 327 K | 40.2 | 10 |

- **by mean tokens:** plain < nokgAK < nokg  → plain cheapest
- **by median tokens:** nokgAK < nokg < plain → nokgAK cheapest on the *typical* case
- **by mean calls:** nokgAK < plain < nokg (≈ tie)

## Per-case tokens / calls (✦ = lowest tokens)

| case | plain | nokg | nokgAK |
|---|---|---|---|
| 15  | ✦ 302K/30 | 337K/30 | 678K/44 |
| 44  | ✦ 243K/26 | 316K/34 | 308K/30 |
| 45  | 363K/38 | ✦ 234K/20 | 290K/36 |
| 53  | 455K/58 | 115K/20 | ✦ 111K/18 |
| 55  | ✦ 141K/24 | 374K/38 | 330K/38 |
| 95  | ✦ 654K/74 | 1.28M/76 | 975K/60 |
| 171 | 164K/28 | ✦ 150K/26 | 170K/22 |
| 175 | ✦ 145K/30 | 276K/30 | 166K/16 |
| 386 | ✦ 414K/46 | 539K/52 | — (missing) |
| 388 | ✦ 429K/42 | 1.07M/76 | 985K/78 |

## Findings (token axis)

1. **semfs is bimodal, not uniformly worse.** Big wins where retrieval lands clean
   (53: −75% vs plain; 45, 171) and big blowups from over-exploration (95: 1.28M/76
   calls; 388: 1.07M/76 calls). The two tails drag the semfs *mean* above plain even
   though the *median* is lower.
2. **Mean vs median diverge** → the headline depends on the statistic. Plain wins mean;
   nokgAK wins median + calls. Don't quote one without the other.
3. **Plain wins per-case head-to-head 7–8/10 on tokens.** semfs wins rarely but large.
4. **Over-exploration is the cost driver** (cases 95, 388: 76 calls): the re-grep/crawl
   pattern documented in the campaign analysis — same failure mode, codex.
5. **adaptive-K (nokgAK) ≥ nokg on both axes** — fewer calls, lower median; it trims
   the tail somewhat (95: 975K vs nokg 1.28M) but doesn't eliminate it.

## Hard caveats
- **n=1 per cell** — bimodal call count ⇒ ±30% swing. Require n≥2 before any win claim.
- **No accuracy** ⇒ NO winner. Case 53's −75% could be a cheap wrong answer. The verdict
  is PENDING-ACCURACY until the judge scores these deliverables.
- `pm_codex_386_nokgAK` missing (sandbox died at that cell) — backfill in cleanup.

## Next
1. Stage WB-Lite rubrics → run the judge over these 29 deliverables → fill the
   `(accuracy, tokens)` pair → real gated verdict.
2. Backfill `386_nokgAK`; raise to n≥2 on the discriminating cases (53, 95, 388).
