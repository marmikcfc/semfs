# SWE-Atlas Codebase QnA — benchmark integration & mode A/B

**Linear:** [SEM-50](https://linear.app/semfs/issue/SEM-50) · folder `tickets/swe-atlas-qa/`
**Upstream:** https://github.com/scaleapi/SWE-Atlas · paper arXiv `2605.08366`

## Goal
Evaluate our agent/retrieval modes on **SWE-Atlas Codebase QnA** — a benchmark where the
agent answers questions *about* a real codebase, **rubric-graded by an LLM judge (Claude
Opus 4.5)**. This is a **code + retrieval-centric** arena, unlike WB-Lite (Chinese business
docs), so it's the first proper test of our code-retrieval work in its home turf.

SWE-Atlas has 3 modes (`data/qa`, `data/tw` test-writing, `data/rf` refactoring). **Scope
here = QnA only** (`data/qa`).

## Why this benchmark (fit)
- **Retrieval is the task**: "find the code that answers this question, then explain it."
  Retrieval quality is the lever (unlike doc-personas where `plain` kept winning).
- **Code + identifier-heavy questions** → **late interaction (LateOn-Code, ColBERT-style
  token matching)** is theoretically strongest here. WB-Lite was the wrong arena for it.
- **Rubric-judge shape == our WB-Lite harness** → the judge integration is familiar.

## Recommended modes (which arms to run) — see forming-opinions rationale in the chat log
| mode | run? | why |
|---|---|---|
| **`plain`** | ✅ baseline (mandatory) | seed-free, runs on any repo immediately; our repeatedly-winning baseline; QnA identifier lookups are answerable by a single `grep` |
| **`late on`** (colgrep / LateOn-Code) | ✅ primary bet (~58% to beat plain) | code + identifier QnA = late-interaction's home turf; the reason late-on exists |
| `ppr_on` / `ppr_off` / `ppr_map` | ⏸ later | need a **per-codebase semfs seed** (heavy if many repos); test only if plain shows retrieval-limited failures |
| `Headroom + plain` | ⏸ likely skip | QnA is short-turn → little re-sent transcript → headroom's compression lever is weak |
| `plain/ppr + model routing` | ❓ | "model routing" is undefined in our project — clarify what it means before scoping |

**The `plain` vs `late on` A/B is the real deliverable.**

## Open questions — RESOLVED 2026-07-01 (from the repo directly; paper abstract was thin)
1. **Codebase size** — **LARGE, real production repos** (wp-calypso, grafana, minio, kitty, scapy, paperless-ngx…). Retrieval-favorable → late-on's edge should grow. ✅
2. **Is context GIVEN or must be RETRIEVED?** — **RETRIEVED.** Repo is checked out at a pinned `base_commit` inside a prebuilt Docker image, uploaded to `/app`; agent explores with bash. No snippet is handed over. **This is our home arena.** ✅
3. **How many distinct codebases** — **11 repos across 124 tasks** (see Inventory). Per-repo index cost = **11 colgrep indexes**, not 124 → late-on AND `ppr` seeds are both tractable.
4. **Question type mix** — **mixed & multi-part.** Categories: Architecture & system design (44), Root-cause analysis (37), Code Onboarding (28), Security (11), API/library (4). A single question bundles conceptual ("how does login detection work") *and* exact-value/identifier lookups (port numbers, exact CSS margin/padding px, Redux action prefixes, CSS custom-property names) → favors deep retrieval + late-interaction on identifiers.
5. **Judge/rubric format** — **SWE-Atlas ships its own harness.** `harbor` verifier runs `tests/evaluate_answer.py`: per-task `rubrics.json` = list of atomic items (each `must have`/nice-to-have), graded independently YES/NO by **Claude Opus 4.5** (`anthropic/claude-opus-4-5-20251101`, any OpenAI-compat endpoint via `EVAL_BASE_URL`). **`reward=1` only if ALL scored must-have rubrics = 1** (strict binary pass); also emits `agg_score` = fraction of rubrics passed (finer signal). Shape ≈ our WB-Lite rubric-judge, so we can reuse familiarity — but the harness is theirs, not ours.

## Inventory (resolved 2026-07-01)
- **124 QnA tasks**, one prebuilt GHCR Docker image each (`ghcr.io/scaleapi/swe-atlas:swe_atlas_QnA_<org>_<repo>_1.0`). Repo at `/app`, agent writes `/logs/agent/answer.txt` wrapped in `<<FINAL_ANSWER>>`.
- **11 distinct repos** (task count): kitty 26 (C), simple-login/app 15 (py), paperless-ngx 15 (py), scapy 14 (py), maddy 10 (go), trufflehog 8 (go), grafana 8 (go/ts), wp-calypso 8 (ts), k6 7 (go), sftpgo 7 (go), minio 6 (go).
- **Languages:** go 38 · ts 31 · python 29 · c 26 (balanced).
- **Harness:** `harbor` (laude-institute) orchestrator + **Modal** sandboxes (`-e modal`). Per-task budget: 16 CPU / 16 GB / 20 GB disk / no GPU / internet on; verifier 900 s, agent 10800 s.
- **Reference agents provided:** `claude-code` and `mini-swe-agent`, both on `anthropic/claude-opus-4-6`, `-k 3` rollouts. **"plain" ≈ their stock `claude-code`/`mini-swe-agent` over raw `/app`** (seed-free), so plain is nearly free to run natively.

## Work breakdown
1. Read paper `2605.08366` → answer the 5 open questions. **Gate the rest on this.**
2. Pull `data/qa` (codebases + questions + rubrics); inventory size/count.
3. Harness decision: reuse SWE-Atlas's own eval harness, or adapt to our E2B/run_matrix.
   (Note: the standing "all semfs benchmarks on E2B" rule — decide if it applies to a 3rd-party bench.)
4. **`plain` baseline** — agent over the raw codebase (seed-free), rubric-judged. Tokens + accuracy.
5. **`late on`** — build a colgrep LateOn-Code index per codebase, run, rubric-judged.
6. **A/B**: plain vs late-on — accuracy AND tokens (paired, per metric rule).
7. (Conditional) add `ppr_*` if plain shows retrieval limits worth the per-repo seed cost.

## Metric discipline (standing rules)
- Track **TOKEN usage** and **accuracy** together (never accuracy alone).
- Compare on the **same cases, same judge, same model** — no cross-run/cross-judge mixing
  (the recurring artifact trap: 9.7% → old-judge ppr → mean-vs-median).

## Status
- Created 2026-07-01. **Inventory + 5 open questions RESOLVED 2026-07-01** (above).
- Nothing run yet. **Two decisions gate the first run:**
  1. **Harness** — run on SWE-Atlas's native `harbor + Modal` (comparable/leaderboard-legit, but violates the standing "all semfs benchmarks on E2B / real-FUSE" rule and doesn't exercise our mount) **vs** port the 124 Dockerized tasks onto our E2B/run_matrix (honors the rule + real FUSE + our token accounting, but re-implements their rubric verifier and drops leaderboard comparability).
  2. **Agent/model** — their stock `claude-code` on Opus-4.6 (comparable to their leaderboard) **vs** our codex + GLM-5.1 path (comparable to our WB-Lite numbers). Comparability points in opposite directions; pick the axis that matters.
- **Then:** `plain` baseline (nearly free — it's their stock agent) → build 11 colgrep LateOn-Code indexes → `late on` arm → plain-vs-late-on A/B (tokens + `agg_score`/reward, same cases/judge/model).
