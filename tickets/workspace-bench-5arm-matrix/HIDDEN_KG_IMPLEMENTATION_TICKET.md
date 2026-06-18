# Ticket: Hidden KG Retrieval Architecture

## Goal

Build a hidden knowledge-graph retrieval layer that improves `semfs grep` accuracy and reduces agent token usage without exposing graph artifacts to the agent.

The agent should still ask normal questions:

```bash
semfs grep "best selling product revenue conversion rate"
```

SemFS should internally use graph structure to route, expand, and rank candidates, then return ordinary source excerpts.

## Problem

The current KG behavior is split in a way that is not ideal for agent benchmarks:

- `/kg/`, `graph.json`, and `/by-topic` are agent-visible surfaces.
- `SEMFS_COMENTION` is only a small post-rerank co-mention boost.
- KG is not currently used to improve candidate generation.

This means the agent can burn tokens browsing graph artifacts, while the useful KG signal arrives too late to rescue files that never entered the candidate pool.

The core token problem is repeated search:

```text
search -> uncertain result -> search again -> open files -> crawl tree -> search again
```

Hidden KG should reduce that loop by making the first or second grep more likely to contain the right source excerpts.

## Non-Goals

- Do not make the agent browse `/kg`.
- Do not expose community IDs, graph JSON, or graph traversal instructions as normal grep output.
- Do not replace BM25/vector retrieval with KG-only retrieval.
- Do not send an "accuracy score" to the agent. Accuracy is only known after judging.

## Proposed Architecture

Use KG as an internal candidate prior before rerank.

Current pipeline:

```text
BM25 + vector + code vector
-> RRF
-> rerank
-> co-mention boost
-> salience
-> excerpts
```

Target hidden-KG pipeline:

```text
query
-> lightweight query entity matching
-> KG file/community priors
-> BM25 + vector + code vector retrieval
-> RRF with bounded KG prior
-> rerank
-> optional tiny KG consistency boost
-> excerpts
```

The graph should change which candidates become easy to rank, not become content shown to the agent.

## Why This Architecture

Rerankers only sort what they see. If the right file is rank 80 after first-stage retrieval and the reranker only sees the top 50, the right answer is unrecoverable.

KG helps most when it moves likely files into the candidate pool earlier:

- entity overlap can recover files with different wording
- communities can route broad queries to the right neighborhood
- graph neighbors can surface related files that share products, owners, metrics, symbols, or APIs
- giant-community penalties prevent generic clusters from dominating

Use KG as a bounded prior, not as a hard filter, because exact lexical hits for IDs, filenames, constants, and numbers should still win.

## Query Walkthrough

Example query:

```text
Find the best-selling product and report revenue plus conversion rate.
```

Without hidden KG:

```text
query
-> BM25/vector retrieve broad sales/product/campaign files
-> RRF
-> rerank
-> top excerpts may include near-misses
-> agent searches again
```

With hidden KG:

```text
query
-> detect terms/entities: product, revenue, conversion rate, best selling
-> map to graph entities and aliases: GMV, 成交额, 转化率, top product
-> score files in matching product-sales communities
-> normal BM25/vector retrieval with graph prior
-> RRF and rerank
-> top excerpts are more likely to contain exact source values
-> agent transcribes and stops
```

## Implementation Plan

### 1. Add Hidden KG Flag

Add a separate flag:

```bash
SEMFS_HIDDEN_KG=on|off
```

This must be independent from:

- `SEMFS_KG`
- `SEMFS_GRAPH_FS`
- `SEMFS_COMENTION`

Desired semantics:

- `SEMFS_HIDDEN_KG=on`: internal graph priors may affect retrieval/ranking.
- `SEMFS_GRAPH_FS=off`: no `/by-topic` overlay.
- `SEMFS_KG_SURFACE=off` or equivalent future flag: no agent-visible `/kg` docs.

### 2. Add Backend Module

Create:

```text
crates/semfs-core/src/backend/hidden_kg.rs
```

Initial API:

```rust
pub struct KgPrior {
    pub filepath: String,
    pub score: f64,
}

pub fn query_kg_priors(conn: &Connection, query: &str) -> anyhow::Result<HashMap<String, f64>>;
```

The first version should be deterministic and non-LLM.

### 3. Query Entity Matching

Start simple:

- tokenize query
- match tokens and phrases against graph entity names
- include aliases from graph/entity tables if available
- include bilingual metric aliases for PM corpus only if already present in graph data

Avoid expensive LLM query rewriting initially.

### 4. File And Community Prior

Compute a bounded prior for candidate files:

