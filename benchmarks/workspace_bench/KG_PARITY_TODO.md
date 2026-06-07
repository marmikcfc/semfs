# KG → graphify parity — detailed TODO (tackle one at a time)

Goal: bring semfs's knowledge graph to parity with graphify, **reconstruct the KG from scratch**,
**replace it in the local seed**, then **re-test case 289 locally** once the KG is fully generated.

Status legend: ⬜ todo · 🔄 in progress · ✅ done · ⏭️ deferred (with reason)

Current parity ≈ 60%: ✅ Leiden oversized-split, ✅ graph.json, ✅ god-nodes/hub-exclusion.
Core gaps: typed entity→entity relations, AMBIGUOUS confidence, AST code lane, rich GRAPH_REPORT.md.

---

## Phase 0 — Study graphify exactly (prerequisite, no code)
- [ ] **T0.1** Extract graphify's **LLM extraction prompt** verbatim + the **relation ontology**, entity types, per-edge/per-node fields, and confidence semantics (EXTRACTED/INFERRED/AMBIGUOUS). Source: `graphify/extract.py` (+ any prompt files).
- [ ] **T0.2** Document graphify's **graph.json schema** and **GRAPH_REPORT.md** section generators (`report.py`, `export.py`): god nodes, surprising connections, hyperedges, communities (cohesion), ambiguous edges, knowledge gaps, suggested questions.

## Phase 1 — Extraction parity (the core)
- [ ] **T1.1** Define the **relation ontology** (entity→entity) + entity-type ontology to match graphify (e.g. calls/cites/depends_on/part_of/relates_to/contradicts/…). Document in code.
- [ ] **T1.2** Rewrite extraction (`backend/graph.rs`): one LLM pass per file → **entities AND typed entity→entity relations**, each with `confidence ∈ {EXTRACTED,INFERRED,AMBIGUOUS}` + `source_file` + (best-effort) `source_location`. Structured-output enforced. [gaps E + B]
- [ ] **T1.3** **Storage schema** (`cache/schema.sql`): extend `edges` (or new `graph_relation` table) to hold entity→entity rows: `from_path(/memories/A), to_path(/memories/B), relation, confidence, source_file, source_location, weight`. Migration-safe (ALTER/CREATE IF NOT EXISTS).
- [ ] **T1.4** tree-sitter **AST code lane** for code files (deterministic contains/imports/inherits/calls). DECISION: chanpin corpus is ~all docs → likely ⏭️ defer for this seed, but implement the hook OR explicitly note the parity gap.

## Phase 2 — Artifacts parity
- [ ] **T2.1** `graph.json` (`cache/graph_file.rs build_graph_json`): add node fields (`file_type`,`source_file`,`source_location`) and edge fields (`source_location`,`weight`), and include typed entity→entity edges.
- [ ] **T2.2** New **GRAPH_REPORT.md** generator (deterministic, no LLM except suggested-questions): god nodes, **surprising connections** (needs T1.2), hyperedges, communities + cohesion, ambiguous edges, **knowledge gaps** (isolated/thin/high-ambiguity), suggested questions. Materialize as `/GRAPH_REPORT.md`.
- [ ] **T2.3** Keep `KNOWLEDGE_GRAPH.md` compact (orientation); `GRAPH_REPORT.md` = the rich graphify-style report. Add to SEARCH_ALWAYS_VISIBLE.

## Phase 3 — Reconstruct KG from scratch & replace in local
- [ ] **T3.1** New extraction driver (replace `examples/build_graph.rs`): runs T1.2 typed-relation extraction over a seed DB, concurrent, idempotent (wipes old edges/relations first).
- [ ] **T3.2** Run it on the **local seed** (`~/.semfs/chanpin-gemma.db` — has KG already; or e5-nosum) → regenerate entities + typed relations from scratch. Preserve the embedding/vector data (only rebuild the graph tables).
- [ ] **T3.3** Materialize `KNOWLEDGE_GRAPH.md` + `GRAPH_REPORT.md` + `graph.json` on a fresh mount; verify counts + sample typed relations.
- [ ] **T3.4** Verify KG is **fully generated** (every file with entities processed; relation/entity/community counts sane; no truncation).

## Phase 4 — Re-test local
- [ ] **T4.1** Re-run case 289 **kg_on** (local), graded with `seed-2.0-lite`; compare vs kg_off + pure codex. (Research says KG won't move 289, but the user wants the measured test once the KG is fully built.)
- [ ] **T4.2** Record results in GOAL.md run log + this file.

---
## Run log
- 2026-06-07: tests cleared; H-C/H-D removed (revert to 7947f1b, commit 3a1ea50); TODO created. Starting T0.1.
