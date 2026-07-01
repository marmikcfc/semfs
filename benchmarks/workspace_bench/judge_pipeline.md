# Workspace-Bench Judge Pipeline

Reference for future agent sessions. Covers architecture, env vars, models, invocation, and output schema.

---

## What it is

The judge is an **agent-as-a-judge**: a Claude Code agent that inspects candidate output files and scores rubrics — not a static diff or regex scorer. This matters for Workspace-Bench because tasks produce open-ended deliverables (Excel files, reports, data transforms) that require semantic understanding to evaluate.

---

## Key files

| File | Role |
|---|---|
| `evaluation/src/agent_as_a_judge.py` | Judge entry point. `evaluate_task()` per case. |
| `evaluation/src/agent_runner.py` | Orchestrates agent runs; optionally calls judge inline via `eval_while_running`. |
| `evaluation/runs/judge.yaml` | Judge model config — all values are env-var templates. |
| `evaluation/baselines/ClaudeCode.js` | Node runner that executes the judge agent via `@anthropic-ai/claude-agent-sdk`. |
| `evaluation/src/agents/claudecode.py` | Python shim that shells out to `ClaudeCode.js`; called by both runner and judge. |
| `benchmarks/aws/run_workspace_bench.sh` | Top-level EC2 script; sources `benchmark.env`, selects agent+model, invokes runner. |
| `evaluation/scripts/build_run_config.py` | Generates per-run YAML config with model/env-var wiring. |

---

## Pipeline flow

```
run_workspace_bench.sh  (sources benchmark.env, picks target)
    └─ agent_runner.py  (iterates tasks, runs agent under test)
           ├─ agents/claudecode.py → ClaudeCode.js  (runs agent on task)
           └─ if eval_while_running=true:
                  agent_as_a_judge.py:evaluate_task()
                      └─ agents/claudecode.py → ClaudeCode.js  (runs judge on outputs)
```

Judge can also be run as a separate post-hoc pass:
```bash
python3 src/agent_as_a_judge.py \
    --task-dir /path/to/output/Codex--GPT-5.4--smoke \
    --eval-yaml runs/judge.yaml \
    --parallel --workers 3
```

---

## Environment variables

All variables live in `/srv/semfs-benchmark/benchmark.env` on the EC2 host (never committed). Sourced by `run_workspace_bench.sh` at startup.

### Always required

| Variable | Purpose |
|---|---|
| `OPENROUTER_API_KEY` | LLM calls for agent + judge (via OpenRouter) |
| `SUPERMEMORY_API_KEY` | Required for `semfs-*` targets only |
| `CODEX_SANDBOX_MODE` | Defaults to `danger-full-access` |

### Judge-specific

| Variable | Purpose |
|---|---|
| `JUDGE_MODEL` | Model slug for the judge agent |
| `JUDGE_BASE_URL` | Endpoint for the judge |
| `JUDGE_API_KEY` | API key for the judge |

### Per-agent (prefix derived from model)

`build_run_config.py` generates these; prefix is e.g. `SONNET46` for Claude, `GPT54` for Codex.

| Pattern | Example | Purpose |
|---|---|---|
| `{PREFIX}_API_KEY` | `SONNET46_API_KEY` | Agent API key |
| `{PREFIX}_BASE_URL` | `SONNET46_BASE_URL` | Agent endpoint |
| `{PREFIX}_ANTHROPIC_BASE_URL` | `SONNET46_ANTHROPIC_BASE_URL` | Anthropic-specific override (falls back to BASE_URL) |
| `{PREFIX}_ANTHROPIC_MODEL` | `SONNET46_ANTHROPIC_MODEL` | Override model slug |

---

## Models in use

