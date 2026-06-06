# Graphify-style KG for semfs — APPROVED architecture

**Status:** approved (design); implementation NOT started · **Date:** 2026-06-06 · **Parent:** `ls-kg-semantic-readdir/`
**Goal:** a workspace-root knowledge-graph digest, dynamically maintained, surfaced to the agent so it
orients (grep-first) instead of crawling. It is an **experiment**: the bar is **measurably fewer tokens/
tool-calls than the current dir-map `profile.md`** (else we revert).

## Approved decisions
| # | decision | choice |
|---|---|---|
| Leiden impl | community detection in the offline Rust daemon | **Rust-native: Louvain first, then add Leiden's refinement/well-connectedness + p99 hub-exclusion** |
| Confidence | scoring on edges | **categorical column `EXTRACTED / INFERRED / AMBIGUOUS`** (today all `INFERRED`; future-proofs an AST/structured path — no extra LLM call now) |
| Surfacing | where the digest lives | **reuse `profile.md`** (one file; swap content to the KG digest; no new VFS wiring, no hint change, no cloud/local divergence) |
| Scope | MVP | **communities + god-nodes, dynamic on add/remove**; must beat dir-map on tokens |

## Design principles
- **DRY:** reuse what exists — `extract_entities` (8-type ontology), the `edges` table, `index_graph`
  (per-file dynamic), `run_graph_worker`, and the `profile.md` virtual-file slot. We add only the
  *analytics* + *digest* layers.
- **YAGNI (skip):** graph.html viz, GRAPH_REPORT.md, query API (`get_neighbors`/`shortest_path`/MCP),
  tree-sitter AST, per-subdirectory KGs. Defer until a real consumer needs them.
- **SOLID:** each new layer is one focused module with a pure-function core (no I/O) so it's unit-testable;
  detection sits behind a trait so Louvain→Leiden is a swap, not a rewrite.

## What exists vs what we add
```
EXISTS (reuse):                           ADD (this ticket):
  extract_entities ──► edges(file↔entity)   L2 community.rs   (Leiden over the graph)
  index_graph (write/delete → edge delta)   L3 god-nodes      (degree + p99 hub-exclusion)
  run_graph_worker (async, off write-path)  L4 digest         (communities → md, into profile.md)
  apply_comention_boost                     L6 recompute      (debounced, on edge change)
  profile.md virtual file (in ls + hint)    L1 +confidence col, +graph_node cache table
```

## Graph model (projection)
Edges are bipartite **file↔entity**. For the digest the agent wants **file clusters labeled by their key
concepts**, so:
1. Project to a weighted **file↔file** graph: weight(f1,f2) = #shared entities.
2. **Leiden** on that → **file communities** (topics).
3. Per community, **god-nodes = the highest-degree entities** among its files (the concepts everything
   flows through).
4. Digest line = `★ <god entities> — <member files>`.

(One projection, computed from `edges`; no second source of truth — DRY.)

## Module / layer breakdown (SOLID)
| layer | module | responsibility | I/O? |
|---|---|---|---|
| L0 extract | `backend/graph.rs` (exists) | content → typed entities (+ confidence tag) | LLM |
| L1 store | `cache/schema.sql` + `edges` (exists, **+`confidence`**) ; new cache table `graph_node(name, kind, community_id, degree, is_hub)` | persist edges + computed community/degree cache | sqlite |
| **L2 detect** | **`backend/community.rs` (NEW)** | `trait CommunityDetector` + Louvain→Leiden impl. **Pure fn** `(node_ids, weighted_edges, resolution, exclude_hub_pctl) → Vec<(node, community_id)>` | none (testable) |
| **L3 rank** | `community.rs` | degree per node, p99 hub-exclusion, god-node = top-degree entity per community | none |
| **L4 digest** | **`cache/digest.rs` (NEW)** or extend `profile.rs` | **pure fn** `(communities, god-nodes, members) → markdown`; replaces dir-map in `build_local_profile` | none |
| L5 surface | `cache/profile.rs` + `fs.rs` (exists) | `warm_profile` writes the digest into `profile.md` | sqlite read |
| **L6 dynamic** | extend `cache/graph_queue.rs::run_graph_worker` | on edge add/remove → mark KG dirty → **debounced** recompute of L2–L4 → refresh cache + profile.md | orchestration |

## Dynamic maintenance (L6)
- Per-file edges are **already** dynamic (`index_graph` on write; edge-removal on delete). Reuse that.
- Add a **dirty flag + debounce**: a file change marks the workspace KG dirty; the worker recomputes
  communities/god-nodes after a quiet period (or every N changes), not on every file. Recompute is
  O(edges) Louvain/Leiden — cheap for this corpus (≈600 files, ≈100s of entities).
- Recompute repopulates `graph_node` and re-renders `profile.md`. Add/remove of files therefore updates
  the KG the agent sees. (Workspace-root scope only.)

## Confidence (categorical) — minimal
Add `edges.confidence TEXT DEFAULT 'INFERRED'`. LLM extraction → `INFERRED`. Reserve `EXTRACTED` for a
future structured/AST path and `AMBIGUOUS` for a future low-confidence signal. Digest may show the tag.
No extra LLM call now (YAGNI-honored while future-proofing per the approved choice).

## Surfacing (L5) — reuse profile.md
`build_local_profile` becomes: **KG digest** (communities → god-nodes → member files) + a **compact dir
map** appended (so the agent still sees structure and can `cat` real files). `ls` already returns the real
directory entries; `profile.md` carries the KG. No new file, no hint change.

## Phased plan (each phase independently testable; STOP after MVP to measure)
- **P1 — L2/L3 core:** `community.rs` (Louvain + degree + hub-exclusion), pure, unit-tested on a synthetic graph. No wiring.
- **P2 — Leiden refinement:** add the refinement/well-connectedness step behind the same trait; test modularity ≥ Louvain.
- **P3 — L4 digest + L5 surface:** render into `profile.md` from a one-shot compute at mount (reuse `warm_profile`). **Measure E2E tokens vs dir-map (the experiment gate).**
- **P4 — L6 dynamic:** debounced recompute on add/remove; test KG updates after a file write/delete.
- **P5 — confidence column** (cheap, additive).

## Success criterion (experiment gate) — REVISED per 2026-06-06 scope correction
The KG is **NOT measured on case 289** (a pinpoint lookup `grep`+rewrite already solves — see issue.md
Scope Correction). Measure it on **exploratory / corpus-understanding tasks** (e.g. "what is this workspace
about", "summarize the org structure", multi-file synthesis): does the KG orientation reduce tokens/calls
vs no-orientation on *those*? The **case-289 token lever is the separate TRUST FIX** (per-hit completeness
annotation on grep output), not the KG. Delivery of the KG = `AGENTS.md` FS-contract + a cat-able
`_graph.md` for exploratory entry; NOT injected into every grep result.

## NOT building (explicit)
graph.html · GRAPH_REPORT.md · query/path/neighbors API · AST extraction · per-subdir KGs · numeric
confidence · a second virtual file. (All deferred; revisit only on a concrete need.)
