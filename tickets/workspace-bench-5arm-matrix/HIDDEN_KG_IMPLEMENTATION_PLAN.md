# Hidden KG Implementation Plan

## Objective

Implement `SEMFS_HIDDEN_KG=on` as an internal, bounded KG prior in SQLite retrieval, then make the E2B harness able to run:

```text
plain
best
hiddenkg
```

where `best` and `hiddenkg` use the same surface-clean `chanpin-4arm.db`, and the only intended difference is `SEMFS_HIDDEN_KG`.

Success means hidden KG can affect first-stage candidate ranking without exposing `/kg`, `/by-topic`, `graph.json`, community IDs, or KG explanations to the agent.

## Current Readiness

Ready:

- `chanpin-4arm.db` exists on the Modal volume and has the needed internal tables: `edges`, `graph_entity`, `graph_relation`, `graph_community`, `graph_god_node`.
- The seed is surface-clean: no root `/AGENTS.md`, `/CLAUDE.md`, or `/kg/` should appear on mount.
- SQLite search already has a clean insertion point: after vector/code/BM25/path/integrity lanes fill `by_file`, before `rank::to_hits()` and before rerank.

Not ready:

- `crates/semfs-core/src/backend/hidden_kg.rs` does not exist.
- `SEMFS_HIDDEN_KG` is not implemented.
- `rank::FileAcc` has no bounded prior field.
- E2B template build does not yet bake `/opt/chanpin-4arm.db`.
- `benchmarks/e2b/run_matrix.py` still maps `best` and `hiddenkg` to older seeds/configs.

## Design Decisions

1. Hidden KG is a soft prior, not a hard filter.
2. Hidden KG runs before rerank, not after rerank.
3. Hidden KG affects ranking only; it must not add KG text to returned excerpts.
4. Exact lexical/path/numeric hits must still win. KG boost is bounded to avoid overpowering BM25/path matches.
5. `SEMFS_COMENTION` remains a separate post-rerank consistency nudge. It is not the hidden KG implementation.
6. v1 is SQLite-only. PgVector parity can follow after the E2B result is credible.

## Product Implementation

### Step 1: Add `hidden_kg` Backend Module

Create:

```text
crates/semfs-core/src/backend/hidden_kg.rs
```

Expose:

```rust
pub fn enabled() -> bool;
pub fn query_kg_priors(
    conn: &rusqlite::Connection,
    query: &str,
    candidate_files: impl IntoIterator<Item = String>,
) -> anyhow::Result<KgPriorResult>;
```

Suggested result type:

```rust
pub struct KgPriorResult {
    pub priors: std::collections::HashMap<String, f64>,
    pub matched_entities: Vec<String>,
    pub matched_communities: Vec<i64>,
}
```

Verify:

- Unit test `enabled()` accepts `on`, `1`, `true`, `yes`.
- Unit test off/default path returns no priors and does not query KG tables.

### Step 2: Query Entity Matching

Implement deterministic matching:

- tokenize query into lowercase alphanumeric tokens
- keep tokens with length >= 2 for CJK-safe names and >= 3 for Latin terms unless exact phrase matching applies
- match against `lower(graph_entity.name)`
- cap matched entities to a small number, for example 32
- no LLM query rewrite in v1

Initial SQL can use `LIKE` with bounded result limits. If this is too slow later, add a proper FTS table for entities.

Verify:

- Fixture DB with `graph_entity` rows matches query terms.
- Empty/missing graph tables degrade to no priors, not search failure.
- Query with no entity match returns no priors.

### Step 3: Compute Bounded File Priors

Compute:

```text
kg_prior(file) =
  entity_overlap_score
+ community_match_score
+ neighbor_file_score
- giant_community_penalty
```

Recommended v1 scoring:

```text
entity_overlap_score    max 0.08
community_match_score   max 0.05
neighbor_file_score     max 0.04
giant_cluster_penalty   max 0.03
final clamp             0.00..0.15
```

Data sources:

- entity overlap: `edges.from_path -> edges.to_path` where `to_path` is a matched entity
- community match: `graph_community.file_path` for communities containing direct entity-overlap files
- neighbor file score: other files sharing entities with direct files through `edges`
- giant penalty: community size from `graph_community`, penalize unusually large communities

Important constraint:

- Only apply priors to candidate files already present in `by_file` for v1.
- Do not add brand-new files to the candidate pool until we have a separate recall-focused test. This keeps the first implementation low-risk and makes rank movement attributable.

Verify:

- Direct entity overlap receives higher prior than community-only file.
- Large community files get less boost than small coherent community files.
- All scores are clamped to `0.0..0.15`.
- Candidate not present in `candidate_files` is ignored.

### Step 4: Add Prior Support To Ranking

Modify:

```text
crates/semfs-core/src/backend/rank.rs
```

Add to `FileAcc`:

```rust
pub prior: f64
```

Update `FileAcc::score()`:

```rust
rrf_score + prior.clamp(0.0, 0.15)
```

Add helper:

```rust
pub fn apply_file_priors(
    acc: &mut HashMap<String, FileAcc>,
    priors: &HashMap<String, f64>,
)
```

Verify:

- Existing RRF tests still pass.
- New test proves a bounded prior can move close candidates but cannot dominate a strong multi-lane exact hit.

### Step 5: Wire Hidden KG In SQLite Search

Modify:

```text
crates/semfs-core/src/backend/sqlite_vec.rs
```

Insertion point:

```text
after vector/code/BM25/path/integrity lanes
before drop(conn)
before by_file.remove("/KNOWLEDGE_GRAPH.md")
before rank::to_hits()
```

