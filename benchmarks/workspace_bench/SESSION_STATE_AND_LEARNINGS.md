# semfs × Workspace-Bench — State, Tests, Learnings, Next Steps

_Snapshot: 2026-06-07. Branch: `feat/backend-agnostic-store`. Focus case: Workspace-Bench **289** (chanpin, codex GPT-5.4)._
_Companion docs: `experiments_results.md` (full configs/tokens/retrieval log), `KG_GRAPHIFY_COMPARISON.md` (KG vs graphify)._

---

## 1. TL;DR — where we are

- **semfs cuts cost massively but trades correctness on this task.** vs plain codex: **tokens −76–83%, tool calls 8→2–3**, but **rubric correctness drops 5.3 → ~4–4.7/15**.
- **The tool-call/token-reduction gate is MET** (semfs kg_off ran 2,3,2 calls — 3 consecutive, vs cloud 2,2,2).
- **Correctness was never being measured before this session.** `agent.json status=passed` is *path-existence only*; the real metric is the **rubric pass rate** from `agent_eval.py` / `agent_as_a_judge.py`. We wired it up.
- **The KG is a net negative on 289** (more tokens, no correctness gain) — consistent with the retrieval matrix (KG has no rank effect when the answer is already top-ranked).
- **289 is an *error-detection* task**, not a copy task — the correct answer reports that the source `.xlsx` is a **403 Forbidden** HTML page. This reframes everything (see Learnings).

---

## 2. Tests done

### 2.1 Four-condition graded experiment (n=3 each, judge = `claude-sonnet-4.6` via OpenRouter)

| condition | avg tokens | avg tool calls | rubrics (3 reps) | avg rubrics | format-traps |
|---|---|---|---|---|---|
| **plain codex (no semfs)** | **108.4K** | **8.0** | 6 / 6 / 4 | **5.3/15** | 2–3 / run |
| semfs kg_off | 25.6K | 2.3 | 4 / 5 / 4 | 4.3/15 | 0 |
| semfs kg_on | 41.6K | 3.3 | 4 / 5 / 5 | 4.7/15 | 0 |
| cloud (Supermemory) | 18.9K | 2.0 | 3 / 4 / 4 | 3.7/15 | 0 |

Per-rep tokens/calls: kg_off 24.2K/2, 28.4K/3, 24.1K/2 · kg_on 40.2K/4, 55.6K/4, 29.1K/2 · cloud 18.5K/2, 19.7K/2, 18.4K/2 · plain 83.2K/8, 110.7K/9, 131.2K/7.

### 2.2 Retrieval matrix (earlier, 3× each — see `experiments_results.md` §2)
Answer file ranks **FINAL #0 in every viable config** (e5/gemma/supermemory embedders; Local/Cohere rerankers; BM25/sparse; KG on/off). The one retrieval fix that mattered: the **path-token lane** (#417 → #1). Sparse only helps if multilingual (BGE-M3); Cohere rerank ~35× faster than Local.

### 2.3 Correctness grading validated
- `agent_eval.py` (chat-completions judge) works via OpenRouter with **gpt-5.4 (5/15)**, **claude-sonnet-4.6 (6/15)** on the identical copy-list baseline → judge model shifts ±1–2 rubrics.
- `agent_as_a_judge.py` (canonical agentic ClaudeCode judge) **blocked** — needs an Anthropic-compatible endpoint (no Anthropic key / OAuth on box).

---

## 3. What shipped (commits on `feat/backend-agnostic-store`)

| commit | what |
|---|---|
| `10cefd1` | Surface inaccessible/error-page sources (annotate-not-drop 403s; path-pin; small-file inline) + `experiments_results.md` with the correctness-grading discovery |
| `34430bd` | graphify-parity **Leiden oversized-community split** + `KG_GRAPHIFY_COMPARISON.md` |
| `6dbf7dc` | **`graph.json`** artifact + codex-exec **stdin-bug mitigation** (PTY + positional prompt) in vendored harness |
| `b344b6c` | KG comparison status update |
| `1dc9ba8` | Four-condition graded results (§10 of `experiments_results.md`) |

Tests: 296 core lib + 21 grep, all green.

---

## 4. Key learnings

