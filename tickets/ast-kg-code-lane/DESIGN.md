# Design: AST knowledge-graph code lane (graphify parity)

**Date:** 2026-06-14 · **Branch base:** `feat/backend-agnostic-store`
**Ticket:** `tickets/ast-kg-code-lane/issue.md`

## Decisions locked (user, 2026-06-14)

1. **Full parity in v1** — produce the complete graphify ontology including the
   `INFERRED` edges (`calls`, cross-file `uses`) via a symbol-resolution pass,
   not just the cheap intra-file `EXTRACTED` edges.
2. **All 14 graphify grammars** up front: Python, JavaScript, TypeScript, TSX,
   Go, Rust, Java, C, C++, Ruby, C#, Kotlin, Scala, PHP.
3. **Code-specific vocabulary** — entity kinds `{class, interface, function,
   method, module}`; extend the relation vocabulary with `contains`, `method`,
   `inherits`. The doc-lane LLM ontology is left untouched.
4. **`semfs-core` owns all logic; Modal is orchestration-only.** The AST lane is
   a pure module in `crates/semfs-core`; the Modal seed-build function only
   invokes the core binary. No semfs logic in `benchmarks/modal/`.

Defaults taken on the two flagged sub-questions:
- `uses` (cross-file class usage) resolves through the **same global symbol
  table** as `calls`.
- Code files whose extension is **not** one of the 14 grammars fall back to the
  **existing LLM doc lane** (never dropped/unindexed).

## Why

`kaifa` (BackendDeveloper) is a code-heavy Workspace-Bench workspace. semfs has
no code lane: today every file — including source — goes through the LLM entity
extractor (`extract_graph`, gpt-4.1-nano), producing only a bipartite
file↔entity co-mention graph, not the typed entity→entity relations a code
graph needs. graphify's deterministic, local, free tree-sitter lane is the
parity target.

## Architecture

```
build_kg <db> <corpus_dir>
   │ per file → file_type_of(path)
   ├── "code" + supported ext ─► graph_ast::parse_file(path, full_src) ─► FileAst
   │                              (collected across all code files)
   │                                       │
   │                              graph_ast::resolve(&[FileAst]) ─► Vec<CodeRelation>
   │                              (global symbol table → calls/uses/inherits)
   │
   └── doc/pdf/image OR unsupported-ext code ─► extract_graph (LLM, existing)
                          │
                          ▼
   graph_entity (kind, file_type, source_file) + graph_relation
   (relation, confidence, confidence_score, source_location, weight) + edges
                          │
                          ▼  (unchanged) Leiden communities → KNOWLEDGE_GRAPH.md / graph.json
```

### Module: `crates/semfs-core/src/backend/graph_ast.rs` (pure)

tree-sitter only — no network, no DB, no Modal. Public surface:

```rust
pub enum Lang { Python, JavaScript, TypeScript, Tsx, Go, Rust, Java, C, Cpp, Ruby, CSharp, Kotlin, Scala, Php }
impl Lang { pub fn from_path(path: &str) -> Option<Lang>; }

pub enum CodeKind { Class, Interface, Function, Method, Module }

pub struct CodeEntity {
    pub name: String,        // simple name (Foo)
    pub qualified: String,   // module-qualified (pkg.mod.Foo.bar)
    pub kind: CodeKind,
    pub line: usize,         // 1-based → source_location "path:line"
}

pub struct FileAst {
    pub path: String,
    pub module: String,                       // derived from path
    pub entities: Vec<CodeEntity>,
    pub contains: Vec<(String, String)>,      // (parent qualified, child qualified) — EXTRACTED
    pub imports: Vec<String>,                 // imported module/symbol strings — EXTRACTED (file→module)
    pub inherits: Vec<(String, String)>,      // (class qualified, base name) — resolve target
    pub calls: Vec<(String, String, usize)>,  // (caller qualified, callee name, line) — resolve → INFERRED
    pub uses: Vec<(String, String, usize)>,   // (user qualified, class name, line) — resolve → INFERRED
}

pub fn parse_file(path: &str, src: &str) -> Option<FileAst>;   // None if Lang unsupported

pub struct CodeRelation {
    pub source: String, pub target: String, pub relation: String,
    pub confidence: String, pub confidence_score: f64,
    pub source_location: String, pub weight: f64,
}
pub fn resolve(files: &[FileAst]) -> Vec<CodeRelation>;
```

`resolve` builds `symbol_table: name → entity node path` from every file's
entities, then:
- emits `contains` / `method` (parent→child, EXTRACTED 1.0) directly from each
  `FileAst.contains` (no resolution needed);
