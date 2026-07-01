> <!-- STALE-BANNER --> ⚠️ **SUPERSEDED (2026-06-25)** — this hidden-KG design SHIPPED as code (`crates/semfs-core/src/backend/hidden_kg.rs`, `SEMFS_HIDDEN_KG` / `SEMFS_KG_PPR`). Kept for design history. Current → [/CURRENT_STATE.md](../../CURRENT_STATE.md) · [PPR EXPERIMENT.md](../wblite-ppr-ab/EXPERIMENT.md).

# KG Candidate Lane Implementation Plan

## Objective

Implement hidden KG as a retrieval-time candidate lane so arms 8 and 9 can be tested.

This is the first implementation of "KG in retrieval proper": KG must be able to introduce files into the first-stage candidate pool before RRF, rerank, and optional L7 co-mention. It must not expose `/kg`, graph JSON, community IDs, or graph explanations to the agent.

## Experiment Arms Unblocked

### Arm 8

```bash
SEMFS_HIDDEN_KG_RETRIEVAL=on
SEMFS_COMENTION=off
SEMFS_GRAPH_FS=off
SEMFS_KG=off
```

Uses:

```text
compress + dedup + prompt + hidden KG candidate lane
```

### Arm 9

```bash
SEMFS_HIDDEN_KG_RETRIEVAL=on
SEMFS_COMENTION=on
SEMFS_GRAPH_FS=off
SEMFS_KG=off
```

Uses:

```text
compress + dedup + prompt + hidden KG candidate lane + L7 co-mention
```

## Current State

Current SQLite retrieval:

```text
text vector + code vector + FTS + path lane + integrity lane
-> by_file
-> optional SEMFS_HIDDEN_KG bounded file prior
-> rank::to_hits
-> rerank
-> optional SEMFS_COMENTION / L7 salience
-> excerpts
```

Current `SEMFS_HIDDEN_KG=on` only reorders files that are already present in `by_file`. It cannot rescue a graph-relevant file that lexical/vector retrieval missed.

Target candidate-lane retrieval:

```text
text vector + code vector + FTS + path lane + integrity lane + KG candidate lane
-> RRF
-> optional SEMFS_HIDDEN_KG bounded file prior
-> rank::to_hits
-> rerank
-> optional SEMFS_COMENTION / L7 salience
-> excerpts
```

## Design Constraints

- SQLite backend only for this implementation.
- No graph files or graph summaries should be mounted for the agent.
- No KG text should be returned in `semfs grep` output.
- KG is a soft candidate source, not a hard filter.
- Exact BM25/path/code-symbol hits must still win over weak KG evidence.
- The implementation must tolerate missing graph tables and continue normal retrieval.
- Candidate caps must stay small enough for the E2B 8 GB template.

## Flag Semantics

Add:

```bash
SEMFS_HIDDEN_KG_RETRIEVAL=on|off
```

Recommended semantics:

- `SEMFS_HIDDEN_KG_RETRIEVAL=off`: current behavior.
- `SEMFS_HIDDEN_KG_RETRIEVAL=on`: KG can add files into the candidate pool through a dedicated lane.
- `SEMFS_HIDDEN_KG=on`: existing bounded prior for files already in the pool.
- `SEMFS_COMENTION=on`: existing L7 post-rerank co-mention/salience step.

For the 9-arm matrix, arms 8 and 9 should set `SEMFS_HIDDEN_KG_RETRIEVAL=on`. Keep `SEMFS_HIDDEN_KG` explicit in the harness rather than silently coupling it to the retrieval flag, so experiments can isolate candidate injection from prior-only reranking if needed.

## Implementation Steps

### 1. Add KG Lane To Ranking

Modify:

```text
crates/semfs-core/src/backend/rank.rs
```

Add a fifth lane:

```rust
pub enum Lane {
    Text = 0,
    Code = 1,
    Fts = 2,
    Path = 3,
    Kg = 4,
}

pub const N_LANES: usize = 5;
```

KG candidates should enter `FileAcc` through the existing `rrf_bump` path, not by directly mutating final scores. This keeps KG comparable to other retrieval lanes and makes ranking behavior easier to reason about.

Verification:

- Existing RRF tests still pass.
- New test proves `Lane::Kg` contributes one bounded RRF vote.
- New test proves a strong exact lexical/path hit is not dominated by a low-rank KG hit.

### 2. Extend Hidden KG API

Modify:

```text
crates/semfs-core/src/backend/hidden_kg.rs
```

Add a candidate API alongside the existing prior API:

```rust
pub struct KgCandidate {
    pub filepath: String,
    pub reason: KgCandidateReason,
    pub score: f64,
}

pub enum KgCandidateReason {
    DirectEntity,
    Community,
    NeighborEntity,
}

pub struct KgCandidateResult {
    pub candidates: Vec<KgCandidate>,
    pub matched_entities: Vec<String>,
    pub matched_communities: Vec<i64>,
}

pub fn query_kg_candidates(
    conn: &rusqlite::Connection,
    query: &str,
    scope: Option<&str>,
    limit: usize,
) -> anyhow::Result<KgCandidateResult>;
```

The function should be deterministic and non-LLM.

Verification:

- Query with no entity match returns an empty result.
- Missing graph tables return an empty result, not an error that breaks search.
- Scope filtering excludes files outside the requested path prefix.

### 3. Generate KG Candidates

Use the existing graph tables:

```text
graph_entity(path, name, kind, file_type, source_file, rationale)
edges(from_path, to_path, edge_kind, created_at, confidence)
graph_community(file_path, community_id, is_primary)
graph_god_node(community_id, entity_path, rank)
```

