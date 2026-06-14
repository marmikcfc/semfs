# Ticket: AST knowledge-graph code lane (graphify parity) + backend-dev seed

**Status:** SPEC / design — NOT yet implemented. Created 2026-06-14.
**Owner:** Marmik · **Branch base:** `feat/backend-agnostic-store`
**Decisions locked (user):** (1) build the AST lane FIRST; (2) target **parity with
graphify** (`github.com/safishamsi/graphify`); (3) first experiment = **gemma for
everything** + **KG from AST (code) + documents**; (4) **build the seed on Modal using
GPUs**, **run the benchmark on E2B** (real FUSE mount).

---

## 1. Why this exists

The next benchmark persona is the **backend developer** (`kaifa` = 开发, the code-heavy
Workspace-Bench workspace). Its workspace is mostly *source code*, but **semfs has no
code lane**: today every file — including code — goes through the *LLM* entity extractor
(`backend::graph::extract_graph`, gpt-4.1-nano), which is slow, costs API tokens, and
produces only a bipartite *file↔entity co-mention* graph, **not** the typed
*entity→entity* relations a code graph needs.

This is the long-deferred **T1.4** in `benchmarks/workspace_bench/KG_PARITY_TODO.md`
("tree-sitter AST code lane … DECISION: chanpin corpus is ~all docs → defer"). chanpin
(PM) was all docs so it never mattered. `kaifa` is code, so it matters now.

graphify (the reference system we benchmark KG quality against) already has this: a
**deterministic, local, free** tree-sitter code lane. We want parity.

## 2. graphify parity spec (the target)

Source of truth: `graphify/extract.py` (fetched 2026-06-14). **14 languages**, each via a
per-language tree-sitter grammar (NOT 32 — the `graphify_explained.html` "32" figure is
wrong):

> Python, JavaScript, TypeScript, Go, Rust, Java, C, C++, Ruby, C#, Kotlin, Scala, PHP.

**Relation ontology to reproduce:**

| edge | meaning | lane | confidence |
|---|---|---|---|
| `contains` | file/class → function/class | AST | `EXTRACTED` |
| `method` | class → method | AST | `EXTRACTED` |
| `imports` / `imports_from` | file → module | AST | `EXTRACTED` |
| `inherits` | class → base class | AST | `EXTRACTED` |
| `calls` | function → function | AST + call-graph | `INFERRED` (weight 0.8) |
| `uses` | cross-file class usage | AST + resolve | `INFERRED` |

Non-code files are **not** in graphify's `extract.py` — its doc/PDF lane is a *separate*
LLM path. So "AST + documents" maps cleanly: **new AST code lane ∥ the EXISTING semfs LLM
doc lane** (`extract_graph`). No `AMBIGUOUS` label appears in graphify's code (only
`EXTRACTED` / `INFERRED`).

Full gap analysis already written: `benchmarks/workspace_bench/KG_GRAPHIFY_COMPARISON.md`
(§1 Extraction). The biggest non-code gap it names — semfs emits *entities only, no
entity→entity relations* — is in scope to close for code (AST gives them for free) and
noted as a follow-up for docs.

## 3. Proposed design (NEEDS APPROVAL — brainstorm not finished)

Implementation lands in **`crates/semfs-core/src/backend/graph.rs`** (where
`extract_graph` / `extract_entities` already live) + a new `graph_ast` submodule.

```
build_kg (examples/build_kg.rs)  ── per file ──►  file_type_of(path)
                                                      │
                        ┌─────────────────────────────┴───────────────────┐
                  "code" (.py/.ts/.go/.rs/…)                        else (doc/pdf/img/xlsx)
                        │                                                   │
              NEW: graph_ast::extract(src, lang)                 extract_graph (LLM, existing)
              tree-sitter parse → query                          gpt-4.1-nano via OpenRouter
              ▼                                                   ▼
              graph_entity (class/fn/module, kind, source_location)
              graph_relation (contains/method/imports/inherits = EXTRACTED;
                              calls/uses = INFERRED, weight 0.8)
                        └─────────────────────────┬─────────────────────────┘
                                                   ▼
                          graph_entity + graph_relation tables (same schema as today)
                          → Leiden communities → KNOWLEDGE_GRAPH.md / graph.json
```