- emits `imports` (file→module, EXTRACTED 1.0) from `imports`;
- resolves `inherits` base names → known class entities → `inherits` EXTRACTED
  (if base is a known local class) else drops;
- resolves `calls` callee names → known fn/method entities → `calls` INFERRED
  weight 0.8;
- resolves `uses` class names → known class entities → `uses` INFERRED weight
  0.8.
Unresolved refs are **dropped** (same discipline as `clean_relations`). This is
graphify's "only edges between known nodes" rule.

Each language has a `.scm` tree-sitter query (embedded `const &str`) capturing
class/interface/function/method definitions, import statements, inheritance
clauses, and call expressions. Tests exercise one representative query per
language.

### Vocabulary changes (`backend/graph.rs`)

- Extend `RELATION_TYPES` with `"contains"`, `"method"`, `"inherits"` (existing
  `imports`/`calls`/`references` stay). Doc-lane JSON schema enum is unchanged
  (these are emitted only by the AST lane, which doesn't go through the LLM).
- No struct/schema change: `graph_entity.kind` and `graph_relation.relation`
  are free-text columns; `CodeKind`/relation strings write directly.

### Driver (`crates/semfs-core/examples/build_kg.rs`)

- New signature: `build_kg <db> [corpus_dir]`. Without `corpus_dir` it behaves
  as today (LLM lane for everything — back-compat for the chanpin path).
- With `corpus_dir`: partition files into code (supported ext) vs rest. Code
  files → read **full source from `corpus_dir/<relpath>`** (not the 6000-char
  capped chunk blob the LLM lane uses) → `parse_file` → collect → one `resolve`
  pass → write entities/relations/edges. Rest → existing concurrent LLM
  `extract_graph` path.
- `source_location` populated as `"<path>:<line>"`; `confidence` =
  EXTRACTED/INFERRED per the table; `weight` = 1.0 / 0.8.

## Relation/confidence table (graphify parity)

| edge | lane | confidence | weight |
|---|---|---|---|
| `contains` (file/class → fn/class) | AST | EXTRACTED | 1.0 |
| `method` (class → method) | AST | EXTRACTED | 1.0 |
| `imports` (file → module) | AST | EXTRACTED | 1.0 |
| `inherits` (class → base) | AST + resolve | EXTRACTED | 1.0 |
| `calls` (fn → fn) | AST + resolve | INFERRED | 0.8 |
| `uses` (cross-file class usage) | AST + resolve | INFERRED | 0.8 |

## Modal seed pipeline (`benchmarks/modal/semfs_modal.py`, orchestration only)

New `build_kaifa_seed` (`@app.function(gpu="A10G", volumes={VOL: data_volume},
timeout=3600)`):
1. Stage `kaifa_raw` (BackendDeveloper_Workdir) from HF →
   `/data/corpus/kaifa_standard`.
2. Build POSIX tree + chunks + embed every chunk with **gemma-q4** on GPU
   (`SEMFS_EMBED_MODEL=gemma-q4`, ONNX `ort` CUDA EP), via the existing
   `gemma_seed` pattern.
3. Run `seed_complete.sh` to defeat the <50% incomplete-warm bug
   (`rcas/2026-06-08-partial-seed-indexing.md`).
4. Run `build_kg <db> /data/corpus/kaifa_standard` (dual lane; doc lane needs
   `OPENROUTER_API_KEY`).
5. Emit `kaifa-gemma-q4.db` → `/data/seeds/`, `data_volume.commit()`.

The Modal function shells out to the core binary; it contains **no** parsing,
embedding, or graph logic of its own.

## Testing

1. **Unit (pure, CI-safe):** per-language fixture snippets → `parse_file` +
   `resolve` → assert exact entity set (kinds, lines) and edge set (relation +
   confidence). No network.
2. **Local mount integration (goal-mandated):** create a small fixture code
   folder, mount semfs over it (real NFS/FUSE), build the KG, and assert the
   correct knowledge graph is produced (entities + typed edges with
   confidences) — end-to-end through the real mount path, not just the unit
   harness.
3. **Modal seed:** confirm the gemma seed warms to ~100% (`seed_complete.sh`)
   and `kaifa-gemma-q4.db` contains code-lane `graph_relation` rows with
   EXTRACTED/INFERRED confidences and populated `source_location`.

## Out of scope (v1)

- E2B benchmark run against the new seed (separate step; HARD RULE: benchmarks
  run on E2B, this spec only builds the seed on Modal).
- Doc-lane entity→entity relation improvements (tracked separately in the
  comparison doc).