| Target | Agent model | Slug |
|---|---|---|
| `codex` | GPT-5.4 | `openai/gpt-5.4` |
| `semfs-codex` | GPT-5.4 | `openai/gpt-5.4` |
| `claudecode` | Claude Sonnet 4.6 | `anthropic/claude-sonnet-4.6` |
| `semfs-claudecode` | Claude Sonnet 4.6 | `anthropic/claude-sonnet-4.6` |

The **judge model** is set independently via `JUDGE_MODEL` — it is never hardcoded.

---

## Sandbox isolation (critical correctness gate)

Before calling the judge, `_prepare_judge_view()` builds a restricted workspace under `task_dir/raw/agent_as_a_judge/try_N/judge_view/` containing only:

- `inputs/` — symlink to original task input files (NOT ground truth)
- `candidate_output/` — symlink to the agent's outputs
- `original_task_metadata.json` — sanitized metadata (rubrics, task description, no GT paths)
- `README.txt` — explicit instruction to the judge not to use any other directories

The judge's `cwd` is set to `judge_view/`. It **cannot** see `output/`, `output_cc/`, or `gt/` directories from the task.

---

## Judge prompt schema

The prompt is a JSON blob (in Chinese — upstream prompt language) sent to the judge containing:

```json
{
  "taskId": "...",
  "task": "...",
  "steps": [...],
  "rubrics": ["rubric text 0", "rubric text 1", ...],
  "judgeView": {
    "cwd": "/path/to/judge_view",
    "inputsPath": "...",
    "candidateOutputPath": "..."
  }
}
```

Required output from the judge (parsed by `_json_first_object()`):

```json
{
  "rubrics": [
    {"index": 0, "passed": true,  "confidence": 0.9, "evidence": "checked file X, found Y"},
    {"index": 1, "passed": false, "confidence": 0.8, "evidence": "file Z missing"}
  ]
}
```

---

## Output files (per task directory)

| File | Contents |
|---|---|
| `rubrics_judge--{model_name}.json` | Full rubric scores, evidence, token usage, timing, judge metadata |
| `dependency_graph--{model_name}.json` | File dependency graph for the task (built alongside scoring) |
| `raw/agent_as_a_judge/try_N/` | Per-retry judge sandbox artifacts |

### `rubrics_judge` schema summary

```json
{
  "taskId": "...",
  "agentKind": "...",
  "createdAt": "...",
  "rubrics": [{"index": 0, "rubric": "...", "passed": true, "confidence": 0.9, "evidence": "..."}],
  "summary": {"total": 3, "passed": 2, "failed": 1},
  "judge": {
    "model": "...", "modelName": "...", "baseUrl": "...",
    "usage": {...}, "durationMs": 12000, "tries": 1, "error": null
  }
}
```

---

## Retry logic

The judge retries up to `max_retries=6` (exponential backoff, capped at 60s) until `_json_first_object()` finds a valid JSON object with a `rubrics` list in the agent's final text. If all retries fail, every rubric is marked `passed=false` with the error as evidence.

---

## Parallelism

- `agent_runner.py` groups tasks by `file_system` field and runs groups in parallel via `ThreadPoolExecutor(max_workers=5)` — tasks within a group run sequentially (shared workdir).
- `agent_as_a_judge.py` (standalone mode) parallelises across task dirs: `--parallel --workers N`.

---

## Quick invocation reference

```bash
# Run a single target (sources benchmark.env automatically)
DATASET=smoke ./benchmarks/aws/run_workspace_bench.sh codex
DATASET=smoke ./benchmarks/aws/run_workspace_bench.sh semfs-claudecode

# Judge a completed run post-hoc
cd benchmarks/vendor/Workspace-Bench/evaluation
python3 src/agent_as_a_judge.py \
    --task-dir output/ClaudeCode--Claude-Sonnet-4.6--smoke \
    --eval-yaml runs/judge.yaml \
    --parallel --workers 3

# Force fresh (wipe semfs cache + prior output)
SEMFS_FRESH=1 DATASET=smoke ./benchmarks/aws/run_workspace_bench.sh semfs-codex
```