- **Grammars:** add `tree-sitter` + the 13–14 `tree-sitter-<lang>` crates to
  `crates/semfs-core/Cargo.toml`. Per-language `.scm` queries for class/fn/import nodes.
- **EXTRACTED edges** (intra-file: contains/method/imports/inherits) are deterministic
  and cheap — **v1 target**.
- **INFERRED edges** (`calls`, cross-file `uses`) need a name-resolution pass across the
  file set — **harder; candidate for v1.1** (see Open Questions).
- Confidence column already exists on `edges` (hard-coded `INFERRED` today) — populate
  real `EXTRACTED`/`INFERRED`.
- Reuse existing community/digest pipeline (`backend::community`, `cache::graph_file`) —
  no change.

## 4. Build on Modal (GPU) — the seed pipeline

**Why Modal+GPU:** the embedding pass (gemma EmbeddingGemma-300M ONNX over the whole
code corpus) is the compute-heavy step; a GPU accelerates it. The AST lane itself is
CPU/deterministic; the LLM doc lane is network-bound (OpenRouter). Today
`benchmarks/modal/semfs_modal.py` functions request **no GPU** — adding GPU is net-new.

Pipeline (new Modal function, e.g. `build_kaifa_seed`, `@app.function(gpu="A10G",
volumes={VOL: data_volume}, timeout=3600)`):

1. **Stage corpus** — `kaifa` is NOT in `/data/corpus/` yet (only chanpin + e11). Acquire
   the `kaifa` workspace from WB-Full (HF) → `/data/corpus/kaifa_standard`. (Gap — see §6.)
2. **Embed (GPU)** — build the POSIX tree + chunks + embed every chunk with **gemma-q4**
   (`SEMFS_EMBED_MODEL=gemma-q4`, ONNX dir `/data/models/gemma_q4`). Pattern:
   `crates/semfs-core/examples/gemma_seed.rs` (re-embeds text lane at 768d). Run ONNX on
   GPU via `ort` CUDA EP. ⚠️ **incomplete-warm bug** (`rcas/2026-06-08-partial-seed-indexing.md`):
   local seeds historically index <50%; use the wait-for-completion procedure
   (`benchmarks/workspace_bench/seed_complete.sh`) so the seed is FULLY warm.
3. **Build KG** — run the NEW dual-lane build (AST for code + LLM `extract_graph` for
   docs). Needs `OPENROUTER_API_KEY` (in `.env`) for the doc lane.
4. **Emit** `kaifa-gemma-q4.db` → `/data/seeds/`, `data_volume.commit()`.

Modal refs: `benchmarks/modal/semfs_modal.py` (volume `semfs-bench-data` @ `/data`,
image ubuntu:24.04+py3.11 for glibc≥2.38, `pull_from_box`), `benchmarks/modal/README.md`.

## 5. Run on E2B (real FUSE mount)

**HARD RULE (memory `all-benchmark-tests-on-e2b`): every semfs benchmark runs on E2B, never
Modal.** Modal builds the seed; **E2B runs the agent** against it via a real `semfs mount`.

- Harness: `benchmarks/e2b/run_matrix.py` + `benchmarks/e2b/cell_driver.py` (built
  2026-06-14, committed). Point it at the `kaifa` cases + the `kaifa-gemma-q4.db` seed
  (pull from the Modal volume into the E2B template or upload at runtime).
