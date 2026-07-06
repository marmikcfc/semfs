# Wire the tree-sitter AST code lane into the daemon's LIVE index path (dual-lane on mount)

**Folder:** `tickets/live-ast-code-lane/` · **Linear:** [SEM-55](https://linear.app/semfs/issue/SEM-55) · Date: 2026-07-06

## ✅ DONE + verified (uncommitted) — 2026-07-06

The live path is now **dual-lane**: `index_graph` routes code files (`Lang::from_path` Some) →
`index_graph_ast` (tree-sitter), non-code → gliner. Added `graph_ast::resolve_refs` (per-file
counterpart to global `resolve()`, closure-based DB symbol lookup) + `CodeKind::from_label`.
`calls`-resolution: **option (a)** — incremental DB resolve (`SELECT path,kind FROM graph_entity
WHERE name=?`), self-healing on re-index. Verified (independent re-run): mounting 2 `.go` files →
`graph_entity` kinds **function=2, method=1, class=1** + relations **calls=2, contains=4, imports=1**,
**zero** gliner labels on the `.go` files. Default suite 382 green (flaky NFS-port passed);
gliner-kg 390 green. Changes: `sqlite_vec.rs`, `graph_ast.rs`, `graph_ast/tests.rs`.
**Only Leiden communities (`ppr_map`) remain batch-only** (inherently global).

---

## Problem (this is "2b" from the mount-vs-batch analysis)

The daemon's **live** KG path (`SqliteVecStore::index_graph`) now runs **gliner on *every* file**
(via `index_graph_gliner`, added in SEM-54 follow-up). For **code** files that means gliner reads
raw Go/Rust/… as prose — it has no idea about functions, methods, `calls`, or `imports`. So a
**live-mounted code repo gets a badly degraded KG**: e.g. for sftpgo the *batch* seed has
**864 functions / 292 methods / 6140 `calls` / 491 `imports`** (from tree-sitter), while the live
path produces a handful of gliner-guessed entities. This makes `ppr_off`/`ppr_on` much weaker when
a seed is built live vs batch.

The **batch** builder (`examples/build_kg.rs`) already does the right thing — it's **dual-lane**:
code files → the tree-sitter **AST lane**, non-code → the gliner/LLM doc lane. The live path is
single-lane (gliner-only). This ticket brings the AST lane to the live path.

## Goal

Make the live path **dual-lane, matching batch `build_kg`**: when indexing a **code** file
(`graph_ast::Lang::from_path(path).is_some()`), extract with the **tree-sitter AST lane**; otherwise
keep the gliner doc lane. Result: `graph_entity` on a live-mounted code repo has `Function`/`Class`/
`Method`/`Module`/`Interface` kinds + `contains`/`imports`/`calls` edges — not gliner-on-code.

## Current state (verified)

- **AST lane API** (`crates/semfs-core/src/backend/graph_ast.rs`):
  - `Lang::from_path(path) -> Option<Lang>` — is this a supported code file? (14 langs)
  - `parse_file(path, src) -> Option<FileAst>` — **per-file**: `CodeEntity`s (name + `CodeKind`),
    `imports` (resolved, EXTRACTED/1.0), intra-file `contains`/`method` edges, and **unresolved
    cross-file refs** (`calls`/`uses`/`inherits`).
  - `resolve(&[FileAst]) -> Vec<CodeRelation>` — **GLOBAL** pass: matches call refs to defined
    entities across all files → `calls` edges (INFERRED/0.8). *This is the wrinkle — see below.*
- **Batch write** (`examples/build_kg.rs`, "AST code-lane writes" ~line 325): per-file entities →
  `graph_entity` (node key = module-qualified name) + `edges` (`contains`/`method`/`imports`);
  `resolve()` output → `calls` edges. Mirror this schema exactly.
- **Live path** (`crates/semfs-core/src/backend/sqlite_vec.rs`): `index_graph` → `index_graph_gliner`
  for all files (SEM-54 follow-up). Add the code branch here.

## The wrinkle: `resolve()` is global, the live path is per-file

`resolve()` needs ALL files to match a `calls` ref in file A to the function defined in file B.
The live path indexes **one file at a time**, so a per-file parse yields the file's own
entities + `imports` + intra-file edges + **unresolved** call refs. Options for `calls`
(implementer picks the cleanest, documents the choice + trade-off):
- **(a) Incremental resolve against the DB** — for each call ref, look up `graph_entity` for a
  matching function name and write the `calls` edge if found. Order-dependent but converges as more
  files index; files indexed later resolve backward on their own re-index.
- **(b) Name-keyed `calls` edges** — write `calls` edges to the ref's node path (module-qualified
  name) regardless of whether that entity exists yet; it connects when/if that name is defined
  (mirrors the doc lane's slugified-name matching). Simplest, fully incremental.
- Do **not** attempt a global re-resolve on every file write (O(N²)). Entities + `imports` +
  `contains` are the high-value, fully-incremental part; `calls` is best-effort per-file.

## Scope / constraints (CLAUDE.md)

- Enable the AST branch when **`gliner_mode_active()`** (the dual-lane mode) — the live path becomes
  dual-lane exactly like batch `build_kg`. The AST lane itself has **no heavy deps** (tree-sitter is
  always compiled), so no new feature/dep — reuse the existing `gliner-kg` gate for the *mode*.
- **Idempotent re-derive** per file: `drop_file_edges` + `drop_file_relations` before writing (as
  `index_graph_gliner` already does), so re-index replaces cleanly.
- Keep gliner for non-code, and the LLM path as the non-gliner fallback. Surgical; match style.
- GPU-free, no ort, no cloud/GPU during dev.

## Testing (verify, don't assume — two bugs hid behind "it compiles" this session)

1. Default suite green: `cargo test -p semfs-core` (~380) + `--features gliner-kg` suite.
2. Both feature builds compile (`-p semfs-core` and `-p semfs --bin semfs --features gliner-kg`).
3. Focused unit test for the code-branch routing + write (can use a tiny Go snippet + `parse_file`).
4. **Decisive live E2E (mirror SEM-54's repro, code files):**
   - `mkdir /tmp/astlf` and drop 2–3 small `.go` files where one calls a function defined in another.
   - `SEMFS_EMBED_BACKEND=local SEMFS_EMBED_MODEL=gemma <semfs> mount astlf --path /tmp/astlf --backend nfs --no-push --no-sync`
   - wait for the async indexer, then
     `sqlite3 ~/.semfs/astlf.db "SELECT kind,COUNT(*) FROM graph_entity GROUP BY kind; SELECT relation,COUNT(*) FROM graph_relation GROUP BY relation; SELECT edge_kind,COUNT(*) FROM edges GROUP BY edge_kind;"`
   - **EXPECT:** `Function`/`Method`/`Class` entity kinds (from AST) + `imports`/`contains`/`calls`
     edges — NOT the gliner dev-pack kinds (software/module/…) for the `.go` files. Report the actual
     numbers. Baseline (current live path): those `.go` files produce gliner-guessed kinds only.
   - unmount + clean up; kill stray daemon; `rm -f ~/.semfs/astlf.db*`.

## Reference
- **SEM-54** (`tickets/mount-live-index/`) — the gliner-into-live wiring; **same shape**, mirror its
  `index_graph_gliner` / `GlinerCell` / idempotent-write patterns.
- `examples/build_kg.rs` — the batch dual-lane partition + AST-write logic to mirror column-for-column.

Related: SEM-50 (SWE-Atlas benchmark), SEM-51 (dual-lane KG), SEM-54 (live gliner KG).
