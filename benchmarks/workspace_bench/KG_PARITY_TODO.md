# KG → graphify parity — detailed TODO (tackle one at a time)

Goal: bring semfs's knowledge graph to parity with graphify, **reconstruct the KG from scratch**,
**replace it in the local seed**, then **re-test case 289 locally** once the KG is fully generated.

Status legend: ⬜ todo · 🔄 in progress · ✅ done · ⏭️ deferred (with reason)

Current parity ≈ 60%: ✅ Leiden oversized-split, ✅ graph.json, ✅ god-nodes/hub-exclusion.
Core gaps: typed entity→entity relations, AMBIGUOUS confidence, AST code lane, rich GRAPH_REPORT.md.

---

## Phase 0 — Study graphify exactly (prerequisite, no code)
- [x] **T0.1** ✅ graphify spec captured (from `graphify/skill.md` — it's a Claude-Code skill; the agent does doc extraction, `extract.py` does code AST).
- [x] **T0.2** ✅ report sections known (from `analyze.py`/`report.py`): god nodes, surprising connections, hyperedges, communities+cohesion, ambiguous edges, knowledge gaps, suggested questions.

### graphify extraction spec (authoritative, to replicate)
**Entity `file_type`:** `code | document | paper | image` (never "concept"/"rationale").
**Node fields:** `id (filestem_entityname), label, file_type, source_file, source_location?, source_url?, captured_at?, author?, contributor?, rationale?` (rationale = design-intent text ON the node).
**Relation types:**
- code: `calls, implements, references, imports`
- docs/papers: `cites, conceptually_related_to, semantically_similar_to, depends_on, contradicts, mentions`
- cross-domain: `shares_data_with, part_of, relates_to`
**Edge fields:** `source, target, relation, confidence{EXTRACTED|INFERRED|AMBIGUOUS}, confidence_score(0-1), source_file, source_location?, weight`.
**Confidence rule:** EXTRACTED→1.0 (explicit AST / explicit citation); INFERRED→0.4-0.9 (structural inference); AMBIGUOUS→0.1-0.3 (uncertain — flag but INCLUDE, never omit).
**Hyperedges (optional, ≤3/chunk):** `{id,label,nodes[],relation(participate_in|implement|form),confidence,confidence_score,source_file}`.
**Quality:** never invent edges (mark AMBIGUOUS if unsure); semantic_similar only when non-obvious; YAML frontmatter → copy source_url/captured_at/author/contributor onto nodes; output valid JSON only.

## Phase 1 — Extraction parity (the core)
- [x] **T1.1** Define the **relation ontology** (entity→entity) + entity-type ontology to match graphify (e.g. calls/cites/depends_on/part_of/relates_to/contradicts/…). Document in code.
- [x] **T1.2** Rewrite extraction (`backend/graph.rs`): one LLM pass per file → **entities AND typed entity→entity relations**, each with `confidence ∈ {EXTRACTED,INFERRED,AMBIGUOUS}` + `source_file` + (best-effort) `source_location`. Structured-output enforced. [gaps E + B]
- [x] **T1.3** **Storage schema** (`cache/schema.sql`): extend `edges` (or new `graph_relation` table) to hold entity→entity rows: `from_path(/memories/A), to_path(/memories/B), relation, confidence, source_file, source_location, weight`. Migration-safe (ALTER/CREATE IF NOT EXISTS).
- [ ] **T1.4** tree-sitter **AST code lane** for code files (deterministic contains/imports/inherits/calls). DECISION: chanpin corpus is ~all docs → likely ⏭️ defer for this seed, but implement the hook OR explicitly note the parity gap.

## Phase 2 — Artifacts parity
- [x] **T2.1** `graph.json` (`cache/graph_file.rs build_graph_json`): add node fields (`file_type`,`source_file`,`source_location`) and edge fields (`source_location`,`weight`), and include typed entity→entity edges.
- [x] **T2.2** New **GRAPH_REPORT.md** generator (deterministic, no LLM except suggested-questions): god nodes, **surprising connections** (needs T1.2), hyperedges, communities + cohesion, ambiguous edges, **knowledge gaps** (isolated/thin/high-ambiguity), suggested questions. Materialize as `/GRAPH_REPORT.md`.
- [x] **T2.3** Keep `KNOWLEDGE_GRAPH.md` compact (orientation); `GRAPH_REPORT.md` = the rich graphify-style report. Add to SEARCH_ALWAYS_VISIBLE.

## Phase 3 — Reconstruct KG from scratch & replace in local
- [x] **T3.1** New extraction driver (replace `examples/build_graph.rs`): runs T1.2 typed-relation extraction over a seed DB, concurrent, idempotent (wipes old edges/relations first).
- [x] **T3.2** Run it on the **local seed** (`~/.semfs/chanpin-gemma.db` — has KG already; or e5-nosum) → regenerate entities + typed relations from scratch. Preserve the embedding/vector data (only rebuild the graph tables).
- [ ] **T3.3** Materialize `KNOWLEDGE_GRAPH.md` + `GRAPH_REPORT.md` + `graph.json` on a fresh mount; verify counts + sample typed relations.
- [ ] **T3.4** Verify KG is **fully generated** (every file with entities processed; relation/entity/community counts sane; no truncation).

## Phase 4 — Re-test local
- [ ] **T4.1** Re-run case 289 **kg_on** (local), graded with `seed-2.0-lite`; compare vs kg_off + pure codex. (Research says KG won't move 289, but the user wants the measured test once the KG is fully built.)
- [ ] **T4.2** Record results in GOAL.md run log + this file.

---
## Run log
- 2026-06-07: tests cleared; H-C/H-D removed (revert to 7947f1b, commit 3a1ea50); TODO created. Starting T0.1.
- 2026-06-07: T1.1-T1.3 + T3.1 done. First rebuild=0 (strict-schema confidence_score not required + 512-tok truncation) → fixed (complete_structured_n 2048 + required). Smoke OK (3 ent/2 rel). Full rebuild running.
- 2026-06-07: T3.2 ✅ KG rebuilt from scratch on chanpin-e5-nosum: 8302 entities, 4237 typed entity→entity relations (mentions/references/relates_to/part_of/implements...), 4138 EXTRACTED+99 INFERRED. T2.1-T2.3 graph.json+GRAPH_REPORT.md done.
