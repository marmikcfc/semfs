# E2B WB-PM experiment report (consolidated)

**Date:** 2026-06-15 · **Platform:** E2B real FUSE mount · **Judge:** Seed-2.0-Lite (the paper's judge) via OpenRouter
Detail docs: [`E2B_CODEX_ANALYSIS.md`](E2B_CODEX_ANALYSIS.md) (per-cell gated table), [`E2B_RUNBOOK.md`](E2B_RUNBOOK.md), [`E2B_EXPERIMENT_LEDGER.md`](E2B_EXPERIMENT_LEDGER.md).

## TL;DR
- **`nokg` (plain semfs, semantic search) is the best arm on BOTH agents** — ~2.5–2.7× plain's rubric accuracy — at a token premium. This is the real result, and it REVERSES the token-only read ("plain cheapest") that ignored accuracy.
- **adaptive-K (`nokgAK`) is an accuracy trap on both agents** — trims tokens by returning fewer hits, but starves the agent and tanks accuracy (case 175: nokg 9/12 → nokgAK 0/12).
- **Absolute accuracy is low (7–18%) and that is mostly GENUINE** (hard multi-rubric tasks + incomplete deliverables), NOT a harness artifact — verified against pristine upstream WB code. So compare arms RELATIVELY (same harness for all), not against the paper's leaderboard.
- **Cloud arm is infra-void** (Supermemory + OpenRouter credit exhaustion), needs a re-run.

## 1. Setup
- **Agents:** codex (gpt-5.5, ChatGPT subscription) · claude (sonnet-4.6; was OpenRouter, now switched to native Claude OAuth).
- **Arms:** `plain` (raw 3,207-file tree, no semfs) · `nokg` (gemma-q4 semfs mount, `semfs grep`) · `nokgAK` (+ adaptive-K). (`kg` arm wired, unrun.)
- **Cases:** 10 chanpin/PM cases {15,44,45,53,55,95,171,175,386,388}; **289 excluded** (seed leak).
- **Harness:** upstream WB agent adapters (`claudecode.py`/`codex.py`) via a thin `benchmarks/e2b/cell_driver.py`; judge = upstream `agent_eval.py` + Seed-2.0-Lite. n=1/cell.

## 2. Results — gated (accuracy, tokens), mean over 10 cases
| agent | arm | rubric accuracy | mean tokens |
|---|---|---:|---:|
| codex | plain | 7% | 331 K |
| codex | **nokg** | **18%** | 484 K |
| codex | nokgAK | 6% | 455 K |
| claude | plain | 4% | 747 K |
| claude | **nokg** | **11%** | 1,219 K |
| claude | nokgAK | 7% | 2,012 K |

Both-axes wins for `nokg` (more accurate AND fewer tokens than plain): **codex case 171** (10/18 vs 2/18, 149K vs 164K) and **codex case 45** (4/19 vs 1/19, 233K vs 363K). codex > claude on both axes throughout.

## 3. Why token usage went up (RCA — systems + five-whys)
Under the `cached_input=0` convention, cost ≈ Σ_turns(accumulated context) = `T·S + Σₛ(T−s)·oₛ`. Two inflows:
- **Per-call blob `oₛ`** — dominant for **claude**: semfs grep blobs arrive **un-clipped** (47–72K tok/call vs plain 8–21K), re-paid every turn. codex clips tool output (~10KiB) so its blobs stay 5–16K/call. → the blowup is an **emergent interaction** (dense blob × harness clip), not a semfs property.
- **Turn count `T`** — a **reinforcing loop** on cases semfs can't solve: no resolution → re-search → context grows → costs more → still unresolved. Confirmed: within semfs, **>600K-token cells score ~1–7%; ≤600K cells score 12–16%** — i.e. high tokens are a *symptom of failure*, not work. **adaptive-K amplifies the loop** by starving results (claude 175·nokgAK = 205 calls / 12M tokens / 1-of-12).
- **`cached_input=0` is the amplifier** (every byte re-paid each turn); production caching would cut much of this — part of the gap is a metric artifact.

**semfs is cheap when it works (codex 53 −75%, 171 −9%, 45 −36%) and expensive when it fails** — the mean rises from a failure tail, not a uniform tax.

## 4. Why accuracy is low — and it is NOT a harness artifact we introduced (verified)
"Structural ceiling" = rubrics unsatisfiable regardless of agent quality, due to task/harness setup. Verified against **pristine upstream WB code** (`git`: `agent_runner.py`, `agent_eval.py`, `agent_as_a_judge.py` unmodified):
- **`model_output/` output redirection is UPSTREAM** (`agent_runner.py` tells the agent to ignore the task's stated path and write there). Not our deviation.
- **Path-convention rubrics (`./output_cc`) exist in only case 289 (+128)** — 289 is excluded; so this ceiling does NOT explain our 10 cases. (Earlier I over-generalized a 289-specific note — corrected.)
- **The judge is recall-first** — collects files matching the expected name AND any non-input work-dir file, then content-grades. So a wrong-named-but-right-content deliverable still scores; filename mismatch ≠ auto-zero.
- **Therefore the low absolute scores are mostly GENUINE:** these chanpin/PM tasks have 15–25 strict content rubrics (e.g. case 45: infer 10+ roles + a permission matrix); the agents produce incomplete deliverables (verified case 53: a schema summary, not the populated dataset). This would be similarly low in upstream WB for the same agents.
- **Residual caveats (do NOT inflate the gap, affect all arms equally):** we used `cell_driver` (English prompt, functionally equivalent to upstream's Chinese `_wrap_prompt`) instead of `agent_runner.py`; `SEARCH_ONLY=on`. Cases 44/95/386/388 score 0 for ALL arms — genuinely hard, not suppressed.
- **~~half-warm gemma seed~~ — RETRACTED (2026-06-15):** the seed is **98.2% complete** on real content (616/627 non-empty original files reachable; verified by `semfs seed-verify`). The "half-warm / 28%" reading was a measurement artifact (empty WB placeholders + stale `.semfs-error.txt` stubs + `.extracted.md` sidecars padding the denominator). The semfs arms were NOT on a blind index. Evidence: `tickets/seed-completeness-gate/SEED_COMPLETENESS.md`.
- **Cannot confirm exact parity with the paper's leaderboard** (numbers behind interactive filters, not published). So: trust the **relative** ranking (nokg > plain, identical harness), not absolute-vs-paper.

## 5. Cloud (Supermemory) arm — INFRA-VOID
Both cloud failures = credit exhaustion, NOT code: codex cloud → Supermemory **search** 402 ("run out of credits") → agent floundered → 0 acc; claude cloud → OpenRouter 402 (0 deliverables). The overnight run (claude-local on OpenRouter = 38.3M tokens + the judge) drained OpenRouter; codex-cloud greps drained Supermemory search credits. Re-run after top-up (claude now on native token, so OpenRouter only carries the judge).

## 6. Open items / next experiments
1. **Cloud re-run** — top up Supermemory + (small) OpenRouter, re-run 20 cloud cells + judge.
2. **Token fix A (highest leverage):** cap the *total* grep render (not per-hit) → kills claude's 47–72K/call blob.
3. **Token fix B:** stop adaptive-K from starving results (widen, don't narrow, on low confidence) → kills the loop amplifier.
4. **n≥2** on discriminating cases (45,171,175) to lock the nokg win vs per-run variance.
5. **0-for-all cases (44,95,386,388):** confirm genuine difficulty vs any residual suppression.
6. Optional: align to upstream `agent_runner.py` exactly (Chinese wrapper) on `plain` to test absolute paper-comparability.