```text
kg_prior(file) =
  entity_overlap_score
+ community_match_score
+ neighbor_file_score
- giant_community_penalty
```

Bound the prior so KG cannot dominate exact lexical evidence.

Suggested initial range:

```text
0.0 <= kg_prior <= 0.15
```

### 5. Integrate Before Rerank

In SQLite search flow:

- candidate generation currently lives in [sqlite_vec.rs](../../crates/semfs-core/src/backend/sqlite_vec.rs)
- RRF/rerank helpers live in [rank.rs](../../crates/semfs-core/src/backend/rank.rs)

Apply hidden KG before rerank, after initial lane aggregation.

Preferred v1:

```text
normal retrieval lanes produce candidates
-> apply bounded KG prior to file-level candidate scores
-> RRF/rerank continues normally
```

Do not make KG a hard filter in v1.

### 6. Keep Co-Mention As A Separate Stage

`SEMFS_COMENTION` should remain a post-rerank consistency nudge.

Hidden KG should not be implemented by just enabling co-mention. Co-mention can stay as the final small graph agreement boost, but the new value is earlier candidate routing.

### 7. Add Observability

Log hidden KG decisions to daemon logs, not to the agent:

```text
HIDDEN_KG query_entities=[...]
HIDDEN_KG top_priors=[/file/a:0.12, /file/b:0.09]
HIDDEN_KG community_hits=[17,23]
```

Add `SEMFS_DEBUG_RANKING` output for before/after candidate rank movement.

### 8. Optional Agent Confidence Header

Do not expose graph details.

If needed, expose only a minimal retrieval sufficiency hint:

```text
# high confidence: top results agree on requested entities
```

This should be gated separately because prompt text can itself change behavior and confound experiments.

## Experiment Design

Use three arms:

```text
plain
best_exp0002
best_exp0002 + hiddenKG
```

Initial proxy today:

```text
hiddenkg = best_exp0002 + SEMFS_COMENTION=on + no graph surface
```

Real hiddenKG after implementation:

```text
hiddenkg = best_exp0002 + SEMFS_HIDDEN_KG=on + no graph surface
```

Run order:

1. Preflight:
   ```bash
   python3 benchmarks/e2b/run_matrix.py --preflight --arms best,hiddenkg --knobs benchmarks/e2b/knobs/best_exp0002.json
   ```
2. Cheap validation:
   - cases: `53,171`
   - arms: `plain,best,hiddenkg`
   - reps: `n=1`
3. Real opinion-forming run:
   - same arms
   - increase reps after validation is clean

## Generalizability To Developer Workspaces

The best architecture for developer workspaces is still hidden internal KG, but the graph entities change.

PM workspace KG entities:

- product names
- revenue fields
- dates
- conversion metrics
- campaign names
- owners

Developer workspace KG entities:

- symbols
- functions/classes
- modules/packages
- imports
- callers/callees
- tests
- config files
- routes/endpoints
- database models

Developer query example:

```text
Where is invoice reconciliation timeout handled and what tests cover it?
```

Without hidden KG:

```text
BM25/vector finds files mentioning invoice/reconciliation/timeout
-> may return docs, logs, config, or unrelated timeout utilities
-> agent searches repeatedly
```

With hidden KG:

```text
query terms map to symbols/modules
-> graph expands definition -> callers -> tests -> config
-> retrieval boosts structurally related files
-> reranker sees the implementation and tests earlier
-> agent opens fewer files and writes a more accurate answer
```

This is more important in code than PM data because code relevance is often structural, not semantic. A file can be relevant because it calls a function, imports a module, or owns a test, even if it does not repeat the exact natural-language query terms.

## Architectural Choice For General Use

Best long-term architecture:

```text
retrieval lanes:
  lexical/BM25
  dense vector
  sparse vector if available
  code/symbol lane
  hidden KG prior

fusion:
  RRF or weighted fusion

ranking:
  reranker
  bounded structural consistency boost

surface:
  source excerpts only
```

Do not build a separate "graph browsing" product into the default agent path. Keep graph browsing as a debug/admin surface.

This generalizes because each workspace can provide different graph edges, while the search contract stays stable:

```text
agent asks normal query
SemFS returns better source excerpts
agent stops sooner
```

## Success Criteria

Hidden KG is working if:

- first or second grep contains the answer more often
- grep count decreases
- file-open/crawl count decreases
- total turns decrease
- accuracy is maintained or improves
- `/kg` and `/by-topic` are not read by the agent

Failure modes:

- KG prior overpowers exact lexical hits
- giant communities boost generic files
- graph stale state routes queries to deleted/renamed files
- confidence header changes behavior enough to confound backend measurement

