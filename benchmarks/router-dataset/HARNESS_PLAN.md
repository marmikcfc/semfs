# Router dataset generation harness — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the 4-path pipeline that generates the labeled DSPy prompt-tuning dataset per `HARNESS_SPEC.md` / `DATASET_SPEC.md`.

**Architecture:** Four standalone Python scripts in `benchmarks/router-dataset/`. Pure-data paths (gen_tasks, derive_oracle) are unit-tested; network paths (run_matrix on OAuth, judge_deepseek on OpenRouter) have unit-tested parsers + a live smoke. Task-by-task atomic checkpointing so a truncated run yields complete rows.

**Tech Stack:** Python 3.10 stdlib (`urllib.request`, `json`), `datasets` (streaming sources), `pytest`. No new heavy deps.

## Global Constraints
- Gen backend = **OAuth only**: `authorization: Bearer $CLAUDE_CODE_OAUTH_TOKEN`, `anthropic-beta: oauth-2025-04-20`, `anthropic-version: 2023-06-01`, `system="You are Claude Code, Anthropic's official CLI for Claude."`
- Models: `claude-haiku-4-5-20251001`, `claude-sonnet-5`, `claude-opus-4-8`, `claude-fable-5`.
- Judge = DeepSeek Pro via OpenRouter (`OPENROUTER_API_KEY`); id resolved in Task 0.
- Secrets: read from env only; log ≤14-char prefixes, never full tokens.
- Every path: resume-by-skip + incremental checkpoint. run_matrix commits **per task** (all 4 models atomically).

## File Structure
| File | Responsibility |
|---|---|
| `gen_tasks.py` | assemble `tasks_prompt.jsonl` (dims, decoupled context buckets, token-budgeted prior_context, reference) |
| `run_matrix.py` | OAuth caller + per-task 4-model loop + checkpoint → `results.jsonl` |
| `judge_deepseek.py` | DeepSeek rubric+reference 0/1 + n≥2 → `results_judged.jsonl` |
| `derive_oracle.py` | cheapest-passing-tier oracle → `tasks_prompt_labeled.jsonl` |
| `_common.py` | shared: `count_tokens`, `read_env`, `http_json`, cost table |
| `tests/test_harness.py` | unit tests for pure units |

---

### Task 0: Resolve IDs + OAuth/DeepSeek smoke

**Files:** Create `benchmarks/router-dataset/_common.py`

**Interfaces produced:** `count_tokens(s)->int`, `read_env(name,*alts)->str`, `http_json(url,body,headers,timeout)->dict`, `PRICE` (dict tier→(in,out) $/Mtok).

- [ ] **Step 1:** Write `_common.py` with `count_tokens` (use `len(s)//4` fallback; try `tiktoken` if present), `read_env`, `http_json` (urllib POST/GET, JSON), and `PRICE = {"haiku":(...),"sonnet":(...),"fable":(...),"opus":(...)}` from published pricing.
- [ ] **Step 2:** Live smoke — one call per Claude model via OAuth returns 200 + `usage`; print prefixes only. Confirms all 4 IDs + the OAuth path.

```bash
python3 -c "from _common import *; import json,urllib.request; ..."   # 'hello' to each of the 4 models
```
Expected: 4× `{model, output_tokens>0}`.
- [ ] **Step 3:** Resolve DeepSeek Pro id — GET `https://openrouter.ai/api/v1/models`, grep `deepseek`, pick the "pro"/reasoning id; record in `_common.py` as `JUDGE_MODEL`.
- [ ] **Step 4:** Commit.

### Task 1: `gen_tasks.py` — task generator (pure, unit-tested)

**Files:** Create `gen_tasks.py`, `tests/test_harness.py`
**Interfaces produced:** `build_prior_context(turns, target_tokens)->(str,int)`, `assign_axes(rng_seed, i)->(difficulty, context_bucket)`, `BUCKET_TOKENS`.

- [ ] **Step 1: failing test** — context decoupled from difficulty + budget honored:
```python
def test_context_bucket_decoupled_from_difficulty():
    from gen_tasks import assign_axes
    pairs=[assign_axes(0,i) for i in range(400)]
    # difficulty and context_bucket should be ~independent (chi-sq-ish: each combo present)
    combos={(d,c) for d,c in pairs}
    assert len({d for d,_ in pairs})==3 and len({c for _,c in pairs})==5
    assert len(combos)>=12   # not collapsed onto a diagonal

def test_build_prior_context_hits_budget():
    from gen_tasks import build_prior_context
    turns=[{"role":"user","content":"x"*4000} for _ in range(50)]
    s,tok=build_prior_context(turns, target_tokens=16000)
    assert 8000 <= tok <= 24000   # within band of the 16k budget
```
- [ ] **Step 2:** Run → FAIL (module missing).
- [ ] **Step 3:** Implement `gen_tasks.py`: reuse `build_base_dataset.py` adapters for prompts; `BUCKET_TOKENS={"fresh":(0,1e3),"small":(1e3,16e3),"medium":(16e3,64e3),"large":(64e3,256e3),"xlarge":(256e3,600e3)}`; `assign_axes` draws difficulty and context_bucket from independent deterministic sequences; `build_prior_context` accumulates real turns / repo text until `count_tokens` reaches target; write `tasks_prompt.jsonl` balanced ~64/dim, split 60/20/20, carrying `reference`.
- [ ] **Step 4:** Run → PASS. Then run `python3 gen_tasks.py out/ --n 512` and eyeball the printed dim×difficulty×context cross-tab (expect no diagonal collapse).
- [ ] **Step 5:** Commit.

