# Ticket: KG quality — fragmented communities from sparse edges

**Folder:** `tickets/kg-quality/`
**Origin:** evo glm-5.1 session (2026-06-17), investigating whether the KG can stop over-exploration.
**Routing:** mirror to Linear (team `SemFS`, key `SEM`) per CLAUDE.md §0.

## Finding (artifact-grounded, chanpin seed)

The chanpin KG (173 communities / 636 member files / 9,300 entities / 5,139 relations) is **too
fragmented to navigate**:

```
community size distribution:
  1 file : 66 communities   ← 38% SINGLETONS
  2 files: 46               ← 65% are ≤2 files
  3–5    : 44
  6–30   : 11
  61, 62 : 2                ← two giant junk-drawer buckets
```

**Root cause = the EDGE graph is too sparse, not the algorithm.** Edges are built as
`weight(a,b) = #shared entities` (`backend/community.rs::Graph::from_file_entities`). chanpin's
entities are *specific* (product IDs, company names, unique terms), so most files share **zero**
entities → isolated nodes → singletons, regardless of clustering algorithm. Density is **~0.55
edges/entity** (vs the kaifa CODE seed at ~15 — AST gives dense, precise edges).

Nuance (not uniformly bad): files that genuinely share entities DO cluster — e.g. case-53's four
`interaction_document_*` docs all landed in **community 17 (size 5)** because they share a template.
The failure is the **related-but-no-shared-entity** files (the 38% singletons).

## Fix — densify the edges BEFORE swapping the algorithm

Add edge types beyond exact shared-entity:

| edge type | wires two files when… | cost |
|---|---|---|
| shared entity (today) | same exact named thing | built |
| **embedding-kNN** ⭐ | their meaning is similar (vector neighbors) | **~free — embeddings already in the index** |
| shared structure | same fields/template | cheap (field-name regex) |
| value / cross-ref | A references a value B is about | cheap |
| co-location / co-access | same folder / opened together | cheap |

**embedding-kNN is the highest-leverage + cheapest:** semfs already embedded every chunk for vector
search. "Connect each file to its k nearest embedding-neighbors" reuses that index to wire
semantically related files even when they share no exact name → absorbs most singletons into real
communities → a navigable map → bounded, one-pass retrieval (the over-exploration fix).

## Deliverables
- Add embedding-kNN edges (+ optional structure/value edges) to `Graph` construction.
- Re-materialize the chanpin KG; re-check the size distribution (target: <10% singletons, no 60-file buckets).
- A/B the KG arm (`SEMFS_KG=on` + navigation prompt) vs nokg/plain on E2B (53/171 + a discovery case).

## Caveats
- Prior KG A/B (6/15 @ 100K vs 8/15 @ 838K) hurt accuracy — but on the *fragmented* KG; a *dense* KG is untested.
- For a genuinely heterogeneous doc corpus, good communities may not fully exist (vs code, where AST is dense).
- Full Leiden is a *secondary* improvement (see `tickets/full-leiden/`); density first, algorithm second.

## Relation
- `sufficiency-resurfacing` = the KG-INDEPENDENT (session-state) way to make coverage knowable; works today.
- `full-leiden` = the clustering-algorithm upgrade, only worth it once edges are dense.