Flow:

```text
if hidden_kg::enabled() {
    let candidate_files = by_file.keys().cloned().collect::<Vec<_>>();
    let result = hidden_kg::query_kg_priors(&conn, query, candidate_files)?;
    rank::apply_file_priors(&mut by_file, &result.priors);
}
```

Failure behavior:

- Hidden KG query errors should warn and continue with normal retrieval.
- Do not let KG table corruption make `semfs grep` return empty.

Verify:

- Search works with `SEMFS_HIDDEN_KG=off`.
- Search works with `SEMFS_HIDDEN_KG=on` and no graph tables.
- Debug logs show priors when enabled.
- Agent-visible output does not mention KG.

### Step 6: Observability

When `SEMFS_DEBUG_RANKING=1`, log:

```text
HIDDEN_KG matched_entities=[...]
HIDDEN_KG matched_communities=[...]
HIDDEN_KG top_priors=[/path/a:0.12,/path/b:0.08]
```

Do not print these into grep output.

Verify:

- Logs include hidden KG diagnostics.
- `semfs grep` stdout contains only normal search results.

## Harness And Template Implementation

### Step 7: Bake `chanpin-4arm.db` Into E2B Template

Modify:

```text
benchmarks/modal/semfs_modal.py
benchmarks/e2b/bake_template_v2.py
```

Required template asset:

```text
/opt/chanpin-4arm.db
```

Verify:

- Modal build checks `seeds/chanpin-4arm.db` exists before E2B template build.
- Fresh E2B sandbox lists `/opt/chanpin-4arm.db`.
- Preflight can copy it to `~/.semfs/chanpin.db`.

### Step 8: Update E2B Arm Config

Modify:

```text
benchmarks/e2b/run_matrix.py
```

Recommended arms:

```text
plain
best
hiddenkg
hiddenkg_edges
```

Seed mapping:

```text
best           -> /opt/chanpin-4arm.db
hiddenkg       -> /opt/chanpin-4arm.db
hiddenkg_edges -> /opt/chanpin-4arm.db
```

Env mapping:

```text
best:
  SEMFS_KG=off
  SEMFS_GRAPH_FS=off
  SEMFS_COMENTION=off
  SEMFS_HIDDEN_KG=off

hiddenkg:
  SEMFS_KG=off
  SEMFS_GRAPH_FS=off
  SEMFS_COMENTION=off
  SEMFS_HIDDEN_KG=on

hiddenkg_edges:
  SEMFS_KG=off
  SEMFS_GRAPH_FS=off
  SEMFS_COMENTION=on
  SEMFS_HIDDEN_KG=off
```

Keep `hiddenkg_edges` only as the old L7 proxy/control. The primary experiment should compare `best` vs `hiddenkg`.

Verify:

- Preflight rejects missing `/opt/chanpin-4arm.db`.
- Surface contamination check passes for `best` and `hiddenkg`.
- `SEMFS_HIDDEN_KG` differs only in the intended arm.

## Test Plan

Run local Rust tests first:

```bash
cargo test -p semfs-core hidden_kg
cargo test -p semfs-core backend::rank
cargo test -p semfs-core sqlite_vec
```

Then run broader tests if compile time is acceptable:

```bash
cargo test -p semfs-core
```

E2B preflight:

```bash
python3 benchmarks/e2b/run_matrix.py \
  --preflight \
  --arms best,hiddenkg \
  --knobs benchmarks/e2b/knobs/best_exp0002.json
```

Cheap validation:

```bash
python3 benchmarks/e2b/run_matrix.py \
  --cases 53,171 \
  --agents codex \
  --arms plain,best,hiddenkg \
  --knobs benchmarks/e2b/knobs/best_exp0002.json \
  --reps 1
```

Opinion-forming run:

```bash
python3 benchmarks/e2b/run_matrix.py \
  --cases 15,44,45,53,55,95,171,175,386,388 \
  --agents codex \
  --arms plain,best,hiddenkg \
  --knobs benchmarks/e2b/knobs/best_exp0002.json \
  --reps 10
```

Only add `claude` or `glm-5.1` after codex preflight and cheap validation pass.

## Acceptance Criteria

Product:

- `SEMFS_HIDDEN_KG=off` preserves current behavior except for harmless struct defaults.
- `SEMFS_HIDDEN_KG=on` changes pre-rerank scores through bounded priors.
- No KG artifacts are returned to the agent.
- Missing graph tables degrade to normal retrieval.

Benchmark:

- E2B template contains `/opt/chanpin-4arm.db`.
- `best` and `hiddenkg` use the same seed.
- Both arms keep `SEMFS_GRAPH_FS=off`.
- `best` has hidden KG off; `hiddenkg` has hidden KG on.
- Preflight passes before any paid/long run.

Decision:

- If `hiddenkg` improves or maintains accuracy with lower tokens, promote the architecture.
- If accuracy improves but tokens do not, inspect whether result payload size or agent re-search behavior dominates.
- If tokens drop but accuracy drops, reduce prior cap or restrict KG to high-confidence entity overlap only.
- If no movement, allow v2 to add KG recall injection for top community files, but only after v1 establishes safe ranking behavior.

## Deferred V2

- Add an entity FTS table for faster query-to-entity matching.
- Add KG recall injection for files not already in `by_file`.
- Implement PgVector parity.
- Add developer-workspace graph semantics: symbol definition, callers, tests, config, docs.
- Add AST-derived typed relation weighting for code workspaces.
