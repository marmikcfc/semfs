# Ticket: Move community detection to full Leiden

**Folder:** `tickets/full-leiden/`
**Origin:** evo glm-5.1 session (2026-06-17), KG-quality investigation.
**Routing:** mirror to Linear (team `SemFS`, key `SEM`) per CLAUDE.md §0.

## Current state (verified in code)

We do NOT run full Leiden today. `cache/graph_file.rs` runs:

```rust
Louvain { leiden_refine: true }.detect(&g, RESOLUTION)
// = louvain_one_level(g)            ← single-level LOUVAIN core
//   → refine_connected(g, comm)     ← Leiden-STYLE: split internally-disconnected communities
//   → split_oversized(g, comm)      ← recursively split the giant buckets
```

So it's a **hybrid: single-level Louvain core + the one Leiden idea (split-disconnected)**. The
comments confirm Leiden was the intended direction (`community.rs`: *"the Louvain core can be swapped
for / refined into Leiden"*) but only the splitting half landed.

**Two gaps vs real Leiden:**
1. The core is **single-level** — no multi-level *aggregation* (merge communities, re-cluster, repeat),
   the phase that recovers higher-level structure.
2. The Leiden piece we use (`refine_connected`) **splits** communities → makes them *smaller/more
   fragmented*; the part that would *help form* well-connected communities (the local-move +
   refinement + aggregation loop with the well-connectedness guarantee) is missing.

## Deliverable

Implement full Leiden (Traag et al. 2019) behind the existing `CommunityDetector` trait:
- local moving → refinement (well-connected sub-partitions) → aggregation → repeat to convergence;
- keep the resolution param; keep the hard-partition output contract (one community per file).
- A/B vs the current hybrid on the SAME graph (modularity, size distribution, singleton %).

## Priority caveat (read first)

**Density before algorithm.** On the current **sparse** shared-entity graph, full Leiden helps only
marginally — it still cannot connect files that share no edges (singletons come from missing edges,
not bad clustering). Do `tickets/kg-quality` (embedding-kNN edges) FIRST; full Leiden is the
second-order win that pays off once the graph is dense enough to have structure worth optimizing.

## Success criteria
- [ ] Full Leiden implemented behind the trait; existing tests green; new test on a known graph.
- [ ] On a DENSIFIED graph: lower singleton %, higher modularity, no degenerate giant buckets vs the hybrid.
- [ ] KG-arm accuracy A/B shows no regression vs the hybrid (ideally a gain via more coherent communities).
