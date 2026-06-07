# semfs KG vs graphify — exact comparison & gap analysis

Source: `github.com/safishamsi/graphify` (`graphify/extract.py`, `cluster.py`, `report.py`, `export.py`).
semfs side: `backend/graph.rs` (extraction), `backend/community.rs` (clustering), `cache/digest.rs` + `cache/graph_file.rs` (artifacts), `examples/build_graph.rs` (driver).

## 1. Extraction — what & how

| Dimension | graphify | semfs (current) | gap |
|---|---|---|---|
| **Code files** | tree-sitter AST, **deterministic, no LLM** → relations `contains/method/imports/inherits/calls/uses` | LLM for everything (no AST path) | semfs has no free/deterministic code lane |
| **Docs/PDF/img** | LLM semantic extraction (entities **+ typed relations**) | LLM (`gpt-4.1-nano`), **entities only** | **semfs extracts NO entity→entity relations** |
| **Entity fields** | `id, label, file_type, source_file, source_location` | `name, type` (+ derived `/memories/<slug>.md` path) | missing source_file/location/file_type |
| **Edge fields** | `source, target, relation, confidence, source_file, source_location, weight` | `from_path(FILE), to_path(/memories/<entity>), edge_kind(=entity type), confidence, created_at` | **edges are file→entity co-mention, NOT entity→entity typed relations** |
| **Ontology** | code relations above; doc relations incl. `calls/cites/conceptually_related/semantically_similar` | 8 entity **types** (Person/Org/Project/Decision/Task/Event/Artifact/Concept); **no relation types** | add relation ontology |
| **Confidence** | `EXTRACTED` (AST) / `INFERRED` (call-graph, weight 0.8) / `AMBIGUOUS` | column exists, **hard-coded `INFERRED`**; no `AMBIGUOUS` | populate real levels; add AMBIGUOUS |

**Biggest gap:** semfs builds a **bipartite file↔entity co-mention graph**; graphify builds a **typed entity↔entity relationship graph**. Communities/god-nodes still work on co-mention, but "surprising connections", relation-typed edges, and a queryable `graph.json` need real A→B relations.

## 2. Community detection

| Dimension | graphify | semfs (current) | gap |
|---|---|---|---|
| Algorithm | **Leiden** (`graspologic.partition.leiden`) | **Louvain + Leiden-style refinement** (`refine_connected` splits internally-disconnected communities) | not full Leiden, but modularity-equivalent for MVP |
| Oversized split | communities >25% of nodes (min 10) **recursively split** (`_MAX_COMMUNITY_FRACTION=0.25`, `_MIN_SPLIT_SIZE=10`) | **none** | **add recursive split** |
| Determinism | IDs by size desc (0=largest); no seed exposed | deterministic (`densify` by size, stable) | parity ✓ |
| Hub handling | `--exclude-hubs` flag (not in cluster core) | `hub_entities(pctl)` excludes hubs from god-node labels | parity ✓ (semfs arguably better) |
| Resolution | graspologic default | `RESOLUTION` knob (default 1.0) | parity ✓ |

## 3. Artifacts generated

| Artifact | graphify | semfs (current) | gap |
|---|---|---|---|
| `graph.json` (queryable nodes+edges) | ✅ | ❌ | **add** |
| `GRAPH_REPORT.md` | ✅ rich (below) | `KNOWLEDGE_GRAPH.md` (Topics+god-nodes+dir map) | **enrich** |
| `graph.html` / `.svg` / `.graphml` / obsidian | ✅ optional | ❌ | out of scope (visualization) |

**graphify GRAPH_REPORT.md sections** (deterministic, no LLM except Suggested Questions passed in):
1. Header + corpus check (date, file/word count, "substantial enough?" verdict)
2. Summary (nodes, edges, communities, **confidence breakdown EXTRACTED/INFERRED/AMBIGUOUS + token cost**)
3. **God Nodes** (most-connected core abstractions)
4. **Surprising Connections** ("you probably didn't know these" — unexpected relations w/ confidence + source files)
5. **Hyperedges** (multi-node group relations)
6. **Communities** (cohesion score + first 8 nodes)
7. **Ambiguous Edges** (low-certainty, for manual review)
8. **Knowledge Gaps** (isolated nodes, thin communities, high-ambiguity %)
9. **Suggested Questions** (queries the graph can answer)

semfs `KNOWLEDGE_GRAPH.md` currently has: Topics (communities by god-node) + Directory map. **Missing: summary/confidence breakdown, surprising connections, ambiguous edges, knowledge gaps, suggested questions, graph.json.**

## 4. On/off switch

| | graphify | semfs |
|---|---|---|
| toggle | CLI flags (`--cluster-only`, `--exclude-hubs`, `--resolution`) | `SEMFS_KG=off/0/false/no` (mount-time), `kg_enabled()` gates build + mount-time refresh | parity ✓ |

## 5. Prioritized work to reach parity

| # | gap | effort | value | status |
|---|---|---|---|---|
| A | Leiden oversized-community recursive split (>25%, min 10) | S | M | ⬜ |
| B | `AMBIGUOUS` confidence level + populate EXTRACTED vs INFERRED | S | M | ⬜ |
| C | `graph.json` artifact (nodes+edges, queryable) | M | M | ⬜ |
| D | enrich `GRAPH_REPORT.md` (summary/confidence, surprising connections, knowledge gaps, suggested questions) | M | H | ⬜ |
| E | **typed entity→entity relation extraction** (relation ontology + confidence + source) | L | **H** | ⬜ |
| F | tree-sitter AST code lane (deterministic, free) | L | M (low for chanpin doc corpus) | ⬜ |

## 6. Note on 289 relevance
The retrieval matrix already showed **KG has no rank effect on case 289** (answer already #0; the 403 source now surfaces via the path-lane, not the KG). So richer KG improves *orientation/quality* generally but is **not** the lever for 289's tool-call count. The E2E KG on/off experiment is run below to confirm, not assumed.