- Arms: `plain` / `nokg` / `nokgAK` (+ KG-on arm to exercise the new code graph).
- Auth: codex → ChatGPT subscription; Claude → OpenRouter (per current matrix).
- Platform constraints + boot-prep: `tickets/workspace-bench-5arm-matrix/E2B_RUNBOOK.md`,
  `E2B_EXPERIMENT_LEDGER.md`. (Note the 8 GB RAM cap → `SEMFS_SEARCH_ONLY=on`; 1 h sandbox
  cap → the orchestrator's auto-reboot.)

## 6. Prerequisites / known gaps

- [x] **Stage `kaifa` corpus** — already on the Modal volume at
      `/data/wb/evaluation/filesys/kaifa_standard` (+ `kaifa_raw`). No HF download needed.
- [x] **AST lane implementation** (§3) — DONE 2026-06-14. `backend/graph_ast.rs`
      (parse_file + resolve, 14 grammars, full ontology) + `build_kg` dual lane +
      `seed_dir` indexer. Unit + E2E tests green. See `DESIGN.md`.
- [x] **Seed build on Modal** — `build_kaifa_seed` (orchestration-only) =
      `seed_dir` (gemma-q4 ONNX) → coverage gate → `build_kg` dual lane → commit
      `kaifa-gemma-q4.db`. Run with `SEMFS_SEED_ONLY=1 modal run …::build_kaifa_seed`.
- [ ] **GPU acceleration** — currently CPU (fastembed's prebuilt ONNX runtime is CPU).
      `gpu="A10G"` needs an `ort` CUDA-EP build; deferred (correctness first). Follow-up.
- [ ] Build a **KG-on E2B arm** so the code graph is actually exercised (current matrix is
      KG-off `nokg`). Follow-up (HARD RULE: the benchmark runs on E2B, not Modal).
- [x] Confirm the gemma seed warms to ~100% — `seed_dir` indexes synchronously, so the
      <50% incomplete-warm bug doesn't apply; `build_kaifa_seed` prints a coverage gate.

## 7. Open design questions (resolve in brainstorm before coding)

1. **`calls` / cross-file `uses`** — needs a symbol-resolution pass. v1 = intra-file
   `EXTRACTED` relations only, defer `calls`/`uses` to v1.1? Or full parity in v1?
2. **Grammar set** — all 14 graphify languages up front (heavier compile), or start with
   the languages the `kaifa` corpus actually contains (need its composition first)?
3. **Schema** — current `edges` is file→entity co-mention; `graph_relation` holds typed
   entity→entity. Confirm the AST lane writes `graph_relation` with `source_location` +
   `weight` + real `confidence` (closes the comparison-doc §1 gap).
4. **Determinism/perf** — tree-sitter is local+fast; confirm we can parse the whole `kaifa`
   tree in-process during the seed build without the LLM doc-lane's per-file API latency.

## 8. References (project docs)

| Doc | What |
|---|---|
| `benchmarks/workspace_bench/KG_GRAPHIFY_COMPARISON.md` | **the gap analysis** — semfs KG vs graphify, §1 extraction |
| `benchmarks/workspace_bench/SEMFS_GRAPHIFY_DESIGN.md` | §II.9 tree-sitter, §II.11 property graph & confidence |
| `benchmarks/workspace_bench/KG_PARITY_TODO.md` | **T1.4** (this work) + T2.x artifact parity |
| `crates/semfs-core/src/backend/graph.rs` | `extract_graph` / `extract_entities` — where the AST lane slots in |
| `crates/semfs-core/examples/build_kg.rs` | KG builder/driver (per-file `file_type_of` dispatch) |
| `crates/semfs-core/examples/build_graph.rs` | L7 LLM entity extraction (doc lane) |
| `crates/semfs-core/examples/gemma_seed.rs` | gemma re-embed seed builder |
| `benchmarks/modal/semfs_modal.py` · `README.md` | Modal volume + functions (add GPU here) |
| `benchmarks/workspace_bench/seed-coverage.md` · `seed_complete.sh` | seed coverage + full-warm procedure |
| `benchmarks/e2b/run_matrix.py` · `cell_driver.py` | E2B run harness (built 2026-06-14) |
| `tickets/workspace-bench-5arm-matrix/E2B_RUNBOOK.md` · `E2B_EXPERIMENT_LEDGER.md` | E2B platform rules |
| `rcas/2026-06-08-partial-seed-indexing.md` | incomplete-warm seed bug (<50% index) |
| `github.com/safishamsi/graphify` (`extract.py`) | parity target (14 langs + ontology) |
| memory: `all-benchmark-tests-on-e2b`, `e2b-mount-platform`, `semfs-seed-quality-findings` | platform + seed gotchas |
