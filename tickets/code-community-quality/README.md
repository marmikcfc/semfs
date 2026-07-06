# Community / `ppr_map` quality for code — resolve cross-file refs (SEM-51 sub-item)

**Folder:** `tickets/code-community-quality/` · **Linear:** [SEM-57](https://linear.app/semfs/issue/SEM-57) (sub-issue of SEM-51) · Date: 2026-07-06

## ⚠️ FINDING (2026-07-06) — item 1 done, but communities need a DIFFERENT fix

Item 1 (settle-time global `resolve()`) is **implemented + verified**: live `graph_relation` `calls`
now match batch exactly (E2E: 2→9, identical set). This improves **`ppr_off`/`ppr_on`** (they traverse
`graph_relation`). BUT it does **NOT** move communities, because — verified from code —
**`materialize_projection` never reads `calls`**: the file↔file graph is `edges` (file→*defined*
entity) + kNN only (`graph_file.rs` `build_digest`/`build_file_graph`); `calls` live in `graph_relation`
(`sqlite_vec.rs::write_ast_extraction`). Code files never share *defined* entities, so the ONLY
cross-file connector for code is **kNN**, and `file_mean_embeddings` queries only `vchunks`, never
`vchunks_code` (`graph_file.rs:38`) → a live pure-code mount gets **zero kNN edges** → fragments.
Batch's `seed_dir` is single-lane (all in `vchunks`) → full kNN → clusters (parity E2E: batch 1 vs
live 6 communities). **The community-parity lever is the `vchunks_code` kNN omission, not calls.**
New scope item **1b**: `file_mean_embeddings` must include the code lane (`vchunks_code`), handling
per-lane dims (text 768d vs code model) so cross-lane cosine doesn't break. Item 1 stays (KG
completeness win). Status: uncommitted.

---

## Context

`materialize_projection` clusters files on a **file↔file graph** = shared-entity edges (`from_file_entities`)
+ kNN embedding edges (`add_knn_edges`), then Louvain→Leiden. A file's "entities" are all its `edges`
rows including `calls`/`imports`, so **a resolved cross-file call IS a shared entity** → the call graph
*should* cluster code. Communities materialize correctly on both batch and live (SEM-56) — the open
issue is **quality**, and it's entirely downstream of **cross-file reference resolution**, i.e. SEM-51's
core (entity resolution + cross-lane bridge).

Observed: pure-code / doc-poor corpora fragment (my re-verify: 4 communities / 7 entities — clustered,
not singletons, but weak). Batch sftpgo got 7 real communities (global `resolve()` + doc lane). So this
does **not** block the SEM-50 arms (batch seed); it matters for live-mounted code seeds and general
`ppr_map` quality.

## Scope (this sub-item = the two concrete, bounded wins)

1. **Close the live incremental-resolve gap (highest value — DO THIS FIRST).**
   **Root cause (traced):** two resolvers in `graph_ast.rs` — batch `resolve(&[FileAst])` builds an
   in-memory symbol table of ALL files then matches (complete); live `resolve_refs(file, lookup)`
   (SEM-55) matches each ref against a DB closure of *already-indexed* symbols (incomplete →
   order-dependent → a call to a later-indexed file is dropped and never retried). Same inner matching
   loop, different symbol source = the duplication smell.
   **Fix:** queue-settle is the "all files indexed" moment = batch's precondition. So in the settle
   hook (`fs::refresh_knowledge_graph` / SEM-56's `kg_refresh`), **before `materialize_projection`**,
   run one global resolve over all code files and rewrite `calls`:
   - collect all code files (`graph_ast::Lang::from_path(fp).is_some()`), read each file's content
     from the DB, `graph_ast::parse_file` → `Vec<FileAst>`;
   - `graph_ast::resolve(&asts)` (the REAL batch fn — reuse, do not reimplement);
   - replace the live `calls` edges/relations with the resolved set (drop old `calls` for those files,
     write the resolved ones), mirroring `build_kg.rs`'s AST `calls` write exactly.
   Bounded: runs once per settle (debounced, not per-write); single-writer on the daemon conn lock.
   `resolve_refs` stays for fast per-file feedback. Optional (flag, don't force): converge the two by
   expressing `resolve` via `resolve_refs` + an in-memory lookup to kill the duplication.
2. **Project `imports` at package granularity.** Files importing the same package cluster even when
   call resolution is sparse — a cheap structural signal to add to the file↔file graph.

Out of scope here (larger SEM-51 work, separate items): kNN tuning; full doc↔code cross-lane linking;
entity-name normalization.

## Success / test
- A live-mounted multi-file code repo where files call across files yields communities that reflect
  the **call/import structure** (files that call each other land together), measurably closer to the
  batch `materialize_kg` result on the same corpus. Compare `graph_community` (community count +
  member distribution) live-vs-batch on the same input; the gap should shrink.
- Default + gliner-kg suites stay green.

## Priority
**Defer** — quality improvement, not on the critical path to the sftpgo arms numbers (batch seed
already has good communities). Promote if the arms show `ppr_map` underperforming *because of* weak
communities (i.e. with evidence).

Related: SEM-51 (parent — dual-lane KG / entity resolution), SEM-54, SEM-55, SEM-56, SEM-50.