1. **Measure correctness, not completion.** `status=passed` = "the file you named exists." It is **not** correctness. Real metric = rubric pass rate (LLM-as-judge over the task's 15 rubrics in `metadata.json`). The whole prior token race was between *unverified* answers.

2. **289 is corrupted-source detection, not copy.** The 3 `.xlsx` are 403 HTML error pages on purpose; the rubric-correct output **reports the 403** (HTML-not-Excel, access-denied). A correctly-named `best_selling_product_core_data_list.txt` with real data is planted as a decoy. Copying the decoy = 5/15; reporting the 403 well ≈ ceiling **~10/15** ([5][6] want `./output_cc` path the harness blocks; [8][9][10] are an embedded "edit metadata.json" meta-task — both structurally unwinnable here).

3. **THE big finding — efficiency vs error-discovery trade-off.** Plain codex scores *higher* (5.3) *because* it wastes tokens: its `os.walk` + `pandas`-open attempts (format-traps) **fail on the 403 `.xlsx`, which is how it learns the source is broken** → it reports it. semfs makes codex grep-only/efficient → it never opens the broken file → never discovers the 403 → copies the list excerpt → 4–5/15. **semfs trades the exploration-that-finds-the-error for token efficiency.**

4. **KG doesn't help retrieval-saturated tasks.** When base retrieval already returns the answer at #0, the KG adds tokens (the agent reads `KNOWLEDGE_GRAPH.md` + extra greps) with no rank/correctness benefit. KG value is for queries where retrieval *misses* — not 289.

5. **Judge model matters.** gpt-5.4 vs claude-4.6 differ ±1–2 rubrics on identical output → always label the judge; treat scores as *relative under one judge*, not absolute leaderboard numbers.

6. **The token lever is codex's behavior, not the retrieval stack.** Same first-grep output → codex sometimes trusts+stops (2–3 calls), sometimes re-explores (9–12). Confirmed by identical-input runs diverging.

---

## 5. Known limitations / blockers

- **codex 0.133.0 `exec_command` stdin bug** — intermittent `write_stdin failed: stdin is closed for this session`; degenerates multi-command turns into 1-command stubs. PTY + positional-prompt mitigation in vendored `codex.py` helped but did **not** fully fix (it's codex-internal; no config toggle found). Pollutes ~20–40% of reps.
- **Canonical judge needs Anthropic creds.** `agent_as_a_judge.py` runs the judge through ClaudeCode.js (Anthropic Messages wire format + native model ids) → can't use OpenRouter. Needs `ANTHROPIC_API_KEY` or `claude login` OAuth on the box. We used `agent_eval.py` + claude-via-OpenRouter as a proxy.
- **Paper's judge = `seed-2.0-lite`** (ByteDance), confirmed in the paper. It **is** on OpenRouter (`bytedance-seed/seed-2.0-lite`) — a 1-line yaml swap for exact judge-model parity via the `agent_eval.py` path (non-agentic mechanism).
- **graphify parity is partial** — done: Leiden oversized-split, `graph.json`, exact comparison. **Not done:** typed **entity→entity** relations (biggest gap; needs LLM re-extraction), `AMBIGUOUS` confidence level, tree-sitter AST code lane.
- **API keys exposed this session** (my mistakes): OpenRouter key (`${VAR:-NO}` printed value) and Supermemory key (`pgrep -fa` showed daemon `--key` argv). **Both should be rotated.** Side note: semfs passes keys on argv (world-readable via `ps`) — worth moving to env/stdin.

---

## 6. Potential next steps (by leverage)

1. **Close the correctness gap without losing the token win (highest value).**
   Make the 403 annotation surface for the query codex *actually* issues. Today the path-lane pin fires for `top10 product`-style queries, but codex queries `best-selling product data` → gets the list. Options:
   - Broaden the error-page pin: always surface a query-matching error-page source (≥1–2 path-token overlap), or
   - Protocol nudge: "verify the named source's accessibility before reporting data."
   Then re-run the four-condition graded experiment; target: semfs correctness ≥ plain (5.3) at semfs token cost.

2. **Exact judge parity:** swap judge yaml to `bytedance-seed/seed-2.0-lite` (the paper's judge) via OpenRouter, re-grade the four conditions. One-line change; gives paper-comparable absolute numbers.

3. **Validate across the dataset, not just 289.** Grade plain vs kg_off vs kg_on vs cloud on the full smoke/Lite set to see where semfs helps vs hurts correctness generally (289 may be an adversarial outlier). Wire `agent_eval.py` into `run_workspace_bench.sh` so every run is auto-graded.

4. **Finish graphify parity (if KG quality is the goal):** typed entity→entity relation extraction (relation ontology + confidence EXTRACTED/INFERRED/AMBIGUOUS + source) → enables real "surprising connections" + a true entity graph; then AST code lane.

5. **Harden the harness:** escalate/work around the codex stdin bug (e.g. pin codex version, or retry degenerate runs), so experiments aren't polluted.

6. **Security:** rotate the exposed keys; move semfs key passing off argv.

---

## 7. Reproduce / where things live (EC2 box `ubuntu@13.201.35.159`)
- Drivers: `/tmp/run289.sh` (semfs kg_off/kg_on/cloud), `/tmp/plain289.sh` (no-semfs), `/tmp/graded_batch.sh`, `/tmp/plain_after.sh`, `/tmp/parse289.py`, `/tmp/cmd_seq.py`.
- Judge yamls: `/tmp/judge_claude_eval.yaml` (claude via OpenRouter), `/tmp/judge_real.yaml` (gpt-5.4). Key injected from `~/.semfs_seed_env` env, never echoed.
- Grade a run: `python3 evaluation/src/agent_eval.py --task-dir <OUT> --eval-yaml <yaml> --overwrite`.
- Seeds (intact): `~/.semfs/chanpin-gemma.db` (KG built: 909 edges/763 entities), e5-nosum cache (126 edges).
- Repo: `/srv/semfs-benchmark/semantic-filesystem` (rebuild: `cargo build --release -p semfs && cp target/release/semfs ~/.local/bin/semfs`; unmount daemons first — text-busy).
