# Router benchmark dataset — split & arrangement spec (SEM-52)

Design the dataset BEFORE generation. Two versions (prompt-tuning small, fine-tuning large),
arranged **CodeRouterBench-style** (long-format matrix), with a proper task split.

## 0. Sources & their role

| Source | Role |
|---|---|
| nvidia/Open-SWE-Traces (207K) | tasks + deep multi-turn context (bug/feature/refactor) — relabel |
| lambda/hermes-agent-...traces (14.7K) | agentic tool tasks + multi-turn context — relabel |
| HumanEval / MBPP / BigCodeBench | `code_gen` prompts |
| Lance1573/CodeRouterBench | `code_gen` **difficulty oracle** (8-model pass pattern; join by task_id) |
| pmarmik/cc-testset + synth | easy tasks (label-entropy floor) |
| WildChat-4.8M | general Q&A + generic long-context (not dev) |
| **semianalysisai (25 Weka)** | **CALIBRATION ONLY** — real context-token-size, turns/session, model-mix. No task text. |

## 1. Task split — CREATE it (CodeRouterBench has only `code_generation`)

**8 dimensions** (the router's decision surface) + **2 orthogonal axes**:

| Dimension | Difficulty skew | Primary source |
|---|---|---|
| qa_explain | easy | cc-testset · WildChat · synth |
| small_edit | easy | cc-testset · Open-SWE 1-file · synth |
| code_gen | mixed | HumanEval/MBPP/BigCodeBench (+ CodeRouterBench difficulty) |
| single_file_feature | medium | Open-SWE small patches · hermes |
| test_or_review | medium | hermes · Open-SWE |
| multi_file_refactor | strong | Open-SWE multi-file |
| root_cause_debug | strong | Open-SWE bug · CodeRouterBench-hard |
| perf_optimization | strong | kernelbook |

**Axis 1 — difficulty** (the tier the task needs): easy / medium / hard.
**Axis 2 — context_bucket (TOKEN size, calibrated to cc-traces-weka, DECOUPLED from difficulty):**

| bucket | tokens | share | note |
|---|---|---|---|
| fresh | <1k | 25% | single-turn |
| small | 1–16k | 25% | a few turns |
| medium | 16–64k | 20% | |
| large | 64–256k | 20% | cc-traces tail begins |
| xlarge | >256k | 10% | real production median is ~306k; kept minority to preserve label entropy |

> **Decoupling is mandatory:** assign context size INDEPENDENTLY of difficulty → generate
> `easy@xlarge` and `hard@fresh` cells. cc-traces shows context size *alone* drives escalation
> (>64k ⇒ ~100% opus); if context ⟂ difficulty are entangled, the router can't learn it.
> Build `prior_context` by accumulating real trace turns / repo dumps to hit the token budget.

## 2. CodeRouterBench-style arrangement (two tables)

**`tasks`** (one row per task — the "base dataset"):
```
task_id · dimension · difficulty · context_bucket · split · source_dataset ·
prompt · prior_context · context_tokens · meta ·
oracle_route · oracle_effort · label_confidence        (derived after the matrix)
```

**`results`** (long-format matrix — CodeRouterBench layout, one row per task × model × effort):
```
task_id · dimension · difficulty · context_bucket · split · model · effort ·
score(0/1) · cost_usd · input_tokens · cached_tokens · output_tokens · total_tokens ·
latency_ms · cost_source · n_judges
```
Models = {haiku, sonnet, opus, fable}; effort = supported grid per model.

## 3. The split — counts

Both versions: **difficulty ≈ easy 35 / med 35 / hard 30 (%)**, **context per the 25/25/20/20/10 table**, decoupled. Per-dimension counts skew by the table above.

### Prompt-tuning (DSPy) — GOLD labels (n≥2 judge)
- **~64 tasks/dimension × 8 = ~512 tasks**
- split **train 60% (~307) / val 20% (~102) / test 20% (~103)**
- matrix `results` ≈ 512 × 4 models × ~3 effort ≈ **~6,100 rows** (+ n≥2 judge on val/test)

### Fine-tuning (SFT) — volume, weaker labels OK
- **~384 tasks/dimension × 8 = ~3,072 tasks** (scalable to 10K by raising caps + WildChat)
- split **train 80% / val 10% / test 10%**
- matrix `results` ≈ 3,072 × 4 × ~3 ≈ **~37K rows** (SFT may use single-effort + single-judge to cut cost)
- **prompt-set ⊂ fine-tune pool** — hold the prompt-set eval slice OUT of SFT training.

## 4. Calibration from semianalysisai (Weka, deduped)
Fit the `context_bucket` token thresholds + the "context→escalation" prior + the model-mix
baseline from the aggregated Weka traces. This is a distribution fit, not task ingestion.