Candidate sources:

- Direct entity files: files connected to matched entities through `edges`.
- Community files: files in communities containing direct entity files.
- Neighbor files: files sharing graph entities with direct files.

Initial caps:

```text
matched entities      <= 32
direct files          <= 40
community files       <= 80, max 8 per community
neighbor files        <= 40
final KG candidates   <= 80
```

Initial scoring:

```text
DirectEntity   highest
NeighborEntity medium
Community      lower
```

Apply a giant-community penalty before final sorting. Large communities are useful for recall but dangerous for precision.

Verification:

- Direct entity candidate ranks above community-only candidate.
- Community cap is enforced.
- Giant-community penalty reduces, but does not necessarily remove, broad-cluster files.

### 4. Select Representative Chunks

KG is file-level, but the rest of the pipeline expects chunk-level evidence.

For each KG candidate file:

1. Prefer the best FTS chunk for the query within that file.
2. Fall back to the first indexed chunk by `MIN(id)`.
3. Skip the file if no chunk exists.

The chosen chunk must populate `rep_chunk` correctly so phase-2 revalidation and reranking can still read real source text.

Verification:

- KG candidate with an FTS chunk uses that chunk.
- KG candidate without an FTS hit falls back to a real indexed chunk.
- Candidate with no chunk is skipped.

### 5. Wire Into SQLite Retrieval

Modify:

```text
crates/semfs-core/src/backend/sqlite_vec.rs
```

Insertion point:

```text
after vector/code/FTS/path/integrity lanes populate by_file
before current SEMFS_HIDDEN_KG prior block
before rank::to_hits()
```

Flow:

```text
if hidden_kg::retrieval_enabled() {
    result = hidden_kg::query_kg_candidates(conn, query, scope, 80)
    for candidate in result.candidates {
        select representative chunk
        rank::rrf_bump(by_file, Lane::Kg, candidate rank, representative chunk)
    }
}
```

Failure behavior:

- Warn in debug logs and continue normal retrieval.
- Never return empty results just because KG tables are malformed.
- Never print KG reasons in grep output.

Verification:

- With flag off, search output is unchanged except for unrelated nondeterminism.
- With flag on, a graph-linked file can appear even when it was absent from baseline `by_file`.
- L7 on/off changes only post-rerank behavior, not whether KG can inject candidates.

### 6. Harness Wiring

Modify:

```text
benchmarks/e2b/run_matrix.py
benchmarks/e2b/cell_driver.py
```

Add arms:

```text
hiddenkg_retrieval
hiddenkg_retrieval_l7
```

Expected env:

```bash
SEMFS_GRAPH_FS=off
SEMFS_KG=off
SEMFS_HIDDEN_KG_RETRIEVAL=on
```

Arm-specific env:

```bash
# arm 8
SEMFS_COMENTION=off

# arm 9
SEMFS_COMENTION=on
```

Use the same clean DB as the current hidden KG reranking arms:

```text
/opt/chanpin-4arm.db
```

Verification:

```bash
python3 -m py_compile benchmarks/e2b/run_matrix.py benchmarks/e2b/cell_driver.py
```

### 7. Observability

When `SEMFS_DEBUG_RANKING=1`, log:

```text
HIDDEN_KG_RETRIEVAL matched_entities=[...]
HIDDEN_KG_RETRIEVAL matched_communities=[...]
HIDDEN_KG_RETRIEVAL candidate_counts direct=... community=... neighbor=...
HIDDEN_KG_RETRIEVAL injected=[/file/a:DirectEntity,/file/b:Community]
```

Do not print these details to grep output.

## Tests

Add tests for:

- `retrieval_enabled()` flag parsing.
- Missing graph tables degrade cleanly.
- Direct entity candidates are returned.
- Community candidates are capped.
- Scope filtering works.
- `Lane::Kg` contributes to RRF.
- KG can add a file that was not already in the normal candidate set.
- L7 on/off does not control candidate injection.

Preferred verification commands:

```bash
cargo test -p semfs-core
python3 -m py_compile benchmarks/e2b/run_matrix.py benchmarks/e2b/cell_driver.py
```

E2B preflight after Linux binary/template rebuild:

```bash
set -a; . ./.env; set +a
WB_FIXED_BIN=benchmarks/e2b/assets/semfs-fixed \
  python3 benchmarks/e2b/run_matrix.py --preflight \
    --arms hiddenkg_retrieval,hiddenkg_retrieval_l7 \
    --knobs benchmarks/e2b/knobs/best_exp0002.json
```

## Acceptance Criteria

- Arms 8 and 9 are runnable by the harness.
- `/kg` and `/by-topic` remain absent from the mounted workspace.
- `semfs grep` output contains only normal source excerpts.
- KG candidate lane can introduce files not present in the normal retrieval pool.
- `SEMFS_COMENTION` only toggles L7 post-rerank behavior.
- Missing or empty graph tables do not break search.
- Preflight passes for both new arms.

## Risks

- Large communities may inject noisy files. Mitigation: cap per community and penalize giant communities.
- Representative chunk selection may choose weak excerpts. Mitigation: prefer file-local FTS hit, then fallback to first chunk.
- KG lane may overweight ambiguous entity matches. Mitigation: bounded lane contribution and exact lexical/path lanes remain active.
- Candidate injection can increase rerank cost. Mitigation: final KG cap of 80 and dedupe by file.
- Experiment attribution can get blurry if `SEMFS_HIDDEN_KG` prior and candidate lane are always coupled. Mitigation: keep flags explicit.
