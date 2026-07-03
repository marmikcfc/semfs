# Generation harness — design spec (SEM-52)

Builds the labeled DSPy prompt-tuning dataset per `DATASET_SPEC.md`. Four independent paths;
generation on the **OAuth (Max) token**, judging on **DeepSeek Pro (OpenRouter)**.

Decisions locked in brainstorming (2026-07-03):
- **Gen backend = OAuth only** (Bearer + `anthropic-beta: oauth-2025-04-20` + Claude Code system prompt).
- **Judge = DeepSeek Pro**, **rubric + reference** basis (n≥2 on val/test, n=1 on train).
- **Run order = task-by-task, all 4 models, atomic checkpoint per task** → quota exhaustion loses
  *quantity* not *quality* (every committed task is fully labeled across tiers).
- First run: build all 4 → smoke ~15 tasks → full ~512 at **default effort** (effort sweep deferred).

## Data flow
```
gen_tasks.py ─► tasks_prompt.jsonl ─► run_matrix.py (OAuth) ─► results.jsonl
  ─► judge_deepseek.py (DeepSeek) ─► results_judged.jsonl ─► derive_oracle.py ─► tasks_prompt_labeled.jsonl
```
Paths 1 & 4 run free (no model calls). Path 2 spends Max quota. Path 3 spends OpenRouter.

## Path 1 — `gen_tasks.py`  (free)
Reuse the source adapters from `build_base_dataset.py` (+ kernelbook=perf_optimization, WildChat=qa/general).
For each task assign: `dimension` (8), `difficulty` (easy/med/hard), and `context_bucket` **sampled
independently of difficulty**. Then build `prior_context` to the bucket's **token budget** by
accumulating real trace turns / repo dumps; measure `context_tokens` (tokenizer, fallback chars/4).
Carry `reference` (HumanEval/MBPP expected · Open-SWE `reference_patch`) for the judge.
Balance ~64/dim ≈ 512; difficulty 35/35/30; context 25/25/20/20/10; split 60/20/20.

**`tasks` row:** `task_id · dimension · difficulty · context_bucket · context_tokens · split ·
source_dataset · prompt · prior_context · reference · meta`.

## Path 2 — `run_matrix.py`  (OAuth quota)
```
for task in tasks:
    if task_id in done: continue          # resume
    rows = []
    for model in [haiku, sonnet, opus, fable]:
        r = call_oauth(model, system=CC_SYSTEM, context=prior_context, prompt=prompt, effort=default)
        rows.append({usage, latency_ms, answer})
    append(results.jsonl, rows)            # ATOMIC per-task checkpoint
```
- Models: `claude-haiku-4-5-20251001`, `claude-sonnet-5`, `claude-opus-4-8`, `claude-fable-5`.
- Capture `usage`: input_tokens, cache_read_input_tokens, cache_creation_input_tokens, output_tokens; latency client-side.
- 429/5xx → exponential backoff; gentle inter-call sleep. Fail-soft: a model erroring records `error`, task still commits.

**`results` row (long-format):** `task_id · dimension · difficulty · context_bucket · split · model ·
effort · answer · input_tokens · cached_tokens · output_tokens · total_tokens · latency_ms · error`.

## Path 3 — `judge_deepseek.py`  (OpenRouter)
Per result row: DeepSeek Pro scores **0/1** from `task + answer + reference` (rubric when no reference).
`n≥2` on val/test with agreement → `label_confidence`; `n=1` on train. Incremental checkpoint + resume.
Model id: `deepseek/…` (resolve exact "pro" id at build). Adds `score`, `n_judges` → `results_judged.jsonl`.

## Path 4 — `derive_oracle.py`  (free)
Per task: among `score==1` rows pick the **cheapest tier** by `cost_usd = tokens × published_price`
(ranking only — OAuth bills flat). Ties → lower tier. None passed → `oracle_route="none"` (flag).
Writes `oracle_route · oracle_effort · label_confidence` back → `tasks_prompt_labeled.jsonl` (DSPy trainset).

Cost weights (published $/Mtok, in/out) for ranking: haiku < sonnet < fable < opus.

## Safeguards
Atomic per-task checkpoint · resume-by-skip on every path · 429 backoff · gentle pacing ·
smoke ~15 tasks before the full run · secrets read from env, never logged (prefix only).
