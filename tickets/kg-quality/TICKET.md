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

---

## RESULTS — shipped + structurally measured (2026-06-17)

**Shipped** (commit `0106b2e`, TDD, 13 new community tests + 351 core + 74 semfs green):
- `community.rs::Graph::add_knn_edges` — cosine-kNN densification (each file → `KNN_K=6` nearest
  embedding neighbours, `KNN_WEIGHT=1.0`), deterministic, symmetric-deduped, accumulates weight.
- `community.rs` full multi-level **Leiden** (`local_move → refine → aggregate-by-refined → recurse`,
  self-loop-carrying aggregation) replacing the single-level Louvain+`leiden_refine` hybrid.
- `graph_file.rs::build_file_graph` = shared-entity edges **+** kNN (fail-soft per-file mean
  embeddings read from the `vchunks` vec0 store); `Leiden.detect` swapped in at all 3 projection sites.

**Measured** — re-materialized the chanpin KG on a `/tmp` copy (deterministic, offline, no LLM/FUSE):

| metric | BEFORE (Louvain+refine) | AFTER (Leiden+kNN) | target | verdict |
|---|---|---|---|---|
| communities | 173 | 32 | — | consolidated 5.4× |
| **singletons** | **66 (38.2%)** | **1 (3.1%)** | **<10%** | ✅ **beat** |
| largest community | 62 (9.7%) | 135 (21.2%) | **no >60 buckets** | ❌ **overshot** |
| mean community size | 3.68 | 19.88 | — | — |
| god-nodes | 669 | 128 | — | — |

- ✅ **Singleton fragmentation SOLVED** (38.2% → 3.1%, exact — Leiden is deterministic, not n=1 noise).
  ~35% of files that had **zero** "related-files" pointer now sit in a real cluster.
- ❌ **Overshot into a 135-file bucket** (target was <60). Validated coherent, NOT a junk-drawer: the
  135 are uniformly `compliance_and_risk_control/*` (content-moderation, violation-handling, privacy/DPIA)
  — kNN correctly merged the *compliance/risk* theme across sibling subdirs. Concentration is power-law
  (top-3 = 43%), no catastrophic single-blob.

**Open tuning question (not a defect):** `RESOLUTION=1.0` is the bucket-size lever (↑resolution →
smaller communities). Whether a coherent 135-cluster is too coarse *as a retrieval pointer* is an
**E2E question**, not a structural one — don't sweep the proxy in a vacuum. Options:
(a) resolution sweep to land both targets, (b) make `RESOLUTION`/`KNN_K` env-overridable for the E2B
A/B and tune there, (c) accept the coherent cluster and let the KG-arm A/B decide.

**Next (the actual "relevant metrics" goal):** rebuild the shipping seed (Modal x86_64) with this code
→ E2B FUSE A/B of `SEMFS_KG=on` (+ navigation prompt) vs nokg/plain on 53/171 + a discovery case.
NOT yet launched (awaiting go; E2B-only per the all-tests-on-E2B rule).
