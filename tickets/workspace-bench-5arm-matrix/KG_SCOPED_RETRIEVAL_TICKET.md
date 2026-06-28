> <!-- STALE-BANNER --> ⚠️ **SUPERSEDED (2026-06-25)** — this hidden-KG design SHIPPED as code (`crates/semfs-core/src/backend/hidden_kg.rs`, `SEMFS_HIDDEN_KG` / `SEMFS_KG_PPR`). Kept for design history. Current → [/CURRENT_STATE.md](../../CURRENT_STATE.md) · [PPR EXPERIMENT.md](../wblite-ppr-ab/EXPERIMENT.md).

# Ticket: KG-Scoped Retrieval

## Objective

Implement a second, more invasive hidden KG retrieval architecture after the KG candidate lane has been tested.

KG-scoped retrieval should route the query to likely graph communities or entity neighborhoods before retrieval, then search those neighborhoods more deeply than the rest of the corpus. This is not required to run arms 8 and 9 initially.

## Why This Is Separate From KG Candidate Lane

KG candidate lane asks:

```text
Can KG add useful files into the normal candidate pool?
```

KG-scoped retrieval asks:

```text
Can KG reduce the search space and spend retrieval budget only where the answer is likely to be?
```

The second question is higher-risk because a wrong route can suppress the correct answer. It also needs more retrieval changes than simply adding a fifth candidate lane.

## Target Architecture

Current retrieval:

```text
global BM25/vector/path/code retrieval
-> RRF
-> rerank
-> optional L7 co-mention
-> excerpts
```

KG candidate lane:

```text
global BM25/vector/path/code retrieval
+ KG candidate lane
-> RRF
-> rerank
-> optional L7 co-mention
-> excerpts
```

KG-scoped retrieval:

```text
query
-> KG route: top entities, top communities, route confidence
-> deep retrieval inside matched communities
-> shallow global fallback retrieval
-> merge scoped + global pools
-> RRF
-> rerank
-> optional L7 co-mention
-> excerpts
```

## Expected Value

KG-scoped retrieval should help when the corpus has many semantically similar files and the agent is repeating searches because top results are close but wrong.

The intended improvements are:

- fewer repeated `semfs grep` calls
- fewer irrelevant excerpts in the first result page
- lower token usage from less search/open/crawl behavior
- equal or better task accuracy if fallback retrieval prevents over-scoping failures

## Non-Goals

- Do not expose `/kg`, `graph.json`, `/by-topic`, community IDs, or graph reasoning to the agent.
- Do not make KG a hard-only filter.
- Do not replace BM25/vector retrieval.
- Do not implement adaptive `k` before route confidence exists.
- Do not use LLM query rewriting in v1.

## Dependencies

- KG candidate lane implemented and evaluated.
- Evidence that retrieval noise or candidate miss remains a limiting factor.
- Route confidence metrics from hidden KG entity/community matching.
- Reliable graph tables in the clean E2B DB.

## Implementation Phases

### Phase 1: Route Query To Communities

Extend `hidden_kg.rs` with:

```rust
pub struct KgRoute {
    pub entities: Vec<String>,
    pub communities: Vec<KgCommunityRoute>,
    pub confidence: f64,
}

pub struct KgCommunityRoute {
    pub community_id: i64,
    pub score: f64,
    pub size: usize,
}

pub fn route_query(
    conn: &rusqlite::Connection,
    query: &str,
    scope: Option<&str>,
) -> anyhow::Result<KgRoute>;
```

Route inputs:

- exact or phrase matches against `graph_entity.name`
- files connected to matched entities through `edges`
- communities from `graph_community`
- optional community anchors from `graph_god_node`

Route confidence should be low when:

- no entities match
- only generic entities match
- matched communities are extremely large
- top communities have flat scores

Acceptance:

- Low-confidence route triggers global fallback behavior.
- High-confidence route returns top 1 to 3 communities.
- Routing logs are available under `SEMFS_DEBUG_RANKING=1`.

### Phase 2: Add Scoped Candidate Pools

Add retrieval pools that are aware of matched communities:

```text
FTS within matched communities
path matching within matched communities
KG candidate lane within matched communities
global BM25/vector/path fallback
```

Vector retrieval is harder because sqlite-vec is naturally global. Start with one of these:

- Overfetch globally, then post-filter/boost matched-community files.
- Add a later exact-cosine path over embeddings for matched-community files only.

Do not block v1 on perfect scoped vector search.

Acceptance:

- Scoped FTS/path candidates can enter the candidate pool.
- Global fallback candidates still enter the pool.
- Exact filename, path, ID, and number queries still work when KG route confidence is low.

### Phase 3: Merge Scoped And Global Pools

Use bounded merge budgets:

```text
scoped community pool: larger budget when confidence is high
global fallback pool: always non-zero
KG candidate lane: capped
```

Example starting point:

```text
high confidence:
  70% scoped / 30% global

medium confidence:
  50% scoped / 50% global

low confidence:
  0% scoped / 100% global
```

Keep the final candidate count similar to current retrieval so rerank cost does not explode.

Acceptance:

- Route confidence changes retrieval budget allocation.
- Low-confidence route is behaviorally close to current global retrieval.
- High-confidence route increases matched-community candidates without removing all global fallback.

### Phase 4: Optional Adaptive K

Adaptive `k` should be a later lever, not the first scoped-retrieval feature.

Use it only after route confidence is measurable:

```text
if route confidence high:
  increase k inside top communities
  reduce global k modestly

if route confidence low:
  keep default global k
```

Acceptance:

- Adaptive `k` is disabled by default behind a separate flag.
- Metrics show candidate quality improves before enabling it in benchmark arms.

### Phase 5: Harness Arms

Add separate future arms only after the feature is implemented:

```text
hiddenkg_scoped
hiddenkg_scoped_l7
```

Expected env:

```bash
SEMFS_HIDDEN_KG_SCOPED_RETRIEVAL=on
SEMFS_GRAPH_FS=off
SEMFS_KG=off
```

L7 toggle:

```bash
SEMFS_COMENTION=off
SEMFS_COMENTION=on
```

## Tests

Add tests for:

- query-to-community routing
- route confidence high/medium/low cases
- giant-community penalty
- low-confidence global fallback
- scoped FTS/path candidate generation
- exact path/numeric query fallback behavior
- no agent-visible graph output

Verification:

```bash
cargo test -p semfs-core
python3 -m py_compile benchmarks/e2b/run_matrix.py benchmarks/e2b/cell_driver.py
```

## Acceptance Criteria

- KG route is computed without exposing graph artifacts.
- Scoped retrieval can increase candidate depth in top communities.
- Global fallback remains active.
- Low-confidence routing behaves close to current retrieval.
- Accuracy does not regress on exact/path/numeric queries.
- Token usage improves only if accuracy is maintained or improved.

## Risks

- Wrong community routing can hide the answer.
- sqlite-vec scoped vector retrieval may require overfetching or a new exact-cosine path.
- Too many knobs can make experiment attribution hard.
- If the graph has noisy or giant communities, scoping may amplify bad structure.

## When To Implement

Do not implement this before the candidate-lane experiment unless there is clear evidence that reranker/candidate noise remains the bottleneck.

Implement this next if:

- arms 8/9 show positive signal but still return too many near-misses
- KG candidate lane improves recall but increases rerank noise
- developer-workspace tasks need symbol/community routing beyond file-level candidate injection
- repeated search remains the main token driver after candidate lane
