# Graphify-style KG for semfs — APPROVED architecture

**Status:** ✅ IMPLEMENTED (2026-06-07) · design approved 2026-06-06 · **Parent:** `ls-kg-semantic-readdir/`

## ✅ Implementation (2026-06-07)
Built + verified end-to-end on `chanpin-gemma` (615 files):
- **L2/L3** `backend/community.rs` — Louvain modularity + Leiden well-connectedness
  refinement (split disconnected communities) + p99 hub-exclusion + god-nodes. Pure,
  deterministic, 6 unit tests pass.
- **L4** `cache/digest.rs` — pure markdown render (communities → god-nodes → members
  + dir-map). 3 unit tests.
- **L5 compute** `cache/graph_file.rs` — reads `edges`+`graph_entity`, projects to
  file↔file (weight = #shared entities), detects communities, picks god-nodes, renders.
  2 unit tests. **surface**: `CacheFs::refresh_knowledge_graph()` materializes
  `/KNOWLEDGE_GRAPH.md` as a **derived (local-only, not pushed/indexed) root file** —
  `ls` lists it, `cat` serves it. Built at mount (daemon_runtime).
- **L6 dynamic** `cache/graph_queue.rs` — `run_graph_worker` now debounce-recomputes
  the KG once entity extraction settles after add/remove (wired with a refresh closure).
- **L1** `schema.sql` — `edges.confidence` (categorical, `INFERRED`) + new `graph_entity(path,name,kind)`
  so CJK god-node names survive `slugify` (lossy). `index_graph` populates both.
- **Contract** `agent_hint.rs` — the `CLAUDE.md`/`AGENTS.md` block now names
  `KNOWLEDGE_GRAPH.md` ("read the KG to orient OR grep to find").
- **K3 extraction** `examples/build_graph.rs` — comprehensive L7 over an existing seed
  (no re-embed). Ran on chanpin-gemma → 110 files w/ entities, 832 edges, 703 entities.
- **Verified:** `ls` shows `KNOWLEDGE_GRAPH.md` (19.8 KB) beside real dirs; `cat` →
  **77 topic clusters** with named god-nodes (CJK preserved, e.g. "KPMG, 毕马威, 2025年度";
  "Changan Automobile…"; "OpenAI, ChatGPT, Java, Codex") + dir-map; contract written.
- **Known MVP gap (follow-up):** some god-nodes are numeric (¥amounts / %) the LLM
  extracted as entities — add a numeric-entity filter. Aggregation level of Louvain
  omitted (YAGNI). Not measured on exploratory tasks yet (the experiment gate).

**Original design (approved 2026-06-06) follows.**

**Goal:** a workspace-root knowledge-graph digest, dynamically maintained, surfaced to the agent so it
orients (grep-first) instead of crawling. It is an **experiment**: the bar is **measurably fewer tokens/
tool-calls than the no-orientation baseline (profile.md is now deleted)** on EXPLORATORY tasks (else we revert).

## Approved decisions
| # | decision | choice |
|---|---|---|
| Leiden impl | community detection in the offline Rust daemon | **Rust-native: Louvain first, then add Leiden's refinement/well-connectedness + p99 hub-exclusion** |
| Confidence | scoring on edges | **categorical column `EXTRACTED / INFERRED / AMBIGUOUS`** (today all `INFERRED`; future-proofs an AST/structured path — no extra LLM call now) |
| Surfacing | where the digest lives | **new root virtual file `KNOWLEDGE_GRAPH.md`** (profile.md is now DELETED) + an **`AGENTS.md`/`CLAUDE.md` FS-contract** that names it. Aptly-titled so the LLM infers purpose from `ls` itself; `cat` works; the LLM reads it to orient OR greps directly. |
| Scope | MVP | **communities + god-nodes, dynamic on add/remove**; must beat dir-map on tokens |

## Design principles
- **DRY:** reuse what exists — `extract_entities` (8-type ontology), the `edges` table, `index_graph`
  (per-file dynamic), `run_graph_worker`, and the (now-vacated) root virtual-file VFS pattern. We add only the
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
  run_graph_worker (async, off write-path)  L4 digest         (communities → md, into KNOWLEDGE_GRAPH.md)
  apply_comention_boost                     L6 recompute      (debounced, on edge change)
  (profile.md DELETED 5fc0904)              L1 +confidence col, +graph_node cache table
  → NEW KNOWLEDGE_GRAPH.md root file + AGENTS.md/CLAUDE.md contract (L5)
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
| **L4 digest** | **`cache/digest.rs` (NEW)** | **pure fn** `(communities, god-nodes, members) → markdown` | none |
| **L5 surface** | **`cache/graph_file.rs` (NEW) + `fs.rs`** | new root virtual file `KNOWLEDGE_GRAPH.md` (readdir lists it, read serves the digest); generated at mount + on recompute | sqlite read |
| **L6 dynamic** | extend `cache/graph_queue.rs::run_graph_worker` | on edge add/remove → mark KG dirty → **debounced** recompute of L2–L4 → refresh cache + KNOWLEDGE_GRAPH.md | orchestration |

## Dynamic maintenance (L6)
- Per-file edges are **already** dynamic (`index_graph` on write; edge-removal on delete). Reuse that.
- Add a **dirty flag + debounce**: a file change marks the workspace KG dirty; the worker recomputes
  communities/god-nodes after a quiet period (or every N changes), not on every file. Recompute is
  O(edges) Louvain/Leiden — cheap for this corpus (≈600 files, ≈100s of entities).
- Recompute repopulates `graph_node` and re-renders `KNOWLEDGE_GRAPH.md`. Add/remove of files therefore updates
  the KG the agent sees. (Workspace-root scope only.)

## Confidence (categorical) — minimal
Add `edges.confidence TEXT DEFAULT 'INFERRED'`. LLM extraction → `INFERRED`. Reserve `EXTRACTED` for a
future structured/AST path and `AMBIGUOUS` for a future low-confidence signal. Digest may show the tag.
No extra LLM call now (YAGNI-honored while future-proofing per the approved choice).

## Surfacing (L5) — named root file + FS-contract (REVISED; profile.md deleted)
profile.md has been removed from the system (commit 5fc0904). The KG is surfaced as a **new root virtual
file `KNOWLEDGE_GRAPH.md`** (the VFS pattern is the freshly-vacated profile.md slot — a root-ino virtual file
that `readdir` lists and `read` serves — but local-only, no cloud API, aptly named). Content = the KG digest
(communities → god-nodes → member files) + a compact dir-map.

`ls` lists `KNOWLEDGE_GRAPH.md` alongside the real entries (= "directory + KG", the POSIX-clean realization
of the "ls returns KG" idea — literal readdir-injection is infeasible, see issue.md). The agent reads it to
orient OR greps directly. The **`agent_hint.rs` contract** (written to `AGENTS.md`/`CLAUDE.md`) states:
"This mount is a dynamic semantic index. It maintains a workspace knowledge graph at
`<mount>/KNOWLEDGE_GRAPH.md` and answers `semfs grep` with ranked excerpts. To orient, read the KG; to find
content, grep." That shifts the agent's prior from crawl→query.

### Phasing the surfacing (validate cheap delivery before rich payload)
- **Phase 0 (cheap):** ship `KNOWLEDGE_GRAPH.md` with a SIMPLE digest (structural dir-map / topic list we
  already generate) + the contract. Measure on EXPLORATORY tasks: does the named file + contract raise the
  read-first / grep-first rate vs nothing? If a simple payload doesn't move behavior, the full Leiden KG won't.
- **Phase 1+ (only if Phase 0 pays):** swap the payload for the real Leiden community + god-node KG (L2-L4),
  dynamic, fed by comprehensive L7 extraction.

### Steelman / strawman of this approach (2026-06-06)
STEELMAN: POSIX-clean & buildable (real file; ls lists it; cat works); apt name is self-documenting (beats
profile.md); the contract shifts the prior (the real lever); "read KG or grep" matches the two task types;
clean slate (no profile.md collision).
STRAWMAN: still a skippable PULL (codex read profile.md ~half the time AFTER walking; os.walk is
uninterceptable so the file can't PREVENT the crawl); the name only helps if the agent uses `ls` (it often
uses `os.walk`, where the file is buried); for PINPOINT tasks (289) the KG is redundant with grep (extra
tokens); the hint is a prompt (weak, can backfire — strengthened grep header was +53%); big build for an
unproven, exploratory-only payoff (KG currently sparse: 21/595 files) → validate Phase 0 first.

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