### Task 2: `run_matrix.py` — OAuth labeler (per-task atomic checkpoint)

**Files:** Create `run_matrix.py`; add tests to `tests/test_harness.py`
**Interfaces produced:** `call_oauth(model, system, context, prompt)->dict{answer,usage,latency_ms,error}`, `already_done(path)->set`.

- [ ] **Step 1: failing test** (resume logic, no network):
```python
def test_already_done_skips(tmp_path):
    from run_matrix import already_done
    p=tmp_path/"results.jsonl"; p.write_text('{"task_id":"A"}\n{"task_id":"A"}\n{"task_id":"B"}\n')
    assert already_done(str(p))=={"A","B"}
```
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3:** Implement: `call_oauth` (POST messages API per Global Constraints; user content = `f"{context}\n\n---\n\n{prompt}"` when context else prompt; capture `usage.input_tokens/cache_read_input_tokens/cache_creation_input_tokens/output_tokens` + `time.monotonic` latency; 429/5xx → exp backoff up to 5). Main loop: `for task: if id in done: continue; rows=[call_oauth(m,...) for m in MODELS]; append all 4 to results.jsonl` (atomic). `--limit N` for smoke.
- [ ] **Step 4:** Run unit test → PASS.
- [ ] **Step 5:** Commit.

### Task 3: `judge_deepseek.py` — DeepSeek rubric+reference judge

**Files:** Create `judge_deepseek.py`; add tests
**Interfaces produced:** `parse_verdict(text)->int|None`, `judge_one(task, answer, reference, n)->(score,n_judges)`.

- [ ] **Step 1: failing test** (verdict parsing, no network):
```python
def test_parse_verdict():
    from judge_deepseek import parse_verdict
    assert parse_verdict('reasoning... VERDICT: 1')==1
    assert parse_verdict('{"score": 0}')==0
    assert parse_verdict('no verdict here') is None
```
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3:** Implement: prompt = rubric ("Given TASK, the model's ANSWER, and REFERENCE if any, score 1 if it correctly/completely addresses the task else 0. End with `VERDICT: <0|1>`"); call OpenRouter `JUDGE_MODEL`; `parse_verdict` extracts trailing int / json; `judge_one` runs n times (n=2 for split in {val,test}, else 1), majority + agreement→`label_confidence`. Loop over `results.jsonl`, checkpoint per row → `results_judged.jsonl`.
- [ ] **Step 4:** Run → PASS.
- [ ] **Step 5:** Commit.

### Task 4: `derive_oracle.py` — cheapest passing tier (pure)

**Files:** Create `derive_oracle.py`; add tests
**Interfaces produced:** `derive_oracle(rows, price)->dict{oracle_route,oracle_effort,label_confidence}`.

- [ ] **Step 1: failing test:**
```python
def test_oracle_picks_cheapest_passing():
    from derive_oracle import derive_oracle
    from _common import PRICE
    rows=[{"model":"opus","score":1,"input_tokens":100,"output_tokens":50},
          {"model":"haiku","score":1,"input_tokens":100,"output_tokens":50},
          {"model":"sonnet","score":0,"input_tokens":100,"output_tokens":50}]
    assert derive_oracle(rows, PRICE)["oracle_route"]=="haiku"

def test_oracle_none_passed():
    from derive_oracle import derive_oracle
    from _common import PRICE
    assert derive_oracle([{"model":"haiku","score":0,"input_tokens":1,"output_tokens":1}], PRICE)["oracle_route"]=="none"
```
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3:** Implement: group by task; among `score==1` pick min `cost_usd = in/1e6*price_in + out/1e6*price_out`; ties→cheaper tier order; none→`"none"`; join back to tasks → `tasks_prompt_labeled.jsonl`.
- [ ] **Step 4:** Run → PASS.
- [ ] **Step 5:** Commit.

### Task 5: End-to-end smoke → full run

- [ ] **Step 1:** `python3 gen_tasks.py out/ --n 512` (free).
- [ ] **Step 2:** `python3 run_matrix.py out/ --limit 15` (OAuth smoke; ~60 calls) → inspect `results.jsonl` (4 rows/task, usage populated).
- [ ] **Step 3:** `python3 judge_deepseek.py out/ --limit 15` → `results_judged.jsonl` (scores present).
- [ ] **Step 4:** `python3 derive_oracle.py out/` → inspect oracle-route distribution (expect spread, not all-`none`/all-`opus`).
- [ ] **Step 5:** If smoke healthy: `python3 run_matrix.py out/` (full, resumable) → judge → oracle. Report oracle distribution + quota-consumed.

---

## Self-Review
- **Spec coverage:** gen_tasks(Path1)→Task1 · run_matrix(Path2)→Task2 · judge(Path3)→Task3 · oracle(Path4)→Task4 · smoke/run-order→Task5 · IDs/pricing/OAuth→Task0. All spec sections covered.
- **Placeholders:** none — pure units have real test code; network units have real callers + smoke.
- **Type consistency:** `call_oauth` dict keys, `derive_oracle(rows,price)`, `PRICE`, `JUDGE_MODEL`, `count_tokens` used consistently across tasks.
