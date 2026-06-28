# 9-Arm Experiment Matrix

## Goal

Separate three questions cleanly:

1. How much of the gain comes from the `compress + dedup + prompt` stack?
2. How much comes from existing L7 co-mention (`SEMFS_COMENTION`)?
3. How much comes from hidden KG, first as a reranking prior, then later as true retrieval-time routing/scoping?

## Recommended 9 Arms

### 1. Plain

- `plain`
- No semfs mount.
- Status: `ready now`

### 2. Compress Only

- `plain` + `SEMFS_GREP_COMPRESS=on`
- No dedup, no turnbrake prompt, no semfs mount.
- Canonical knob preset: `benchmarks/e2b/knobs/compress_only_clean.json`
- Status: `ready now`

### 3. Compress + Dedup

- `plain` + `SEMFS_GREP_COMPRESS=on` + `SEMFS_DEDUP_WINDOW=5`
- No turnbrake prompt, no semfs mount.
- Canonical knob preset: `benchmarks/e2b/knobs/compress_dedup_clean.json`
- Status: `ready now`

### 4. Compress + Dedup + Prompt, L7 Off

- `best_exp0002`
- `SEMFS_COMENTION=off`
- `SEMFS_HIDDEN_KG=off`
- Current closest arm: `best`
- Status: `ready now`

### 5. Compress + Dedup + Prompt, L7 On

- `best_exp0002`
- `SEMFS_COMENTION=on`
- `SEMFS_HIDDEN_KG=off`
- Current closest arm: `hiddenkg_edges`
- Status: `ready now`

### 6. Compress + Dedup + Prompt + Hidden KG, Reranking Only, L7 Off

- `best_exp0002`
- `SEMFS_COMENTION=off`
- `SEMFS_HIDDEN_KG=on`
- Current closest arm: `hiddenkg`
- Status: `ready now`

### 7. Compress + Dedup + Prompt + Hidden KG, Reranking Only, L7 On

- `best_exp0002`
- `SEMFS_COMENTION=on`
- `SEMFS_HIDDEN_KG=on`
- Current arm: `hiddenkg_l7`
- Status: `ready now`

### 8. Compress + Dedup + Prompt + Hidden KG In Proper Retrieval Stage, L7 Off

- `best_exp0002`
- `SEMFS_COMENTION=off`
- Hidden KG used before candidate-set finalization, not just as a file prior.
- Implementation ticket: [KG_CANDIDATE_LANE_IMPLEMENTATION_PLAN.md](./KG_CANDIDATE_LANE_IMPLEMENTATION_PLAN.md)
- Status: `not implemented in product`

### 9. Compress + Dedup + Prompt + Hidden KG In Proper Retrieval Stage, L7 On

- `best_exp0002`
- `SEMFS_COMENTION=on`
- Hidden KG used in retrieval proper plus existing L7 co-mention.
- Implementation ticket: [KG_CANDIDATE_LANE_IMPLEMENTATION_PLAN.md](./KG_CANDIDATE_LANE_IMPLEMENTATION_PLAN.md)
- Status: `not implemented in product`

## Current Code Reality

## What is already implemented

- `SEMFS_COMENTION=on`
  - Existing L7 co-mention boost.
  - Runs post-rerank in `sqlite_vec.rs`.
- `SEMFS_HIDDEN_KG=on`
  - Implemented in `hidden_kg.rs`.
  - Reads `graph_entity`, `edges`, and `graph_community`.
  - Applies a bounded file prior to already retrieved candidates.
  - This is hidden KG as a reranking / pre-rerank-prior stage, not retrieval proper.

## What is not implemented yet

- Hidden KG routing/scoping before retrieval.
- Community-aware candidate generation.
- KG-driven retrieval expansion that can introduce new files into the candidate set.
- Adaptive `k` coupled to KG communities.

## Ready vs Not Ready

### Ready now

- Arm 1: `plain`
- Arm 2: `compress only`
- Arm 3: `compress + dedup`
- Arm 4: `compress + dedup + prompt, L7 off`
- Arm 5: `compress + dedup + prompt, L7 on`
- Arm 6: `compress + dedup + prompt + hidden KG reranking, L7 off`
- Arm 7: `compress + dedup + prompt + hidden KG reranking, L7 on`

### Needs small harness wiring

- None

### Not implemented in product

- Arm 8: `compress + dedup + prompt + hidden KG in retrieval proper, L7 off`
- Arm 9: `compress + dedup + prompt + hidden KG in retrieval proper, L7 on`

## What "Retrieval Proper" Requires

Hidden KG becomes part of retrieval proper only when it affects candidate generation or search scope before the current `by_file` pool is finalized.

The recommended first implementation is KG candidate lane, not full KG-scoped retrieval. Candidate lane lets KG add files before RRF/rerank while preserving global BM25/vector/path retrieval. Full KG-scoped retrieval is tracked separately in [KG_SCOPED_RETRIEVAL_TICKET.md](./KG_SCOPED_RETRIEVAL_TICKET.md) and should be deferred until candidate-lane results justify the extra complexity.

Concrete work still needed:

1. Query-to-entity / query-to-community routing before final retrieval.
2. Community-scoped or community-boosted BM25/vector/path retrieval, not just post-hoc file priors.
3. Ability for KG to add or rescue files not already present in the retrieval pool.
4. Optional adaptive `k` / widened candidate budget only for matched communities.
5. A separate arm flag, likely something like `SEMFS_HIDDEN_KG_RETRIEVAL=on`, to avoid overloading the current rerank-prior implementation.

## Recommended Run Order

1. Run the four arms that are already ready:
   - plain
   - compress only
   - compress + dedup
   - cdp L7 off
   - cdp L7 on
   - cdp hidden-KG reranking L7 off
   - cdp hidden-KG reranking L7 on
2. Only after that, decide whether implementing retrieval-proper hidden KG is worth the engineering time.
